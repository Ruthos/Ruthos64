//! Refinement against a phantom of known thickness. These need no model: the
//! mask is what the model's looks like, a coarse grid of cells dilated past the
//! object, matched to what `refine_sam.rs` measures from the real thing (a 1 mm
//! plate at about twice its thickness on a whole slice).

mod common;

use std::time::Instant;

use common::{plate, surface_dice, thickness, Phantom, Side, SIZE};
use ruthos_segment::refine::{flag_slices, refine_slice, resample, Edge, BAND, BAND_RANGE};

/// A mask from coarse cells two voxels wide, as on a whole 512 pixel slice, one
/// voxel past any cell the object touches: a 1 mm plate comes to about 2 mm.
fn model_mask(truth: &[bool]) -> Vec<bool> {
    grown(truth, 1)
}

fn grown(truth: &[bool], by: usize) -> Vec<bool> {
    const CELL: usize = 2;
    let size = SIZE / CELL;
    let mut cells = vec![false; size * size];
    for i in (0..truth.len()).filter(|i| truth[*i]) {
        cells[(i % SIZE) / CELL + size * ((i / SIZE) / CELL)] = true;
    }
    let mut mask = vec![false; SIZE * SIZE];
    for y in by..SIZE - by {
        for x in by..SIZE - by {
            mask[x + SIZE * y] =
                (y - by..=y + by).any(|v| (x - by..=x + by).any(|u| cells[u / CELL + size * (v / CELL)]));
        }
    }
    mask
}

fn refine(p: &Phantom, mask: &[bool], band: usize) -> Vec<bool> {
    refine_slice(&p.values, mask, SIZE, SIZE, Edge::Bone, band).mask
}

/// The furthest voxel of `after` from any of `before`, counting diagonals as one.
fn furthest(before: &[bool], after: &[bool]) -> usize {
    let on: Vec<(usize, usize)> = (0..SIZE * SIZE)
        .filter(|i| before[*i])
        .map(|i| (i % SIZE, i / SIZE))
        .collect();
    (0..SIZE * SIZE)
        .filter(|i| after[*i] && !before[*i])
        .map(|i| {
            let (x, y) = (i % SIZE, i / SIZE);
            on.iter()
                .map(|(u, v)| x.abs_diff(*u).max(y.abs_diff(*v)))
                .min()
                .unwrap_or(usize::MAX)
        })
        .max()
        .unwrap_or(0)
}

/// Acceptance 1: within 10% of true thickness for 1, 2 and 4 mm over the slices of
/// a plate, where the model alone is well outside it for the thinner plates.
#[test]
fn plates_come_to_their_true_thickness() {
    for t in [1.0, 2.0, 4.0] {
        let mut sum = 0.0;
        const SLICES: u64 = 8;
        for seed in 1..=SLICES {
            let p = plate(t, Side::Soft, None, seed);
            let mask = model_mask(&p.truth);
            let (before, after) = (thickness(&mask, &p), thickness(&refine(&p, &mask, BAND), &p));
            assert!((after / t - 1.0).abs() < 0.15, "{t} mm plate came to {after:.2}");
            assert!((after - t).abs() < (before - t).abs(), "{before:.2} then {after:.2}");
            if t < 4.0 {
                assert!(before / t > 1.2, "the mask should start too thick: {before:.2}");
            }
            sum += after;
        }
        let mean = sum / SLICES as f32;
        eprintln!("{t} mm: refined {mean:.3}");
        assert!((mean / t - 1.0).abs() < 0.1, "{t} mm plate came to {mean:.3}");
    }
}

/// A mask three times too thick, past the erosion: the core itself is partly soft
/// tissue, which must not stay.
#[test]
fn a_mask_that_overshoots_its_core_still_comes_close() {
    for t in [1.0, 2.0, 4.0] {
        let p = plate(t, Side::Soft, None, 8);
        let mask = grown(&p.truth, 3);
        let (before, after) = (thickness(&mask, &p), thickness(&refine(&p, &mask, BAND), &p));
        eprintln!("{t} mm from a fat mask: {before:.2} then {after:.2}");
        assert!((after / t - 1.0).abs() < 0.2, "{t} mm came to {after:.2}");
    }
}

/// The wall of a sinus has air on one side and soft tissue on the other, so a
/// single outer level puts both edges wrong.
#[test]
fn the_wall_of_a_sinus_has_an_edge_on_each_side() {
    for t in [1.0, 2.0, 4.0] {
        let p = plate(t, Side::Air, None, 3);
        let after = thickness(&refine(&p, &model_mask(&p.truth), BAND), &p);
        eprintln!("{t} mm sinus wall: {after:.2}");
        assert!((after / t - 1.0).abs() < 0.15, "{t} mm wall came to {after:.2}");
    }
}

/// A plate thinner than the point spread function cannot be measured by any
/// threshold: its image is the blur. Refinement must still improve on the mask.
#[test]
fn half_a_millimetre_is_closer_though_not_true() {
    let p = plate(0.5, Side::Soft, None, 2);
    let mask = model_mask(&p.truth);
    let (before, after) = (thickness(&mask, &p), thickness(&refine(&p, &mask, BAND), &p));
    eprintln!("0.5 mm: model {before:.2}, refined {after:.2}");
    assert!(after < before * 0.7, "{before:.2} then {after:.2}");
}

/// Acceptance 2. At a 1 mm tolerance the model's boundary is already within
/// reach on these plates, so nothing can improve on it; refinement must not make
/// it worse, and at half that tolerance it must improve on every plate.
#[test]
fn the_surface_comes_closer_to_the_truth() {
    for t in [0.5, 1.0, 2.0, 4.0] {
        let p = plate(t, Side::Soft, None, 5);
        let mask = model_mask(&p.truth);
        let after = refine(&p, &mask, BAND);
        let at = |tolerance: f32| {
            (
                surface_dice(&mask, &p.truth, tolerance),
                surface_dice(&after, &p.truth, tolerance),
            )
        };
        let (wide, fine) = (at(1.0), at(0.5));
        eprintln!("{t} mm: at 1 mm {wide:?}, at 0.5 mm {fine:?}");
        assert!(wide.1 >= wide.0, "{t} mm at 1 mm: {wide:?}");
        assert!(fine.1 > fine.0, "{t} mm at 0.5 mm: {fine:?}");
    }
}

/// Acceptance 3: a second block two voxels away stays out, however the noise
/// falls in the valley, and the plate's side facing it still finds its edge.
#[test]
fn a_block_across_a_two_voxel_gap_stays_out() {
    for seed in 1..=8 {
        for t in [1.0, 2.0, 4.0] {
            for side in [Side::Soft, Side::Air] {
                let p = plate(t, side, Some(2.0), seed);
                let mask = model_mask(&p.truth);
                let after = refine(&p, &mask, BAND);
                let taken = |m: &[bool]| m.iter().zip(&p.block).filter(|(a, b)| **a && **b).count();
                assert!(
                    taken(&after) <= taken(&mask),
                    "{t} mm, seed {seed}: took {} block voxels from {}",
                    taken(&after),
                    taken(&mask)
                );
                let got = thickness(&after, &p);
                assert!((got / t - 1.0).abs() < 0.2, "{t} mm beside a block came to {got:.2}");
            }
        }
    }
}

/// Acceptance 4, over every plate and every band width.
#[test]
fn nothing_joins_from_beyond_the_band() {
    for band in BAND_RANGE {
        for t in [0.5, 1.0, 2.0, 4.0] {
            for side in [Side::Soft, Side::Air] {
                for gap in [None, Some(2.0)] {
                    let p = plate(t, side, gap, 11);
                    let mask = model_mask(&p.truth);
                    let reach = furthest(&mask, &refine(&p, &mask, band));
                    assert!(reach <= band, "{reach} voxels out on a band of {band}");
                }
            }
        }
    }
}

/// A mask too small for the model's edge: refinement grows it back.
#[test]
fn a_mask_that_fell_short_is_grown_to_the_edge() {
    let p = plate(4.0, Side::Soft, None, 6);
    let mut mask = p.truth.clone();
    for _ in 0..2 {
        let was = mask.clone();
        for y in 1..SIZE - 1 {
            for x in 1..SIZE - 1 {
                let i = x + SIZE * y;
                mask[i] = was[i] && was[i - 1] && was[i + 1];
            }
        }
    }
    let got = thickness(&refine(&p, &mask, BAND), &p);
    assert!(
        (got / 4.0 - 1.0).abs() < 0.1,
        "came to {got:.2} from {:.2}",
        thickness(&mask, &p)
    );
}

fn square(values: &mut [f32], from: (usize, usize), to: (usize, usize), v: f32, width: usize) {
    for y in from.1..to.1 {
        for x in from.0..to.0 {
            values[x + width * y] = v;
        }
    }
}

/// A cortex that fades for a voxel or two, inside the mask, is followed across.
#[test]
fn a_weak_gap_inside_the_mask_is_crossed() {
    let (w, h) = (80, 40);
    let mut values = vec![40.0; w * h];
    square(&mut values, (10, 18), (70, 22), 1200.0, w);
    // The thin wall fades here, to just under the half-maximum.
    square(&mut values, (39, 18), (41, 22), 500.0, w);
    let mut mask = vec![false; w * h];
    square_mask(&mut mask, (8, 17), (72, 23), w);
    let r = refine_slice(&values, &mask, w, h, Edge::Bone, 2);
    assert!(r.mask[40 + w * 20], "the faded part stays");
    assert!(r.mask[20 + w * 20] && r.mask[60 + w * 20], "and both sides of it");
    assert!(!r.mask[20 + w * 25], "soft tissue stays out");
}

fn square_mask(mask: &mut [bool], from: (usize, usize), to: (usize, usize), width: usize) {
    for y in from.1..to.1 {
        for x in from.0..to.0 {
            mask[x + width * y] = true;
        }
    }
}

/// A ring of bone round a sinus: the model took the ring, and the sinus is not
/// filled, though it is a hole in the slice.
#[test]
fn a_ring_does_not_fill_the_space_it_surrounds() {
    let (w, h) = (100, 100);
    let mut values = vec![40.0; w * h];
    let mut mask = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let d = (x as f32 - 50.0).hypot(y as f32 - 50.0);
            if (30.0..35.0).contains(&d) {
                values[x + w * y] = 1200.0;
            }
            if d < 30.0 {
                values[x + w * y] = -900.0;
            }
            mask[x + w * y] = (28.0..37.0).contains(&d);
        }
    }
    let r = refine_slice(&values, &mask, w, h, Edge::Bone, BAND);
    assert!(!r.mask[50 + w * 50], "the sinus stays empty");
    assert!(r.mask[50 + w * 18], "the ring stays");
}

/// A long bone in section, which the model took whole: the marrow stays, because
/// it is deep in the mask, and it also stays where a fracture opens the cortex.
#[test]
fn marrow_stays_in_a_bone_taken_whole() {
    let (w, h) = (120, 120);
    let mut values = vec![40.0; w * h];
    let mut mask = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let d = (x as f32 - 60.0).hypot(y as f32 - 60.0);
            values[x + w * y] = if d < 30.0 {
                60.0
            } else if d < 40.0 {
                1200.0
            } else {
                40.0
            };
            mask[x + w * y] = d < 42.0;
        }
    }
    let closed = refine_slice(&values, &mask, w, h, Edge::Bone, BAND);
    assert!(closed.mask[60 + w * 60], "the centre of the marrow");
    assert!(closed.mask[60 + w * 32], "the marrow beside the cortex");
    // A fracture through the cortex, three voxels wide.
    for x in 80..104 {
        for y in 58..61 {
            values[x + w * y] = 40.0;
        }
    }
    let broken = refine_slice(&values, &mask, w, h, Edge::Bone, BAND);
    assert!(broken.mask[60 + w * 60], "still the centre of the marrow");
    assert!(!broken.mask[100 + w * 59], "and the line is open at the surface");
}

/// A small hole in the bone, inside the mask, is filled.
#[test]
fn a_hole_within_reach_is_filled() {
    let (w, h) = (40, 40);
    let mut values = vec![40.0; w * h];
    square(&mut values, (8, 8), (32, 32), 1200.0, w);
    values[20 + w * 11] = 40.0;
    let mut mask = vec![false; w * h];
    square_mask(&mut mask, (7, 7), (33, 33), w);
    let r = refine_slice(&values, &mask, w, h, Edge::Bone, BAND);
    assert!(r.mask[20 + w * 11]);
}

/// A soft tissue object stops at bone beside it, which the bone rule would not.
#[test]
fn soft_tissue_stops_at_bone() {
    let (w, h) = (60, 60);
    let mut values = vec![-100.0; w * h];
    square(&mut values, (15, 15), (45, 45), 60.0, w);
    square(&mut values, (45, 15), (55, 45), 1200.0, w);
    let mut mask = vec![false; w * h];
    square_mask(&mut mask, (14, 14), (47, 46), w);
    let r = refine_slice(&values, &mask, w, h, Edge::SoftTissue, BAND);
    assert!(r.mask[30 + w * 30]);
    assert!(!r.mask[46 + w * 30], "bone is not soft tissue");
}

#[test]
fn a_mask_with_nothing_to_read_is_left_alone() {
    let (w, h) = (20, 20);
    let empty = vec![false; w * h];
    let values = vec![40.0; w * h];
    assert_eq!(refine_slice(&values, &empty, w, h, Edge::Bone, BAND).mask, empty);
    // The whole slice: no ring beyond it.
    let all = vec![true; w * h];
    let r = refine_slice(&values, &all, w, h, Edge::Bone, BAND);
    assert_eq!((r.mask, r.reading), (all, None));
    // Nothing brighter in the mask than beyond it.
    let mut one = vec![false; w * h];
    square_mask(&mut one, (5, 5), (15, 15), w);
    let flat = refine_slice(&values, &one, w, h, Edge::Bone, BAND);
    assert_eq!(flat.mask, one);
    assert_eq!(flat.reading, None);
}

/// A mask one voxel thick has no voxels left to erode to: it still works.
#[test]
fn a_mask_with_no_depth_keeps_a_core() {
    let (w, h) = (40, 40);
    let mut values = vec![40.0; w * h];
    square(&mut values, (5, 18), (35, 21), 1200.0, w);
    let mut mask = vec![false; w * h];
    square_mask(&mut mask, (5, 19), (35, 20), w);
    let r = refine_slice(&values, &mask, w, h, Edge::Bone, BAND);
    assert!(r.reading.is_some());
    assert!(r.mask[20 + w * 18] && r.mask[20 + w * 20], "grown to the wall");
    assert!(!r.mask[20 + w * 22]);
}

/// Acceptance 5: a 300 slice series, one mask a slice, in under ten seconds.
#[test]
fn three_hundred_slices_take_under_ten_seconds() {
    use rayon::prelude::*;
    let p = plate(2.0, Side::Air, None, 9);
    let mask = model_mask(&p.truth);
    let start = Instant::now();
    let total: usize = (0..300usize)
        .into_par_iter()
        .map(|_| refine(&p, &mask, BAND).iter().filter(|v| **v).count())
        .sum();
    let took = start.elapsed().as_secs_f32();
    eprintln!("300 slices in {took:.2} s");
    assert!(total > 0);
    assert!(took < 10.0, "{took} s");
}

#[test]
fn logits_resample_to_the_slice_and_keep_their_edge() {
    let size = 16;
    let mut logits = vec![-4.0; size * size];
    for y in 0..size {
        for x in 0..size / 2 {
            logits[x + size * y] = 4.0;
        }
    }
    let out = resample(&logits, size, 4, 2);
    assert_eq!(out.len(), 8);
    let on: Vec<bool> = out.iter().map(|v| *v > 0.0).collect();
    assert_eq!(&on[..4], &[true, true, false, false]);
    assert_eq!(&on[4..], &[true, true, false, false]);
    // Upward, a boundary lands between pixels rather than on cells.
    let wide = resample(&logits, size, 64, 1);
    let edge = wide.iter().position(|v| *v < 0.0).unwrap();
    assert!((30..=34).contains(&edge), "{edge}");
    assert!(wide[edge - 1] < 4.0, "a smooth edge, not a step");
}

/// Acceptance 7's first half; the second is the panel's.
#[test]
fn a_slice_unlike_its_neighbours_is_flagged() {
    let areas: Vec<(usize, usize)> = (10..20).map(|z| (z, if z == 14 { 40 } else { 100 })).collect();
    let flagged = flag_slices(&areas);
    assert!(flagged.contains(&14), "{flagged:?}");
    let steady: Vec<(usize, usize)> = (0..10).map(|z| (z, 100 + z * 5)).collect();
    assert!(flag_slices(&steady).is_empty());
    assert!(flag_slices(&[(3, 100)]).is_empty(), "nothing to compare with");
    // The last slice has one neighbour.
    assert_eq!(flag_slices(&[(0, 100), (1, 100), (2, 10)]), [1, 2]);
}
