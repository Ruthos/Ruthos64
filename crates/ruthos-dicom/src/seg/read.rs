//! Reading a Segmentation object, Ruthos's or anyone else's, and placing it on a
//! volume. A file like this came from somewhere else, so every count and offset
//! in it is checked, and it goes on the volume only if it lines up: nothing is
//! ever resampled, because a segment moved without saying so is worse than none.

use std::path::Path;

use dicom_core::value::Value;
use dicom_core::Tag;
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;

use super::{from_cielab, Algorithm, Code, Segment, SOP_CLASS};
use crate::open::open_any;
use crate::{DicomError, Result};

const SEGMENT_IDENTIFICATION_SEQUENCE: Tag = Tag(0x0062, 0x000A);
const REFERENCED_SEGMENT_NUMBER: Tag = Tag(0x0062, 0x000B);
const RECOMMENDED_DISPLAY_CIELAB: Tag = Tag(0x0062, 0x000D);
const SEGMENT_ALGORITHM_NAME: Tag = Tag(0x0062, 0x0009);

/// How far apart orientations may be, as a difference of direction cosines.
const ORIENTATION_TOLERANCE: f64 = 1e-3;
/// How far out of line a frame may be, as a share of a voxel.
const ALIGNMENT_TOLERANCE: f64 = 0.1;
/// How much pixel spacing may differ, as a share.
const SPACING_TOLERANCE: f64 = 0.01;

/// Grey used for a segment that recommends no colour.
const NO_COLOUR: [u8; 3] = [200, 200, 200];

fn refused(why: impl Into<String>) -> DicomError {
    DicomError::Unsupported(why.into())
}

/// One frame of a SEG.
#[derive(Debug, Clone, PartialEq)]
pub struct SegFrame {
    pub segment: u16,
    pub position: [f64; 3],
    pub source_instance: Option<String>,
}

/// A Segmentation object, read and checked but not yet placed on anything.
#[derive(Debug, Clone)]
pub struct SegFile {
    pub sop_instance_uid: String,
    pub series_uid: String,
    pub study_uid: String,
    /// The series it says it was drawn on.
    pub referenced_series: Vec<String>,
    pub rows: usize,
    pub columns: usize,
    pub orientation: [f64; 6],
    pub pixel_spacing: [f64; 2],
    /// Between slices, or else their thickness, when the SEG gives either.
    pub slice_spacing: Option<f64>,
    pub segments: Vec<Segment>,
    pub frames: Vec<SegFrame>,
    pub creator: String,
    /// `YYYYMMDD` and `HHMMSS`, which is how the newest is found.
    pub content_date: String,
    pub content_time: String,
    pixels: Vec<u8>,
}

/// A series' grid in patient space, in the terms of a volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub dims: [usize; 3],
    /// Between columns, between rows and between slices.
    pub spacing: [f64; 3],
    pub origin: [f64; 3],
    pub axes: [[f64; 3]; 3],
}

/// A segment on the volume: its description and its voxels, sorted, as indices
/// into the volume.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub segment: Segment,
    pub voxels: Vec<usize>,
}

fn text(obj: &InMemDicomObject, tag: Tag) -> Option<String> {
    let s = obj.get(tag)?.to_str().ok()?;
    let s = s.trim_end_matches(['\0', ' ']).to_string();
    (!s.is_empty()).then_some(s)
}

fn items(obj: &InMemDicomObject, tag: Tag) -> &[InMemDicomObject] {
    match obj.get(tag).map(|e| e.value()) {
        Some(Value::Sequence(s)) => s.items(),
        _ => &[],
    }
}

fn first(obj: &InMemDicomObject, tag: Tag) -> Option<&InMemDicomObject> {
    items(obj, tag).first()
}

fn number(obj: &InMemDicomObject, tag: Tag) -> Option<i64> {
    obj.get(tag)?.to_int::<i64>().ok()
}

fn floats<const N: usize>(obj: &InMemDicomObject, tag: Tag) -> Option<[f64; N]> {
    let v = obj.get(tag)?.to_multi_float64().ok()?;
    (v.len() >= N).then(|| std::array::from_fn(|i| v[i]))
}

fn code(obj: &InMemDicomObject, tag: Tag) -> Code {
    let item = first(obj, tag);
    let get = |tag: Tag| item.and_then(|i| text(i, tag)).unwrap_or_default();
    Code {
        value: get(tags::CODE_VALUE),
        scheme: get(tags::CODING_SCHEME_DESIGNATOR),
        meaning: get(tags::CODE_MEANING),
    }
}

fn segment(item: &InMemDicomObject) -> Result<Segment> {
    let number = number(item, tags::SEGMENT_NUMBER)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| refused("A segment in this SEG has no number"))?;
    let label = text(item, tags::SEGMENT_LABEL).unwrap_or_else(|| format!("Segment {number}"));
    let name = text(item, SEGMENT_ALGORITHM_NAME).unwrap_or_default();
    let algorithm = match text(item, tags::SEGMENT_ALGORITHM_TYPE).as_deref() {
        Some("AUTOMATIC") => Algorithm::Automatic(name),
        Some("SEMIAUTOMATIC") => Algorithm::SemiAutomatic(name),
        _ => Algorithm::Manual,
    };
    let colour = item
        .get(RECOMMENDED_DISPLAY_CIELAB)
        .and_then(|e| e.to_multi_int::<i64>().ok())
        .filter(|v| v.len() >= 3)
        .map(|v| from_cielab(std::array::from_fn(|k| v[k].clamp(0, 65535) as u16)))
        .unwrap_or(NO_COLOUR);
    Ok(Segment {
        number,
        label,
        colour,
        algorithm,
        category: code(item, tags::SEGMENTED_PROPERTY_CATEGORY_CODE_SEQUENCE),
        kind: code(item, tags::SEGMENTED_PROPERTY_TYPE_CODE_SEQUENCE),
    })
}

/// A value from a frame's own groups, or else the groups all frames share.
fn group<'a>(
    frame: &'a InMemDicomObject,
    shared: Option<&'a InMemDicomObject>,
    tag: Tag,
) -> Option<&'a InMemDicomObject> {
    first(frame, tag).or_else(|| shared.and_then(|s| first(s, tag)))
}

fn frames_of(obj: &InMemDicomObject, count: usize, shared: Option<&InMemDicomObject>) -> Result<Vec<SegFrame>> {
    let per_frame = items(obj, tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE);
    if per_frame.len() != count {
        return Err(refused(format!(
            "This SEG says it has {count} frames but describes {}",
            per_frame.len()
        )));
    }
    let mut frames = Vec::with_capacity(count);
    for frame in per_frame {
        let segment = group(frame, shared, SEGMENT_IDENTIFICATION_SEQUENCE)
            .and_then(|i| number(i, REFERENCED_SEGMENT_NUMBER))
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| refused("A frame in this SEG does not say which segment it belongs to"))?;
        let position = group(frame, shared, tags::PLANE_POSITION_SEQUENCE)
            .and_then(|i| floats::<3>(i, tags::IMAGE_POSITION_PATIENT))
            .ok_or_else(|| refused("A frame in this SEG has no position, so it cannot be placed on the images"))?;
        let source_instance = group(frame, shared, tags::DERIVATION_IMAGE_SEQUENCE)
            .and_then(|d| first(d, tags::SOURCE_IMAGE_SEQUENCE))
            .and_then(|s| text(s, tags::REFERENCED_SOP_INSTANCE_UID));
        frames.push(SegFrame {
            segment,
            position,
            source_instance,
        });
    }
    Ok(frames)
}

/// Read and check a SEG. A kind Ruthos does not read is refused in words a user
/// can act on.
pub fn read_seg(path: &Path) -> Result<SegFile> {
    crate::structure::check_file(path)?;
    let file = open_any(path, None)?;
    if file.meta().transfer_syntax().trim_end_matches('\0') == "1.2.840.10008.1.2.5" {
        return Err(refused(
            "This SEG is compressed with RLE, which Ruthos does not read yet",
        ));
    }
    let obj: InMemDicomObject = file.into_inner();
    if text(&obj, tags::SOP_CLASS_UID).as_deref() != Some(SOP_CLASS) {
        return Err(refused("This file is not a DICOM Segmentation"));
    }
    match text(&obj, tags::SEGMENTATION_TYPE).as_deref() {
        Some("BINARY") => {}
        Some("FRACTIONAL") => {
            return Err(refused(
                "This SEG holds probabilities rather than yes or no for each voxel, which Ruthos does not read yet",
            ))
        }
        _ => return Err(refused("This SEG does not say what type it is")),
    }
    let rows = number(&obj, tags::ROWS).filter(|n| (1..=65535).contains(n));
    let columns = number(&obj, tags::COLUMNS).filter(|n| (1..=65535).contains(n));
    let count = number(&obj, tags::NUMBER_OF_FRAMES).filter(|n| *n >= 1);
    let (Some(rows), Some(columns), Some(count)) = (rows, columns, count) else {
        return Err(refused("This SEG does not give its size"));
    };
    let (rows, columns) = (rows as usize, columns as usize);
    let count = usize::try_from(count).map_err(|_| refused("This SEG has too many frames"))?;
    if number(&obj, tags::BITS_ALLOCATED) != Some(1) {
        return Err(refused("This SEG is not stored at one bit a voxel"));
    }
    let bits = rows
        .checked_mul(columns)
        .and_then(|n| n.checked_mul(count))
        .ok_or_else(|| refused("This SEG's size is impossible"))?;
    let pixels: Vec<u8> = match obj.get(tags::PIXEL_DATA).map(|e| e.value()) {
        Some(Value::Primitive(v)) => v.to_bytes().into_owned(),
        Some(_) => {
            return Err(refused(
                "This SEG's voxels are compressed, which Ruthos does not read yet",
            ))
        }
        None => return Err(refused("This SEG holds no voxels")),
    };
    if pixels.len() < bits.div_ceil(8) {
        return Err(refused("This SEG is cut short: it has fewer voxels than it says"));
    }

    let shared = first(&obj, tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE);
    let frames = frames_of(&obj, count, shared)?;
    let any = items(&obj, tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE).first();
    let either = |tag: Tag| any.and_then(|f| group(f, shared, tag));
    let orientation = either(tags::PLANE_ORIENTATION_SEQUENCE)
        .and_then(|i| floats::<6>(i, tags::IMAGE_ORIENTATION_PATIENT))
        .ok_or_else(|| refused("This SEG does not give its orientation"))?;
    let measures = either(tags::PIXEL_MEASURES_SEQUENCE);
    let pixel_spacing = measures
        .and_then(|i| floats::<2>(i, tags::PIXEL_SPACING))
        .ok_or_else(|| refused("This SEG does not give its pixel spacing"))?;
    let slice_spacing = measures.and_then(|i| {
        floats::<1>(i, tags::SPACING_BETWEEN_SLICES)
            .or_else(|| floats::<1>(i, tags::SLICE_THICKNESS))
            .map(|v| v[0].abs())
    });

    let segments = items(&obj, tags::SEGMENT_SEQUENCE)
        .iter()
        .map(segment)
        .collect::<Result<Vec<_>>>()?;
    if segments.is_empty() {
        return Err(refused("This SEG describes no segments"));
    }
    if let Some(f) = frames.iter().find(|f| !segments.iter().any(|s| s.number == f.segment)) {
        return Err(refused(format!(
            "A frame in this SEG belongs to segment {}, which it does not describe",
            f.segment
        )));
    }
    let referenced_series = items(&obj, tags::REFERENCED_SERIES_SEQUENCE)
        .iter()
        .filter_map(|i| text(i, tags::SERIES_INSTANCE_UID))
        .collect();
    Ok(SegFile {
        sop_instance_uid: text(&obj, tags::SOP_INSTANCE_UID).unwrap_or_default(),
        series_uid: text(&obj, tags::SERIES_INSTANCE_UID).unwrap_or_default(),
        study_uid: text(&obj, tags::STUDY_INSTANCE_UID).unwrap_or_default(),
        referenced_series,
        rows,
        columns,
        orientation,
        pixel_spacing,
        slice_spacing,
        segments,
        frames,
        creator: text(&obj, tags::CONTENT_CREATOR_NAME).unwrap_or_default(),
        content_date: text(&obj, tags::CONTENT_DATE).unwrap_or_default(),
        content_time: text(&obj, tags::CONTENT_TIME).unwrap_or_default(),
        pixels,
    })
}

/// What can be told of a SEG without reading its voxels.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Peek {
    pub series_uid: String,
    pub series_number: i32,
    pub instance_number: i32,
    pub referenced_series: Vec<String>,
    pub labels: Vec<String>,
    pub content_date: String,
    pub content_time: String,
}

/// Read what a SEG refers to and holds, stopping before its voxels.
pub fn peek(path: &Path) -> Result<Peek> {
    let obj: InMemDicomObject = open_any(path, Some(tags::PIXEL_DATA))?.into_inner();
    if text(&obj, tags::SOP_CLASS_UID).as_deref() != Some(SOP_CLASS) {
        return Err(refused("This file is not a DICOM Segmentation"));
    }
    Ok(Peek {
        series_uid: text(&obj, tags::SERIES_INSTANCE_UID).unwrap_or_default(),
        series_number: number(&obj, tags::SERIES_NUMBER).unwrap_or(0) as i32,
        instance_number: number(&obj, tags::INSTANCE_NUMBER).unwrap_or(0) as i32,
        referenced_series: items(&obj, tags::REFERENCED_SERIES_SEQUENCE)
            .iter()
            .filter_map(|i| text(i, tags::SERIES_INSTANCE_UID))
            .collect(),
        labels: items(&obj, tags::SEGMENT_SEQUENCE)
            .iter()
            .filter_map(|i| text(i, tags::SEGMENT_LABEL))
            .collect(),
        content_date: text(&obj, tags::CONTENT_DATE).unwrap_or_default(),
        content_time: text(&obj, tags::CONTENT_TIME).unwrap_or_default(),
    })
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl SegFile {
    fn bit(&self, frame: usize, pixel: usize) -> bool {
        let at = frame * self.rows * self.columns + pixel;
        self.pixels[at / 8] >> (at % 8) & 1 == 1
    }

    /// Whether this was drawn on that series.
    pub fn refers_to(&self, series_uid: &str) -> bool {
        self.referenced_series.iter().any(|s| s == series_uid)
    }

    /// Each segment with its volume in millilitres. Zero if the SEG gives no
    /// slice spacing to work one out from.
    pub fn volumes(&self) -> Vec<(&Segment, f64)> {
        let voxel = self.pixel_spacing[0] * self.pixel_spacing[1] * self.slice_spacing.unwrap_or(0.0) / 1000.0;
        self.segments
            .iter()
            .map(|s| {
                let on: usize = self
                    .frames
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.segment == s.number)
                    .map(|(i, _)| (0..self.rows * self.columns).filter(|p| self.bit(i, *p)).count())
                    .sum();
                (s, on as f64 * voxel)
            })
            .collect()
    }

    /// Why these frames do not line up with these images, if they do not.
    fn check_geometry(&self, g: &Geometry) -> Result<()> {
        let o = &self.orientation;
        let frame_axes = [[o[0], o[1], o[2]], [o[3], o[4], o[5]]];
        for (axis, want) in frame_axes.iter().zip(&g.axes) {
            if (0..3).any(|k| (axis[k] - want[k]).abs() > ORIENTATION_TOLERANCE) {
                return Err(refused(
                    "These segments were drawn at a different orientation from this series, so they were not loaded. \
                     Ruthos does not resample segments",
                ));
            }
        }
        let close = |a: f64, b: f64| (a - b).abs() <= b.abs() * SPACING_TOLERANCE;
        if !close(self.pixel_spacing[0], g.spacing[1]) || !close(self.pixel_spacing[1], g.spacing[0]) {
            return Err(refused(
                "These segments have a different pixel spacing from this series, so they were not loaded. \
                 Ruthos does not resample segments",
            ));
        }
        Ok(())
    }

    /// Put each segment on the volume, refusing with an explanation if any frame
    /// does not fall on the volume's own voxels. A frame may be a cropped part of
    /// a slice, as long as it falls on whole voxels.
    pub fn place(&self, g: &Geometry) -> Result<Vec<Placed>> {
        self.check_geometry(g)?;
        let [w, h, d] = g.dims;
        let mut placed: Vec<Placed> = self
            .segments
            .iter()
            .map(|s| Placed {
                segment: s.clone(),
                voxels: Vec::new(),
            })
            .collect();
        for (i, frame) in self.frames.iter().enumerate() {
            let from = [
                frame.position[0] - g.origin[0],
                frame.position[1] - g.origin[1],
                frame.position[2] - g.origin[2],
            ];
            let at: [f64; 3] = std::array::from_fn(|k| dot(from, g.axes[k]) / g.spacing[k].max(1e-9));
            let whole = at.map(f64::round);
            if (0..3).any(|k| (at[k] - whole[k]).abs() > ALIGNMENT_TOLERANCE) {
                return Err(refused(
                    "These segments lie between the slices of this series, so they were not loaded. \
                     Ruthos does not resample segments",
                ));
            }
            let inside = whole[0] >= 0.0
                && whole[1] >= 0.0
                && whole[2] >= 0.0
                && whole[0] as usize + self.columns <= w
                && whole[1] as usize + self.rows <= h
                && (whole[2] as usize) < d;
            if !inside {
                return Err(refused(
                    "These segments reach outside this series, so they were not loaded",
                ));
            }
            let (x0, y0, z) = (whole[0] as usize, whole[1] as usize, whole[2] as usize);
            let Some(target) = placed.iter_mut().find(|p| p.segment.number == frame.segment) else {
                continue;
            };
            for y in 0..self.rows {
                for x in 0..self.columns {
                    if self.bit(i, x + self.columns * y) {
                        target.voxels.push((x0 + x) + w * ((y0 + y) + h * z));
                    }
                }
            }
        }
        for p in &mut placed {
            p.voxels.sort_unstable();
            p.voxels.dedup();
        }
        Ok(placed)
    }
}
