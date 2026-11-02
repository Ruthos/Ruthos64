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
// JPEG-lossy: 12-bit JPEG Extended (Process 2 & 4); the Rust JPEG decoder
// only handles 8-bit lossy. Tracked as a known gap.
const KNOWN_UNSUPPORTED: &[&str] = &["rtdose.dcm", "JPEG-lossy.dcm"];

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
