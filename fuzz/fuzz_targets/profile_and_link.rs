//! What arrives from outside for guided review: `ruthos:` links, signature
//! checks and the allowlist on `.ruthosprofile` files, and `ruthos.ini`.
#![no_main]
use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use libfuzzer_sys::fuzz_target;
use ruthos_session::{deployment, links, profile};

// Ed25519's base point: a valid public key, so a signature reaches the
// verifier rather than stopping at the key's decoding.
const KEY: &str = "WGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmY=";

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = links::parse(text);
    let _ = deployment::from_ini(text);
    let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
    let keys = BTreeMap::from([("k".to_string(), KEY.to_string())]);
    let _ = profile::check(text, now, &keys, &profile::Pins::new());
});
