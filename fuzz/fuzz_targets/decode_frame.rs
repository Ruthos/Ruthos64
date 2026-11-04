//! A whole file decoded: the guard, then libjpeg-turbo, OpenJPEG, CharLS,
//! RLE and deflate, whichever its transfer syntax names.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(obj) = ruthos_dicom::open::open_from_bytes(data) {
        for frame in 0..3 {
            if ruthos_dicom::pixels::decode_object_frame(&obj, frame).is_err() {
                break;
            }
        }
    }
});
