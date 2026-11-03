//! Volumes and multiplanar reconstruction (MPR).
//!
//! A volume is built from a series of parallel slices (CT, MR, CBCT): the
//! slices are sorted along their normal by Image Position (Patient), and
//! their spacing comes from the positions, not from Slice Thickness. Voxels
//! keep the modality values (Hounsfield units on CT), as 16-bit integers when
//! every value fits, else as 32-bit floats.
//!
//! A plane through the volume, at any position and angle, is resampled with
//! trilinear interpolation into an ordinary [`Frame`], with square pixels of
//! a known size, so the 2D renderer, the window and colour tables and the
//! pixel readout all work on it unchanged. A thick slab combines several
//! parallel planes by maximum, minimum or mean intensity.
//!
//! Coordinates are the patient's (DICOM LPS, millimetres): x towards the
//! patient's left, y towards posterior, z towards the head.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;

use ruthos_dicom::pixels::FramePixels;
use ruthos_dicom::{Frame, InstanceMeta};

use crate::{CoreError, Result};

pub type V3 = [f64; 3];

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub fn normalize(a: V3) -> V3 {
    let n = norm(a);
    if n < 1e-12 {
        a
    } else {
        scale(a, 1.0 / n)
    }
}

/// Rotate `v` about the unit axis `axis` by `angle` radians (Rodrigues).
pub fn rotate(v: V3, axis: V3, angle: f64) -> V3 {
    let (s, c) = angle.sin_cos();
    add(
        add(scale(v, c), scale(cross(axis, v), s)),
        scale(axis, dot(axis, v) * (1.0 - c)),
    )
}

#[derive(Debug, Clone)]
pub enum VoxelData {
    I16(Vec<i16>),
    F32(Vec<f32>),
}

/// A stack of parallel slices on a regular grid.
#[derive(Debug, Clone)]
pub struct Volume {
    /// Columns, rows and slices.
    pub dims: [usize; 3],
    /// Millimetres between columns, rows and slices.
    pub spacing: V3,
    /// Patient position of the centre of voxel (0, 0, 0).
    pub origin: V3,
    /// Unit directions of increasing column, row and slice index.
    pub axes: [V3; 3],
    pub data: VoxelData,
    pub min: f32,
    pub max: f32,
    /// The first slice's header: modality, window, patient details.
    pub meta: InstanceMeta,
    /// Slices whose spacing differs from the rest by more than a fifth
    /// (a gap in the series); measurements across them are approximate.
    pub irregular_spacing: bool,
}

/// Why a series cannot be reconstructed, in words for the user.
pub fn unsuitable(frames: &[Frame]) -> Option<String> {
    if frames.len() < 3 {
        return Some("MPR needs a series of at least three slices.".into());
    }
    let first = &frames[0];
    if first.is_color() {
        return Some("MPR works on grayscale series (CT, MR, CBCT), not colour images.".into());
    }
    if frames
        .iter()
        .any(|f| f.width != first.width || f.height != first.height)
    {
        return Some("The slices of this series are not all the same size.".into());
    }
    if let Some(o) = first.meta.image_orientation {
        let parallel = frames.iter().all(|f| {
            f.meta
                .image_orientation
                .is_none_or(|p| (0..6).all(|i| (p[i] - o[i]).abs() < 1e-3))
        });
        if !parallel {
            return Some("The slices of this series are not parallel (it holds more than one orientation).".into());
        }
    }
    None
}

impl Volume {
    /// Build a volume from the decoded slices of a series, in any order.
    pub fn from_frames(mut frames: Vec<Frame>) -> Result<Volume> {
        if let Some(why) = unsuitable(&frames) {
            return Err(CoreError::Other(why));
        }
        let o = frames[0]
            .meta
            .image_orientation
            .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let ax_x = normalize([o[0], o[1], o[2]]);
        let ax_y = normalize([o[3], o[4], o[5]]);
        let ax_z = normalize(cross(ax_x, ax_y));

        // Sort along the normal by position, else by instance number.
        let have_positions = frames.iter().all(|f| f.meta.image_position.is_some());
        if have_positions {
            frames.sort_by(|a, b| {
                let pa = dot(a.meta.image_position.unwrap_or_default(), ax_z);
                let pb = dot(b.meta.image_position.unwrap_or_default(), ax_z);
                pa.total_cmp(&pb)
            });
        } else {
            frames.sort_by_key(|f| f.meta.instance_number.unwrap_or(0));
        }
        // Two slices at the same place (a duplicate) are one slice.
        if have_positions {
            frames.dedup_by(|b, a| {
                let pa = dot(a.meta.image_position.unwrap_or_default(), ax_z);
                let pb = dot(b.meta.image_position.unwrap_or_default(), ax_z);
                (pa - pb).abs() < 1e-4
            });
            if frames.len() < 3 {
                return Err(CoreError::Other(
                    "MPR needs at least three slices at different positions.".into(),
                ));
            }
        }

        let first = &frames[0];
        let ps = first
            .meta
            .pixel_spacing
            .or(first.meta.pixel_spacing_tag)
            .unwrap_or([1.0, 1.0]);
        let (row_mm, col_mm) = (ps[0].max(1e-6), ps[1].max(1e-6));
        let (dz, irregular) = if have_positions {
            let pos: Vec<f64> = frames
                .iter()
                .map(|f| dot(f.meta.image_position.unwrap_or_default(), ax_z))
                .collect();
            let mut gaps: Vec<f64> = pos.windows(2).map(|w| w[1] - w[0]).collect();
            gaps.sort_by(f64::total_cmp);
            let median = gaps[gaps.len() / 2].max(1e-6);
            let irregular = gaps.iter().any(|g| (g - median).abs() > median * 0.2);
            (median, irregular)
        } else {
            (first.meta.slice_thickness.unwrap_or(1.0).max(1e-6), false)
        };
        let origin = first.meta.image_position.unwrap_or([0.0, 0.0, 0.0]);

        let (w, h, n) = (first.width as usize, first.height as usize, frames.len());
        let integral = frames.iter().all(|f| match &f.pixels {
            FramePixels::Gray(v) => v
                .iter()
                .all(|x| x.fract() == 0.0 && *x >= i16::MIN as f32 && *x <= i16::MAX as f32),
            FramePixels::Rgba(_) => false,
        });
        let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
        for f in &frames {
            min = min.min(f.min);
            max = max.max(f.max);
        }
        fn slice(f: &Frame) -> &[f32] {
            match &f.pixels {
                FramePixels::Gray(v) => v,
                FramePixels::Rgba(_) => &[],
            }
        }
        let data = if integral {
            let mut v = Vec::with_capacity(w * h * n);
            for f in &frames {
                v.extend(slice(f).iter().map(|x| *x as i16));
            }
            VoxelData::I16(v)
        } else {
            let mut v = Vec::with_capacity(w * h * n);
            for f in &frames {
                v.extend_from_slice(slice(f));
            }
            VoxelData::F32(v)
        };
        Ok(Volume {
            dims: [w, h, n],
            spacing: [col_mm, row_mm, dz],
            origin,
            axes: [ax_x, ax_y, ax_z],
            data,
            min,
            max,
            meta: first.meta.clone(),
            irregular_spacing: irregular,
        })
    }

    /// Decode the files of a series and build its volume. `progress` gets
    /// slices done and the total; `cancel` stops between slices.
    pub fn load(files: &[PathBuf], progress: &(dyn Fn(usize, usize) + Sync), cancel: &AtomicBool) -> Result<Volume> {
        let done = AtomicUsize::new(0);
        let frames: Vec<Result<Frame>> = files
            .par_iter()
            .map(|f| {
                if cancel.load(Ordering::Relaxed) {
                    return Err(CoreError::Other("cancelled".into()));
                }
                let r = ruthos_dicom::decode_frame(f, 0).map_err(CoreError::from);
                progress(done.fetch_add(1, Ordering::Relaxed) + 1, files.len());
                r
            })
            .collect();
        let frames: Vec<Frame> = frames.into_iter().collect::<Result<_>>()?;
        Volume::from_frames(frames)
    }

    fn voxel(&self, i: usize, j: usize, k: usize) -> f32 {
        let idx = (k * self.dims[1] + j) * self.dims[0] + i;
        match &self.data {
            VoxelData::I16(v) => v[idx] as f32,
            VoxelData::F32(v) => v[idx],
        }
    }

    /// Continuous voxel coordinates of a patient point.
    pub fn to_index(&self, p: V3) -> V3 {
        let d = sub(p, self.origin);
        [
            dot(d, self.axes[0]) / self.spacing[0],
            dot(d, self.axes[1]) / self.spacing[1],
            dot(d, self.axes[2]) / self.spacing[2],
        ]
    }

    /// Patient point of continuous voxel coordinates.
    pub fn to_patient(&self, idx: V3) -> V3 {
        let mut p = self.origin;
        for ((axis, i), sp) in self.axes.iter().zip(idx).zip(self.spacing) {
            p = add(p, scale(*axis, i * sp));
        }
        p
    }

    /// Trilinear value at a patient point; `None` outside the volume.
    pub fn sample(&self, p: V3) -> Option<f32> {
        let [x, y, z] = self.to_index(p);
        let lim = |v: f64, n: usize| v >= -0.5 && v <= n as f64 - 0.5;
        if !(lim(x, self.dims[0]) && lim(y, self.dims[1]) && lim(z, self.dims[2])) {
            return None;
        }
        let clamp = |v: f64, n: usize| v.clamp(0.0, (n - 1) as f64);
        let (x, y, z) = (clamp(x, self.dims[0]), clamp(y, self.dims[1]), clamp(z, self.dims[2]));
        let (x0, y0, z0) = (x.floor() as usize, y.floor() as usize, z.floor() as usize);
        let (x1, y1, z1) = (
            (x0 + 1).min(self.dims[0] - 1),
            (y0 + 1).min(self.dims[1] - 1),
            (z0 + 1).min(self.dims[2] - 1),
        );
        let (fx, fy, fz) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32, (z - z0 as f64) as f32);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(self.voxel(x0, y0, z0), self.voxel(x1, y0, z0), fx);
        let c10 = lerp(self.voxel(x0, y1, z0), self.voxel(x1, y1, z0), fx);
        let c01 = lerp(self.voxel(x0, y0, z1), self.voxel(x1, y0, z1), fx);
        let c11 = lerp(self.voxel(x0, y1, z1), self.voxel(x1, y1, z1), fx);
        Some(lerp(lerp(c00, c10, fy), lerp(c01, c11, fy), fz))
    }

    /// The centre of the volume, in patient coordinates.
    pub fn centre(&self) -> V3 {
        self.to_patient([
            (self.dims[0] - 1) as f64 / 2.0,
            (self.dims[1] - 1) as f64 / 2.0,
            (self.dims[2] - 1) as f64 / 2.0,
        ])
    }

    /// The length of the volume's diagonal in millimetres.
    pub fn diagonal(&self) -> f64 {
        let e = [
            self.dims[0] as f64 * self.spacing[0],
            self.dims[1] as f64 * self.spacing[1],
            self.dims[2] as f64 * self.spacing[2],
        ];
        norm(e)
    }

    /// The finest voxel spacing.
    pub fn finest(&self) -> f64 {
        self.spacing.iter().copied().fold(f64::INFINITY, f64::min)
    }

    /// Bytes of voxel data.
    pub fn bytes(&self) -> usize {
        match &self.data {
            VoxelData::I16(v) => v.len() * 2,
            VoxelData::F32(v) => v.len() * 4,
        }
    }
}

/// How a thick slab combines its planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlabMode {
    /// Maximum intensity projection.
    #[default]
    Mip,
    /// Minimum intensity projection.
    MinIp,
    Mean,
}

/// A plane to resample: its centre, its in-plane directions (unit vectors,
/// right and down on screen) and the image it becomes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub centre: V3,
    pub right: V3,
    pub down: V3,
    /// Output size in pixels and the size of a pixel in mm.
    pub width: u32,
    pub height: u32,
    pub pixel_mm: f64,
    /// Slab thickness in mm; 0 is a single plane.
    pub slab_mm: f64,
    pub slab_mode: SlabMode,
}

impl Plane {
    pub fn normal(&self) -> V3 {
        normalize(cross(self.right, self.down))
    }

    /// The patient point at the centre of output pixel `(x, y)`.
    pub fn point(&self, x: f64, y: f64) -> V3 {
        let dx = (x + 0.5 - self.width as f64 / 2.0) * self.pixel_mm;
        let dy = (y + 0.5 - self.height as f64 / 2.0) * self.pixel_mm;
        add(self.centre, add(scale(self.right, dx), scale(self.down, dy)))
    }

    /// Output pixel coordinates (continuous) of a patient point, projected
    /// onto the plane.
    pub fn pixel_of(&self, p: V3) -> (f64, f64) {
        let d = sub(p, self.centre);
        (
            dot(d, self.right) / self.pixel_mm + self.width as f64 / 2.0,
            dot(d, self.down) / self.pixel_mm + self.height as f64 / 2.0,
        )
    }
}

/// Resample a plane into a frame. Pixels outside the volume take the
/// volume's minimum, so they show as background.
pub fn reslice(volume: &Volume, plane: &Plane) -> Frame {
    let (w, h) = (plane.width as usize, plane.height as usize);
    let normal = plane.normal();
    let steps: Vec<f64> = if plane.slab_mm > 0.0 {
        let step = volume.finest().max(0.1);
        let n = (plane.slab_mm / step).ceil().max(1.0) as usize;
        (0..=n)
            .map(|i| -plane.slab_mm / 2.0 + plane.slab_mm * i as f64 / n as f64)
            .collect()
    } else {
        vec![0.0]
    };
    let background = volume.min;
    let mut values = vec![background; w * h];
    values.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let p = plane.point(x as f64, y as f64);
            if steps.len() == 1 {
                if let Some(v) = volume.sample(p) {
                    *out = v;
                }
                continue;
            }
            let (mut acc, mut n) = (
                match plane.slab_mode {
                    SlabMode::Mip => f32::NEG_INFINITY,
                    SlabMode::MinIp => f32::INFINITY,
                    SlabMode::Mean => 0.0,
                },
                0usize,
            );
            for t in &steps {
                if let Some(v) = volume.sample(add(p, scale(normal, *t))) {
                    acc = match plane.slab_mode {
                        SlabMode::Mip => acc.max(v),
                        SlabMode::MinIp => acc.min(v),
                        SlabMode::Mean => acc + v,
                    };
                    n += 1;
                }
            }
            if n > 0 {
                *out = if plane.slab_mode == SlabMode::Mean {
                    acc / n as f32
                } else {
                    acc
                };
            }
        }
    });
    let mut meta = volume.meta.clone();
    meta.rows = plane.height;
    meta.columns = plane.width;
    meta.number_of_frames = 1;
    meta.pixel_spacing = Some([plane.pixel_mm, plane.pixel_mm]);
    meta.pixel_spacing_tag = Some([plane.pixel_mm, plane.pixel_mm]);
    meta.imager_pixel_spacing = None;
    meta.us_regions = Vec::new();
    meta.image_position = Some(plane.point(-0.5, -0.5));
    meta.image_orientation = Some([
        plane.right[0],
        plane.right[1],
        plane.right[2],
        plane.down[0],
        plane.down[1],
        plane.down[2],
    ]);
    Frame {
        width: plane.width,
        height: plane.height,
        frame_index: 0,
        pixels: FramePixels::Gray(values),
        min: volume.min,
        max: volume.max,
        inverted: false,
        meta,
    }
}

/// The letter for a patient direction on the edge of an image: R/L, A/P,
/// F/H, the strongest first, with a second when the direction is oblique.
pub fn orientation_label(d: V3) -> String {
    let letters = [('R', 'L'), ('A', 'P'), ('F', 'H')];
    let mut parts: Vec<(f64, char)> = (0..3)
        .map(|i| (d[i].abs(), if d[i] < 0.0 { letters[i].0 } else { letters[i].1 }))
        .collect();
    parts.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut s = String::new();
    s.push(parts[0].1);
    if parts[1].0 > 0.25 {
        s.push(parts[1].1);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic CT: value = x + 10 y + 100 z in voxel indices, 0.5 mm
    /// pixels, 2 mm slices, given out of order.
    fn volume() -> Volume {
        let (w, h) = (8u32, 6u32);
        let mut frames = Vec::new();
        for z in [3usize, 0, 4, 1, 2] {
            let values: Vec<f32> = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x + 10 * y) as f32 + 100.0 * z as f32))
                .collect();
            let meta = InstanceMeta {
                pixel_spacing: Some([0.5, 0.5]),
                image_position: Some([10.0, 20.0, -30.0 + 2.0 * z as f64]),
                image_orientation: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
                modality: "CT".into(),
                ..Default::default()
            };
            let (min, max) = values
                .iter()
                .fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            frames.push(Frame {
                width: w,
                height: h,
                frame_index: 0,
                pixels: FramePixels::Gray(values),
                min,
                max,
                inverted: false,
                meta,
            });
        }
        Volume::from_frames(frames).unwrap()
    }

    #[test]
    fn slices_are_sorted_by_position_and_spaced_from_it() {
        let v = volume();
        assert_eq!(v.dims, [8, 6, 5]);
        assert_eq!(v.spacing, [0.5, 0.5, 2.0]);
        assert!(matches!(v.data, VoxelData::I16(_)));
        assert_eq!(v.voxel(3, 2, 4), 3.0 + 20.0 + 400.0);
        assert!(!v.irregular_spacing);
    }

    #[test]
    fn trilinear_sampling_is_exact_on_a_linear_field() {
        let v = volume();
        // Half way between slices 1 and 2, columns 2 and 3, row 4.
        let p = v.to_patient([2.5, 4.0, 1.5]);
        assert!((v.sample(p).unwrap() - (2.5 + 40.0 + 150.0)).abs() < 1e-4);
        assert!(v.sample(v.to_patient([-3.0, 0.0, 0.0])).is_none());
    }

    #[test]
    fn an_axial_plane_reproduces_a_slice_and_a_coronal_one_cuts_across() {
        let v = volume();
        let c = v.to_patient([3.5, 2.5, 2.0]);
        let axial = Plane {
            centre: c,
            right: [1.0, 0.0, 0.0],
            down: [0.0, 1.0, 0.0],
            width: 8,
            height: 6,
            pixel_mm: 0.5,
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
        };
        let f = reslice(&v, &axial);
        assert_eq!(f.value_at(3, 2), Some(3.0 + 20.0 + 200.0));
        assert_eq!(f.meta.pixel_spacing, Some([0.5, 0.5]));
        // Coronal: right is x, down is towards the feet, 0.5 mm pixels;
        // the volume spans 4 mm across and 8 mm (four slice gaps) high.
        let coronal = Plane {
            centre: v.to_patient([3.5, 3.0, 2.0]),
            right: [1.0, 0.0, 0.0],
            down: [0.0, 0.0, -1.0],
            width: 8,
            height: 16,
            pixel_mm: 0.5,
            ..axial
        };
        let f = reslice(&v, &coronal);
        // A row down is a quarter slice lower (25); a column across is one voxel.
        let a = f.value_at(2, 6).unwrap();
        let b = f.value_at(2, 7).unwrap();
        assert!((a - b - 25.0).abs() < 1e-3, "{a} {b}");
        assert!((f.value_at(3, 7).unwrap() - b - 1.0).abs() < 1e-3);
    }

    #[test]
    fn an_oblique_plane_and_a_slab() {
        let v = volume();
        let c = v.centre();
        // Tilt the axial plane 30 degrees about x: values change with y and z.
        let down = rotate([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], 30f64.to_radians());
        let plane = Plane {
            centre: c,
            right: [1.0, 0.0, 0.0],
            down,
            width: 8,
            height: 8,
            pixel_mm: 0.5,
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
        };
        let f = reslice(&v, &plane);
        let p = plane.point(4.0, 4.0);
        let idx = v.to_index(p);
        let expect = idx[0] + 10.0 * idx[1] + 100.0 * idx[2];
        assert!((f.value_at(4, 4).unwrap() as f64 - expect).abs() < 1e-2);
        // A 4 mm MIP slab through the middle slice reaches one slice either side.
        let slab = Plane {
            right: [1.0, 0.0, 0.0],
            down: [0.0, 1.0, 0.0],
            slab_mm: 4.0,
            ..plane
        };
        let mip = reslice(&v, &slab);
        let single = reslice(&v, &Plane { slab_mm: 0.0, ..slab });
        let (x, y) = (2, 2);
        assert!((mip.value_at(x, y).unwrap() - single.value_at(x, y).unwrap() - 100.0).abs() < 1e-2);
        let min = reslice(
            &v,
            &Plane {
                slab_mode: SlabMode::MinIp,
                ..slab
            },
        );
        assert!((single.value_at(x, y).unwrap() - min.value_at(x, y).unwrap() - 100.0).abs() < 1e-2);
    }

    #[test]
    fn planes_map_points_both_ways() {
        let v = volume();
        let plane = Plane {
            centre: v.centre(),
            right: normalize([1.0, 1.0, 0.0]),
            down: [0.0, 0.0, -1.0],
            width: 40,
            height: 30,
            pixel_mm: 0.7,
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
        };
        let (x, y) = plane.pixel_of(plane.point(12.0, 7.0));
        assert!((x - 12.5).abs() < 1e-9 && (y - 7.5).abs() < 1e-9);
    }

    #[test]
    fn unsuitable_series_are_explained() {
        assert!(unsuitable(&[]).is_some());
        let mut v = volume();
        v.irregular_spacing = false;
        assert_eq!(orientation_label([-1.0, 0.0, 0.0]), "R");
        assert_eq!(orientation_label([0.0, 0.0, 1.0]), "H");
        assert_eq!(orientation_label(normalize([0.0, -1.0, 0.7])), "AH");
    }
}
