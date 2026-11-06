//! Anatomical M-mode: a time-and-depth strip read from a 2D loop along a line
//! the user draws, straight or curved, after the study.
//!
//! A true M-mode samples about a thousand times a second, and this has only the
//! frames of the loop. So time is never interpolated: each frame is one block of
//! identical columns, and a strip cannot look sharper than its loop. Everything
//! that shows or saves a strip says the rate it was derived at.
//!
//! A saved strip is an ordinary ultrasound image with an M-mode region, so the
//! M-mode measuring tools work on it with no special case.

use std::path::Path;

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{dicom_value, DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use crate::open::open_any;
use crate::pixels::{Frame, FramePixels};
use crate::sc::new_uid;
use crate::seg::write::{add_required_empty, COPIED};
use crate::{DicomError, Result};

/// How many columns one frame takes up in a saved strip, wide enough to measure on.
pub const COLUMNS_PER_FRAME: usize = 8;

/// The most points along a path.
const MOST_SAMPLES: usize = 4096;

/// Below this rate a strip says plainly that it may miss fast events.
pub const LOW_FRAME_RATE: f64 = 30.0;

/// Points on the smooth curve between two clicked points.
const CURVE_STEPS: usize = 16;

/// The scale a strip is read at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// Millimetres per image pixel, across and down.
    pub mm: [f64; 2],
    pub ms_per_frame: f64,
}

/// Depth down and time across.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    pub frames: usize,
    pub samples: usize,
    /// `samples` rows of `frames` values.
    pub values: Vec<f32>,
    pub mm_per_sample: f64,
    pub ms_per_frame: f64,
    pub curved: bool,
}

impl Strip {
    pub fn frames_per_second(&self) -> f64 {
        1000.0 / self.ms_per_frame
    }

    pub fn length_mm(&self) -> f64 {
        self.samples.saturating_sub(1) as f64 * self.mm_per_sample
    }

    /// What must go with a strip wherever it is seen.
    pub fn label(&self) -> String {
        format!("Anatomical M-mode, derived at {:.0} fps", self.frames_per_second())
    }

    /// Words about how far to trust this strip, when it needs them.
    pub fn warning(&self) -> Option<String> {
        (self.frames_per_second() < LOW_FRAME_RATE).then(|| {
            format!(
                "Only {:.0} frames a second: fast events can fall between frames.",
                self.frames_per_second()
            )
        })
    }

    pub fn at(&self, frame: usize, sample: usize) -> f32 {
        self.values[sample * self.frames + frame]
    }
}

/// Smooth a path of three or more points through every one of them. Two points
/// stay a straight line.
pub fn smooth(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let at = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    let mut out = Vec::with_capacity(points.len() * CURVE_STEPS);
    for i in 0..points.len() as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for step in 0..CURVE_STEPS {
            let t = step as f64 / CURVE_STEPS as f64;
            let (t2, t3) = (t * t, t * t * t);
            out.push(std::array::from_fn(|a| {
                0.5 * (2.0 * p1[a]
                    + (p2[a] - p0[a]) * t
                    + (2.0 * p0[a] - 5.0 * p1[a] + 4.0 * p2[a] - p3[a]) * t2
                    + (3.0 * p1[a] - p0[a] - 3.0 * p2[a] + p3[a]) * t3)
            }));
        }
    }
    out.extend(points.last());
    out
}

/// The scale at a path, or why there is none to be had.
pub fn calibration(frame: &Frame, points: &[[f64; 2]]) -> std::result::Result<Calibration, String> {
    let ms_per_frame = frame
        .meta
        .frame_time_ms
        .filter(|ms| *ms > 0.0)
        .ok_or("This loop does not say how long a frame lasts, so a strip has no time scale.")?;
    let first = points.first().ok_or("Click the image to start a line.")?;
    let region = frame
        .meta
        .us_region_at(first[0], first[1])
        .filter(|r| r.spatial_format == 1 && r.units_x == 3 && r.units_y == 3 && r.delta_x != 0.0 && r.delta_y != 0.0)
        .ok_or("That point is not inside a calibrated 2D region, so a strip has no depth scale.")?;
    if smooth(points).iter().any(|p| !region.contains(p[0], p[1])) {
        return Err("The line leaves the 2D region. Keep it inside the image sector.".to_string());
    }
    Ok(Calibration {
        mm: [region.delta_x.abs() * 10.0, region.delta_y.abs() * 10.0],
        ms_per_frame,
    })
}

/// Points evenly spaced in millimetres along a path, in image pixels.
fn along(path: &[[f64; 2]], mm: [f64; 2]) -> (Vec<[f64; 2]>, f64) {
    let step = mm[0].min(mm[1]);
    let length = |a: [f64; 2], b: [f64; 2]| ((b[0] - a[0]) * mm[0]).hypot((b[1] - a[1]) * mm[1]);
    let total: f64 = path.windows(2).map(|w| length(w[0], w[1])).sum();
    let count = ((total / step).round() as usize + 1).clamp(2, MOST_SAMPLES);
    let spacing = total / (count - 1) as f64;
    let mut out = Vec::with_capacity(count);
    let (mut segment, mut covered) = (0, 0.0);
    for n in 0..count {
        let wanted = spacing * n as f64;
        while segment + 2 < path.len() && covered + length(path[segment], path[segment + 1]) < wanted {
            covered += length(path[segment], path[segment + 1]);
            segment += 1;
        }
        let (a, b) = (path[segment], path[segment + 1]);
        let span = length(a, b);
        let t = if span > 0.0 {
            ((wanted - covered) / span).min(1.0)
        } else {
            0.0
        };
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    (out, spacing)
}

/// The brightness at a place in a frame, between pixels.
fn value_at(frame: &Frame, x: f64, y: f64) -> f32 {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (x, y) = (x.clamp(0.0, (w - 1) as f64), y.clamp(0.0, (h - 1) as f64));
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let pixel = |px: usize, py: usize| -> f32 {
        let i = py * w + px;
        match &frame.pixels {
            FramePixels::Gray(v) => v[i],
            FramePixels::Rgba(v) => {
                let c = &v[i * 4..i * 4 + 3];
                0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
            }
        }
    };
    let top = pixel(x0, y0) * (1.0 - fx) + pixel(x1, y0) * fx;
    let bottom = pixel(x0, y1) * (1.0 - fx) + pixel(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Read a strip from the frames of a loop, along the points clicked.
pub fn read_strip<F: std::ops::Deref<Target = Frame> + Sync>(
    frames: &[F],
    points: &[[f64; 2]],
    scale: Calibration,
) -> Strip {
    let (path, spacing) = along(&smooth(points), scale.mm);
    let samples = path.len();
    let mut values = Vec::with_capacity(samples * frames.len());
    for p in &path {
        values.extend(frames.iter().map(|f| value_at(f, p[0], p[1])));
    }
    Strip {
        frames: frames.len(),
        samples,
        values,
        mm_per_sample: spacing,
        ms_per_frame: scale.ms_per_frame,
        curved: points.len() > 2,
    }
}

fn string(obj: &mut InMemDicomObject, tag: Tag, vr: VR, value: &str) {
    obj.put(DataElement::new(tag, vr, dicom_value!(Str, value)));
}

fn short(obj: &mut InMemDicomObject, tag: Tag, value: u16) {
    obj.put(DataElement::new(tag, VR::US, PrimitiveValue::from(value)));
}

fn long(obj: &mut InMemDicomObject, tag: Tag, value: u32) {
    obj.put(DataElement::new(tag, VR::UL, PrimitiveValue::from(value)));
}

fn double(obj: &mut InMemDicomObject, tag: Tag, value: f64) {
    obj.put(DataElement::new(tag, VR::FD, PrimitiveValue::from(value)));
}

/// The one region of a strip: time across in seconds, depth down in centimetres.
fn region(strip: &Strip, columns: usize) -> InMemDicomObject {
    let mut r = InMemDicomObject::new_empty();
    short(&mut r, tags::REGION_SPATIAL_FORMAT, 2);
    short(&mut r, tags::REGION_DATA_TYPE, 1);
    long(&mut r, tags::REGION_FLAGS, 0);
    long(&mut r, tags::REGION_LOCATION_MIN_X0, 0);
    long(&mut r, tags::REGION_LOCATION_MIN_Y0, 0);
    long(&mut r, tags::REGION_LOCATION_MAX_X1, columns as u32 - 1);
    long(&mut r, tags::REGION_LOCATION_MAX_Y1, strip.samples as u32 - 1);
    short(&mut r, tags::PHYSICAL_UNITS_X_DIRECTION, 4);
    short(&mut r, tags::PHYSICAL_UNITS_Y_DIRECTION, 3);
    double(
        &mut r,
        tags::PHYSICAL_DELTA_X,
        strip.ms_per_frame / COLUMNS_PER_FRAME as f64 / 1000.0,
    );
    double(&mut r, tags::PHYSICAL_DELTA_Y, strip.mm_per_sample / 10.0);
    r
}

/// Write a strip as an ultrasound image of the study `template` belongs to,
/// through the given window, and return its file.
pub fn write_strip(template: &Path, strip: &Strip, window: (f32, f32), folder: &Path) -> Result<std::path::PathBuf> {
    let source: InMemDicomObject = open_any(template, Some(tags::PIXEL_DATA))?.into_inner();
    let columns = strip.frames * COLUMNS_PER_FRAME;
    if columns > u16::MAX as usize || strip.samples > u16::MAX as usize {
        return Err(DicomError::Pixels("the strip is too large to save".to_string()));
    }

    let mut obj = InMemDicomObject::new_empty();
    string(&mut obj, tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192");
    for tag in COPIED {
        if let Some(element) = source.get(*tag) {
            obj.put(element.clone());
        }
    }
    obj.remove_element(tags::FRAME_OF_REFERENCE_UID);
    add_required_empty(&mut obj);
    obj.remove_element(tags::POSITION_REFERENCE_INDICATOR);

    let now = chrono::Local::now();
    let (date, time) = (now.format("%Y%m%d").to_string(), now.format("%H%M%S").to_string());
    let (sop, series) = (new_uid(), new_uid());
    let label = strip.label();
    let path_kind = if strip.curved {
        "a curved path"
    } else {
        "a straight line"
    };
    let derivation = format!(
        "Anatomical M-mode in Ruthos, read along {path_kind} of {:.1} mm through {} frames at {:.0} frames a second",
        strip.length_mm(),
        strip.frames,
        strip.frames_per_second()
    );
    string(&mut obj, tags::SOP_CLASS_UID, VR::UI, uids::ULTRASOUND_IMAGE_STORAGE);
    string(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, &sop);
    string(&mut obj, tags::SERIES_INSTANCE_UID, VR::UI, &series);
    string(&mut obj, tags::MODALITY, VR::CS, "US");
    string(&mut obj, tags::SERIES_NUMBER, VR::IS, "9500");
    string(&mut obj, tags::INSTANCE_NUMBER, VR::IS, "1");
    string(&mut obj, tags::SERIES_DESCRIPTION, VR::LO, &label);
    string(&mut obj, tags::IMAGE_COMMENTS, VR::LT, &label);
    string(&mut obj, tags::IMAGE_TYPE, VR::CS, "DERIVED\\SECONDARY");
    string(&mut obj, tags::DERIVATION_DESCRIPTION, VR::ST, &derivation);
    string(&mut obj, tags::CONTENT_DATE, VR::DA, &date);
    string(&mut obj, tags::CONTENT_TIME, VR::TM, &time);
    string(&mut obj, tags::SERIES_DATE, VR::DA, &date);
    string(&mut obj, tags::SERIES_TIME, VR::TM, &time);
    string(&mut obj, tags::MANUFACTURER, VR::LO, "Ruthos");
    string(&mut obj, tags::PATIENT_ORIENTATION, VR::CS, "");
    string(&mut obj, tags::LATERALITY, VR::CS, "");
    string(&mut obj, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    string(&mut obj, tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
    string(&mut obj, tags::LOSSY_IMAGE_COMPRESSION, VR::CS, "00");
    short(&mut obj, tags::ROWS, strip.samples as u16);
    short(&mut obj, tags::COLUMNS, columns as u16);
    short(&mut obj, tags::SAMPLES_PER_PIXEL, 1);
    short(&mut obj, tags::BITS_ALLOCATED, 8);
    short(&mut obj, tags::BITS_STORED, 8);
    short(&mut obj, tags::HIGH_BIT, 7);
    short(&mut obj, tags::PIXEL_REPRESENTATION, 0);

    if let (Some(class), Some(instance)) = (source.get(tags::SOP_CLASS_UID), source.get(tags::SOP_INSTANCE_UID)) {
        let mut item = InMemDicomObject::new_empty();
        let text = |e: &crate::seg::write::Element| e.to_str().map(|s| s.trim_end_matches('\0').to_string());
        if let (Ok(class), Ok(instance)) = (text(class), text(instance)) {
            string(&mut item, tags::REFERENCED_SOP_CLASS_UID, VR::UI, &class);
            string(&mut item, tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, &instance);
            obj.put(DataElement::new(
                tags::SOURCE_IMAGE_SEQUENCE,
                VR::SQ,
                Value::Sequence(DataSetSequence::new(vec![item], Length::UNDEFINED)),
            ));
        }
    }
    obj.put(DataElement::new(
        tags::SEQUENCE_OF_ULTRASOUND_REGIONS,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(vec![region(strip, columns)], Length::UNDEFINED)),
    ));

    let (centre, width) = (window.0, window.1.max(1.0));
    let mut pixels = Vec::with_capacity(columns * strip.samples + 1);
    for sample in 0..strip.samples {
        for frame in 0..strip.frames {
            let shade = (((strip.at(frame, sample) - centre) / width + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels.extend(std::iter::repeat_n(shade, COLUMNS_PER_FRAME));
        }
    }
    if pixels.len() % 2 == 1 {
        pixels.push(0);
    }
    obj.put(DataElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::from(pixels)));

    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(uids::ULTRASOUND_IMAGE_STORAGE)
        .media_storage_sop_instance_uid(sop.as_str())
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .implementation_version_name("RUTHOS_0_1")
        .build()
        .map_err(|e| DicomError::Pixels(e.to_string()))?;
    std::fs::create_dir_all(folder).map_err(|e| DicomError::Pixels(e.to_string()))?;
    let file = folder.join(format!("{sop}.dcm"));
    obj.with_exact_meta(meta)
        .write_to_file(&file)
        .map_err(|e| DicomError::Pixels(e.to_string()))?;
    Ok(file)
}
