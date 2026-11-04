//! The classical tools. They work on every machine and give results a user can
//! predict and explain, and they remain the fallback when a model does poorly
//! on an animal it was not trained on.

use std::collections::VecDeque;

use rayon::prelude::*;

use crate::{Field, Grid, Mask};

/// Voxels between two values.
pub fn threshold<F: Field>(field: &F, low: f32, high: f32) -> Mask {
    let grid = field.grid();
    Mask {
        grid,
        on: (0..grid.len())
            .into_par_iter()
            .map(|i| (low..=high).contains(&field.at(i)))
            .collect(),
    }
}

/// The 26 neighbours within bounds. Thin bone is often joined only across
/// edges and corners, so faces alone would lose it.
#[inline]
fn neighbours(grid: &Grid, i: usize, mut visit: impl FnMut(usize)) {
    let [x, y, z] = grid.position(i);
    let span = |v: usize, n: usize| v.saturating_sub(1)..=(v + 1).min(n - 1);
    for nz in span(z, grid.dims[2]) {
        for ny in span(y, grid.dims[1]) {
            for nx in span(x, grid.dims[0]) {
                let n = grid.index(nx, ny, nz);
                if n != i {
                    visit(n);
                }
            }
        }
    }
}

/// How region grow decides. Every value is shown beside the result, so a user
/// can see why a wall was or was not included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grow {
    pub low: f32,
    pub high: f32,
    /// The largest step between connected voxels. Growth stops at a strong
    /// edge rather than where a value crosses a line, and thin bone that
    /// partial volume has pulled below `low` is followed by up to this much.
    pub gradient: f32,
}

/// Add connected voxels from the seeds. A voxel joins from a neighbour already
/// in if the step between them is within the gradient limit and its value is
/// within the range, extended downward by that limit. So thin bone whose
/// values are low stays connected, and air, a large step away, does not.
pub fn region_grow<F: Field>(field: &F, seeds: &[usize], g: Grow) -> Mask {
    let grid = field.grid();
    let mut mask = Mask::empty(grid);
    let floor = g.low - g.gradient;
    let allowed = |v: f32| v >= floor && v <= g.high;
    let mut queue = VecDeque::new();
    for &s in seeds {
        if s < grid.len() && !mask.on[s] {
            mask.on[s] = true;
            queue.push_back(s);
        }
    }
    while let Some(i) = queue.pop_front() {
        let here = field.at(i);
        neighbours(&grid, i, |n| {
            if !mask.on[n] {
                let v = field.at(n);
                if allowed(v) && (v - here).abs() <= g.gradient {
                    mask.on[n] = true;
                    queue.push_back(n);
                }
            }
        });
    }
    mask
}

/// Everything connected to a seed through voxels `allowed` accepts, such as
/// the table a user has clicked. Air usually separates it from the patient.
pub fn flood(grid: &Grid, seed: usize, allowed: impl Fn(usize) -> bool) -> Vec<usize> {
    if seed >= grid.len() || !allowed(seed) {
        return Vec::new();
    }
    let mut seen = vec![false; grid.len()];
    seen[seed] = true;
    let mut piece = vec![seed];
    let mut next = 0;
    while next < piece.len() {
        let i = piece[next];
        next += 1;
        neighbours(grid, i, |n| {
            if !seen[n] && allowed(n) {
                seen[n] = true;
                piece.push(n);
            }
        });
    }
    piece
}

/// A closed outline on the view, rasterised once so that testing a hundred
/// million voxels against it takes a lookup each.
pub struct Outline {
    size: usize,
    inside: Vec<bool>,
}

impl Outline {
    /// `points` are in -1 to 1 across the view.
    pub fn new(points: &[[f32; 2]]) -> Self {
        const SIZE: usize = 2048;
        let mut inside = vec![false; SIZE * SIZE];
        if points.len() >= 3 {
            let to_pixel = |v: f32| (v + 1.0) * 0.5 * SIZE as f32;
            for row in 0..SIZE {
                let y = row as f32 + 0.5;
                let mut crossings = Vec::new();
                for (k, a) in points.iter().enumerate() {
                    let b = points[(k + 1) % points.len()];
                    let (ay, by) = (to_pixel(a[1]), to_pixel(b[1]));
                    if (ay > y) != (by > y) {
                        let (ax, bx) = (to_pixel(a[0]), to_pixel(b[0]));
                        crossings.push(ax + (y - ay) / (by - ay) * (bx - ax));
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for pair in crossings.as_chunks::<2>().0 {
                    let from = pair[0].max(0.0) as usize;
                    let to = (pair[1].max(0.0) as usize).min(SIZE);
                    for x in from..to {
                        inside[row * SIZE + x] = true;
                    }
                }
            }
        }
        Self { size: SIZE, inside }
    }

    #[inline]
    pub fn contains(&self, p: [f32; 2]) -> bool {
        let pixel = |v: f32| ((v + 1.0) * 0.5 * self.size as f32).floor();
        let (x, y) = (pixel(p[0]), pixel(p[1]));
        x >= 0.0
            && y >= 0.0
            && (x as usize) < self.size
            && (y as usize) < self.size
            && self.inside[y as usize * self.size + x as usize]
    }
}

/// Connected pieces of a mask, largest first.
pub fn islands(mask: &Mask) -> Vec<Vec<usize>> {
    let grid = mask.grid;
    let mut seen = vec![false; grid.len()];
    let mut out = Vec::new();
    for start in 0..grid.len() {
        if !mask.on[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut piece = vec![start];
        let mut next = 0;
        while next < piece.len() {
            let i = piece[next];
            next += 1;
            neighbours(&grid, i, |n| {
                if mask.on[n] && !seen[n] {
                    seen[n] = true;
                    piece.push(n);
                }
            });
        }
        out.push(piece);
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.len()));
    out
}

fn from_voxels(grid: Grid, voxels: &[usize]) -> Mask {
    let mut m = Mask::empty(grid);
    for &i in voxels {
        m.on[i] = true;
    }
    m
}

/// The largest piece: table, collar and noise fall away.
pub fn keep_largest(mask: &Mask) -> Mask {
    match islands(mask).first() {
        Some(p) => from_voxels(mask.grid, p),
        None => Mask::empty(mask.grid),
    }
}

/// The piece under a click.
pub fn keep_at(mask: &Mask, voxel: usize) -> Mask {
    match islands(mask).iter().find(|p| p.contains(&voxel)) {
        Some(p) => from_voxels(mask.grid, p),
        None => Mask::empty(mask.grid),
    }
}

/// Remove pieces smaller than a volume in cubic millimetres.
pub fn remove_smaller(mask: &Mask, mm3: f64) -> Mask {
    let least = (mm3 / mask.grid.voxel_mm3()).ceil() as usize;
    let mut out = Mask::empty(mask.grid);
    for piece in islands(mask).iter().filter(|p| p.len() >= least) {
        for &i in piece {
            out.on[i] = true;
        }
    }
    out
}

/// Each piece as its own mask, such as the fragments of a displaced fracture.
pub fn split(mask: &Mask) -> Vec<Mask> {
    islands(mask).iter().map(|p| from_voxels(mask.grid, p)).collect()
}

/// Offsets within an ellipsoid of `radius_mm`, in voxels on this grid, so a
/// radius means the same in every direction on anisotropic data.
fn ball(grid: &Grid, radius_mm: f64) -> Vec<[isize; 3]> {
    let reach = |a: usize| (radius_mm / grid.spacing[a]).floor() as isize;
    let (rx, ry, rz) = (reach(0), reach(1), reach(2));
    let mut out = Vec::new();
    for z in -rz..=rz {
        for y in -ry..=ry {
            for x in -rx..=rx {
                let d = |v: isize, a: usize| (v as f64 * grid.spacing[a]).powi(2);
                if d(x, 0) + d(y, 1) + d(z, 2) <= radius_mm * radius_mm {
                    out.push([x, y, z]);
                }
            }
        }
    }
    out
}

fn offset(grid: &Grid, p: [usize; 3], o: [isize; 3]) -> Option<usize> {
    let mut q = [0usize; 3];
    for a in 0..3 {
        let v = p[a] as isize + o[a];
        if v < 0 || v >= grid.dims[a] as isize {
            return None;
        }
        q[a] = v as usize;
    }
    Some(grid.index(q[0], q[1], q[2]))
}

pub fn dilate(mask: &Mask, radius_mm: f64) -> Mask {
    let grid = mask.grid;
    let shape = ball(&grid, radius_mm);
    let on = (0..grid.len())
        .into_par_iter()
        .map(|i| {
            let p = grid.position(i);
            mask.on[i] || shape.iter().any(|o| offset(&grid, p, *o).is_some_and(|n| mask.on[n]))
        })
        .collect();
    Mask { grid, on }
}

/// The edge of the volume counts as outside.
pub fn erode(mask: &Mask, radius_mm: f64) -> Mask {
    let grid = mask.grid;
    let shape = ball(&grid, radius_mm);
    let on = (0..grid.len())
        .into_par_iter()
        .map(|i| {
            let p = grid.position(i);
            mask.on[i] && shape.iter().all(|o| offset(&grid, p, *o).is_some_and(|n| mask.on[n]))
        })
        .collect();
    Mask { grid, on }
}

pub fn open(mask: &Mask, radius_mm: f64) -> Mask {
    dilate(&erode(mask, radius_mm), radius_mm)
}

/// Close small gaps in thin bone before building a surface.
pub fn close(mask: &Mask, radius_mm: f64) -> Mask {
    erode(&dilate(mask, radius_mm), radius_mm)
}

/// Fill every space not reachable from the edge of the volume through faces.
pub fn fill_holes(mask: &Mask) -> Mask {
    let grid = mask.grid;
    let [w, h, d] = grid.dims;
    let mut outside = vec![false; grid.len()];
    let mut stack = Vec::new();
    for (i, out) in outside.iter_mut().enumerate() {
        let [x, y, z] = grid.position(i);
        let edge = x == 0 || y == 0 || z == 0 || x == w - 1 || y == h - 1 || z == d - 1;
        if edge && !mask.on[i] {
            *out = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        let p = grid.position(i);
        for o in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
            if let Some(n) = offset(&grid, p, o) {
                if !outside[n] && !mask.on[n] {
                    outside[n] = true;
                    stack.push(n);
                }
            }
        }
    }
    Mask {
        grid,
        on: outside.iter().map(|o| !o).collect(),
    }
}

/// The voxels of a sphere in millimetres on the grid, for the brush. A value
/// range limits it, so it paints only bone.
pub fn brush<F: Field>(field: &F, centre: [usize; 3], radius_mm: f64, range: Option<(f32, f32)>) -> Mask {
    let grid = field.grid();
    let mut m = Mask::empty(grid);
    for o in ball(&grid, radius_mm) {
        if let Some(n) = offset(&grid, centre, o) {
            if range.is_none_or(|(low, high)| (low..=high).contains(&field.at(n))) {
                m.on[n] = true;
            }
        }
    }
    m
}

/// Fill voxels missing from a single slice but present on the slices either
/// side, which is what a slice lost to a tracking slip leaves behind.
pub fn fill_slice_gaps(mask: &Mask) -> Mask {
    let grid = mask.grid;
    let plane = grid.dims[0] * grid.dims[1];
    let on = (0..grid.len())
        .into_par_iter()
        .map(|i| mask.on[i] || (i >= plane && i + plane < grid.len() && mask.on[i - plane] && mask.on[i + plane]))
        .collect();
    Mask { grid, on }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine {
    Union,
    Intersection,
    /// The first without the second: bone minus implant.
    Subtraction,
}

pub fn combine(a: &Mask, b: &Mask, how: Combine) -> Mask {
    let on =
        a.on.iter()
            .zip(&b.on)
            .map(|(x, y)| match how {
                Combine::Union => *x || *y,
                Combine::Intersection => *x && *y,
                Combine::Subtraction => *x && !*y,
            })
            .collect();
    Mask { grid: a.grid, on }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Values;

    fn cube(n: usize) -> Grid {
        Grid {
            dims: [n, n, n],
            spacing: [1.0, 1.0, 1.0],
        }
    }

    /// Acceptance 1: a solid sphere and, around it, a curved shell one voxel
    /// thick. Partial volume makes the shell's values wander from well above
    /// the bone level to well below it, as thin bone's do.
    fn sphere_and_shell() -> (Values, f32, usize, Mask) {
        const LEVEL: f32 = 1000.0;
        let g = cube(64);
        let mut data = vec![0.0; g.len()];
        let mut shell = Mask::empty(g);
        let mut seed = 0;
        let c = 31.5;
        for (i, value) in data.iter_mut().enumerate() {
            let [x, y, z] = g.position(i);
            let (dx, dy, dz) = (x as f32 - c, y as f32 - c, z as f32 - c);
            let r = (dx * dx + dy * dy + dz * dz).sqrt();
            if r <= 10.0 {
                *value = 1500.0;
            } else if (r - 26.0).abs() <= 0.5 {
                // Two lobes low and two high, varying smoothly over the shell.
                let wave = (dx * 0.35).sin() * (dy * 0.35).cos();
                *value = 1000.0 + 450.0 * wave;
                shell.on[i] = true;
                if wave > 0.9 {
                    seed = i;
                }
            }
        }
        (Values { grid: g, data }, LEVEL, seed, shell)
    }

    #[test]
    fn region_grow_keeps_a_thin_shell_that_a_threshold_breaks() {
        let (field, level, seed, shell) = sphere_and_shell();
        let wanted = shell.count();

        let global = threshold(&field, level, f32::MAX);
        let kept = global.on.iter().zip(&shell.on).filter(|(g, s)| **g && **s).count();
        assert!(
            kept < wanted * 3 / 4,
            "a threshold at the same level must leave holes: it kept {kept} of {wanted}"
        );

        let grown = region_grow(
            &field,
            &[seed],
            Grow {
                low: level,
                high: f32::MAX,
                gradient: 500.0,
            },
        );
        let captured = grown.on.iter().zip(&shell.on).filter(|(g, s)| **g && **s).count();
        assert_eq!(captured, wanted, "from one seed, the whole shell");
        assert_eq!(grown.count(), wanted, "and nothing else: not the sphere, not the air");
    }

    #[test]
    fn a_strong_edge_stops_growth_even_within_the_range() {
        let g = cube(8);
        let mut data = vec![100.0; g.len()];
        for z in 4..8 {
            for y in 0..8 {
                for x in 0..8 {
                    data[g.index(x, y, z)] = 900.0;
                }
            }
        }
        let f = Values { grid: g, data };
        let m = region_grow(
            &f,
            &[0],
            Grow {
                low: 0.0,
                high: 1000.0,
                gradient: 50.0,
            },
        );
        assert_eq!(m.count(), 8 * 8 * 4);
    }

    fn two_blobs() -> Mask {
        let g = cube(16);
        let mut m = Mask::empty(g);
        for z in 1..7 {
            for y in 1..7 {
                for x in 1..7 {
                    m.on[g.index(x, y, z)] = true;
                }
            }
        }
        for z in 10..12 {
            for y in 10..12 {
                for x in 10..12 {
                    m.on[g.index(x, y, z)] = true;
                }
            }
        }
        m
    }

    #[test]
    fn flood_takes_only_what_is_connected() {
        let m = two_blobs();
        let g = m.grid;
        let table = flood(&g, g.index(10, 10, 10), |i| m.on[i]);
        assert_eq!(table.len(), 8);
        assert!(flood(&g, 0, |i| m.on[i]).is_empty(), "a seed in the air takes nothing");
    }

    #[test]
    fn an_outline_knows_its_inside() {
        let square = Outline::new(&[[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]);
        assert!(square.contains([0.0, 0.0]));
        assert!(square.contains([0.49, -0.49]));
        assert!(!square.contains([0.6, 0.0]));
        assert!(!square.contains([-2.0, 0.0]), "off the view is outside");
        // A shape with a bay: the bay is outside.
        let c = Outline::new(&[
            [-0.8, -0.8],
            [0.8, -0.8],
            [0.8, -0.4],
            [-0.4, -0.4],
            [-0.4, 0.4],
            [0.8, 0.4],
            [0.8, 0.8],
            [-0.8, 0.8],
        ]);
        assert!(c.contains([-0.6, 0.0]));
        assert!(!c.contains([0.2, 0.0]));
        assert!(
            !Outline::new(&[[0.0, 0.0], [1.0, 1.0]]).contains([0.5, 0.5]),
            "a line encloses nothing"
        );
    }

    #[test]
    fn islands_separate_fragments() {
        let m = two_blobs();
        let g = m.grid;
        assert_eq!(keep_largest(&m).count(), 216);
        assert_eq!(keep_at(&m, g.index(10, 10, 10)).count(), 8);
        assert_eq!(keep_at(&m, 0).count(), 0);
        assert_eq!(remove_smaller(&m, 9.0).count(), 216);
        let pieces = split(&m);
        assert_eq!(pieces.iter().map(Mask::count).collect::<Vec<_>>(), [216, 8]);
    }

    #[test]
    fn close_bridges_a_slit_and_open_removes_a_speck() {
        // Two blocks of bone with a one-voxel crack between them.
        let g = cube(20);
        let mut bone = Mask::empty(g);
        for i in 0..g.len() {
            let [x, y, z] = g.position(i);
            bone.on[i] = (3..16).contains(&x) && x != 9 && (5..15).contains(&y) && (5..15).contains(&z);
        }
        assert_eq!(islands(&bone).len(), 2);
        assert_eq!(islands(&close(&bone, 1.5)).len(), 1);
        assert!(open(&two_blobs(), 1.0).count() < two_blobs().count());
    }

    #[test]
    fn a_radius_is_in_millimetres_on_anisotropic_data() {
        let g = Grid {
            dims: [21, 21, 21],
            spacing: [0.5, 0.5, 2.0],
        };
        let mut m = Mask::empty(g);
        m.on[g.index(10, 10, 10)] = true;
        let d = dilate(&m, 2.0);
        assert!(d.on[g.index(14, 10, 10)], "four 0.5 mm voxels along x");
        assert!(d.on[g.index(10, 10, 11)], "one 2 mm voxel along z");
        assert!(!d.on[g.index(10, 10, 12)]);
    }

    #[test]
    fn holes_fill_but_open_spaces_do_not() {
        let g = cube(9);
        let mut shell = Mask::empty(g);
        for i in 0..g.len() {
            let [x, y, z] = g.position(i);
            let on_wall = [x, y, z].iter().any(|v| *v == 2 || *v == 6);
            shell.on[i] = on_wall && [x, y, z].iter().all(|v| (2..=6).contains(v));
        }
        assert_eq!(fill_holes(&shell).count(), 125);
        let mut open_box = shell.clone();
        open_box.on[g.index(2, 4, 4)] = false;
        assert_eq!(fill_holes(&open_box).count(), open_box.count());
    }

    #[test]
    fn the_brush_can_be_limited_to_bone() {
        let g = cube(11);
        let mut data = vec![0.0; g.len()];
        data[g.index(5, 5, 5)] = 1000.0;
        let f = Values { grid: g, data };
        assert!(brush(&f, [5, 5, 5], 2.0, None).count() > 20);
        assert_eq!(brush(&f, [5, 5, 5], 2.0, Some((500.0, 2000.0))).count(), 1);
    }

    #[test]
    fn a_slice_gap_fills_but_a_wider_one_does_not() {
        let g = cube(4);
        let mut m = Mask::empty(g);
        for z in [0, 2, 3] {
            m.on[g.index(1, 1, z)] = true;
        }
        let filled = fill_slice_gaps(&m);
        assert!(filled.on[g.index(1, 1, 1)]);
        assert_eq!(filled.count(), 4);
        let mut wide = Mask::empty(g);
        wide.on[g.index(1, 1, 0)] = true;
        wide.on[g.index(1, 1, 3)] = true;
        assert_eq!(fill_slice_gaps(&wide).count(), 2);
    }

    #[test]
    fn combining_subtracts_an_implant_from_bone() {
        let g = cube(4);
        let (mut bone, mut implant) = (Mask::empty(g), Mask::empty(g));
        bone.on[0] = true;
        bone.on[1] = true;
        implant.on[1] = true;
        implant.on[2] = true;
        assert_eq!(combine(&bone, &implant, Combine::Subtraction).count(), 1);
        assert_eq!(combine(&bone, &implant, Combine::Union).count(), 3);
        assert_eq!(combine(&bone, &implant, Combine::Intersection).count(), 1);
    }
}
