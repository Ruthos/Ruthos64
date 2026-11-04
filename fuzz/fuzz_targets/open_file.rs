//! A whole file: dicom-rs parsing, Ruthos's metadata and ultrasound regions.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(obj) = ruthos_dicom::open::open_from_bytes(data) {
        let meta = ruthos_dicom::meta::meta_from_object(&obj);
        let _ = meta.us_region_at(10.0, 10.0);
        let _ = meta.length([0.0, 0.0], [10.0, 10.0]);
    }
});
