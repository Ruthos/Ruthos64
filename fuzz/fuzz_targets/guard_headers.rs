//! The guard's own marker parsers.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ruthos_dicom::guard::jpeg_header(data);
    let _ = ruthos_dicom::guard::jpeg_ls_header(data);
    let _ = ruthos_dicom::guard::jpeg2000_header(data);
});
