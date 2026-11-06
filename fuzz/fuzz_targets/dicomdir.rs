#![no_main]
//! A DICOMDIR comes from whoever made the disc. Reading one must not panic,
//! and no path it yields may lead out of its folder.

use libfuzzer_sys::fuzz_target;
use std::path::Component;

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = ruthos_dicom::dicomdir::parse(data) else {
        return;
    };
    for series in dir.series() {
        for file in &series.files {
            assert!(file.path.components().all(|c| matches!(c, Component::Normal(_))));
        }
    }
});
