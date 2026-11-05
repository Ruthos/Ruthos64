//! Models that see a volume rather than a slice. A click gives a whole 3D object in
//! one step, and further clicks correct it. Each works on one window of the volume
//! around the latest click, so a click stays quick however long the series is.
//!
//! A GPU out of memory can hand back nonsense without reporting an error: on an
//! 8 GB card a 32-bit export returned an empty mask and then lost the device. So
//! every result is checked, and a bad one is rerun on the CPU rather than shown.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ort::session::Session;
use ort::value::Tensor;
use rayon::prelude::*;

use crate::ai::{open_session, AiError, Engine, Manifest};
use crate::Field;

type Result<T> = std::result::Result<T, AiError>;

fn run_error(e: impl std::fmt::Display) -> AiError {
    AiError(format!("The model failed: {e}"))
}

/// A click on a voxel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Click3 {
    pub x: usize,
    pub y: usize,
    pub z: usize,
    pub include: bool,
}

/// What a click produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// Each slice the window covers, whole, as a mask.
    pub slices: Vec<(usize, Vec<bool>)>,
    /// The object reached the window's side, so it may carry on past it.
    pub edge: bool,
}

/// Where bone starts, in HU, for aiming a click at it.
pub const BONE: f32 = 250.0;
/// The bottom of a histogram's first bin, and each bin's width, in HU.
pub const HISTOGRAM_LOW: f32 = -1024.0;
pub const HISTOGRAM_STEP: f32 = 16.0;
const HISTOGRAM_BINS: usize = 256;
/// How far past the model's mask bone may still be taken, in voxels on the slice. The
/// model's edge can sit inside the blurred edge of a thin bone, where the image still
/// reads as bone, and without this no threshold could reach it.
const MARGIN: usize = 2;
/// How far a click looks for the bone it was meant for, in voxels. Thin cortex is
/// only a few voxels across, and a click three voxels off it was 37% to 59% bone.
const SNAP: isize = 4;

/// A click moved to the brightest voxel near it, so that anywhere near a bone
/// means that bone. It stays put if nothing near is bone.
pub fn snap_to_bone<F: Field + ?Sized>(field: &F, click: Click3) -> Click3 {
    let grid = field.grid();
    let [w, h, d] = grid.dims;
    let mut best = (BONE, click);
    for dz in -SNAP..=SNAP {
        for dy in -SNAP..=SNAP {
            for dx in -SNAP..=SNAP {
                if dx * dx + dy * dy + dz * dz > SNAP * SNAP {
                    continue;
                }
                let (x, y, z) = (click.x as isize + dx, click.y as isize + dy, click.z as isize + dz);
                if x < 0 || y < 0 || z < 0 || x >= w as isize || y >= h as isize || z >= d as isize {
                    continue;
                }
                let (x, y, z) = (x as usize, y as usize, z as usize);
                let v = field.at(grid.index(x, y, z));
                if v > best.0 {
                    best = (v, Click3 { x, y, z, ..click });
                }
            }
        }
    }
    best.1
}

/// A slice's mask, grown by `by` voxels, diagonals counting as one.
fn grown(mask: &[bool], width: usize, height: usize, by: usize) -> Vec<bool> {
    let mut on = mask.to_vec();
    if !on.iter().any(|v| *v) {
        return on;
    }
    for _ in 0..by {
        let was = on.clone();
        for y in 0..height {
            for x in 0..width {
                if was[x + width * y] {
                    for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                        for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                            on[nx + width * ny] = true;
                        }
                    }
                }
            }
        }
    }
    on
}

/// The values in a result, for choosing a range by eye.
pub fn histogram<F: Field + ?Sized>(found: &Found, field: &F) -> Vec<u32> {
    let grid = field.grid();
    let plane = grid.dims[0] * grid.dims[1];
    let mut bins = vec![0u32; HISTOGRAM_BINS];
    for (z, mask) in &found.slices {
        for (i, _) in mask.iter().enumerate().filter(|(_, on)| **on) {
            let bin = (field.at(i + plane * z) - HISTOGRAM_LOW) / HISTOGRAM_STEP;
            bins[(bin.max(0.0) as usize).min(HISTOGRAM_BINS - 1)] += 1;
        }
    }
    bins
}

/// Hold a result to the voxels between `low` and `high` joined to the clicks. The
/// model finds an object, not a material, so bone it took with muscle around it comes
/// out as just the bone. What those voxels enclose in a slice, such as marrow, stays
/// if the model took it, and voxels in the range just past the model's edge are taken
/// too. If no click is in the range, everything in it within the result is kept.
pub fn hold_to_range<F: Field + ?Sized>(found: &Found, field: &F, clicks: &[Click3], low: f32, high: f32) -> Found {
    let grid = field.grid();
    let [w, h, _] = grid.dims;
    let plane = w * h;
    let Some(first) = found.slices.first().map(|s| s.0) else {
        return found.clone();
    };
    let depth = found.slices.len();
    let mut bone = vec![false; plane * depth];
    bone.par_chunks_mut(plane).enumerate().for_each(|(k, sheet)| {
        let (z, mask) = &found.slices[k];
        let reach = grown(mask, w, h, MARGIN);
        for (i, out) in sheet.iter_mut().enumerate() {
            *out = reach[i] && (low..=high).contains(&field.at(i + plane * z));
        }
    });
    let mut kept = vec![false; bone.len()];
    let mut stack: Vec<usize> = clicks
        .iter()
        .filter(|c| c.include && c.z >= first && c.z < first + depth)
        .map(|c| (c.z - first) * plane + c.x + w * c.y)
        .filter(|at| bone[*at])
        .collect();
    if stack.is_empty() {
        kept.copy_from_slice(&bone);
    }
    stack.iter().for_each(|at| kept[*at] = true);
    while let Some(at) = stack.pop() {
        let (x, y, k) = (at % w, (at / w) % h, at / plane);
        for nk in k.saturating_sub(1)..=(k + 1).min(depth - 1) {
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let n = nk * plane + ny * w + nx;
                    if bone[n] && !kept[n] {
                        kept[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
    }
    let slices = found
        .slices
        .iter()
        .enumerate()
        .map(|(k, (z, mask))| {
            let held = &kept[k * plane..(k + 1) * plane];
            // Only what is darker than the range, such as marrow in bone: bone inside
            // soft tissue is not soft tissue.
            let darker = |i: usize| mask[i] && field.at(i + plane * z) < low;
            (*z, crate::refine::fill_enclosed(held, w, h, darker))
        })
        .collect();
    Found {
        slices,
        edge: found.edge,
    }
}

/// Part of the volume, as first and one past last on each axis.
pub type Window = [(usize, usize); 3];

/// The most windows one Follow goes through.
const MOST_WINDOWS: usize = 24;
/// How far in from a window's side to look for the object leaving it, as the very
/// edge of a window is its least certain part.
const SIDE_INSET: usize = 2;
/// Fewer voxels than this on a side is a stray, not the object carrying on.
const LEAST_ON_SIDE: usize = 6;

/// Follow's progress, kept so that Stop can be followed by carrying on.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    queue: VecDeque<Click3>,
    visited: Vec<Window>,
    begun: bool,
    done: usize,
}

/// The middle of a window: a place already seen well, which needs no window of its own.
fn seen_well(window: &Window, c: &Click3) -> bool {
    let at = [c.x, c.y, c.z];
    (0..3).all(|a| {
        let (low, high) = window[a];
        let quarter = (high - low) / 4;
        at[a] >= low + quarter && at[a] < high - quarter
    })
}

/// Where the object runs off the sides of this window: a point on it at each side
/// that is not the end of the volume, from which to carry on. With a range, only
/// voxels in that range count, so Follow stays with the bone and not the muscle.
fn onward<F: Field + ?Sized>(found: &Found, window: &Window, field: &F, range: Option<(f32, f32)>) -> Vec<Click3> {
    let grid = field.grid();
    let [w, h, d] = grid.dims;
    let plane = w * h;
    let wanted = |x: usize, y: usize, z: usize| {
        range.is_none_or(|(low, high)| (low..=high).contains(&field.at(x + w * y + plane * z)))
    };
    let [(x0, x1), (y0, y1), (z0, z1)] = *window;
    let mask = |z: usize| found.slices.iter().find(|s| s.0 == z).map(|s| &s.1);
    let pick = |points: Vec<[usize; 3]>| -> Option<Click3> {
        if points.len() < LEAST_ON_SIDE {
            return None;
        }
        let mean: [f64; 3] =
            std::array::from_fn(|a| points.iter().map(|p| p[a] as f64).sum::<f64>() / points.len() as f64);
        let distance = |p: &[usize; 3]| (0..3).map(|a| (p[a] as f64 - mean[a]).powi(2)).sum::<f64>();
        let best = points.iter().min_by(|a, b| distance(a).total_cmp(&distance(b)))?;
        Some(Click3 {
            x: best[0],
            y: best[1],
            z: best[2],
            include: true,
        })
    };
    let inset = SIDE_INSET;
    let mut out = Vec::new();
    for (open, z) in [(z0 > 0, z0 + inset), (z1 < d, z1.saturating_sub(inset + 1))] {
        if let (true, Some(m)) = (open, mask(z)) {
            let points = (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| [x, y, z]))
                .filter(|p| m[p[0] + w * p[1]] && wanted(p[0], p[1], z))
                .collect();
            out.extend(pick(points));
        }
    }
    let on_side = |fixed_x: Option<usize>, fixed_y: Option<usize>| {
        let mut points = Vec::new();
        for z in z0..z1 {
            let Some(m) = mask(z) else { continue };
            for y in fixed_y.map_or(y0..y1, |y| y..y + 1) {
                for x in fixed_x.map_or(x0..x1, |x| x..x + 1) {
                    if m[x + w * y] && wanted(x, y, z) {
                        points.push([x, y, z]);
                    }
                }
            }
        }
        points
    };
    for (open, x) in [(x0 > 0, x0 + inset), (x1 < w, x1.saturating_sub(inset + 1))] {
        if open {
            out.extend(pick(on_side(Some(x), None)));
        }
    }
    for (open, y) in [(y0 > 0, y0 + inset), (y1 < h, y1.saturating_sub(inset + 1))] {
        if open {
            out.extend(pick(on_side(None, Some(y))));
        }
    }
    out
}

/// Why a result cannot be believed: logits that are not numbers, or all the same.
fn believable(logits: &[f32]) -> bool {
    let Some(first) = logits.first() else {
        return false;
    };
    logits.iter().all(|v| v.is_finite()) && logits.iter().any(|v| v != first)
}

// nnInteractive: the image and its interactions, one 192 voxel window, on the
// series' own voxels.

const NNI_PATCH: usize = 192;
const NNI_CHANNELS: usize = 8;
const NNI_PREVIOUS: usize = 1;
const NNI_INCLUDE: usize = 4;
const NNI_EXCLUDE: usize = 5;
const POINT_RADIUS: isize = 4;
/// How much each later click outweighs an earlier one.
const DECAY: f32 = 0.98;

/// A click as the network was trained on: a ball of radius 4 whose values are the
/// distance from its surface, 0 to 1. Indexed from `-POINT_RADIUS`.
fn soft_ball() -> Vec<f32> {
    let r = POINT_RADIUS;
    let side = (2 * r + 1) as usize;
    let inside = |x: isize, y: isize, z: isize| x * x + y * y + z * z <= r * r;
    let range = || -r..=r;
    let mut out = vec![0f32; side * side * side];
    let mut most = 0f32;
    for z in range() {
        for y in range() {
            for x in range() {
                if !inside(x, y, z) {
                    continue;
                }
                let mut nearest = i64::MAX;
                for k in range() {
                    for j in range() {
                        for i in range() {
                            if !inside(i, j, k) {
                                let (a, b, c) = ((x - i) as i64, (y - j) as i64, (z - k) as i64);
                                nearest = nearest.min(a * a + b * b + c * c);
                            }
                        }
                    }
                }
                let d = (nearest as f32).sqrt();
                most = most.max(d);
                let at = |v: isize| (v + r) as usize;
                out[at(x) + side * (at(y) + side * at(z))] = d;
            }
        }
    }
    out.iter_mut().for_each(|v| *v /= most);
    out
}

pub struct NnInteractive {
    session: Session,
    file: std::path::PathBuf,
    root: std::path::PathBuf,
    pub on_gpu: bool,
    ball: Vec<f32>,
    /// Mean and deviation over the region holding anything but zero.
    stats: Option<(f32, f32)>,
    /// The result so far, which is what the next click corrects.
    previous: Vec<bool>,
    /// The part of the volume the last click looked at.
    window: Window,
}

/// The first voxel of a window of `size` centred on `centre`. It may lie off the
/// volume, which reads as zero, as the model expects.
fn window_start(centre: usize) -> isize {
    centre as isize - (NNI_PATCH / 2) as isize
}

impl NnInteractive {
    fn load(manifest: &Manifest, root: &Path) -> Result<Self> {
        let file = manifest.folder(root).join(manifest.model[0].name);
        let gpu = open_session(root, &file, true);
        let on_gpu = gpu.is_ok();
        let session = match gpu {
            Ok(s) => s,
            Err(_) => open_session(root, &file, false)?,
        };
        Ok(Self {
            session,
            file,
            root: root.to_path_buf(),
            on_gpu,
            ball: soft_ball(),
            stats: None,
            previous: Vec::new(),
            window: [(0, 0); 3],
        })
    }

    fn statistics<F: Field + ?Sized>(field: &F) -> (f32, f32) {
        let grid = field.grid();
        let (mut low, mut high) = ([usize::MAX; 3], [0usize; 3]);
        for i in 0..grid.len() {
            if field.at(i) != 0.0 {
                let p = grid.position(i);
                for a in 0..3 {
                    low[a] = low[a].min(p[a]);
                    high[a] = high[a].max(p[a]);
                }
            }
        }
        if low[0] == usize::MAX {
            return (0.0, 1.0);
        }
        let (mut sum, mut squares, mut n) = (0f64, 0f64, 0f64);
        for z in low[2]..=high[2] {
            for y in low[1]..=high[1] {
                for x in low[0]..=high[0] {
                    let v = field.at(grid.index(x, y, z)) as f64;
                    sum += v;
                    squares += v * v;
                    n += 1.0;
                }
            }
        }
        let mean = sum / n;
        let deviation = (squares / n - mean * mean).max(1e-6).sqrt();
        (mean as f32, deviation as f32)
    }

    fn input<F: Field + ?Sized>(&self, field: &F, clicks: &[Click3], start: [isize; 3]) -> Vec<f32> {
        let grid = field.grid();
        let [w, h, d] = grid.dims;
        let (mean, deviation) = self.stats.unwrap_or((0.0, 1.0));
        let p = NNI_PATCH;
        let plane = p * p;
        let volume = plane * p;
        let mut input = vec![0f32; NNI_CHANNELS * volume];
        let at = |c: usize, x: usize, y: usize, z: usize| c * volume + z * plane + y * p + x;
        let inside = |v: isize, n: usize| v >= 0 && (v as usize) < n;
        let (image, rest) = input.split_at_mut(volume);
        image.par_chunks_mut(plane).enumerate().for_each(|(k, sheet)| {
            let z = start[2] + k as isize;
            if !inside(z, d) {
                return;
            }
            for (j, row) in sheet.chunks_mut(p).enumerate() {
                let y = start[1] + j as isize;
                if !inside(y, h) {
                    continue;
                }
                for (i, out) in row.iter_mut().enumerate() {
                    let x = start[0] + i as isize;
                    if inside(x, w) {
                        let voxel = grid.index(x as usize, y as usize, z as usize);
                        *out = (field.at(voxel) - mean) / deviation;
                    }
                }
            }
        });
        if !self.previous.is_empty() {
            for k in 0..p {
                for j in 0..p {
                    for i in 0..p {
                        let (x, y, z) = (start[0] + i as isize, start[1] + j as isize, start[2] + k as isize);
                        if inside(x, w) && inside(y, h) && inside(z, d) {
                            let on = self.previous[grid.index(x as usize, y as usize, z as usize)];
                            rest[at(NNI_PREVIOUS - 1, i, j, k)] = f32::from(u8::from(on));
                        }
                    }
                }
            }
        }
        let side = (2 * POINT_RADIUS + 1) as usize;
        for (age, click) in clicks.iter().rev().enumerate() {
            let weight = DECAY.powi(age as i32);
            let channel = if click.include { NNI_INCLUDE } else { NNI_EXCLUDE } - 1;
            let centre = [click.x as isize, click.y as isize, click.z as isize];
            for c in 0..side {
                for b in 0..side {
                    for a in 0..side {
                        let value = self.ball[a + side * (b + side * c)] * weight;
                        let spot = [
                            centre[0] + a as isize - POINT_RADIUS - start[0],
                            centre[1] + b as isize - POINT_RADIUS - start[1],
                            centre[2] + c as isize - POINT_RADIUS - start[2],
                        ];
                        if value > 0.0 && spot.iter().all(|v| inside(*v, p)) {
                            let cell = &mut rest[at(channel, spot[0] as usize, spot[1] as usize, spot[2] as usize)];
                            *cell = cell.max(value);
                        }
                    }
                }
            }
        }
        input
    }

    fn run(session: &mut Session, input: Vec<f32>) -> Result<Vec<f32>> {
        let p = NNI_PATCH;
        let tensor = Tensor::from_array(([1, NNI_CHANNELS, p, p, p], input)).map_err(run_error)?;
        let outputs = session.run(ort::inputs!["input" => tensor]).map_err(run_error)?;
        let logits = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(run_error)?
            .1
            .to_vec();
        if believable(&logits) {
            Ok(logits)
        } else {
            Err(AiError("The model returned nothing usable".to_string()))
        }
    }

    /// Apply the latest click. `clicks` are all those on this object, oldest first.
    fn click<F: Field + ?Sized>(&mut self, field: &F, clicks: &[Click3]) -> Result<Found> {
        let Some(last) = clicks.last() else {
            return Err(AiError("Nothing to go on".to_string()));
        };
        let grid = field.grid();
        if self.stats.is_none() {
            self.stats = Some(Self::statistics(field));
        }
        if self.previous.len() != grid.len() {
            self.previous = vec![false; grid.len()];
        }
        let start = [window_start(last.x), window_start(last.y), window_start(last.z)];
        let input = self.input(field, clicks, start);
        let logits = match Self::run(&mut self.session, input.clone()) {
            Ok(l) => l,
            Err(_) if self.on_gpu => {
                // Most likely out of memory. The CPU is slower and always has room.
                self.session = open_session(&self.root, &self.file, false)?;
                self.on_gpu = false;
                Self::run(&mut self.session, input)?
            }
            Err(e) => return Err(e),
        };
        let p = NNI_PATCH;
        let volume = p * p * p;
        let [w, h, d] = grid.dims;
        let (background, object) = logits.split_at(volume);
        let on = |i: usize, j: usize, k: usize| {
            let at = k * p * p + j * p + i;
            object[at] > background[at]
        };
        let mut edge = false;
        for k in 0..p {
            for j in 0..p {
                for i in 0..p {
                    let (x, y, z) = (start[0] + i as isize, start[1] + j as isize, start[2] + k as isize);
                    let inside = x >= 0 && y >= 0 && z >= 0 && (x as usize) < w && (y as usize) < h && (z as usize) < d;
                    if !inside {
                        continue;
                    }
                    let voxel = grid.index(x as usize, y as usize, z as usize);
                    let value = on(i, j, k);
                    self.previous[voxel] = value;
                    edge |= value && [i, j, k].iter().any(|v| *v == 0 || *v == p - 1);
                }
            }
        }
        let first = start[2].max(0) as usize;
        let last_slice = ((start[2] + p as isize) as usize).min(d);
        let plane = w * h;
        let span = |a: usize, n: usize| (start[a].max(0) as usize, ((start[a] + p as isize) as usize).min(n));
        self.window = [span(0, w), span(1, h), (first, last_slice)];
        // All of the object so far, not only this window: a click further along adds
        // to what has been mapped.
        let held = |z: &usize| self.previous[plane * z..plane * (z + 1)].iter().any(|v| *v);
        let lowest = (0..first).find(held).unwrap_or(first);
        let highest = (last_slice..d).rev().find(held).map_or(last_slice, |z| z + 1);
        let slices = (lowest..highest)
            .map(|z| (z, self.previous[plane * z..plane * (z + 1)].to_vec()))
            .collect();
        Ok(Found { slices, edge })
    }
}

// VISTA3D: an encoder for a 128 voxel window at 1.5 mm, and a decoder for each click.

const VISTA_PATCH: usize = 128;
const VISTA_MM: f64 = 1.5;
const VISTA_LOW: f32 = -963.82477;
const VISTA_HIGH: f32 = 1053.6785;

pub struct Vista {
    encoder: Session,
    decoder: Session,
    encoder_file: std::path::PathBuf,
    root: std::path::PathBuf,
    pub on_gpu: bool,
    /// The volume at 1.5 mm and scaled, first axis x, and its size.
    small: Vec<f32>,
    size: [usize; 3],
    /// The window encoded last, and its features.
    features: Option<([usize; 3], Vec<f32>)>,
    /// The part of the volume the last click looked at.
    window: Window,
}

/// Linear resampling to `size`, pixel centres aligned as the training pipeline does.
fn resample<F: Field + ?Sized>(field: &F, size: [usize; 3]) -> Vec<f32> {
    let grid = field.grid();
    let axis = |i: usize, to: usize, from: usize| {
        let at = ((i as f64 + 0.5) * from as f64 / to as f64 - 0.5).clamp(0.0, (from - 1) as f64);
        let low = at.floor() as usize;
        (low, (low + 1).min(from - 1), (at - low as f64) as f32)
    };
    let [sx, sy, sz] = size;
    let mut out = vec![0f32; sx * sy * sz];
    out.par_chunks_mut(sy * sz).enumerate().for_each(|(i, sheet)| {
        let (x0, x1, fx) = axis(i, sx, grid.dims[0]);
        for j in 0..sy {
            let (y0, y1, fy) = axis(j, sy, grid.dims[1]);
            for k in 0..sz {
                let (z0, z1, fz) = axis(k, sz, grid.dims[2]);
                let v = |x, y, z| field.at(grid.index(x, y, z));
                let near = v(x0, y0, z0) * (1.0 - fz) + v(x0, y0, z1) * fz;
                let near_y = v(x0, y1, z0) * (1.0 - fz) + v(x0, y1, z1) * fz;
                let far = v(x1, y0, z0) * (1.0 - fz) + v(x1, y0, z1) * fz;
                let far_y = v(x1, y1, z0) * (1.0 - fz) + v(x1, y1, z1) * fz;
                let a = near * (1.0 - fy) + near_y * fy;
                let b = far * (1.0 - fy) + far_y * fy;
                let value = a * (1.0 - fx) + b * fx;
                sheet[j * sz + k] = ((value - VISTA_LOW) / (VISTA_HIGH - VISTA_LOW)).clamp(0.0, 1.0);
            }
        }
    });
    out
}

impl Vista {
    fn load(manifest: &Manifest, root: &Path) -> Result<Self> {
        let folder = manifest.folder(root);
        let encoder_file = folder.join(manifest.model[0].name);
        let decoder_file = folder.join(manifest.model[1].name);
        let gpu = open_session(root, &encoder_file, true);
        let on_gpu = gpu.is_ok();
        let encoder = match gpu {
            Ok(s) => s,
            Err(_) => open_session(root, &encoder_file, false)?,
        };
        let decoder = open_session(root, &decoder_file, on_gpu)?;
        Ok(Self {
            encoder,
            decoder,
            encoder_file,
            root: root.to_path_buf(),
            on_gpu,
            small: Vec::new(),
            size: [0; 3],
            features: None,
            window: [(0, 0); 3],
        })
    }

    fn prepare<F: Field + ?Sized>(&mut self, field: &F) {
        if self.small.is_empty() {
            let grid = field.grid();
            let size: [usize; 3] = std::array::from_fn(|a| {
                (((grid.dims[a] as f64 * grid.spacing[a]) / VISTA_MM).round() as usize).max(VISTA_PATCH)
            });
            self.small = resample(field, size);
            self.size = size;
        }
    }

    fn encode(&mut self, start: [usize; 3]) -> Result<()> {
        if self.features.as_ref().is_some_and(|(s, _)| *s == start) {
            return Ok(());
        }
        let p = VISTA_PATCH;
        let [sx, sy, sz] = self.size;
        let mut patch = vec![0f32; p * p * p];
        for i in 0..p {
            for j in 0..p {
                let from = (start[0] + i) * sy * sz + (start[1] + j) * sz + start[2];
                let to = i * p * p + j * p;
                patch[to..to + p].copy_from_slice(&self.small[from..from + p]);
            }
        }
        let _ = sx;
        let run = |session: &mut Session, patch: Vec<f32>| -> Result<Vec<f32>> {
            let tensor = Tensor::from_array(([1, 1, p, p, p], patch)).map_err(run_error)?;
            let outputs = session.run(ort::inputs!["image" => tensor]).map_err(run_error)?;
            let out = outputs["features"]
                .try_extract_tensor::<f32>()
                .map_err(run_error)?
                .1
                .to_vec();
            if believable(&out) {
                Ok(out)
            } else {
                Err(AiError("The model returned nothing usable".to_string()))
            }
        };
        let features = match run(&mut self.encoder, patch.clone()) {
            Ok(f) => f,
            Err(_) if self.on_gpu => {
                self.encoder = open_session(&self.root, &self.encoder_file, false)?;
                self.on_gpu = false;
                run(&mut self.encoder, patch)?
            }
            Err(e) => return Err(e),
        };
        self.features = Some((start, features));
        Ok(())
    }

    fn click<F: Field + ?Sized>(&mut self, field: &F, clicks: &[Click3]) -> Result<Found> {
        let grid = field.grid();
        let Some(seed) = clicks.iter().find(|c| c.include) else {
            return Err(AiError("Click on the object first".to_string()));
        };
        self.prepare(field);
        let to_small = |v: usize, a: usize| v as f64 * grid.spacing[a] / VISTA_MM;
        let centre = [to_small(seed.x, 0), to_small(seed.y, 1), to_small(seed.z, 2)];
        let start: [usize; 3] = std::array::from_fn(|a| {
            let at = centre[a] as isize - (VISTA_PATCH / 2) as isize;
            at.clamp(0, (self.size[a] - VISTA_PATCH) as isize) as usize
        });
        self.encode(start)?;
        let features = self.features.as_ref().map(|f| f.1.clone()).unwrap_or_default();
        let coords: Vec<f32> = clicks
            .iter()
            .flat_map(|c| {
                [
                    (to_small(c.x, 0) - start[0] as f64) as f32,
                    (to_small(c.y, 1) - start[1] as f64) as f32,
                    (to_small(c.z, 2) - start[2] as f64) as f32,
                ]
            })
            .collect();
        let labels: Vec<i64> = clicks.iter().map(|c| i64::from(c.include)).collect();
        let n = clicks.len();
        let p = VISTA_PATCH;
        let outputs = self
            .decoder
            .run(ort::inputs![
                "features" => Tensor::from_array(([1, 48, p, p, p], features)).map_err(run_error)?,
                "coords" => Tensor::from_array(([1, n, 3], coords)).map_err(run_error)?,
                "labels" => Tensor::from_array(([1, n], labels)).map_err(run_error)?,
            ])
            .map_err(run_error)?;
        let logits = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(run_error)?
            .1
            .to_vec();
        if !believable(&logits) {
            return Err(AiError("The model returned nothing usable".to_string()));
        }
        let logit = |i: usize, j: usize, k: usize| logits[i * p * p + j * p + k];
        // Back onto the series' own voxels, within the window.
        let [w, h, d] = grid.dims;
        let span = |a: usize, n: usize| -> (usize, usize) {
            let low = (start[a] as f64 * VISTA_MM / grid.spacing[a]).floor() as usize;
            let high = (((start[a] + p) as f64 * VISTA_MM / grid.spacing[a]).ceil() as usize).min(n);
            (low.min(n), high)
        };
        let ((x0, x1), (y0, y1), (z0, z1)) = (span(0, w), span(1, h), span(2, d));
        self.window = [(x0, x1), (y0, y1), (z0, z1)];
        let local = |v: usize, a: usize| -> (usize, usize, f32) {
            let at = ((v as f64 * grid.spacing[a] / VISTA_MM) - start[a] as f64).clamp(0.0, (p - 1) as f64);
            let low = at.floor() as usize;
            (low, (low + 1).min(p - 1), (at - low as f64) as f32)
        };
        let (ww, wh, wd) = (x1 - x0, y1 - y0, z1 - z0);
        let mut on = vec![false; ww * wh * wd];
        on.par_chunks_mut(ww * wh).enumerate().for_each(|(kz, sheet)| {
            let (k0, k1, fk) = local(z0 + kz, 2);
            for jy in 0..wh {
                let (j0, j1, fj) = local(y0 + jy, 1);
                for ix in 0..ww {
                    let (i0, i1, fi) = local(x0 + ix, 0);
                    let v = |i, j, k| logit(i, j, k);
                    let c00 = v(i0, j0, k0) * (1.0 - fk) + v(i0, j0, k1) * fk;
                    let c01 = v(i0, j1, k0) * (1.0 - fk) + v(i0, j1, k1) * fk;
                    let c10 = v(i1, j0, k0) * (1.0 - fk) + v(i1, j0, k1) * fk;
                    let c11 = v(i1, j1, k0) * (1.0 - fk) + v(i1, j1, k1) * fk;
                    let a = c00 * (1.0 - fj) + c01 * fj;
                    let b = c10 * (1.0 - fj) + c11 * fj;
                    sheet[jy * ww + ix] = a * (1.0 - fi) + b * fi > 0.0;
                }
            }
        });
        // A click means one object: keep what touches the first include.
        let index = |x: usize, y: usize, z: usize| x + ww * (y + wh * z);
        let mut kept = vec![false; on.len()];
        let inside = seed.x >= x0 && seed.x < x1 && seed.y >= y0 && seed.y < y1 && seed.z >= z0 && seed.z < z1;
        if inside && on[index(seed.x - x0, seed.y - y0, seed.z - z0)] {
            let mut stack = vec![(seed.x - x0, seed.y - y0, seed.z - z0)];
            kept[index(seed.x - x0, seed.y - y0, seed.z - z0)] = true;
            while let Some((x, y, z)) = stack.pop() {
                let mut visit = |a: usize, b: usize, c: usize| {
                    let at = index(a, b, c);
                    if on[at] && !kept[at] {
                        kept[at] = true;
                        stack.push((a, b, c));
                    }
                };
                if x > 0 {
                    visit(x - 1, y, z);
                }
                if x + 1 < ww {
                    visit(x + 1, y, z);
                }
                if y > 0 {
                    visit(x, y - 1, z);
                }
                if y + 1 < wh {
                    visit(x, y + 1, z);
                }
                if z > 0 {
                    visit(x, y, z - 1);
                }
                if z + 1 < wd {
                    visit(x, y, z + 1);
                }
            }
        } else {
            kept = on;
        }
        let mut edge = false;
        let mut slices = Vec::with_capacity(wd);
        for kz in 0..wd {
            let mut mask = vec![false; w * h];
            for jy in 0..wh {
                for ix in 0..ww {
                    if kept[index(ix, jy, kz)] {
                        mask[(x0 + ix) + w * (y0 + jy)] = true;
                        edge |= ix == 0 || jy == 0 || kz == 0 || ix + 1 == ww || jy + 1 == wh || kz + 1 == wd;
                    }
                }
            }
            slices.push((z0 + kz, mask));
        }
        Ok(Found { slices, edge })
    }
}

/// A loaded 3D model.
pub enum VolumeModel {
    NnInteractive(Box<NnInteractive>),
    Vista(Box<Vista>),
}

impl VolumeModel {
    pub fn load(manifest: &Manifest, root: &Path) -> Result<Self> {
        for item in manifest.model {
            crate::ai::verify(&manifest.folder(root).join(item.name), item)?;
        }
        match manifest.engine {
            Engine::NnInteractive => Ok(Self::NnInteractive(Box::new(NnInteractive::load(manifest, root)?))),
            Engine::Vista => Ok(Self::Vista(Box::new(Vista::load(manifest, root)?))),
            Engine::Slices => Err(AiError("That model works on slices".to_string())),
        }
    }

    pub fn on_gpu(&self) -> bool {
        match self {
            Self::NnInteractive(m) => m.on_gpu,
            Self::Vista(m) => m.on_gpu,
        }
    }

    /// Do the slow reading of the volume now, so that the first click is as quick
    /// as the rest.
    pub fn prepare<F: Field + ?Sized>(&mut self, field: &F) {
        match self {
            Self::NnInteractive(m) => m.stats = Some(NnInteractive::statistics(field)),
            Self::Vista(m) => m.prepare(field),
        }
    }

    /// Apply the clicks. With `bone`, they mean the nearest bone. Gives the model's
    /// own result, and the clicks as they were used, for `hold_to_range`.
    pub fn click<F: Field + ?Sized>(
        &mut self,
        field: &F,
        clicks: &[Click3],
        bone: bool,
    ) -> Result<(Found, Vec<Click3>)> {
        let aimed: Vec<Click3> = clicks
            .iter()
            .map(|c| if bone && c.include { snap_to_bone(field, *c) } else { *c })
            .collect();
        let found = match self {
            Self::NnInteractive(m) => m.click(field, &aimed)?,
            Self::Vista(m) => m.click(field, &aimed)?,
        };
        Ok((found, aimed))
    }

    fn window(&self) -> Window {
        match self {
            Self::NnInteractive(m) => m.window,
            Self::Vista(m) => m.window,
        }
    }

    /// Follow the object past the window it was found in: wherever it runs off a side,
    /// another window centres there and carries on, until it ends or the volume does.
    /// `found` grows as it goes. `progress` gets windows done and known so far. True if
    /// `cancel` stopped it; with the same `walk` it carries on from there.
    #[allow(clippy::too_many_arguments)]
    pub fn follow<F: Field + ?Sized>(
        &mut self,
        field: &F,
        clicks: &[Click3],
        range: Option<(f32, f32)>,
        found: &mut Found,
        walk: &mut Walk,
        mut progress: impl FnMut(usize, usize),
        cancel: &AtomicBool,
    ) -> Result<bool> {
        if !walk.begun {
            walk.begun = true;
            let window = self.window();
            walk.queue.extend(onward(found, &window, field, range));
            walk.visited.push(window);
        }
        let mut merged: BTreeMap<usize, Vec<bool>> = found.slices.drain(..).collect();
        let excludes: Vec<Click3> = clicks.iter().filter(|c| !c.include).copied().collect();
        let mut stopped = false;
        let mut failed = None;
        while let Some(next) = walk.queue.pop_front() {
            if walk.done >= MOST_WINDOWS {
                walk.queue.clear();
                break;
            }
            if walk.visited.iter().any(|w| seen_well(w, &next)) {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                walk.queue.push_front(next);
                stopped = true;
                break;
            }
            progress(walk.done, walk.done + walk.queue.len() + 1);
            // The newest click sets a window. nnInteractive also builds on what it
            // has found; VISTA3D starts from the point it is given.
            let result = match self {
                Self::NnInteractive(m) => {
                    let mut chain = clicks.to_vec();
                    chain.push(next);
                    m.click(field, &chain)
                }
                Self::Vista(m) => {
                    let mut chain = vec![next];
                    chain.extend(excludes.iter().copied());
                    m.click(field, &chain)
                }
            };
            let result = match result {
                Ok(r) => r,
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            };
            let window = self.window();
            walk.queue.extend(onward(&result, &window, field, range));
            walk.visited.push(window);
            walk.done += 1;
            for (z, mask) in result.slices {
                match merged.get_mut(&z) {
                    Some(old) => old.iter_mut().zip(&mask).for_each(|(a, b)| *a |= *b),
                    None => {
                        merged.insert(z, mask);
                    }
                }
            }
        }
        // A run of slices with no gaps, as `hold_to_range` takes it.
        let plane = field.grid().dims[0] * field.grid().dims[1];
        if let (Some(low), Some(high)) = (merged.keys().next().copied(), merged.keys().next_back().copied()) {
            for z in low..=high {
                merged.entry(z).or_insert_with(|| vec![false; plane]);
            }
        }
        found.slices = merged.into_iter().collect();
        found.edge = !stopped && walk.done >= MOST_WINDOWS;
        match failed {
            Some(e) => Err(e),
            None => Ok(stopped),
        }
    }

    /// Start a new object.
    pub fn reset(&mut self) {
        if let Self::NnInteractive(m) = self {
            m.previous.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_is_the_ball_the_model_was_trained_on() {
        let ball = soft_ball();
        assert_eq!(ball.iter().filter(|v| **v > 0.0).count(), 257);
        let total: f32 = ball.iter().sum();
        assert!((total - 90.47).abs() < 0.02, "{total}");
        assert_eq!(ball.iter().copied().fold(0.0, f32::max), 1.0);
    }

    /// A wall of bone four voxels thick in muscle, and a second wall beyond a gap.
    fn wall() -> crate::Values {
        let grid = crate::Grid {
            dims: [40, 40, 12],
            spacing: [1.0, 1.0, 1.0],
        };
        let mut data = vec![100.0; grid.len()];
        for (i, value) in data.iter_mut().enumerate() {
            let [x, y, _] = grid.position(i);
            if (18..22).contains(&x) && (5..35).contains(&y) {
                *value = if x == 19 || x == 20 { 800.0 } else { 500.0 };
            }
            if (30..33).contains(&x) {
                *value = 900.0;
            }
        }
        crate::Values { grid, data }
    }

    fn click(x: usize, y: usize, z: usize) -> Click3 {
        Click3 { x, y, z, include: true }
    }

    #[test]
    fn a_click_near_a_thin_wall_finds_the_wall() {
        let f = wall();
        for x in [16, 17, 19, 22, 23] {
            let aimed = snap_to_bone(&f, click(x, 20, 6));
            assert!((19..=20).contains(&aimed.x), "{x} went to {}", aimed.x);
        }
    }

    #[test]
    fn a_click_with_no_bone_near_stays_where_it_is() {
        let f = wall();
        assert_eq!(snap_to_bone(&f, click(6, 20, 6)), click(6, 20, 6));
    }

    #[test]
    fn a_result_is_held_to_the_bone_it_was_clicked_on() {
        let f = wall();
        let w = 40;
        // Everything: the wall, the muscle round it, and the second wall.
        let slices: Vec<(usize, Vec<bool>)> = (0..12).map(|z| (z, vec![true; w * w])).collect();
        let held = hold_to_range(&Found { slices, edge: false }, &f, &[click(19, 20, 6)], BONE, f32::MAX);
        let m = &held.slices[6].1;
        assert!(m[19 + w * 20], "the wall");
        assert!(m[21 + w * 20], "its dimmer edge");
        assert!(!m[10 + w * 20], "not the muscle");
        assert!(!m[31 + w * 20], "not the other bone");
    }

    #[test]
    fn marrow_inside_the_bone_stays_if_the_model_took_it() {
        let grid = crate::Grid {
            dims: [30, 30, 3],
            spacing: [1.0, 1.0, 1.0],
        };
        let mut data = vec![60.0; grid.len()];
        for (i, value) in data.iter_mut().enumerate() {
            let [x, y, _] = grid.position(i);
            let d = (x as f32 - 15.0).hypot(y as f32 - 15.0);
            if (5.0..8.0).contains(&d) {
                *value = 900.0;
            }
        }
        let f = crate::Values { grid, data };
        let w = 30;
        let all: Vec<(usize, Vec<bool>)> = (0..3).map(|z| (z, vec![true; w * w])).collect();
        let held = hold_to_range(
            &Found {
                slices: all,
                edge: false,
            },
            &f,
            &[click(21, 15, 1)],
            BONE,
            f32::MAX,
        );
        assert!(held.slices[1].1[15 + w * 15], "the marrow");
        assert!(!held.slices[1].1[2 + w * 2], "not the outside");
    }

    fn one_slice(w: usize, pixels: &[(usize, usize)]) -> Vec<bool> {
        let mut mask = vec![false; w * w];
        pixels.iter().for_each(|(x, y)| mask[x + w * y] = true);
        mask
    }

    #[test]
    fn an_object_leaving_the_window_gives_a_place_to_carry_on() {
        let f = wall();
        let w = 40;
        let wall: Vec<(usize, usize)> = (10..20).flat_map(|y| [(19, y), (20, y)]).collect();
        // The window stops at slice 8 of 12, with the wall on it.
        let slices = (0..8).map(|z| (z, one_slice(w, &wall))).collect();
        let found = Found { slices, edge: false };
        let window = [(0, w), (0, w), (0, 8)];
        let on = onward(&found, &window, &f, Some((BONE, f32::MAX)));
        assert_eq!(on.len(), 1, "{on:?}");
        assert_eq!(on[0].z, 5);
        assert!((19..=20).contains(&on[0].x) && on[0].include);
    }

    #[test]
    fn the_end_of_the_volume_is_not_a_place_to_carry_on() {
        let f = wall();
        let w = 40;
        let wall: Vec<(usize, usize)> = (10..20).flat_map(|y| [(19, y), (20, y)]).collect();
        let slices = (0..12).map(|z| (z, one_slice(w, &wall))).collect();
        let found = Found { slices, edge: false };
        assert!(onward(&found, &[(0, w), (0, w), (0, 12)], &f, None).is_empty());
    }

    #[test]
    fn a_stray_voxel_on_a_side_is_not_the_object_carrying_on() {
        let f = wall();
        let w = 40;
        let slices = (0..8).map(|z| (z, one_slice(w, &[(19, 12), (20, 12)]))).collect();
        let found = Found { slices, edge: false };
        assert!(onward(&found, &[(0, w), (0, w), (0, 8)], &f, None).is_empty());
    }

    #[test]
    fn muscle_on_a_side_is_not_followed_when_keeping_to_bone() {
        let f = wall();
        let w = 40;
        let muscle: Vec<(usize, usize)> = (10..20).flat_map(|y| [(5, y), (6, y)]).collect();
        let slices = (0..8).map(|z| (z, one_slice(w, &muscle))).collect();
        let found = Found { slices, edge: false };
        let window = [(0, w), (0, w), (0, 8)];
        assert!(onward(&found, &window, &f, Some((BONE, f32::MAX))).is_empty());
        assert_eq!(onward(&found, &window, &f, None).len(), 1);
    }

    #[test]
    fn the_middle_of_a_window_needs_no_window_of_its_own() {
        let window = [(0, 100), (0, 100), (0, 100)];
        assert!(seen_well(&window, &click(50, 50, 50)));
        assert!(!seen_well(&window, &click(50, 50, 97)));
    }

    #[test]
    fn with_no_click_in_the_range_everything_in_it_stays() {
        let (f, w) = (wall(), 40);
        let found = Found {
            slices: vec![(6, vec![true; w * w])],
            edge: false,
        };
        let held = hold_to_range(&found, &f, &[click(6, 20, 6)], BONE, f32::MAX);
        assert!(held.slices[0].1[19 + w * 20] && held.slices[0].1[31 + w * 20]);
        assert!(!held.slices[0].1[6 + w * 20]);
    }

    #[test]
    fn a_top_to_the_range_keeps_out_what_is_brighter() {
        let (f, w) = (wall(), 40);
        let found = Found {
            slices: vec![(6, vec![true; w * w])],
            edge: false,
        };
        let held = hold_to_range(&found, &f, &[click(6, 20, 6)], 50.0, 200.0);
        assert!(held.slices[0].1[6 + w * 20], "soft tissue");
        assert!(!held.slices[0].1[19 + w * 20], "not bone");
    }

    #[test]
    fn a_histogram_counts_what_the_result_holds() {
        let f = wall();
        let w = 40;
        let mut mask = vec![false; w * w];
        mask[19 + w * 20] = true;
        mask[6 + w * 20] = true;
        let bins = histogram(
            &Found {
                slices: vec![(6, mask)],
                edge: false,
            },
            &f,
        );
        assert_eq!(bins.iter().sum::<u32>(), 2);
        let bin = |v: f32| ((v - HISTOGRAM_LOW) / HISTOGRAM_STEP) as usize;
        assert_eq!((bins[bin(800.0)], bins[bin(100.0)]), (1, 1));
    }

    /// The wall's dim edge, 500 HU, with the model's mask only on its bright centre.
    fn centre_only() -> Found {
        let w = 40;
        let mut mask = vec![false; w * w];
        for y in 5..35 {
            mask[19 + w * y] = true;
            mask[20 + w * y] = true;
        }
        Found {
            slices: (0..12).map(|z| (z, mask.clone())).collect(),
            edge: false,
        }
    }

    #[test]
    fn bone_just_past_the_mask_is_taken_up_to_the_level() {
        let (f, w) = (wall(), 40);
        let low = hold_to_range(&centre_only(), &f, &[click(19, 20, 6)], 400.0, f32::MAX);
        assert!(
            low.slices[6].1[18 + w * 20] && low.slices[6].1[21 + w * 20],
            "the dim edge"
        );
        assert!(!low.slices[6].1[17 + w * 20], "not muscle");
        let high = hold_to_range(&centre_only(), &f, &[click(19, 20, 6)], 600.0, f32::MAX);
        assert!(high.slices[6].1[19 + w * 20], "the bright centre");
        assert!(!high.slices[6].1[18 + w * 20], "a higher level drops the dim edge");
    }

    #[test]
    fn the_margin_does_not_reach_a_neighbouring_bone() {
        let (f, w) = (wall(), 40);
        let held = hold_to_range(&centre_only(), &f, &[click(19, 20, 6)], 400.0, f32::MAX);
        assert!(!held.slices[6].1[30 + w * 20]);
    }

    #[test]
    fn nonsense_from_a_gpu_is_not_believed() {
        assert!(believable(&[0.0, 1.0, -2.0]));
        assert!(!believable(&[0.0, 0.0, 0.0]), "all the same is a failed run");
        assert!(!believable(&[1.0, f32::NAN]));
        assert!(!believable(&[1.0, f32::INFINITY]));
        assert!(!believable(&[]));
    }

    #[test]
    fn a_window_may_run_off_the_volume() {
        assert_eq!(window_start(10), 10 - 96);
        assert_eq!(window_start(300), 204);
    }
}
