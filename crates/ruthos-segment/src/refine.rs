//! Edge refinement of a model's mask on the real CT values.
//!
//! The model decides what the object is and the image decides where its edge
//! lies. The model works in cells on a stretched copy of a window of the image,
//! so its boundary is a smoothed estimate. This keeps its judgement of which
//! object was meant and replaces that boundary with one read from the Hounsfield
//! values, on the slice's own pixels, within a narrow band of the model's edge.
//!
//! Refinement never needs the model, so it is here without the `ai` feature and
//! is tested against a phantom in every build.

use std::collections::VecDeque;

/// Band width in voxels, the default and the limits in Preferences.
pub const BAND: usize = 3;
pub const BAND_RANGE: std::ops::RangeInclusive<usize> = 1..=6;

/// A slice whose area differs from its neighbours' mean by more than this share
/// is flagged for review.
const AREA_CHANGE: f32 = 0.4;

/// Whether the edge is the half-maximum of a cortex or a low-contrast midpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Bone,
    SoftTissue,
    Contrast,
}

/// The largest step between voxels, as a share of the object's contrast with
/// what surrounds it.
const STEP: f32 = 0.7;
const SOFT_STEP: f32 = 0.5;
/// How far below the threshold a voxel may lie and still carry growth across a
/// weak gap, as a share of the contrast.
const WEAK: f32 = 0.2;
/// The share of the way from the outer level to the core's brightest above which
/// a core voxel counts as the cortex's plateau.
const PLATEAU: f32 = 0.9;
/// The rise allowed outside the model's mask, as a share of the contrast.
const CLIMB: f32 = 0.15;
/// The longest weak gap crossed, in voxels.
const WEAK_REACH: usize = 2;
/// How many times pieces joined across gaps are looked at again.
const WEAK_ROUNDS: usize = 8;

/// A mask's logits on the slice's own pixels, from a square `size` by `size`
/// map covering a part of a slice `width` by `height`, by bilinear resampling.
pub fn resample(logits: &[f32], size: usize, width: usize, height: usize) -> Vec<f32> {
    let axis = |i: usize, n: usize| {
        let at = (((i as f32 + 0.5) * size as f32 / n as f32) - 0.5).clamp(0.0, (size - 1) as f32);
        let low = at.floor() as usize;
        (low, (low + 1).min(size - 1), at - low as f32)
    };
    let columns: Vec<_> = (0..width).map(|x| axis(x, width)).collect();
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let (y0, y1, fy) = axis(y, height);
        let (top, bottom) = (&logits[size * y0..size * (y0 + 1)], &logits[size * y1..size * (y1 + 1)]);
        out.extend(columns.iter().map(|&(x0, x1, fx)| {
            let upper = top[x0] * (1.0 - fx) + top[x1] * fx;
            let lower = bottom[x0] * (1.0 - fx) + bottom[x1] * fx;
            upper * (1.0 - fy) + lower * fy
        }));
    }
    out
}

/// Distance in voxels, counting diagonals as one, from each voxel of `on` equal
/// to `of` to the nearest that is not. Off the slice counts as not when `of` is
/// true, so that a mask at the side of a slice is eroded there too.
fn distance(on: &[bool], width: usize, height: usize, of: bool) -> Vec<u32> {
    let far = (width + height) as u32;
    let mut d: Vec<u32> = on.iter().map(|v| if *v == of { far } else { 0 }).collect();
    let edge = |x: usize, y: usize| x == 0 || y == 0 || x + 1 == width || y + 1 == height;
    for y in 0..height {
        for x in 0..width {
            let i = x + width * y;
            if d[i] == 0 {
                continue;
            }
            let mut best = if of && edge(x, y) { 1 } else { far };
            if x > 0 {
                best = best.min(d[i - 1] + 1);
            }
            if y > 0 {
                best = best.min(d[i - width] + 1);
                if x > 0 {
                    best = best.min(d[i - width - 1] + 1);
                }
                if x + 1 < width {
                    best = best.min(d[i - width + 1] + 1);
                }
            }
            d[i] = d[i].min(best);
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let i = x + width * y;
            if d[i] == 0 {
                continue;
            }
            let mut best = d[i];
            if x + 1 < width {
                best = best.min(d[i + 1] + 1);
            }
            if y + 1 < height {
                best = best.min(d[i + width] + 1);
                if x + 1 < width {
                    best = best.min(d[i + width + 1] + 1);
                }
                if x > 0 {
                    best = best.min(d[i + width - 1] + 1);
                }
            }
            d[i] = best;
        }
    }
    d
}

/// The value at a share of the way through these values, which are reordered.
fn percentile(values: &mut [f32], share: f32) -> f32 {
    let at = ((values.len() - 1) as f32 * share).round() as usize;
    *values.select_nth_unstable_by(at, f32::total_cmp).1
}

#[inline]
fn around(i: usize, width: usize, height: usize, mut visit: impl FnMut(usize)) {
    let (x, y) = (i % width, i / width);
    for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
        for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
            let n = nx + width * ny;
            if n != i {
                visit(n);
            }
        }
    }
}

/// The level beyond the edge, for each voxel of the band. An edge is halfway to
/// what lies past it, and that differs around one object: the wall of a sinus has
/// air on one side and soft tissue on the other, and one level for both puts both
/// edges wrong. So each voxel takes the ring nearest it, averaged along the ring
/// to quiet noise. Ring voxels at or above `limit` are other bone, not background,
/// so what faces them takes the floor of the valley between the two: the edge of
/// a plate near another bone is halfway down to that, not to the air on its far
/// side, which would let it reach across a narrow gap.
fn surroundings(
    values: &[f32],
    ring: &[usize],
    in_band: &impl Fn(usize) -> bool,
    (width, height): (usize, usize),
    band: u32,
    limit: f32,
    overall: f32,
) -> Vec<f32> {
    let mut on_ring = vec![false; values.len()];
    for &i in ring {
        on_ring[i] = values[i] < limit;
    }
    let mut level = vec![overall; values.len()];
    let mut reached = vec![false; values.len()];
    let mut queue = VecDeque::with_capacity(ring.len());
    let reach = band as usize;
    for &i in ring {
        reached[i] = true;
        queue.push_back(i);
        let (x, y) = (i % width, i / width);
        let (mut sum, mut n, mut floor) = (0.0, 0u32, values[i]);
        for ny in y.saturating_sub(reach)..=(y + reach).min(height - 1) {
            for nx in x.saturating_sub(reach)..=(x + reach).min(width - 1) {
                let k = nx + width * ny;
                if on_ring[k] {
                    sum += values[k];
                    n += 1;
                } else if in_band(k) {
                    floor = floor.min(values[k]);
                }
            }
        }
        level[i] = if on_ring[i] { sum / n as f32 } else { floor };
    }
    while let Some(i) = queue.pop_front() {
        let from = level[i];
        around(i, width, height, |n| {
            if !reached[n] && in_band(n) {
                reached[n] = true;
                level[n] = from;
                queue.push_back(n);
            }
        });
    }
    level
}

/// What refinement read from a slice, kept so a result can be explained.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// The median of the core.
    pub core: f32,
    /// The median of the outer ring, all the way round.
    pub outer: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Refined {
    pub mask: Vec<bool>,
    /// None when the slice gave too little to work from and the model's mask
    /// was left as it was.
    pub reading: Option<Reading>,
}

/// Refine one slice's mask. `values` and `mask` are `width` by `height`.
pub fn refine_slice(values: &[f32], mask: &[bool], width: usize, height: usize, edge: Edge, band: usize) -> Refined {
    let unchanged = || Refined {
        mask: mask.to_vec(),
        reading: None,
    };
    let band = band.clamp(*BAND_RANGE.start(), *BAND_RANGE.end()) as u32;
    if !mask.iter().any(|v| *v) {
        return unchanged();
    }
    let inside = distance(mask, width, height, true);
    let outside = distance(mask, width, height, false);

    // The core: trusted as object. On a thin mask, erode less.
    let erosion = band.saturating_sub(1).max(1);
    let deepest = inside.iter().copied().max().unwrap_or(0);
    let keep = erosion.min(deepest.saturating_sub(1));
    let core: Vec<bool> = inside.iter().map(|d| *d > keep).collect();

    // Only these voxels can ever be in the result.
    let reach = |i: usize| mask[i] || outside[i] <= band;

    let ring_voxels: Vec<usize> = (0..mask.len()).filter(|&i| !mask[i] && outside[i] == band).collect();
    let mut solid: Vec<f32> = (0..mask.len()).filter(|&i| core[i]).map(|i| values[i]).collect();
    if ring_voxels.is_empty() || solid.is_empty() {
        return unchanged();
    }
    let mut ring: Vec<f32> = ring_voxels.iter().map(|&i| values[i]).collect();
    let outer = percentile(&mut ring, 0.5);
    let top = percentile(&mut solid, 0.95);
    let bottom = percentile(&mut solid, 0.05);
    if edge == Edge::Bone {
        // The plateau of the cortex, whose half-maximum this is. A long bone's
        // core is mostly marrow, an overshooting mask's is partly soft tissue,
        // and either way the edge voxels between them and the plateau pull a
        // plain median down, on a thin plate most of all.
        let middle = outer + (top - outer) * PLATEAU;
        solid.retain(|v| *v >= middle);
    }
    if solid.is_empty() {
        return unchanged();
    }
    let centre = percentile(&mut solid, 0.5);
    if centre <= outer {
        return unchanged();
    }
    let high = match edge {
        Edge::Bone => f32::MAX,
        Edge::SoftTissue | Edge::Contrast => top + (top - bottom),
    };
    let surround = surroundings(
        values,
        &ring_voxels,
        &reach,
        (width, height),
        band,
        (outer + centre) / 2.0,
        outer,
    );
    // Half-maximum for bone and the midpoint for the rest come to the same
    // arithmetic, against what lies beyond this part of the edge; what differs
    // is the step allowed and the ceiling.
    let share = match edge {
        Edge::Bone => STEP,
        Edge::SoftTissue | Edge::Contrast => SOFT_STEP,
    };
    let contrast = |i: usize| (centre - surround[i]).max(0.0);
    let threshold = |i: usize| (centre + surround[i]) / 2.0;
    let passes = |i: usize| values[i] >= threshold(i) && values[i] <= high && contrast(i) > 0.0;
    let faint = |i: usize| values[i] < threshold(i) && values[i] >= threshold(i) - contrast(i) * WEAK;

    // What growth starts from. A mask that overshot by more than the erosion has
    // soft tissue in its core, which a thin plate cannot afford, so for bone a
    // core voxel must read as bone. Deep inside the mask it is trusted whatever
    // it reads: that is marrow or a sinus the model took as part of the object.
    let trusted = 2 * band;
    let mut seeds: Vec<bool> = (0..mask.len())
        .map(|i| core[i] && (edge != Edge::Bone || inside[i] > trusted || passes(i)))
        .collect();
    if !seeds.iter().any(|v| *v) {
        seeds = core;
    }
    let in_band = |i: usize| !seeds[i] && reach(i);

    let mut joined = seeds.clone();
    let mut queue: VecDeque<usize> = (0..mask.len()).filter(|&i| seeds[i]).collect();
    let grow = |joined: &mut Vec<bool>, queue: &mut VecDeque<usize>, within: &dyn Fn(usize) -> bool| {
        while let Some(i) = queue.pop_front() {
            let here = values[i];
            around(i, width, height, |n| {
                let v = values[n];
                // Values fall on leaving a bone. Past the model's edge a rise
                // beyond noise is something else, across a valley.
                let climbs = !mask[n] && v > here + contrast(n) * CLIMB;
                if !joined[n] && within(n) && passes(n) && !climbs && (v - here).abs() <= contrast(n) * share {
                    joined[n] = true;
                    queue.push_back(n);
                }
            });
        }
    };
    grow(&mut joined, &mut queue, &in_band);

    // A weak gap: a short run of voxels just under the threshold, joining pieces
    // that are otherwise apart, all of it inside the model's mask. That last part
    // matters: the model says what the object is, so a cortex that fades is
    // followed, while a neighbouring bone it left out is never reached, however
    // little the valley between them dips. And a run must join something, or
    // every notch in an edge would be filled and a thin plate would thicken.
    let in_mask = |i: usize| reach(i) && mask[i];
    let mut solid: Vec<bool> = (0..mask.len())
        .map(|i| joined[i] || (in_mask(i) && passes(i)))
        .collect();
    let mut labels = label(&solid, width, height);
    for _ in 0..WEAK_ROUNDS {
        let path = weak_paths(&solid, &labels, (width, height), |i| in_mask(i) && faint(i));
        if path.is_empty() {
            break;
        }
        path.iter().for_each(|&i| solid[i] = true);
        labels = label(&solid, width, height);
    }
    let mut seeded = vec![false; labels.iter().copied().max().unwrap_or(0) as usize + 1];
    for i in (0..mask.len()).filter(|&i| seeds[i]) {
        seeded[labels[i] as usize] = true;
    }
    let joined: Vec<bool> = labels.iter().map(|l| *l != 0 && seeded[*l as usize]).collect();

    let filled = fill_enclosed(&joined, width, height, reach);
    Refined {
        mask: filled,
        reading: Some(Reading { core: centre, outer }),
    }
}

/// Number the connected pieces of a slice from 1; what is off is 0.
fn label(on: &[bool], width: usize, height: usize) -> Vec<u32> {
    let mut labels = vec![0u32; on.len()];
    let mut next = 0;
    let mut stack = Vec::new();
    for start in 0..on.len() {
        if !on[start] || labels[start] != 0 {
            continue;
        }
        next += 1;
        labels[start] = next;
        stack.push(start);
        while let Some(i) = stack.pop() {
            around(i, width, height, |n| {
                if on[n] && labels[n] == 0 {
                    labels[n] = next;
                    stack.push(n);
                }
            });
        }
    }
    labels
}

/// Runs of `weak` voxels no longer than the reach that join two pieces of
/// `labels`, from both ends at once.
fn weak_paths(
    solid: &[bool],
    labels: &[u32],
    (width, height): (usize, usize),
    weak: impl Fn(usize) -> bool,
) -> Vec<usize> {
    const NONE: usize = usize::MAX;
    let mut depth = vec![0usize; solid.len()];
    let mut source = vec![0u32; solid.len()];
    let mut parent = vec![NONE; solid.len()];
    let mut queue = VecDeque::new();
    for i in (0..solid.len()).filter(|&i| solid[i]) {
        around(i, width, height, |n| {
            if !solid[n] && depth[n] == 0 && weak(n) {
                depth[n] = 1;
                source[n] = labels[i];
                queue.push_back(n);
            }
        });
    }
    let mut ends = Vec::new();
    while let Some(i) = queue.pop_front() {
        around(i, width, height, |n| {
            if solid[n] {
                if labels[n] != source[i] {
                    ends.push(i);
                }
            } else if depth[n] != 0 {
                if source[n] != source[i] && depth[n] + depth[i] <= WEAK_REACH {
                    ends.push(i);
                    ends.push(n);
                }
            } else if depth[i] < WEAK_REACH && weak(n) {
                depth[n] = depth[i] + 1;
                source[n] = source[i];
                parent[n] = i;
                queue.push_back(n);
            }
        });
    }
    let mut path = Vec::new();
    for mut i in ends {
        while i != NONE {
            path.push(i);
            i = parent[i];
        }
    }
    path
}

/// Fill spaces enclosed in the slice, but only a space lying wholly where the
/// mask could reach: a ring around a sinus does not fill the sinus.
fn fill_enclosed(on: &[bool], width: usize, height: usize, reachable: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut out = on.to_vec();
    let mut seen = on.to_vec();
    let mut piece = Vec::new();
    for start in 0..on.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        piece.clear();
        piece.push(start);
        let (mut next, mut open) = (0, false);
        while next < piece.len() {
            let i = piece[next];
            next += 1;
            let (x, y) = (i % width, i / width);
            open |= x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            let mut visit = |n: usize| {
                if !seen[n] {
                    seen[n] = true;
                    piece.push(n);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < width {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - width);
            }
            if y + 1 < height {
                visit(i + width);
            }
        }
        if !open && piece.iter().all(|i| reachable(*i)) {
            for &i in &piece {
                out[i] = true;
            }
        }
    }
    out
}

/// Slices whose area differs by more than the limit from the mean of the slices
/// either side, for a person to look at. `areas` holds `(slice, area)`; a slice
/// at the end of the object has one neighbour.
pub fn flag_slices(areas: &[(usize, usize)]) -> Vec<usize> {
    let mut sorted = areas.to_vec();
    sorted.sort_unstable();
    let area = |z: usize| sorted.binary_search_by_key(&z, |p| p.0).ok().map(|k| sorted[k].1);
    let mut flagged = Vec::new();
    for &(z, here) in &sorted {
        let sides: Vec<usize> = [z.checked_sub(1).and_then(area), area(z + 1)]
            .into_iter()
            .flatten()
            .collect();
        if sides.is_empty() {
            continue;
        }
        let mean = sides.iter().sum::<usize>() as f32 / sides.len() as f32;
        if mean > 0.0 && (here as f32 - mean).abs() / mean > AREA_CHANGE {
            flagged.push(z);
        }
    }
    flagged
}
