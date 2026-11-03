//! Saving MPR planes into the study (Horos: MPR > Export > DICOM).
//!
//! Each plane becomes an image of a new series in the same study, of the
//! source series' kind (a CT reformat stays a CT image), marked DERIVED and
//! REFORMATTED, with its own position, orientation and square pixels, so it
//! opens, measures and reports like any other image. Measurements drawn on
//! a plane in the MPR window are stored on the image made from it.

use std::path::Path;

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_dicom::open::open_any;
use ruthos_dicom::pixels::FramePixels;

use crate::db::Database;
use crate::import::{import_file, FileOutcome, ImportMode, ImportSummary};
use crate::model::RoiRecord;
use crate::paths::DataPaths;
use crate::volume::{reslice, Plane, Volume};
use crate::{CoreError, Result};

/// A new UID under the UUID-derived root (2.25), as DICOM allows.
pub fn new_uid() -> String {
    format!("2.25.{}", uuid::Uuid::new_v4().as_u128())
}

/// One plane to save, with the measurements drawn on it (their image id
/// and frame are filled in once the image exists).
pub struct SavedPlane {
    pub plane: Plane,
    pub rois: Vec<RoiRecord>,
}

/// What was saved.
#[derive(Debug, Clone, Default)]
pub struct MprSaveResult {
    pub series_uid: String,
    pub image_ids: Vec<i64>,
    pub summary: ImportSummary,
}

/// The tags that describe the source's pixels or its place in its series;
/// each saved plane has its own.
const DROPPED: [Tag; 22] = [
    tags::PIXEL_DATA,
    tags::NUMBER_OF_FRAMES,
    tags::ROWS,
    tags::COLUMNS,
    tags::PIXEL_SPACING,
    tags::IMAGE_POSITION_PATIENT,
    tags::IMAGE_ORIENTATION_PATIENT,
    tags::SLICE_LOCATION,
    tags::SLICE_THICKNESS,
    tags::SPACING_BETWEEN_SLICES,
    tags::WINDOW_CENTER,
    tags::WINDOW_WIDTH,
    tags::WINDOW_CENTER_WIDTH_EXPLANATION,
    tags::SMALLEST_IMAGE_PIXEL_VALUE,
    tags::LARGEST_IMAGE_PIXEL_VALUE,
    tags::LOSSY_IMAGE_COMPRESSION,
    tags::LOSSY_IMAGE_COMPRESSION_RATIO,
    tags::LOSSY_IMAGE_COMPRESSION_METHOD,
    tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
    tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
    tags::ICON_IMAGE_SEQUENCE,
    tags::PIXEL_PADDING_VALUE,
];

fn ds(v: f64) -> String {
    // Decimal strings are at most 16 characters.
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.len() > 16 {
        format!("{v:.3e}")
    } else if s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// Save planes of `volume` as a new series of the study `template` (a file
/// of the source series) belongs to. `window` is written as each image's
/// default window.
#[allow(clippy::too_many_arguments)]
pub fn save_planes(
    db: &Database,
    paths: &DataPaths,
    template: &Path,
    volume: &Volume,
    planes: &[SavedPlane],
    description: &str,
    window: (f32, f32),
    progress: &dyn Fn(usize, usize),
) -> Result<MprSaveResult> {
    if planes.is_empty() {
        return Err(CoreError::Other("nothing to save".into()));
    }
    let base: InMemDicomObject = open_any(template, None)?.into_inner();
    let sop_class = base
        .element(tags::SOP_CLASS_UID)
        .ok()
        .and_then(|e| e.value().to_str().ok().map(|s| s.trim_end_matches('\0').to_string()))
        .unwrap_or_else(|| uids::SECONDARY_CAPTURE_IMAGE_STORAGE.to_string());
    // A source in an enhanced (multi-frame) class becomes the classic one.
    let sop_class = match sop_class.as_str() {
        uids::ENHANCED_CT_IMAGE_STORAGE => uids::CT_IMAGE_STORAGE.to_string(),
        uids::ENHANCED_MR_IMAGE_STORAGE => uids::MR_IMAGE_STORAGE.to_string(),
        other => other.to_string(),
    };
    let series_uid = new_uid();
    let series_number = 9000 + (chrono::Local::now().timestamp() % 1000) as i32;
    let dir = paths.temp_dir().join(format!("mpr-{}", &series_uid[5..17]));
    std::fs::create_dir_all(&dir)?;

    let mut result = MprSaveResult {
        series_uid: series_uid.clone(),
        summary: ImportSummary {
            from_network: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let outcome: Result<()> = (|| {
        for (k, sp) in planes.iter().enumerate() {
            progress(k, planes.len());
            let frame = reslice(volume, &sp.plane);
            let FramePixels::Gray(values) = &frame.pixels else {
                return Err(CoreError::Other("only greyscale volumes can be saved".into()));
            };
            let (lo, hi) = values
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
            let signed = lo < 0.0;
            let bytes: Vec<u8> = if signed {
                values
                    .iter()
                    .flat_map(|v| (v.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16).to_le_bytes())
                    .collect()
            } else {
                values
                    .iter()
                    .flat_map(|v| (v.round().clamp(0.0, u16::MAX as f32) as u16).to_le_bytes())
                    .collect()
            };
            let _ = hi;

            let mut obj = base.clone();
            for t in DROPPED {
                obj.remove_element(t);
            }
            let sop = new_uid();
            let s =
                |obj: &mut InMemDicomObject, tag, vr, v: &str| obj.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
            s(&mut obj, tags::SOP_CLASS_UID, VR::UI, &sop_class);
            s(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, &sop);
            s(&mut obj, tags::SERIES_INSTANCE_UID, VR::UI, &series_uid);
            s(&mut obj, tags::SERIES_NUMBER, VR::IS, &series_number.to_string());
            s(&mut obj, tags::SERIES_DESCRIPTION, VR::LO, description);
            s(&mut obj, tags::INSTANCE_NUMBER, VR::IS, &(k + 1).to_string());
            s(&mut obj, tags::IMAGE_TYPE, VR::CS, "DERIVED\\SECONDARY\\REFORMATTED");
            s(
                &mut obj,
                tags::DERIVATION_DESCRIPTION,
                VR::ST,
                "Multiplanar reconstruction in Ruthos",
            );
            let p0 = sp.plane.point(0.0, 0.0);
            s(
                &mut obj,
                tags::IMAGE_POSITION_PATIENT,
                VR::DS,
                &format!("{}\\{}\\{}", ds(p0[0]), ds(p0[1]), ds(p0[2])),
            );
            let (r, d) = (sp.plane.right, sp.plane.down);
            s(
                &mut obj,
                tags::IMAGE_ORIENTATION_PATIENT,
                VR::DS,
                &format!(
                    "{}\\{}\\{}\\{}\\{}\\{}",
                    ds(r[0]),
                    ds(r[1]),
                    ds(r[2]),
                    ds(d[0]),
                    ds(d[1]),
                    ds(d[2])
                ),
            );
            let px = ds(sp.plane.pixel_mm);
            s(&mut obj, tags::PIXEL_SPACING, VR::DS, &format!("{px}\\{px}"));
            let thickness = if sp.plane.slab_mm > 0.0 {
                sp.plane.slab_mm
            } else {
                volume.finest()
            };
            s(&mut obj, tags::SLICE_THICKNESS, VR::DS, &ds(thickness));
            s(&mut obj, tags::WINDOW_CENTER, VR::DS, &ds(window.0 as f64));
            s(&mut obj, tags::WINDOW_WIDTH, VR::DS, &ds(window.1.max(1.0) as f64));
            s(&mut obj, tags::RESCALE_INTERCEPT, VR::DS, "0");
            s(&mut obj, tags::RESCALE_SLOPE, VR::DS, "1");
            s(&mut obj, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
            s(&mut obj, tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
            let us = |obj: &mut InMemDicomObject, tag, v: u16| {
                obj.put(DataElement::new(tag, VR::US, dicom_value!(U16, [v])))
            };
            us(&mut obj, tags::ROWS, sp.plane.height as u16);
            us(&mut obj, tags::COLUMNS, sp.plane.width as u16);
            us(&mut obj, tags::SAMPLES_PER_PIXEL, 1);
            us(&mut obj, tags::BITS_ALLOCATED, 16);
            us(&mut obj, tags::BITS_STORED, 16);
            us(&mut obj, tags::HIGH_BIT, 15);
            us(&mut obj, tags::PIXEL_REPRESENTATION, if signed { 1 } else { 0 });
            obj.put(DataElement::new(tags::PIXEL_DATA, VR::OW, PrimitiveValue::from(bytes)));

            let meta = FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid(sop_class.as_str())
                .media_storage_sop_instance_uid(sop.as_str())
                .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
                .implementation_version_name("RUTHOS_0_1")
                .build()
                .map_err(|e| CoreError::Other(e.to_string()))?;
            let file = dir.join(format!("{sop}.dcm"));
            obj.with_exact_meta(meta)
                .write_to_file(&file)
                .map_err(|e| CoreError::Other(e.to_string()))?;

            match import_file(db, paths, &file, ImportMode::Move)? {
                FileOutcome::Imported {
                    image_id,
                    study_id,
                    new_study,
                    ..
                } => {
                    result.image_ids.push(image_id);
                    result.summary.imported += 1;
                    result.summary.image_ids.push(image_id);
                    if !result.summary.touched_study_ids.contains(&study_id) {
                        result.summary.touched_study_ids.push(study_id);
                    }
                    if new_study {
                        result.summary.new_study_ids.push(study_id);
                    }
                    if !sp.rois.is_empty() {
                        let records: Vec<RoiRecord> = sp
                            .rois
                            .iter()
                            .map(|r| RoiRecord {
                                id: 0,
                                image_id,
                                frame: 0,
                                ..r.clone()
                            })
                            .collect();
                        db.replace_rois(image_id, 0, &records)?;
                    }
                }
                _ => return Err(CoreError::Other("a saved plane was not imported".into())),
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    outcome?;
    progress(planes.len(), planes.len());
    Ok(result)
}

/// Planes for a stack along `base`'s normal, `spacing_mm` apart, across the
/// whole volume (or `count` planes centred on `base` when given).
pub fn stack(volume: &Volume, base: &Plane, spacing_mm: f64, count: Option<usize>) -> Vec<Plane> {
    let n = base.normal();
    let spacing = spacing_mm.max(0.1);
    let (lo, hi) = match count {
        Some(c) => {
            let half = (c.max(1) as f64 - 1.0) / 2.0;
            (-half * spacing, half * spacing)
        }
        None => {
            // How far the volume reaches along the normal from the plane.
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            let [w, h, d] = volume.dims;
            for &i in &[0.0, (w - 1) as f64] {
                for &j in &[0.0, (h - 1) as f64] {
                    for &k in &[0.0, (d - 1) as f64] {
                        let p = volume.to_patient([i, j, k]);
                        let t = crate::volume::dot(crate::volume::sub(p, base.centre), n);
                        lo = lo.min(t);
                        hi = hi.max(t);
                    }
                }
            }
            // As many planes as fit, centred in the volume's reach.
            let count = ((hi - lo) / spacing + 1e-9).floor() + 1.0;
            let start = lo + ((hi - lo) - (count - 1.0) * spacing) / 2.0;
            (start, start + (count - 1.0) * spacing)
        }
    };
    let mut out = Vec::new();
    let mut t = lo;
    while t <= hi + 1e-9 && out.len() < 2000 {
        out.push(Plane {
            centre: crate::volume::add(base.centre, crate::volume::scale(n, t)),
            ..*base
        });
        t += spacing;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_strings_fit() {
        assert_eq!(ds(0.5), "0.5");
        assert_eq!(ds(-63.6), "-63.6");
        assert_eq!(ds(-0.0000001), "0");
        assert!(ds(123456789.123456).len() <= 16);
        assert!(new_uid().starts_with("2.25.") && new_uid().len() <= 64);
    }
}
