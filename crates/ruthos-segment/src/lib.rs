//! ruthos-segment: regions of a volume that the user has defined.
//!
//! Surfaces and volume renderings are built from segments rather than from a
//! window setting alone, because a segment defined by connectivity and edges
//! keeps bone too thin for any single threshold. A segment is a label on a
//! grid of voxels, with a name, a colour and a record of the tool that made
//! it. Every tool here is an assistant: nothing is segmented unless the user
//! asks, and every result can be edited.

#[cfg(feature = "ai")]
pub mod ai;
pub mod mesh;
pub mod refine;
pub mod store;
pub mod tools;
#[cfg(feature = "ai")]
pub mod volume_ai;

use serde::{Deserialize, Serialize};

use ruthos_core::volume::{Volume, VoxelData};

/// Dimensions in voxels and spacing in millimetres, which is often anisotropic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub dims: [usize; 3],
    pub spacing: [f64; 3],
}

impl Grid {
    pub fn len(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn index(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.dims[0] * (y + self.dims[1] * z)
    }

    #[inline]
    pub fn position(&self, i: usize) -> [usize; 3] {
        let (w, h) = (self.dims[0], self.dims[1]);
        [i % w, (i / w) % h, i / (w * h)]
    }

    /// One voxel's volume in cubic millimetres.
    pub fn voxel_mm3(&self) -> f64 {
        self.spacing[0] * self.spacing[1] * self.spacing[2]
    }
}

/// Values on a grid: a series, or a synthetic volume in the tests.
pub trait Field: Sync {
    fn grid(&self) -> Grid;
    fn at(&self, i: usize) -> f32;
}

impl Field for Volume {
    fn grid(&self) -> Grid {
        Grid {
            dims: self.dims,
            spacing: self.spacing,
        }
    }

    #[inline]
    fn at(&self, i: usize) -> f32 {
        match &self.data {
            VoxelData::I16(v) => v[i] as f32,
            VoxelData::F32(v) => v[i],
        }
    }
}

/// A plain field.
#[derive(Debug, Clone)]
pub struct Values {
    pub grid: Grid,
    pub data: Vec<f32>,
}

impl Field for Values {
    fn grid(&self) -> Grid {
        self.grid
    }

    #[inline]
    fn at(&self, i: usize) -> f32 {
        self.data[i]
    }
}

/// Which voxels are in: a tool's working result.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub grid: Grid,
    pub on: Vec<bool>,
}

impl Mask {
    pub fn empty(grid: Grid) -> Self {
        Self {
            grid,
            on: vec![false; grid.len()],
        }
    }

    pub fn count(&self) -> usize {
        self.on.iter().filter(|v| **v).count()
    }
}

/// How a segment was made, which travels with it. An AI-assisted segment
/// names its model and version, so that record survives outside Ruthos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    Manual,
    SemiAutomatic {
        tool: String,
    },
    Automatic {
        model: String,
        version: String,
        /// The model's edge was replaced by one read from the image.
        #[serde(default)]
        refined: bool,
        /// A tool has changed it since.
        #[serde(default)]
        edited: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Its label in the map, 1 to 255; 0 is background.
    pub label: u8,
    pub name: String,
    pub colour: [u8; 3],
    pub visible: bool,
    pub provenance: Provenance,
    /// Category and type codes, when one has been chosen or came with it.
    #[serde(default)]
    pub coding: Option<(ruthos_dicom::seg::Code, ruthos_dicom::seg::Code)>,
}

/// Up to 254 segments on one label map; 255 is kept for what a cut hides.
#[derive(Debug, Clone, PartialEq)]
pub struct Segmentation {
    pub grid: Grid,
    pub labels: Vec<u8>,
    pub segments: Vec<Segment>,
}

/// What a segment contains, as shown in the segment list and in ROI Info.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Statistics {
    pub voxels: usize,
    pub mm3: f64,
    pub mean: f64,
    pub deviation: f64,
    pub min: f32,
    pub max: f32,
    /// Size of the bounding box along each axis, in millimetres.
    pub extent_mm: [f64; 3],
}

impl Statistics {
    pub fn millilitres(&self) -> f64 {
        self.mm3 / 1000.0
    }
}

impl Segmentation {
    pub fn new(grid: Grid) -> Self {
        Self {
            grid,
            labels: vec![0; grid.len()],
            segments: Vec::new(),
        }
    }

    /// Add a segment holding `mask`, over whatever those voxels held. `None`
    /// when all the labels are in use.
    pub fn add(&mut self, name: &str, colour: [u8; 3], provenance: Provenance, mask: &Mask) -> Option<u8> {
        let label = (1..=254u8).find(|l| !self.segments.iter().any(|s| s.label == *l))?;
        self.segments.push(Segment {
            label,
            name: name.to_string(),
            colour,
            visible: true,
            provenance,
            coding: None,
        });
        self.paint(label, mask);
        Some(label)
    }

    /// Add `mask` to a segment.
    pub fn paint(&mut self, label: u8, mask: &Mask) {
        for (l, on) in self.labels.iter_mut().zip(&mask.on) {
            if *on {
                *l = label;
            }
        }
    }

    /// Take `mask` out of a segment; other segments are left alone.
    pub fn erase(&mut self, label: u8, mask: &Mask) {
        for (l, on) in self.labels.iter_mut().zip(&mask.on) {
            if *on && *l == label {
                *l = 0;
            }
        }
    }

    /// Note that a tool has changed a segment the model made.
    pub fn mark_edited(&mut self, label: u8) {
        for s in self.segments.iter_mut().filter(|s| s.label == label) {
            if let Provenance::Automatic { edited, .. } = &mut s.provenance {
                *edited = true;
            }
        }
    }

    pub fn remove(&mut self, label: u8) {
        self.labels.iter_mut().filter(|l| **l == label).for_each(|l| *l = 0);
        self.segments.retain(|s| s.label != label);
    }

    pub fn mask(&self, label: u8) -> Mask {
        Mask {
            grid: self.grid,
            on: self.labels.iter().map(|l| *l == label).collect(),
        }
    }

    pub fn statistics<F: Field>(&self, label: u8, field: &F) -> Option<Statistics> {
        let (mut n, mut sum, mut squares) = (0usize, 0f64, 0f64);
        let (mut min, mut max) = (f32::MAX, f32::MIN);
        let (mut low, mut high) = ([usize::MAX; 3], [0usize; 3]);
        for (i, l) in self.labels.iter().enumerate() {
            if *l != label {
                continue;
            }
            let v = field.at(i);
            n += 1;
            sum += v as f64;
            squares += v as f64 * v as f64;
            min = min.min(v);
            max = max.max(v);
            let p = self.grid.position(i);
            for a in 0..3 {
                low[a] = low[a].min(p[a]);
                high[a] = high[a].max(p[a]);
            }
        }
        if n == 0 {
            return None;
        }
        let mean = sum / n as f64;
        let extent = |a: usize| (high[a] - low[a] + 1) as f64 * self.grid.spacing[a];
        Some(Statistics {
            voxels: n,
            mm3: n as f64 * self.grid.voxel_mm3(),
            mean,
            deviation: (squares / n as f64 - mean * mean).max(0.0).sqrt(),
            min,
            max,
            extent_mm: [extent(0), extent(1), extent(2)],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            dims: [4, 3, 2],
            spacing: [0.5, 0.5, 2.0],
        }
    }

    #[test]
    fn indices_round_trip() {
        let g = grid();
        for i in 0..g.len() {
            let [x, y, z] = g.position(i);
            assert_eq!(g.index(x, y, z), i);
        }
    }

    #[test]
    fn statistics_use_true_volume_on_anisotropic_voxels() {
        let g = grid();
        let values = Values {
            grid: g,
            data: (0..g.len()).map(|i| i as f32).collect(),
        };
        let mut seg = Segmentation::new(g);
        let mut m = Mask::empty(g);
        m.on[0] = true;
        m.on[g.index(3, 2, 1)] = true;
        let label = seg.add("Bone", [255, 255, 255], Provenance::Manual, &m).unwrap();
        let s = seg.statistics(label, &values).unwrap();
        assert_eq!(s.voxels, 2);
        assert!((s.mm3 - 1.0).abs() < 1e-9, "two voxels of 0.5 mm3");
        assert_eq!((s.min, s.max), (0.0, 23.0));
        assert_eq!(s.extent_mm, [2.0, 1.5, 4.0]);
    }

    #[test]
    fn erasing_leaves_other_segments_alone() {
        let g = grid();
        let mut seg = Segmentation::new(g);
        let mut a = Mask::empty(g);
        a.on[0] = true;
        let mut b = Mask::empty(g);
        b.on[1] = true;
        let la = seg.add("A", [1, 1, 1], Provenance::Manual, &a).unwrap();
        let lb = seg.add("B", [2, 2, 2], Provenance::Manual, &b).unwrap();
        let mut both = a.clone();
        both.on[1] = true;
        seg.erase(la, &both);
        assert_eq!(seg.mask(la).count(), 0);
        assert_eq!(seg.mask(lb).count(), 1);
    }

    #[test]
    fn labels_run_out_at_254() {
        let g = grid();
        let mut seg = Segmentation::new(g);
        let m = Mask::empty(g);
        for _ in 0..254 {
            assert!(seg.add("S", [0, 0, 0], Provenance::Manual, &m).is_some());
        }
        assert!(seg.add("S", [0, 0, 0], Provenance::Manual, &m).is_none());
    }
}
