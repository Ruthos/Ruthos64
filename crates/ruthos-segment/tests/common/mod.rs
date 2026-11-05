//! A synthetic CT slice: plates of known thickness in a body, blurred by a point
//! spread function like a bone kernel's, sampled to voxels, with noise.
#![allow(dead_code)]

pub const SIZE: usize = 512;
/// Millimetres a voxel.
pub const VOXEL: f32 = 0.4;
/// Full width at half maximum of the point spread function, in millimetres.
pub const FWHM: f32 = 0.7;
pub const NOISE: f32 = 40.0;
/// Plates lean this far from the vertical, so edges fall at every sub-voxel phase.
pub const TILT: f32 = 3.0 * std::f32::consts::PI / 180.0;
const BONE: f32 = 1200.0;
const SOFT: f32 = 40.0;
const AIR: f32 = -1000.0;
/// Fine samples to a voxel's side.
const FINE: usize = 4;
/// The plate's length in millimetres.
pub const LENGTH: f32 = 50.0;

/// What lies to the left of the plate; to the right is always soft tissue.
#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Soft,
    /// As the wall of a sinus.
    Air,
}

pub struct Phantom {
    pub values: Vec<f32>,
    pub truth: Vec<bool>,
    pub thickness: f32,
    pub click: (f32, f32),
    /// Rows clear of the plate's ends, over which thickness is measured.
    pub rows: std::ops::Range<usize>,
    /// Voxels of a second, separate block, when there is one.
    pub block: Vec<bool>,
}

fn blur(fine: &mut [f32], n: usize) {
    let sigma = FWHM / 2.3548 / (VOXEL / FINE as f32);
    let reach = (sigma * 4.0).ceil() as isize;
    let kernel: Vec<f32> = (-reach..=reach)
        .map(|k| (-(k as f32 * k as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let mut row = vec![0.0; n];
    for y in 0..n {
        for (x, out) in row.iter_mut().enumerate() {
            *out = kernel
                .iter()
                .enumerate()
                .map(|(k, w)| w * fine[(x as isize + k as isize - reach).clamp(0, n as isize - 1) as usize + n * y])
                .sum();
        }
        fine[n * y..n * (y + 1)].copy_from_slice(&row);
    }
    let mut column = vec![0.0; n];
    for x in 0..n {
        for (y, out) in column.iter_mut().enumerate() {
            *out = kernel
                .iter()
                .enumerate()
                .map(|(k, w)| w * fine[x + n * (y as isize + k as isize - reach).clamp(0, n as isize - 1) as usize])
                .sum();
        }
        for (y, v) in column.iter().enumerate() {
            fine[x + n * y] = *v;
        }
    }
}

/// `gap` is the width in voxels between the plate and a block to its right, with
/// no block when it is None.
pub fn plate(thickness: f32, side: Side, gap: Option<f32>, seed: u64) -> Phantom {
    let n = SIZE * FINE;
    let h = VOXEL / FINE as f32;
    let centre = SIZE as f32 * VOXEL / 2.0;
    let half = thickness / 2.0;
    let across = |x: f32, y: f32| (x - centre) * TILT.cos() - (y - centre) * TILT.sin();
    let along = |x: f32, y: f32| (x - centre) * TILT.sin() + (y - centre) * TILT.cos();
    let in_plate = |x: f32, y: f32| across(x, y).abs() <= half && along(x, y).abs() <= LENGTH / 2.0;
    let in_block = |x: f32, y: f32| {
        gap.is_some_and(|g| {
            let from = half + g * VOXEL;
            let a = across(x, y);
            a >= from && a <= from + 8.0 && along(x, y).abs() <= LENGTH / 2.0
        })
    };
    let in_body = |x: f32, y: f32| ((x - centre) / 80.0).powi(2) + ((y - centre) / 70.0).powi(2) < 1.0;
    let in_sinus = |x: f32, y: f32| {
        let a = across(x, y);
        a < 0.0 && a > -25.0 && along(x, y).abs() <= LENGTH / 2.0 + 4.0
    };
    let mut fine = vec![0f32; n * n];
    for iy in 0..n {
        for ix in 0..n {
            let (x, y) = ((ix as f32 + 0.5) * h, (iy as f32 + 0.5) * h);
            fine[ix + n * iy] = if in_plate(x, y) || in_block(x, y) {
                BONE
            } else if !in_body(x, y) || (side == Side::Air && in_sinus(x, y)) {
                AIR
            } else {
                SOFT
            };
        }
    }
    blur(&mut fine, n);
    let mut values = vec![0f32; SIZE * SIZE];
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut sum = 0.0;
            for fy in 0..FINE {
                for fx in 0..FINE {
                    sum += fine[(x * FINE + fx) + n * (y * FINE + fy)];
                }
            }
            let (u, v) = (random().max(1e-9), random());
            let gauss = (-2.0 * u.ln()).sqrt() * (2.0 * std::f32::consts::PI * v).cos();
            values[x + SIZE * y] = sum / (FINE * FINE) as f32 + NOISE * gauss;
        }
    }
    let mut truth = vec![false; SIZE * SIZE];
    let mut block = vec![false; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = ((x as f32 + 0.5) * VOXEL, (y as f32 + 0.5) * VOXEL);
            truth[x + SIZE * y] = in_plate(px, py);
            block[x + SIZE * y] = in_block(px, py);
        }
    }
    let margin = (LENGTH * 0.35 / VOXEL) as usize;
    Phantom {
        values,
        truth,
        thickness,
        click: (SIZE as f32 / 2.0, SIZE as f32 / 2.0),
        rows: SIZE / 2 - margin..SIZE / 2 + margin,
        block,
    }
}

/// Mean thickness in millimetres over the rows clear of the plate's ends.
pub fn thickness(mask: &[bool], p: &Phantom) -> f32 {
    let on: usize = p
        .rows
        .clone()
        .map(|y| {
            (0..SIZE)
                .filter(|x| mask[x + SIZE * y] && x.abs_diff(SIZE / 2) < 60)
                .count()
        })
        .sum();
    on as f32 / p.rows.len() as f32 * VOXEL * TILT.cos()
}

fn boundary(mask: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            if !mask[x + SIZE * y] {
                continue;
            }
            let open = x == 0
                || y == 0
                || x + 1 == SIZE
                || y + 1 == SIZE
                || !mask[x - 1 + SIZE * y]
                || !mask[x + 1 + SIZE * y]
                || !mask[x + SIZE * (y - 1)]
                || !mask[x + SIZE * (y + 1)];
            if open {
                out.push((x, y));
            }
        }
    }
    out
}

/// The share of both boundaries within `tolerance` millimetres of the other.
pub fn surface_dice(a: &[bool], b: &[bool], tolerance: f32) -> f32 {
    let (ea, eb) = (boundary(a), boundary(b));
    let reach = tolerance / VOXEL;
    let near = |from: &[(usize, usize)], to: &[(usize, usize)]| {
        from.iter()
            .filter(|(x, y)| {
                to.iter()
                    .any(|(u, v)| (*x as f32 - *u as f32).hypot(*y as f32 - *v as f32) <= reach)
            })
            .count()
    };
    (near(&ea, &eb) + near(&eb, &ea)) as f32 / (ea.len() + eb.len()).max(1) as f32
}
