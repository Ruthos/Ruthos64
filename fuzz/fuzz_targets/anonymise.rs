//! The teaching-copy allowlist profile and UID remapping on an arbitrary
//! data set, then the verification pass over the result.
#![no_main]
use libfuzzer_sys::fuzz_target;

use ruthos_dicom::deid::{build_teaching_dataset, DateShift, DeidParams, VerifyContext};

fuzz_target!(|data: &[u8]| {
    let Ok(obj) = ruthos_dicom::open::open_from_bytes(data) else { return };
    let params = DeidParams {
        teaching_name: "Fuzz case",
        reference: "FUZZ-0001",
        title: Some("Fuzz"),
        secret: &[7u8; 32],
        dates: DateShift::new(400, false),
        software: "fuzz",
    };
    let out = build_teaching_dataset(&obj, &params);
    let mut findings = Vec::new();
    ruthos_dicom::deid::verify::check_dataset(&out, "", "fuzz", &VerifyContext::default(), &mut findings);
});
