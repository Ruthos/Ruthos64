//! Refinement of the real model's masks on the phantom, run only where the model
//! is installed: set `RUTHOS_SAM_ROOT` to a data folder holding `models/`.
#![cfg(feature = "ai")]

mod common;

use common::{plate, surface_dice, thickness, Side, SIZE};
use ruthos_segment::ai::{to_slice, Click, Region, Sam2, SAM2_TINY};
use ruthos_segment::refine::{refine_slice, resample, Edge, BAND};

fn gray(values: &[f32]) -> Vec<f32> {
    let (centre, width) = (300.0, 1500.0);
    values
        .iter()
        .map(|v| ((v - (centre - width / 2.0)) / width).clamp(0.0, 1.0))
        .collect()
}

#[test]
fn the_real_model_on_the_phantom() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_ROOT") else {
        return;
    };
    let mut model = Sam2::load(&SAM2_TINY, std::path::Path::new(&root)).expect("the model loads");
    eprintln!("on the GPU: {}", model.on_gpu);
    for (side, side_name) in [(Side::Soft, "soft"), (Side::Air, "sinus")] {
        for region in [
            Region::whole(SIZE, SIZE),
            Region::between([186.0, 166.0], [326.0, 346.0], SIZE, SIZE).unwrap(),
        ] {
            for t in [0.5, 1.0, 2.0, 4.0] {
                for gap in [None, Some(2.0)] {
                    let p = plate(t, side, gap, 7);
                    let g = gray(&p.values);
                    let f = model
                        .encode(&region.take(&g, SIZE), region.width, region.height)
                        .unwrap();
                    let click = region.local(Click {
                        x: p.click.0,
                        y: p.click.1,
                        include: true,
                    });
                    let d = model.click(&f, &[click], region.width, region.height).unwrap();
                    let sam = region.place(&to_slice(&d.mask, region.width, region.height), SIZE, SIZE);
                    let native = resample(&d.mask, 1024, region.width, region.height);
                    let inner: Vec<bool> = native.iter().map(|v| *v > 0.0).collect();
                    let hu = region.take(&p.values, SIZE);
                    let r = refine_slice(&hu, &inner, region.width, region.height, Edge::Bone, BAND);
                    let refined = region.place(&r.mask, SIZE, SIZE);
                    let (a, b) = (thickness(&sam, &p), thickness(&refined, &p));
                    eprintln!(
                    "{side_name:5} {:>3}x{:<3} t={t:.1}: SAM {a:.2} ({:+.0}%) refined {b:.2} ({:+.0}%) | sDice@1 {:.3}->{:.3} @0.5 {:.3}->{:.3} | area {} {:?}",
                    region.width,
                    region.height,
                    (a / t - 1.0) * 100.0,
                    (b / t - 1.0) * 100.0,
                    surface_dice(&sam, &p.truth, 1.0),
                    surface_dice(&refined, &p.truth, 1.0),
                    surface_dice(&sam, &p.truth, 0.5),
                    surface_dice(&refined, &p.truth, 0.5),
                    sam.iter().filter(|v| **v).count(),
                    r.reading,
                );
                    if t >= 1.0 {
                        assert!((b / t - 1.0).abs() < 0.11, "{t} mm came to {b:.2}");
                    }
                    if gap.is_some() {
                        let hit = |m: &[bool]| m.iter().zip(&p.block).filter(|(a, b)| **a && **b).count();
                        eprintln!(
                            "   gap: block voxels SAM {} refined {} of {}",
                            hit(&sam),
                            hit(&refined),
                            p.block.iter().filter(|v| **v).count()
                        );
                        assert!(hit(&refined) <= hit(&sam) + 8, "the block must stay out");
                    }
                }
            }
        }
    }
}

/// A fracture across a thin ring: does refinement reopen what the model filled?
#[test]
fn a_fracture_line_on_the_real_model() {
    let Some(root) = std::env::var_os("RUTHOS_SAM_ROOT") else {
        return;
    };
    let mut model = Sam2::load(&SAM2_TINY, std::path::Path::new(&root)).expect("the model loads");
    let size = 600;
    let (cx, cy, radius, wall) = (300.0f32, 260.0f32, 60.0f32, 3.0f32);
    for gap in [3.0f32, 6.0] {
        let mut hu = vec![40.0f32; size * size];
        let mut in_gap = vec![false; size * size];
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = dx.hypot(dy);
                let on = (d - radius + wall / 2.0).abs() <= wall / 2.0;
                let cut = dx > 0.0 && dy.abs() < gap / 2.0;
                if on && !cut {
                    hu[x + size * y] = 1200.0;
                }
                in_gap[x + size * y] = on && cut;
            }
        }
        let g = gray(&hu);
        let click = Click {
            x: 241.5,
            y: 260.0,
            include: true,
        };
        for region in [
            Region::whole(size, size),
            Region::between([224.0, 184.0], [376.0, 336.0], size, size).unwrap(),
        ] {
            let f = model
                .encode(&region.take(&g, size), region.width, region.height)
                .unwrap();
            let d = model
                .click(&f, &[region.local(click)], region.width, region.height)
                .unwrap();
            let sam = region.place(&to_slice(&d.mask, region.width, region.height), size, size);
            let fine: Vec<bool> = resample(&d.mask, 1024, region.width, region.height)
                .iter()
                .map(|v| *v > 0.0)
                .collect();
            let r = refine_slice(
                &region.take(&hu, size),
                &fine,
                region.width,
                region.height,
                Edge::Bone,
                BAND,
            );
            let refined = region.place(&r.mask, size, size);
            let filled = |m: &[bool]| m.iter().zip(&in_gap).filter(|(a, b)| **a && **b).count();
            eprintln!(
                "gap {gap} on {}x{}: gap voxels {} filled by SAM {}, refined {}",
                region.width,
                region.height,
                in_gap.iter().filter(|v| **v).count(),
                filled(&sam),
                filled(&refined)
            );
        }
    }
}
