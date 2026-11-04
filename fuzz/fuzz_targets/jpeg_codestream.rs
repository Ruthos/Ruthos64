//! A bare JPEG codestream: the scan-header repair, then libjpeg-turbo.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut copy = data.to_vec();
    let _ = ruthos_dicom::jpeg::sanitize_sequential_sos(&mut copy);
    // The guard's limit on the frame header first, as in Ruthos itself.
    if let Some(h) = ruthos_dicom::guard::jpeg_header(data) {
        if h.width as u64 * h.height.max(1) as u64 > 16 * 1024 * 1024 {
            return;
        }
    }
    let _ = ruthos_dicom::jpeg::decode(data);
});
