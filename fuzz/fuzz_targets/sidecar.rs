//! The ruthos-case.json sidecar a teaching copy carries.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ruthos_core::teaching::Sidecar>(data);
});
