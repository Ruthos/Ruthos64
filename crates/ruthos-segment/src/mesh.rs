//! Surfaces. A surface is extracted from one segment's mask, never from a raw
//! threshold, so what it shows is exactly what the segment contains.

use std::collections::{HashMap, HashSet};

use crate::{Grid, Mask};

/// A triangle mesh in millimetres.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

/// How far a mesh is from being printable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soundness {
    /// Edges with one triangle: holes.
    pub open_edges: usize,
    /// Edges with more than two triangles.
    pub nonmanifold_edges: usize,
}

impl Soundness {
    pub fn closed_and_manifold(&self) -> bool {
        self.open_edges == 0 && self.nonmanifold_edges == 0
    }
}

/// Extract a surface with surface nets: one vertex per cell the surface
/// crosses, at the mean of the crossings, which gives fewer and better-shaped
/// triangles than marching cubes. Spacing is applied, so the mesh is in true
/// millimetres on anisotropic volumes.
pub fn surface_nets(mask: &Mask) -> Mesh {
    let grid = mask.grid;
    let [w, h, d] = grid.dims;
    // A cell (x, y, z) has corners at voxel centres from (x-1, y-1, z-1) to
    // (x, y, z); cells run one past the volume, so a segment touching its edge
    // still closes.
    let inside = |x: isize, y: isize, z: isize| {
        x >= 0
            && y >= 0
            && z >= 0
            && (x as usize) < w
            && (y as usize) < h
            && (z as usize) < d
            && mask.on[grid.index(x as usize, y as usize, z as usize)]
    };
    let cell_key = |x: usize, y: usize, z: usize| x + (w + 2) * (y + (h + 2) * z);

    const EDGES: [([isize; 3], [isize; 3]); 12] = [
        ([0, 0, 0], [1, 0, 0]),
        ([0, 1, 0], [1, 1, 0]),
        ([0, 0, 1], [1, 0, 1]),
        ([0, 1, 1], [1, 1, 1]),
        ([0, 0, 0], [0, 1, 0]),
        ([1, 0, 0], [1, 1, 0]),
        ([0, 0, 1], [0, 1, 1]),
        ([1, 0, 1], [1, 1, 1]),
        ([0, 0, 0], [0, 0, 1]),
        ([1, 0, 0], [1, 0, 1]),
        ([0, 1, 0], [0, 1, 1]),
        ([1, 1, 0], [1, 1, 1]),
    ];

    let mut mesh = Mesh::default();
    let mut cells: HashMap<usize, u32> = HashMap::new();
    for z in 0..=d {
        for y in 0..=h {
            for x in 0..=w {
                let (ox, oy, oz) = (x as isize - 1, y as isize - 1, z as isize - 1);
                let corner = |c: [isize; 3]| inside(ox + c[0], oy + c[1], oz + c[2]);
                let (mut sum, mut n) = ([0f64; 3], 0);
                for (a, b) in EDGES {
                    if corner(a) != corner(b) {
                        for axis in 0..3 {
                            sum[axis] += (a[axis] + b[axis]) as f64 * 0.5;
                        }
                        n += 1;
                    }
                }
                if n == 0 {
                    continue;
                }
                cells.insert(cell_key(x, y, z), mesh.vertices.len() as u32);
                let at = |axis: usize, o: isize| (o as f64 + sum[axis] / n as f64) * grid.spacing[axis];
                mesh.vertices.push([at(0, ox), at(1, oy), at(2, oz)]);
            }
        }
    }

    // One quad for every pair of neighbouring voxels the surface runs between,
    // joining the four cells around that pair and wound so that normals point
    // out of the segment. Pairs start one before the volume, so a segment on
    // its low edge closes too.
    for z in -1..d as isize {
        for y in -1..h as isize {
            for x in -1..w as isize {
                let here = inside(x, y, z);
                for axis in 0..3 {
                    let step = |a: usize| (a == axis) as isize;
                    if here == inside(x + step(0), y + step(1), z + step(2)) {
                        continue;
                    }
                    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                    let cell = |du: isize, dv: isize| {
                        let mut c = [x + 1, y + 1, z + 1];
                        c[u] += du;
                        c[v] += dv;
                        cells
                            .get(&cell_key(c[0] as usize, c[1] as usize, c[2] as usize))
                            .copied()
                    };
                    let corners = [cell(-1, -1), cell(0, -1), cell(0, 0), cell(-1, 0)];
                    let [Some(a), Some(b), Some(c), Some(e)] = corners else {
                        continue;
                    };
                    if here {
                        mesh.triangles.push([a, b, c]);
                        mesh.triangles.push([a, c, e]);
                    } else {
                        mesh.triangles.push([a, c, b]);
                        mesh.triangles.push([a, e, c]);
                    }
                }
            }
        }
    }
    mesh
}

impl Mesh {
    /// Enclosed volume in cubic millimetres, by the divergence theorem.
    pub fn volume(&self) -> f64 {
        let mut six = 0.0;
        for t in &self.triangles {
            let [a, b, c] = [
                self.vertices[t[0] as usize],
                self.vertices[t[1] as usize],
                self.vertices[t[2] as usize],
            ];
            six += a[0] * (b[1] * c[2] - b[2] * c[1])
                + a[1] * (b[2] * c[0] - b[0] * c[2])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
        six / 6.0
    }

    pub fn soundness(&self) -> Soundness {
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for t in &self.triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        Soundness {
            open_edges: edges.values().filter(|n| **n == 1).count(),
            nonmanifold_edges: edges.values().filter(|n| **n > 2).count(),
        }
    }

    fn neighbours(&self) -> Vec<Vec<u32>> {
        let mut sets = vec![HashSet::new(); self.vertices.len()];
        for t in &self.triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                sets[a as usize].insert(b);
                sets[b as usize].insert(a);
            }
        }
        sets.into_iter().map(|s| s.into_iter().collect()).collect()
    }

    /// Taubin smoothing: a shrinking pass followed by an inflating one, which
    /// removes the staircase of voxels without thinning the object. Plain
    /// Laplacian smoothing would thin exactly the bone this work exists to keep.
    pub fn taubin(&mut self, passes: usize) {
        const LAMBDA: f64 = 0.5;
        const MU: f64 = -0.53;
        let around = self.neighbours();
        for _ in 0..passes {
            for factor in [LAMBDA, MU] {
                let moved: Vec<[f64; 3]> = self
                    .vertices
                    .iter()
                    .zip(&around)
                    .map(|(v, n)| {
                        if n.is_empty() {
                            return *v;
                        }
                        let mut mean = [0.0; 3];
                        for &i in n {
                            for (m, c) in mean.iter_mut().zip(self.vertices[i as usize]) {
                                *m += c;
                            }
                        }
                        std::array::from_fn(|a| v[a] + factor * (mean[a] / n.len() as f64 - v[a]))
                    })
                    .collect();
                self.vertices = moved;
            }
        }
    }

    /// The model's size along each axis, for the print preview.
    pub fn extent(&self) -> [f64; 3] {
        let mut low = [f64::MAX; 3];
        let mut high = [f64::MIN; 3];
        for v in &self.vertices {
            for a in 0..3 {
                low[a] = low[a].min(v[a]);
                high[a] = high[a].max(v[a]);
            }
        }
        std::array::from_fn(|a| (high[a] - low[a]).max(0.0))
    }

    /// From the grid's millimetres to patient coordinates, so a model lines up
    /// with the scan it came from.
    pub fn to_patient(&mut self, origin: [f64; 3], axes: [[f64; 3]; 3]) {
        for v in &mut self.vertices {
            *v = std::array::from_fn(|c| origin[c] + (0..3).map(|a| axes[a][c] * v[a]).sum::<f64>());
        }
    }

    /// Binary STL. The 80-byte header carries `note`, so that a printed model
    /// can be traced back to its scan.
    pub fn to_stl(&self, note: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(84 + self.triangles.len() * 50);
        let mut header = [b' '; 80];
        let n = note.len().min(80);
        header[..n].copy_from_slice(&note.as_bytes()[..n]);
        out.extend_from_slice(&header);
        out.extend_from_slice(&(self.triangles.len() as u32).to_le_bytes());
        for t in &self.triangles {
            let [a, b, c] = [
                self.vertices[t[0] as usize],
                self.vertices[t[1] as usize],
                self.vertices[t[2] as usize],
            ];
            let (u, v) = (sub(b, a), sub(c, a));
            let mut normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let len = normal.iter().map(|n| n * n).sum::<f64>().sqrt();
            if len > 0.0 {
                normal.iter_mut().for_each(|n| *n /= len);
            }
            for p in [normal, a, b, c] {
                for c in p {
                    out.extend_from_slice(&(c as f32).to_le_bytes());
                }
            }
            out.extend_from_slice(&[0, 0]);
        }
        out
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Smoothing passes by strength, which the user sets from none to strong.
pub fn passes(strength: Smoothing) -> usize {
    match strength {
        Smoothing::None => 0,
        Smoothing::Light => 5,
        Smoothing::Medium => 15,
        Smoothing::Strong => 40,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smoothing {
    None,
    Light,
    Medium,
    Strong,
}

/// A mask for tests of geometry: a ball of `radius` in millimetres.
#[doc(hidden)]
pub fn sphere_mask(grid: Grid, radius: f64) -> Mask {
    let mut m = Mask::empty(grid);
    let centre: [f64; 3] = std::array::from_fn(|a| (grid.dims[a] - 1) as f64 * grid.spacing[a] / 2.0);
    for i in 0..grid.len() {
        let p = grid.position(i);
        let r: f64 = (0..3)
            .map(|a| (p[a] as f64 * grid.spacing[a] - centre[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        m.on[i] = r <= radius;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(n: usize, s: f64) -> Grid {
        Grid {
            dims: [n, n, n],
            spacing: [s, s, s],
        }
    }

    /// Acceptance 2.
    #[test]
    fn a_smoothed_sphere_is_closed_and_within_one_per_cent() {
        let radius = 20.0;
        let mut mesh = surface_nets(&sphere_mask(cube(50, 1.0), radius));
        let s = mesh.soundness();
        assert!(s.closed_and_manifold(), "{s:?}");
        mesh.taubin(passes(Smoothing::Medium));
        let truth = 4.0 / 3.0 * std::f64::consts::PI * radius.powi(3);
        let err = (mesh.volume() - truth).abs() / truth;
        assert!(err < 0.01, "volume off by {:.2}%", err * 100.0);
        assert!(mesh.soundness().closed_and_manifold());
    }

    #[test]
    fn spacing_makes_the_mesh_true_millimetres() {
        let g = Grid {
            dims: [80, 80, 20],
            spacing: [0.5, 0.5, 2.0],
        };
        let radius = 15.0;
        let mut mesh = surface_nets(&sphere_mask(g, radius));
        mesh.taubin(passes(Smoothing::Medium));
        for size in mesh.extent() {
            assert!((size - 2.0 * radius).abs() < 2.0, "{size} mm");
        }
    }

    #[test]
    fn a_segment_touching_the_edge_still_closes() {
        let g = cube(4, 1.0);
        let m = Mask {
            grid: g,
            on: vec![true; g.len()],
        };
        let mesh = surface_nets(&m);
        assert!(mesh.soundness().closed_and_manifold());
        assert!(mesh.volume() > 0.0, "normals point out");
    }

    #[test]
    fn an_empty_segment_has_no_surface() {
        let g = cube(4, 1.0);
        assert!(surface_nets(&Mask::empty(g)).triangles.is_empty());
    }

    #[test]
    fn patient_coordinates_follow_the_series() {
        let mut mesh = Mesh {
            vertices: vec![[1.0, 2.0, 3.0]],
            triangles: Vec::new(),
        };
        // Feet first: z runs the other way, from an origin.
        mesh.to_patient([10.0, 20.0, 30.0], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]]);
        assert_eq!(mesh.vertices[0], [11.0, 22.0, 27.0]);
    }

    #[test]
    fn stl_is_the_right_size_and_carries_its_source() {
        let mesh = surface_nets(&sphere_mask(cube(12, 1.0), 4.0));
        let stl = mesh.to_stl("Series 3, Bone, planning and education only");
        assert_eq!(stl.len(), 84 + mesh.triangles.len() * 50);
        assert!(stl.starts_with(b"Series 3, Bone"));
        assert_eq!(
            u32::from_le_bytes(stl[80..84].try_into().unwrap()) as usize,
            mesh.triangles.len()
        );
    }
}
