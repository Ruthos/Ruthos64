//! The memory limit, in a file of its own: the limits are global, so lowering
//! them here must not reach tests decoding images on other threads, and each
//! test file is its own process.

use std::path::PathBuf;

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::tags;

use ruthos_dicom::guard::{self, Limits};
use ruthos_dicom::open::open_any;
use ruthos_dicom::{decode_all_frames, decode_frame};

/// Acceptance 4: a three-frame image over a tiny memory limit is refused whole,
/// with the limit stated, and a single frame still opens.
#[test]
fn a_loop_over_the_memory_limit_is_refused_whole() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom/CT_small.dcm");
    let mut obj = open_any(&src, None).unwrap();
    let data = obj.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap().to_vec();
    let three: Vec<u8> = data.iter().chain(&data).chain(&data).copied().collect();
    obj.put(DataElement::new(tags::PIXEL_DATA, VR::OW, PrimitiveValue::from(three)));
    obj.put(DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, dicom_value!(Str, "3")));
    let multi = std::env::temp_dir().join(format!("ruthos-limit-{}.dcm", std::process::id()));
    obj.write_to_file(&multi).unwrap();

    assert_eq!(decode_all_frames(&multi).unwrap().len(), 3, "whole, within the limit");
    let saved = guard::limits();
    guard::set_limits_exact(Limits {
        series_bytes: 100_000,
        ..saved
    });
    let whole = decode_all_frames(&multi);
    let single = decode_frame(&multi, 1);
    guard::set_limits_exact(saved);
    std::fs::remove_file(&multi).ok();
    let e = whole.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("memory"), "the whole loop is refused with the limit: {e}");
    assert!(single.is_ok(), "one frame still opens: {:?}", single.err());
}
