//! The real model on a synthetic stack, run only where it is installed: set
//! `RUTHOS_SAM_ROOT` to a data folder holding `models/`. Without it these tests
//! pass without doing anything, as they do in CI.
#![cfg(feature = "ai")]

use ruthos_segment::ai::{to_slice, Click, Region, Sam2, SAM2_TINY};

/// A bright disc on a dark field, drifting from slice to slice.
fn slice(z: usize, size: usize) -> (Vec<f32>, Vec<bool>) {
    let (cx, cy, r) = (60.0 + z as f32 * 1.5, 90.0, 26.0);
    let mut gray = vec![0.05; size * size];
    let mut truth = vec![false; size * size];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d <= r {
                gray[x + size * y] = 0.9;
                truth[x + size * y] = true;
            }
        }
    }
    (gray, truth)
}

fn dice(a: &[bool], b: &[bool]) -> f32 {
    let both = a.iter().zip(b).filter(|(x, y)| **x && **y).count() as f32;
    let total = (a.iter().filter(|v| **v).count() + b.iter().filter(|v| **v).count()) as f32;
    2.0 * both / total.max(1.0)
}

#[test]
fn one_click_finds_an_object_and_follows_it_through_the_stack() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_ROOT") else {
        return;
    };
    let mut model = Sam2::load(&SAM2_TINY, std::path::Path::new(&root)).expect("the model loads");
    eprintln!("on the GPU: {}", model.on_gpu);
    let size = 200;

    let (gray, truth) = slice(0, size);
    let f = model.encode(&gray, size, size).unwrap();
    let click = Click {
        x: 60.0,
        y: 90.0,
        include: true,
    };
    let first = model.click(&f, &[click], size, size).unwrap();
    let got = dice(&to_slice(&first.mask, size, size), &truth);
    eprintln!("first mask: dice {got:.3}, iou {:.3}", first.iou);
    assert!(got > 0.9, "first mask dice {got}");

    let mut bank = model.begin(&f, &first).unwrap();
    for step in 1..=12 {
        let (gray, truth) = slice(step, size);
        let f = model.encode(&gray, size, size).unwrap();
        let d = model.track(&f, &mut bank, step).unwrap();
        let got = dice(&to_slice(&d.mask, size, size), &truth);
        eprintln!("step {step}: dice {got:.3}, score {:.1}", d.score);
        assert!(got > 0.9, "step {step} dice {got}");
    }
}

#[test]
fn an_exclude_click_changes_the_mask() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_ROOT") else {
        return;
    };
    let mut model = Sam2::load(&SAM2_TINY, std::path::Path::new(&root)).expect("the model loads");
    let size = 200;
    let (gray, _) = slice(0, size);
    let f = model.encode(&gray, size, size).unwrap();
    let include = Click {
        x: 60.0,
        y: 90.0,
        include: true,
    };
    let exclude = Click {
        x: 60.0,
        y: 90.0,
        include: false,
    };
    let count = |d: &ruthos_segment::ai::Decoded| to_slice(&d.mask, size, size).iter().filter(|v| **v).count();
    let inside = model.click(&f, &[include], size, size).unwrap();
    let out = model.click(&f, &[exclude], size, size).unwrap();
    assert!(count(&inside) > count(&out), "excluding the object must lose it");
}

/// Thin cortical bone on soft tissue, with a fracture gap on its right.
fn fractured_ring(size: usize) -> (Vec<f32>, Vec<bool>) {
    let (cx, cy, radius, wall, gap) = (300.0, 260.0, 60.0, 3.0, 3.0);
    let mut gray = vec![0.25; size * size];
    let mut truth = vec![false; size * size];
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            let gapped = dx > 0.0 && dy.abs() < gap / 2.0;
            if (d - radius + wall / 2.0).abs() <= wall / 2.0 && !gapped {
                gray[x + size * y] = 0.95;
                truth[x + size * y] = true;
            }
        }
    }
    (gray, truth)
}

#[test]
fn a_tight_box_recovers_detail_the_whole_slice_loses() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_ROOT") else {
        return;
    };
    let mut model = Sam2::load(&SAM2_TINY, std::path::Path::new(&root)).expect("the model loads");
    let size = 600;
    let (gray, truth) = fractured_ring(size);
    let click = Click {
        x: 241.5,
        y: 260.0,
        include: true,
    };
    let mut score = |region: Region| {
        let f = model
            .encode(&region.take(&gray, size), region.width, region.height)
            .unwrap();
        let d = model
            .click(&f, &[region.local(click)], region.width, region.height)
            .unwrap();
        let inner = to_slice(&d.mask, region.width, region.height);
        dice(&region.place(&inner, size, size), &truth)
    };
    let whole = score(Region::whole(size, size));
    let tight = score(Region::between([224.0, 184.0], [376.0, 336.0], size, size).unwrap());
    eprintln!("whole slice dice {whole:.3}, tight box dice {tight:.3}");
    assert!(tight > 0.85, "a tight box dice {tight}");
    assert!(tight > whole + 0.1, "the box must help: {whole} then {tight}");
}

/// The real download into an empty folder, about 400 MB: set
/// `RUTHOS_SAM_DOWNLOAD` to a folder to run it.
#[test]
fn the_download_installs_a_working_model() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_DOWNLOAD") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    assert!(!SAM2_TINY.installed(&root));
    let cancel = std::sync::atomic::AtomicBool::new(false);
    ruthos_segment::ai::install(&SAM2_TINY, &root, &|_, _| {}, &cancel).expect("the download");
    assert!(SAM2_TINY.installed(&root));
    assert_eq!(SAM2_TINY.download_bytes(&root), 0);
    let model = Sam2::load(&SAM2_TINY, &root).expect("what was downloaded loads");
    eprintln!("downloaded model on the GPU: {}", model.on_gpu);
}
