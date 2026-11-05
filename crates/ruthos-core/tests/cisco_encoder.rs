//! Fetches Cisco's real binary, which CI should not do on every push:
//! `cargo test -p ruthos-core --test cisco_encoder -- --ignored`

use std::sync::atomic::AtomicBool;

use openh264::OpenH264API;
use ruthos_core::movie;

#[test]
#[ignore = "downloads from Cisco"]
fn the_binary_from_cisco_installs_loads_and_encodes() {
    let dir = std::env::temp_dir().join(format!("ruthos-cisco-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(!movie::encoder_installed(&dir));

    let path = movie::install_encoder(&dir).unwrap();
    assert!(movie::encoder_installed(&dir));

    let api = OpenH264API::from_blob_path(&path).unwrap();
    let out = dir.join("out.mp4");
    let (w, h) = (64u32, 48u32);
    let mut frame = |i: usize| Ok(vec![(i * 20) as u8; (w * h * 3) as usize]);
    let cancel = AtomicBool::new(false);
    movie::write_mp4(api, &out, w, h, 10.0, 5, &mut frame, &|_, _| {}, &cancel).unwrap();
    assert!(std::fs::metadata(&out).unwrap().len() > 100);
    std::fs::remove_dir_all(&dir).ok();
}
