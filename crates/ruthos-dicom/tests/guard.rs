//! The decode guard on whole files (Decoder Hardening Specification,
//! acceptance checks 1 to 5).

use std::path::{Path, PathBuf};

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::tags;

use ruthos_dicom::guard::{self, Limits, MISMATCH};
use ruthos_dicom::open::open_any;
use ruthos_dicom::{decode_all_frames, decode_frame};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/pydicom")
        .join(name)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruthos-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Copy a sample with some attributes changed.
fn altered(src: &str, out: &str, change: impl FnOnce(&mut dicom_object::DefaultDicomObject)) -> PathBuf {
    let mut obj = open_any(&testdata(src), None).unwrap();
    change(&mut obj);
    let p = temp(out);
    obj.write_to_file(&p).unwrap();
    p
}

fn set_rows(obj: &mut dicom_object::DefaultDicomObject, rows: u16) {
    obj.put(DataElement::new(tags::ROWS, VR::US, dicom_value!(U16, [rows])));
}

fn refused(path: &Path) -> String {
    match decode_frame(path, 0) {
        Ok(_) => panic!("{} decoded but should have been refused", path.display()),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_jpeg_whose_frame_header_claims_a_huge_image_is_refused() {
    // Patch the SOF of a real JPEG to claim 65,535 x 65,535.
    let p = altered("JPEG-lossy.dcm", "huge.dcm", |obj| {
        let e = obj.element(tags::PIXEL_DATA).unwrap();
        let mut frags: Vec<Vec<u8>> = e.fragments().unwrap().iter().map(|f| f.to_vec()).collect();
        let f = &mut frags[0];
        let at = (0..f.len() - 1)
            .find(|&i| f[i] == 0xFF && (0xC0..=0xC3).contains(&f[i + 1]))
            .expect("a SOF");
        f[at + 5..at + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let offsets = e.offset_table().map(|t| t.to_vec()).unwrap_or_default();
        obj.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            dicom_core::value::Value::PixelSequence(dicom_core::value::PixelFragmentSequence::new(offsets, frags)),
        ));
    });
    let e = refused(&p);
    assert!(e.contains(MISMATCH), "{e}");
}

#[test]
fn jpeg_2000_and_jpeg_ls_files_that_disagree_with_their_attributes_are_refused() {
    for (src, out) in [
        ("JPEG2000.dcm", "j2k.dcm"),
        ("MR_small_jpeg_ls_lossless.dcm", "jls.dcm"),
    ] {
        let rows = open_any(&testdata(src), None)
            .unwrap()
            .element(tags::ROWS)
            .unwrap()
            .to_int::<u16>()
            .unwrap();
        let p = altered(src, out, |obj| set_rows(obj, rows + 7));
        let e = refused(&p);
        assert!(e.contains(MISMATCH), "{src}: {e}");
    }
}

#[test]
fn native_pixel_data_shorter_than_described_is_refused_without_a_panic() {
    let p = altered("CT_small.dcm", "short.dcm", |obj| {
        let data = obj.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap().to_vec();
        obj.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::from(data[..data.len() / 2].to_vec()),
        ));
    });
    let e = refused(&p);
    assert!(e.contains("shorter"), "{e}");
}

#[test]
fn every_sample_still_decodes_and_a_loop_over_the_memory_limit_is_refused_whole() {
    // Acceptance 5: no real image is refused (the samples test covers each
    // one in detail; this asserts the guard lets them all through).
    for name in [
        "CT_small.dcm",
        "MR_small.dcm",
        "JPEG-LL.dcm",
        "JPEG-lossy.dcm",
        "JPEG2000.dcm",
        "MR_small_jpeg_ls_lossless.dcm",
        "MR_small_RLE.dcm",
        "SC_rgb.dcm",
        "color-pl.dcm",
    ] {
        decode_frame(&testdata(name), 0).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // Acceptance 4: a three-frame image over a tiny memory limit is refused
    // whole, with the limit stated, and a single frame still opens.
    let multi = altered("CT_small.dcm", "three.dcm", |obj| {
        let data = obj.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap().to_vec();
        let three: Vec<u8> = data.iter().chain(&data).chain(&data).copied().collect();
        obj.put(DataElement::new(tags::PIXEL_DATA, VR::OW, PrimitiveValue::from(three)));
        obj.put(DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, dicom_value!(Str, "3")));
    });
    assert_eq!(decode_all_frames(&multi).unwrap().len(), 3, "whole, within the limit");
    let saved = guard::limits();
    guard::set_limits_exact(Limits {
        series_bytes: 100_000,
        ..saved
    });
    let whole = decode_all_frames(&multi);
    let single = decode_frame(&multi, 1);
    guard::set_limits_exact(saved);
    let e = whole.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("memory"), "the whole loop is refused with the limit: {e}");
    assert!(single.is_ok(), "one frame still opens: {:?}", single.err());
}

#[test]
fn bytes_in_memory_open_and_decode_like_the_file() {
    for name in ["CT_small.dcm", "JPEG2000.dcm", "ExplVR_LitEndNoMeta.dcm"] {
        let bytes = std::fs::read(testdata(name)).unwrap();
        let obj = ruthos_dicom::open::open_from_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        if name != "ExplVR_LitEndNoMeta.dcm" {
            let a = ruthos_dicom::pixels::decode_object_frame(&obj, 0).unwrap();
            let b = decode_frame(&testdata(name), 0).unwrap();
            assert_eq!(
                (a.width, a.height, a.min, a.max),
                (b.width, b.height, b.min, b.max),
                "{name}"
            );
        }
    }
    assert!(ruthos_dicom::open::open_from_bytes(b"not dicom at all").is_err());
    assert!(ruthos_dicom::open::open_from_bytes(&[]).is_err());
}
