//! Read and decode every sample file under `testdata/pydicom`.
//!
//! This is the codec coverage test: if a transfer syntax we claim to support
//! stops decoding, this fails and names the file.

use std::path::PathBuf;

use ruthos_dicom::{decode_frame, read_meta, DicomError};

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom")
}

/// Files that are valid DICOM but not images (no pixel data to decode).
const NOT_IMAGES: &[&str] = &["ExplVR_LitEndNoMeta.dcm"];

/// Files whose transfer syntax we knowingly do not decode yet.
// rtdose: 32-bit pixel data (RT Dose, not an image for the viewer).
// JPEG-lossy (12-bit JPEG Extended) now decodes through libjpeg-turbo.

const KNOWN_UNSUPPORTED: &[&str] = &["rtdose.dcm"];

#[test]
fn every_sample_reads_metadata() {
    let mut count = 0;
    for entry in std::fs::read_dir(testdata()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("dcm") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        match read_meta(&path) {
            Ok(meta) => {
                assert!(!meta.sop_instance_uid.is_empty(), "{name}: empty SOP instance UID");
                assert!(!meta.study_instance_uid.is_empty(), "{name}: empty study UID");
                if !NOT_IMAGES.contains(&name.as_str()) {
                    assert!(meta.rows > 0 && meta.columns > 0, "{name}: no image dimensions");
                }
                count += 1;
            }
            Err(e) => panic!("{name}: metadata failed: {e}"),
        }
    }
    assert!(count >= 10, "expected at least 10 samples, found {count}");
}

#[test]
fn every_sample_decodes() {
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(testdata()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("dcm") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if NOT_IMAGES.contains(&name.as_str()) || KNOWN_UNSUPPORTED.contains(&name.as_str()) {
            continue;
        }
        eprintln!("decoding {name}");
        match decode_frame(&path, 0) {
            Ok(frame) => {
                assert_eq!(
                    frame.byte_len(),
                    (frame.width * frame.height) as usize * 4,
                    "{name}: payload size does not match dimensions"
                );
                if !frame.is_color() {
                    assert!(frame.max >= frame.min, "{name}: bad min/max");
                    let (c, w) = frame.default_window();
                    assert!(w > 0.0, "{name}: window width {w} (center {c})");
                }
            }
            Err(DicomError::Pixels(e)) | Err(DicomError::Unsupported(e)) => failures.push(format!("{name}: {e}")),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(failures.is_empty(), "decode failures:\n{}", failures.join("\n"));
}

#[test]
fn ct_small_values_are_hounsfield() {
    let frame = decode_frame(&testdata().join("CT_small.dcm"), 0).unwrap();
    assert_eq!((frame.width, frame.height), (128, 128));
    // Rescale intercept of -1024 puts air well below zero.
    assert!(frame.min < -500.0, "min {} looks unrescaled", frame.min);
    // This sample has no Window Center/Width, so the default window must fall
    // back to the full dynamic range.
    assert!(frame.meta.window_center.is_none());
    assert_eq!(frame.default_window(), frame.full_dynamic_window());
}

#[test]
fn dataset_without_meta_header_is_accepted() {
    let path = testdata().join("ExplVR_LitEndNoMeta.dcm");
    let meta = read_meta(&path).expect("bare data set should open");
    assert_eq!(meta.transfer_syntax, "1.2.840.10008.1.2.1");
    assert!(!meta.sop_instance_uid.is_empty());
    // This particular sample carries no pixel data, so decoding must fail
    // cleanly rather than panic.
    assert!(decode_frame(&path, 0).is_err());
}

#[test]
fn rgb_sample_is_color() {
    let frame = decode_frame(&testdata().join("SC_rgb.dcm"), 0).unwrap();
    assert!(frame.is_color());
    assert_eq!((frame.width, frame.height), (100, 100));
}

#[test]
fn multiframe_file_decodes_whole_and_matches_per_frame() {
    use dicom_core::value::PrimitiveValue;
    use dicom_core::{dicom_value, DataElement, VR};
    use dicom_dictionary_std::{tags, uids};
    use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
    use ruthos_dicom::{decode_all_frames, FramePixels};

    // A synthetic 4-frame RGB loop, 8x6, each frame a different flat colour.
    let (w, h, frames) = (8u16, 6u16, 4u32);
    let mut pixels = Vec::new();
    for k in 0..frames {
        for _ in 0..(w as usize * h as usize) {
            pixels.extend_from_slice(&[(k * 60) as u8, 100, 200 - (k * 40) as u8]);
        }
    }
    let mut obj = InMemDicomObject::new_empty();
    let mut put = |tag, vr, v: &str| obj.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
    put(tags::SOP_CLASS_UID, VR::UI, uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE);
    put(tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4");
    put(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.3");
    put(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.3.1");
    put(tags::MODALITY, VR::CS, "US");
    put(tags::PATIENT_NAME, VR::PN, "LOOP^TEST");
    put(tags::PATIENT_ID, VR::LO, "L1");
    put(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "RGB");
    put(tags::NUMBER_OF_FRAMES, VR::IS, "4");
    put(tags::FRAME_TIME, VR::DS, "40");
    obj.put(DataElement::new(tags::SAMPLES_PER_PIXEL, VR::US, dicom_value!(U16, [3])));
    obj.put(DataElement::new(tags::PLANAR_CONFIGURATION, VR::US, dicom_value!(U16, [0])));
    obj.put(DataElement::new(tags::ROWS, VR::US, dicom_value!(U16, [h])));
    obj.put(DataElement::new(tags::COLUMNS, VR::US, dicom_value!(U16, [w])));
    obj.put(DataElement::new(tags::BITS_ALLOCATED, VR::US, dicom_value!(U16, [8])));
    obj.put(DataElement::new(tags::BITS_STORED, VR::US, dicom_value!(U16, [8])));
    obj.put(DataElement::new(tags::HIGH_BIT, VR::US, dicom_value!(U16, [7])));
    obj.put(DataElement::new(tags::PIXEL_REPRESENTATION, VR::US, dicom_value!(U16, [0])));
    obj.put(DataElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::from(pixels)));
    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE)
        .media_storage_sop_instance_uid("1.2.3.4")
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .build()
        .unwrap();
    let dir = std::env::temp_dir().join(format!("ruthos-loop-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("loop.dcm");
    obj.with_exact_meta(meta).write_to_file(&path).unwrap();

    let meta = read_meta(&path).unwrap();
    assert_eq!(meta.number_of_frames, 4);
    assert_eq!(meta.frame_time_ms, Some(40.0));

    let all = decode_all_frames(&path).unwrap();
    assert_eq!(all.len(), 4);
    for k in 0..frames {
        let single = decode_frame(&path, k).unwrap();
        let (FramePixels::Rgba(a), FramePixels::Rgba(b)) = (&all[k as usize].pixels, &single.pixels) else {
            panic!("expected colour frames");
        };
        assert_eq!(a, b, "frame {k} differs between the two decode paths");
        assert_eq!(a[0], (k * 60) as u8, "frame {k} has the wrong colour");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
