//! Fewer triangles for nearly the same shape, by quadric error edge collapse
//! (Garland and Heckbert, 1997). Each vertex carries the planes of the
//! triangles around it; the edge whose collapse moves its new vertex least far
//! from those planes goes first.
//!
//! A collapse is refused when it would leave the surface no longer closed and
//! manifold, or turn a triangle over, so a printable model stays printable.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::mesh::Mesh;

/// Squared distance to a set of planes, as the ten distinct entries of a
/// symmetric 4 x 4 matrix: aa ab ac ad bb bc bd cc cd dd.
#[derive(Debug, Clone, Copy, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    fn plane([a, b, c]: [f64; 3], d: f64) -> Self {
        Self([a * a, a * b, a * c, a * d, b * b, b * c, b * d, c * c, c * d, d * d])
    }

    fn add(&mut self, other: &Self) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    fn plus(mut self, other: &Self) -> Self {
        self.add(other);
        self
    }

    fn error(&self, [x, y, z]: [f64; 3]) -> f64 {
        let q = &self.0;
        q[0] * x * x
            + 2.0 * q[1] * x * y
            + 2.0 * q[2] * x * z
            + 2.0 * q[3] * x
            + q[4] * y * y
            + 2.0 * q[5] * y * z
            + 2.0 * q[6] * y
            + q[7] * z * z
            + 2.0 * q[8] * z
            + q[9]
    }

    /// The point of least error, unless the planes are too nearly parallel to
    /// fix one.
    fn optimum(&self) -> Option<[f64; 3]> {
        let q = &self.0;
        let m = [[q[0], q[1], q[2]], [q[1], q[4], q[5]], [q[2], q[5], q[7]]];
        let b = [-q[3], -q[6], -q[8]];
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        let scale = m.iter().flatten().map(|v| v.abs()).fold(0.0, f64::max).powi(3);
        if det.abs() <= 1e-9 * scale.max(1e-30) {
            return None;
        }
        let solve = |col: usize| {
            let mut n = m;
            for (row, value) in n.iter_mut().zip(b) {
                row[col] = value;
            }
            (n[0][0] * (n[1][1] * n[2][2] - n[1][2] * n[2][1]) - n[0][1] * (n[1][0] * n[2][2] - n[1][2] * n[2][0])
                + n[0][2] * (n[1][0] * n[2][1] - n[1][1] * n[2][0]))
                / det
        };
        Some([solve(0), solve(1), solve(2)])
    }
}

struct Candidate {
    cost: f64,
    a: u32,
    b: u32,
    /// The ends' versions when costed: a newer version means a stale entry.
    versions: (u32, u32),
    at: [f64; 3],
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    /// Cheapest first from a max-heap.
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.total_cmp(&self.cost)
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(u: [f64; 3], v: [f64; 3]) -> [f64; 3] {
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn dot(u: [f64; 3], v: [f64; 3]) -> f64 {
    u[0] * v[0] + u[1] * v[1] + u[2] * v[2]
}

impl Mesh {
    /// Keep about `share` of the triangles, from 0 to 1.
    pub fn decimate(&mut self, share: f64) {
        if !(0.0..1.0).contains(&share) || self.triangles.len() < 16 {
            return;
        }
        let target = ((self.triangles.len() as f64 * share).ceil() as usize).max(8);
        let mut faces = self.triangles.clone();
        let mut alive = vec![true; faces.len()];
        let mut live = faces.len();
        let mut vertex_faces: Vec<Vec<u32>> = vec![Vec::new(); self.vertices.len()];
        let mut quadrics = vec![Quadric::default(); self.vertices.len()];
        for (f, t) in faces.iter().enumerate() {
            let [a, b, c] = t.map(|i| self.vertices[i as usize]);
            let n = cross(sub(b, a), sub(c, a));
            let len = dot(n, n).sqrt();
            if len > 0.0 {
                let n = n.map(|v| v / len);
                let q = Quadric::plane(n, -dot(n, a));
                for &i in t {
                    quadrics[i as usize].add(&q);
                }
            }
            for &i in t {
                vertex_faces[i as usize].push(f as u32);
            }
        }
        let mut gone = vec![false; self.vertices.len()];
        let mut versions = vec![0u32; self.vertices.len()];

        let cost = |vertices: &[[f64; 3]], quadrics: &[Quadric], versions: &[u32], a: u32, b: u32| {
            let q = quadrics[a as usize].plus(&quadrics[b as usize]);
            let (pa, pb) = (vertices[a as usize], vertices[b as usize]);
            let middle = std::array::from_fn(|k| (pa[k] + pb[k]) / 2.0);
            let at = q
                .optimum()
                // Far from the edge means nearly flat planes: the ends are safer.
                .filter(|p| {
                    let reach = dot(sub(pa, pb), sub(pa, pb));
                    dot(sub(*p, middle), sub(*p, middle)) <= 4.0 * reach
                })
                .into_iter()
                .chain([pa, pb, middle])
                .min_by(|x, y| q.error(*x).total_cmp(&q.error(*y)))
                .unwrap_or(middle);
            Candidate {
                cost: q.error(at),
                a,
                b,
                versions: (versions[a as usize], versions[b as usize]),
                at,
            }
        };

        let mut heap = BinaryHeap::new();
        for t in &faces {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                // Each edge once: from the lower vertex.
                if a < b {
                    heap.push(cost(&self.vertices, &quadrics, &versions, a, b));
                }
            }
        }

        let around = |v: u32, faces: &[[u32; 3]], alive: &[bool], vertex_faces: &[Vec<u32>]| -> Vec<u32> {
            let mut out: Vec<u32> = vertex_faces[v as usize]
                .iter()
                .filter(|f| alive[**f as usize])
                .flat_map(|f| faces[*f as usize])
                .filter(|n| *n != v)
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };

        while live > target {
            let Some(c) = heap.pop() else { break };
            let (a, b) = (c.a, c.b);
            if gone[a as usize] || gone[b as usize] || c.versions != (versions[a as usize], versions[b as usize]) {
                continue;
            }
            let shared: Vec<u32> = vertex_faces[a as usize]
                .iter()
                .copied()
                .filter(|f| alive[*f as usize] && faces[*f as usize].contains(&b))
                .collect();
            if shared.len() != 2 {
                continue;
            }
            // The link condition: the ends share no neighbours but the two
            // across the edge, or the collapse would pinch the surface.
            let (na, nb) = (
                around(a, &faces, &alive, &vertex_faces),
                around(b, &faces, &alive, &vertex_faces),
            );
            let common = na.iter().filter(|n| nb.binary_search(n).is_ok()).count();
            if common != 2 {
                continue;
            }
            // No triangle may turn over or fold flat.
            let turns = [a, b].iter().any(|&end| {
                vertex_faces[end as usize].iter().any(|&f| {
                    if !alive[f as usize] || shared.contains(&f) {
                        return false;
                    }
                    let t = faces[f as usize];
                    let before = t.map(|i| self.vertices[i as usize]);
                    let after = t.map(|i| {
                        if i == a || i == b {
                            c.at
                        } else {
                            self.vertices[i as usize]
                        }
                    });
                    let n0 = cross(sub(before[1], before[0]), sub(before[2], before[0]));
                    let n1 = cross(sub(after[1], after[0]), sub(after[2], after[0]));
                    let (l0, l1) = (dot(n0, n0).sqrt(), dot(n1, n1).sqrt());
                    l1 <= 1e-12 * l0.max(1e-12) || dot(n0, n1) <= 0.2 * l0 * l1
                })
            });
            if turns {
                continue;
            }

            self.vertices[a as usize] = c.at;
            for f in shared {
                alive[f as usize] = false;
                live -= 1;
            }
            let moved: Vec<u32> = std::mem::take(&mut vertex_faces[b as usize]);
            for f in moved {
                if !alive[f as usize] {
                    continue;
                }
                for i in &mut faces[f as usize] {
                    if *i == b {
                        *i = a;
                    }
                }
                vertex_faces[a as usize].push(f);
            }
            vertex_faces[a as usize].retain(|f| alive[*f as usize]);
            gone[b as usize] = true;
            quadrics[a as usize] = quadrics[a as usize].plus(&quadrics[b as usize]);
            versions[a as usize] += 1;
            for n in around(a, &faces, &alive, &vertex_faces) {
                let (x, y) = (a.min(n), a.max(n));
                heap.push(cost(&self.vertices, &quadrics, &versions, x, y));
            }
        }

        // Keep only what is left, numbered afresh.
        let mut index = vec![u32::MAX; self.vertices.len()];
        let mut vertices = Vec::new();
        let mut triangles = Vec::with_capacity(live);
        for (f, t) in faces.iter().enumerate() {
            if !alive[f] {
                continue;
            }
            triangles.push(t.map(|i| {
                if index[i as usize] == u32::MAX {
                    index[i as usize] = vertices.len() as u32;
                    vertices.push(self.vertices[i as usize]);
                }
                index[i as usize]
            }));
        }
        self.vertices = vertices;
        self.triangles = triangles;
    }
}

/// How much of a surface to keep, which the user sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Full,
    Half,
    Quarter,
    Tenth,
}

impl Detail {
    pub const ALL: [Detail; 4] = [Self::Full, Self::Half, Self::Quarter, Self::Tenth];

    pub fn share(self) -> f64 {
        match self {
            Self::Full => 1.0,
            Self::Half => 0.5,
            Self::Quarter => 0.25,
            Self::Tenth => 0.1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "All triangles",
            Self::Half => "Half",
            Self::Quarter => "A quarter",
            Self::Tenth => "A tenth",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{passes, sphere_mask, surface_nets, Smoothing};
    use crate::Grid;

    fn ball() -> Mesh {
        let g = Grid {
            dims: [50, 50, 50],
            spacing: [1.0, 1.0, 1.0],
        };
        let mut mesh = surface_nets(&sphere_mask(g, 20.0));
        mesh.taubin(passes(Smoothing::Medium));
        mesh
    }

    #[test]
    fn a_ball_keeps_its_shape_with_a_tenth_of_its_triangles() {
        let full = ball();
        for detail in [Detail::Half, Detail::Quarter, Detail::Tenth] {
            let mut mesh = full.clone();
            mesh.decimate(detail.share());
            let kept = mesh.triangles.len() as f64 / full.triangles.len() as f64;
            assert!(kept <= detail.share() + 0.02, "{detail:?}: kept {kept:.3}");
            let s = mesh.soundness();
            assert!(s.closed_and_manifold(), "{detail:?}: {s:?}");
            let change = (mesh.volume() - full.volume()).abs() / full.volume();
            assert!(change < 0.02, "{detail:?}: volume changed {:.2}%", change * 100.0);
            assert!(mesh.volume() > 0.0, "still facing out");
        }
    }

    #[test]
    fn a_quadric_finds_where_three_planes_meet() {
        let mut q = Quadric::default();
        q.add(&Quadric::plane([1.0, 0.0, 0.0], -1.0));
        q.add(&Quadric::plane([0.0, 1.0, 0.0], -2.0));
        q.add(&Quadric::plane([0.0, 0.0, 1.0], -3.0));
        let p = q.optimum().unwrap();
        assert!(
            p.iter().zip([1.0, 2.0, 3.0]).all(|(a, b)| (a - b).abs() < 1e-9),
            "{p:?}"
        );
        assert!(q.error(p).abs() < 1e-9);
        let flat = Quadric::plane([0.0, 0.0, 1.0], 0.0);
        assert!(flat.optimum().is_none(), "one plane fixes no point");
    }

    #[test]
    fn full_detail_and_tiny_meshes_are_left_alone() {
        let mut mesh = ball();
        let before = mesh.clone();
        mesh.decimate(1.0);
        assert_eq!(mesh, before);
    }
}
