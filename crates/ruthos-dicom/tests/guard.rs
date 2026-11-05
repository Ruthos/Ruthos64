//! The decode guard on whole files (Decoder Hardening Specification,
//! acceptance checks 1 to 5).

use std::path::{Path, PathBuf};

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::tags;

use ruthos_dicom::guard::MISMATCH;
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
fn every_sample_still_decodes() {
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

/// A JPEG 2000 sample rewritten with these fragments, offset table and frame count.
fn split_jpeg_2000(
    out: &str,
    frames: u32,
    offsets: Vec<u32>,
    fragments: impl FnOnce(Vec<u8>) -> Vec<Vec<u8>>,
) -> PathBuf {
    altered("JPEG2000.dcm", out, |obj| {
        let mut stream = obj.element(tags::PIXEL_DATA).unwrap().fragments().unwrap()[0].to_vec();
        if stream.len() % 2 == 1 {
            stream.push(0);
        }
        obj.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            dicom_core::value::Value::PixelSequence(dicom_core::value::PixelFragmentSequence::new(
                offsets,
                fragments(stream),
            )),
        ));
        obj.put(DataElement::new(
            tags::NUMBER_OF_FRAMES,
            VR::IS,
            dicom_value!(Str, frames.to_string()),
        ));
    })
}

/// Two frames in three fragments, where the offset table sends the second
/// frame to the third fragment. The guard must see the bytes the decoder gets.
fn second_frame_in_third_fragment(out: &str, third: Vec<u8>) -> PathBuf {
    let path = split_jpeg_2000(out, 2, vec![0], |stream| vec![stream, vec![0; 2], third]);
    let mut obj = open_any(&path, None).unwrap();
    let e = obj.element(tags::PIXEL_DATA).unwrap();
    let frags: Vec<Vec<u8>> = e.fragments().unwrap().iter().map(|f| f.to_vec()).collect();
    let second = (frags[0].len() + 8 + frags[1].len() + 8) as u32;
    obj.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        dicom_core::value::Value::PixelSequence(dicom_core::value::PixelFragmentSequence::new(vec![0, second], frags)),
    ));
    obj.write_to_file(&path).unwrap();
    path
}

#[test]
fn a_later_frame_cannot_hide_a_jp2_wrapper_behind_the_offset_table() {
    let mut jp2 = vec![0u8, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10];
    jp2.extend([0u8; 8]);
    let p = second_frame_in_third_fragment("hidden-jp2.dcm", jp2);
    let e = decode_frame(&p, 1)
        .expect_err("the wrapped frame must not decode")
        .to_string();
    assert!(e.contains(MISMATCH), "reached the decoder: {e}");
    let e = decode_all_frames(&p).expect_err("nor the whole image").to_string();
    assert!(e.contains(MISMATCH), "reached the decoder: {e}");
}

#[test]
fn a_later_frame_cannot_hide_a_wrong_size_behind_the_offset_table() {
    let mut other = open_any(&testdata("JPEG2000.dcm"), None)
        .unwrap()
        .element(tags::PIXEL_DATA)
        .unwrap()
        .fragments()
        .unwrap()[0]
        .to_vec();
    // Patch Xsiz in SIZ, four bytes in at Lsiz.
    other[8..12].copy_from_slice(&[0, 0, 0xFF, 0xFF]);
    if other.len() % 2 == 1 {
        other.push(0);
    }
    let p = second_frame_in_third_fragment("hidden-size.dcm", other);
    let e = decode_frame(&p, 1).expect_err("must be refused").to_string();
    assert!(e.contains(MISMATCH), "reached the decoder: {e}");
}

#[test]
fn one_frame_split_across_fragments_still_opens() {
    let whole = decode_frame(&testdata("JPEG2000.dcm"), 0).unwrap();
    let p = split_jpeg_2000("split.dcm", 1, vec![], |stream| {
        let half = (stream.len() / 2) & !1;
        vec![stream[..half].to_vec(), stream[half..].to_vec()]
    });
    let frame = decode_frame(&p, 0).unwrap();
    assert_eq!((frame.width, frame.height), (whole.width, whole.height));
}

#[test]
fn two_frames_each_in_two_fragments_still_open() {
    let whole = decode_frame(&testdata("JPEG2000.dcm"), 0).unwrap();
    let path = split_jpeg_2000("two-by-two.dcm", 2, vec![0], |stream| {
        let half = (stream.len() / 2) & !1;
        let (a, b) = stream.split_at(half);
        vec![a.to_vec(), b.to_vec(), a.to_vec(), b.to_vec()]
    });
    let mut obj = open_any(&path, None).unwrap();
    let e = obj.element(tags::PIXEL_DATA).unwrap();
    let frags: Vec<Vec<u8>> = e.fragments().unwrap().iter().map(|f| f.to_vec()).collect();
    let second = (frags[0].len() + 8 + frags[1].len() + 8) as u32;
    obj.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        dicom_core::value::Value::PixelSequence(dicom_core::value::PixelFragmentSequence::new(vec![0, second], frags)),
    ));
    obj.write_to_file(&path).unwrap();
    let frame = decode_frame(&path, 1).unwrap();
    assert_eq!((frame.width, frame.height), (whole.width, whole.height));
    assert_eq!(decode_all_frames(&path).unwrap().len(), 2);
}
