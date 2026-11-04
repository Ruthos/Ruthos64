//! A whole file decoded: the guard, then libjpeg-turbo, OpenJPEG, CharLS,
//! RLE and deflate, whichever its transfer syntax names.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(
    init: ruthos_dicom::guard::set_limits_exact(ruthos_dicom::guard::Limits {
        // Small, so that running out of memory means something passed the
        // guard: an image it allows can reach gigabytes under AddressSanitizer.
        side: 4096,
        pixels: 8 << 20,
        frames: 64,
        fragment_bytes: 32 << 20,
        series_bytes: 256 << 20,
    }),
    |data: &[u8]| {
    if let Ok(obj) = ruthos_dicom::open::open_from_bytes(data) {
        for frame in 0..3 {
            if ruthos_dicom::pixels::decode_object_frame(&obj, frame).is_err() {
                break;
            }
        }
    }
});
