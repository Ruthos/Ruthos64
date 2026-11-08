//! A study's measurements, calculations, findings and key images saved as DICOM
//! documents in the study, so they travel with it, and documents that arrive
//! with a study turned back into measurements and key images.
//!
//! A document is applied once, and only when every image it refers to is in
//! the archive, so a report sent before its images waits for them. Documents
//! Ruthos writes are marked applied as they are written.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use ruthos_dicom::sr::{self, ImageRef, Outline, SrMeasurement, SrReport};

use crate::db::Database;
use crate::measure::{study_calculations, Library};
use crate::model::{ImageRow, RoiRecord, RoiValue};
use crate::paths::DataPaths;
use crate::Result;

/// A measurement as Ruthos stored it, carried inside a report so it comes
/// back exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Drawn {
    kind: String,
    points: Vec<[f32; 2]>,
    frame: i64,
    name: Option<String>,
    value: Option<f64>,
    unit: String,
    basis: Option<String>,
    def_key: Option<String>,
    #[serde(default)]
    derived: Vec<(String, Option<f64>, String, Option<String>)>,
}

/// What saving wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// Values in the measurement report, if one was written.
    pub measurements: Option<usize>,
    /// Images in the key object document, if one was written.
    pub key_images: Option<usize>,
}

/// What applying documents changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub measurements: usize,
    pub key_images: usize,
}

/// A measurement report or a key image document, which has no pixels.
pub fn is_document_class(sop_class: &str) -> bool {
    sop_class == sr::KEY_OBJECT_SELECTION || sr::SR_CLASSES.contains(&sop_class)
}

/// A series of documents, which the viewers and teaching copies leave out.
pub fn is_document_modality(modality: &str) -> bool {
    matches!(modality, "SR" | "KO")
}

fn is_document(image: &ImageRow) -> bool {
    is_document_class(&image.sop_class_uid)
}

/// The outline other software draws for a measurement, where it has one.
fn outline(kind: &str, p: &[[f32; 2]]) -> Option<Outline> {
    Some(match (kind, p.len()) {
        ("point", 1..) => Outline::Point(p[0]),
        ("length" | "openpolygon", 2..) | ("angle", 3..) => Outline::Polyline(p.to_vec()),
        ("polygon" | "pencil", 3..) => {
            let mut closed = p.to_vec();
            closed.push(p[0]);
            Outline::Polyline(closed)
        }
        ("rect", 2..) => {
            let ([x0, y0], [x1, y1]) = (p[0], p[1]);
            Outline::Polyline(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]])
        }
        ("oval", 2..) => {
            let ([x0, y0], [x1, y1]) = (p[0], p[1]);
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (ax, ay) = (((x1 - x0) / 2.0).abs(), ((y1 - y0) / 2.0).abs());
            if ax >= ay {
                Outline::Ellipse([[cx - ax, cy], [cx + ax, cy], [cx, cy - ay], [cx, cy + ay]])
            } else {
                Outline::Ellipse([[cx, cy - ay], [cx, cy + ay], [cx - ax, cy], [cx + ax, cy]])
            }
        }
        _ => return None,
    })
}

/// A measurement from another program, from its outline.
fn from_outline(o: &Outline) -> (String, Vec<[f32; 2]>) {
    match o {
        Outline::Point(p) => ("point".into(), vec![*p]),
        Outline::Polyline(p) if p.len() == 2 => ("length".into(), p.clone()),
        Outline::Polyline(p) if p.len() >= 4 && p.first() == p.last() => ("polygon".into(), p[..p.len() - 1].to_vec()),
        Outline::Polyline(p) => ("openpolygon".into(), p.clone()),
        Outline::Ellipse(e) => {
            let xs = e.iter().map(|p| p[0]);
            let ys = e.iter().map(|p| p[1]);
            let (x0, x1) = xs.clone().fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(x), b.max(x)));
            let (y0, y1) = ys.clone().fold((f32::MAX, f32::MIN), |(a, b), y| (a.min(y), b.max(y)));
            ("oval".into(), vec![[x0, y0], [x1, y1]])
        }
    }
}

/// A measurement's name in words when it was not given one.
fn tool_name(kind: &str) -> &str {
    match kind {
        "length" => "Length",
        "openpolygon" => "Length of a path",
        "angle" => "Angle",
        "dynangle" => "Angle between two lines",
        "cobb" => "Cobb angle",
        "rect" | "oval" | "polygon" | "pencil" | "brush" => "Area",
        "point" => "Point",
        other => other,
    }
}

/// The study as a measurement report, and its key images.
pub fn build(db: &Database, library: &Library, study_id: i64) -> Result<(SrReport, Vec<ImageRef>)> {
    let images: HashMap<i64, ImageRow> = db.images_for_study(study_id)?.into_iter().map(|i| (i.id, i)).collect();
    let mut series_uids: HashMap<i64, String> = HashMap::new();
    let mut image_ref = |image: &ImageRow, frame: i64| -> Result<ImageRef> {
        if let std::collections::hash_map::Entry::Vacant(slot) = series_uids.entry(image.series_id) {
            let uid = db
                .series(image.series_id)?
                .map(|s| s.series_instance_uid.split('#').next().unwrap_or_default().to_string())
                .unwrap_or_default();
            slot.insert(uid);
        }
        Ok(ImageRef {
            sop_class: image.sop_class_uid.clone(),
            sop_instance: image.sop_instance_uid.clone(),
            series_uid: series_uids[&image.series_id].clone(),
            frame: (image.number_of_frames > 1).then_some(frame as u32 + 1),
        })
    };
    let derived: HashMap<i64, Vec<RoiValue>> = db
        .export_rois(study_id)?
        .into_iter()
        .map(|r| (r.roi_id, r.derived))
        .collect();

    let mut report = SrReport::default();
    for roi in db.rois_for_study(study_id)? {
        let Some(image) = images.get(&roi.image_id) else {
            continue;
        };
        let values = derived.get(&roi.id).cloned().unwrap_or_default();
        let drawn = Drawn {
            kind: roi.kind.clone(),
            points: roi.points.clone(),
            frame: roi.frame,
            name: roi.name.clone(),
            value: roi.value,
            unit: roi.unit.clone(),
            basis: roi.basis.clone(),
            def_key: roi.def_key.clone(),
            derived: values
                .iter()
                .map(|v| (v.def_key.clone(), v.value, v.unit.clone(), v.basis.clone()))
                .collect(),
        };
        let mut numbers: Vec<(String, Option<String>, f64, String)> = Vec::new();
        if let Some(v) = roi.value {
            let label = roi.name.clone().unwrap_or_else(|| tool_name(&roi.kind).to_string());
            numbers.push((label, roi.def_key.clone(), v, roi.unit.clone()));
        }
        for v in &values {
            if let Some(x) = v.value {
                numbers.push((library.label(&v.def_key), Some(v.def_key.clone()), x, v.unit.clone()));
            }
        }
        let place = image_ref(image, roi.frame)?;
        // The first value of a measurement carries its outline and how it was
        // drawn; the rest name the same image.
        let mut first = true;
        for (label, key, value, unit) in numbers {
            let Some(unit) = sr::ucum(&unit) else { continue };
            let concept = key
                .as_deref()
                .and_then(|k| library.definition(k))
                .and_then(|d| d.concept.clone());
            report.measurements.push(SrMeasurement {
                name: label.clone(),
                concept: sr::concept_for(concept.as_deref(), key.as_deref().unwrap_or_default(), &label),
                value,
                unit: unit.to_string(),
                image: Some(place.clone()),
                outline: if first { outline(&roi.kind, &roi.points) } else { None },
                drawn: first.then(|| serde_json::to_string(&drawn).unwrap_or_default()),
                calculated: false,
            });
            first = false;
        }
    }
    let inputs = db.measurement_inputs_for_study(study_id)?;
    let has_mmode = !db.mmode_rows_for_study(study_id)?.is_empty();
    for r in study_calculations(library, &inputs, has_mmode).0 {
        let Some(unit) = sr::ucum(&r.unit) else { continue };
        report.measurements.push(SrMeasurement {
            name: r.label.clone(),
            concept: sr::concept_for(None, &r.key, &r.label),
            value: r.value,
            unit: unit.to_string(),
            image: None,
            outline: None,
            drawn: None,
            calculated: true,
        });
    }
    if let Some(text) = db.report(study_id)? {
        report.findings = text.findings;
        report.conclusion = text.conclusion;
    }
    let keys = db
        .key_images_for_study(study_id)?
        .iter()
        .filter(|i| !is_document(i))
        .map(|i| image_ref(i, 0).map(|r| ImageRef { frame: None, ..r }))
        .collect::<Result<Vec<_>>>()?;
    Ok((report, keys))
}

/// Write the study's measurement report and key image document into it.
pub fn save(db: &Database, paths: &DataPaths, library: &Library, study_id: i64) -> Result<Saved> {
    let (report, keys) = build(db, library, study_id)?;
    let template = db
        .images_for_study(study_id)?
        .into_iter()
        .find(|i| !is_document(i))
        .map(|i| paths.resolve(&i.path))
        .ok_or_else(|| crate::CoreError::Other("the study has no images".into()))?;
    let folder = paths.temp_dir();
    let mut saved = Saved::default();
    let mut files = Vec::new();
    if !report.measurements.is_empty() || !report.findings.trim().is_empty() || !report.conclusion.trim().is_empty() {
        files.push(sr::write_sr(&template, &report, &folder)?);
        saved.measurements = Some(report.measurements.len());
    }
    if !keys.is_empty() {
        files.push(sr::write_kos(&template, &keys, &folder)?);
        saved.key_images = Some(keys.len());
    }
    for file in files {
        // Named by its SOP Instance UID; already true of this archive.
        if let Some(uid) = file.file_stem().and_then(|s| s.to_str()) {
            db.mark_document_applied(uid)?;
        }
        crate::import::import_file(db, paths, &file, crate::import::ImportMode::Move)?;
    }
    Ok(saved)
}

/// Turn the study's documents that have not been applied yet into
/// measurements and key images.
pub fn apply(db: &Database, paths: &DataPaths, library: &Library, study_id: i64) -> Result<Applied> {
    let mut applied = Applied::default();
    for doc in db.images_for_study(study_id)?.into_iter().filter(is_document) {
        if db.document_applied(&doc.sop_instance_uid)? {
            continue;
        }
        let file = paths.resolve(&doc.path);
        if doc.sop_class_uid == sr::KEY_OBJECT_SELECTION {
            let Ok(uids) = sr::read_kos(&file) else { continue };
            let found: Vec<Option<ImageRow>> = uids.iter().map(|u| db.image_by_sop_uid(u).ok().flatten()).collect();
            if found.iter().any(Option::is_none) {
                continue;
            }
            for image in found.into_iter().flatten() {
                if !image.is_key_image {
                    db.set_key_image(image.id, true)?;
                    applied.key_images += 1;
                }
            }
        } else {
            let Ok(report) = sr::read_sr(&file) else { continue };
            let placed: Vec<&SrMeasurement> = report
                .measurements
                .iter()
                .filter(|m| !m.calculated && (m.drawn.is_some() || m.outline.is_some()))
                .collect();
            let found: Vec<Option<ImageRow>> = placed
                .iter()
                .map(|m| {
                    m.image
                        .as_ref()
                        .and_then(|r| db.image_by_sop_uid(&r.sop_instance).ok().flatten())
                })
                .collect();
            if found.iter().any(Option::is_none) {
                continue;
            }
            for (m, image) in placed.into_iter().zip(found.into_iter().flatten()) {
                applied.measurements += place(db, library, &image, m)? as usize;
            }
        }
        db.mark_document_applied(&doc.sop_instance_uid)?;
    }
    Ok(applied)
}

/// Add one measurement to its image, unless it is already there.
fn place(db: &Database, library: &Library, image: &ImageRow, m: &SrMeasurement) -> Result<bool> {
    let drawn: Drawn = match m.drawn.as_deref().and_then(|d| serde_json::from_str(d).ok()) {
        Some(d) => d,
        None => {
            let Some(o) = &m.outline else { return Ok(false) };
            let (kind, points) = from_outline(o);
            // A code names a definition when one of the library's has it.
            let key = if m.concept.scheme == sr::SCHEME {
                library.definition(&m.concept.value).map(|d| d.key.clone())
            } else {
                let wanted = format!("{}:{}", m.concept.scheme, m.concept.value);
                library
                    .definitions
                    .iter()
                    .find(|d| d.concept.as_deref() == Some(wanted.as_str()))
                    .map(|d| d.key.clone())
            };
            Drawn {
                kind,
                points,
                frame: m.image.as_ref().and_then(|r| r.frame).map_or(0, |f| f as i64 - 1),
                name: Some(m.name.clone()),
                value: Some(m.value),
                unit: sr::from_ucum(&m.unit),
                basis: None,
                def_key: key,
                derived: Vec::new(),
            }
        }
    };
    let mut rois = db.rois(image.id, drawn.frame)?;
    let same = |r: &RoiRecord| {
        r.kind == drawn.kind
            && r.name == drawn.name
            && r.points.len() == drawn.points.len()
            && r.points
                .iter()
                .zip(&drawn.points)
                .all(|(a, b)| (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01)
    };
    if rois.iter().any(same) {
        return Ok(false);
    }
    rois.push(RoiRecord {
        id: 0,
        image_id: image.id,
        frame: drawn.frame,
        kind: drawn.kind.clone(),
        points: drawn.points.clone(),
        name: drawn.name.clone(),
        value: drawn.value,
        unit: drawn.unit.clone(),
        basis: drawn.basis.clone(),
        def_key: drawn.def_key.clone(),
    });
    db.sync_rois(image.id, drawn.frame, &mut rois)?;
    if let Some(new) = rois.last().filter(|_| !drawn.derived.is_empty()) {
        let values: Vec<RoiValue> = drawn
            .derived
            .iter()
            .map(|(def_key, value, unit, basis)| RoiValue {
                def_key: def_key.clone(),
                value: *value,
                unit: unit.clone(),
                basis: basis.clone(),
            })
            .collect();
        db.set_roi_values(new.id, &values)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_go_out_and_come_back_as_the_same_tool() {
        for (kind, points) in [
            ("length", vec![[1.0, 2.0], [5.0, 9.0]]),
            ("point", vec![[3.0, 4.0]]),
            ("polygon", vec![[0.0, 0.0], [4.0, 0.0], [2.0, 3.0]]),
            ("openpolygon", vec![[0.0, 0.0], [4.0, 0.0], [2.0, 3.0]]),
            ("oval", vec![[2.0, 1.0], [10.0, 5.0]]),
        ] {
            let o = outline(kind, &points).unwrap();
            assert_eq!(from_outline(&o), (kind.to_string(), points), "{kind}");
        }
        // A rectangle goes out as a closed outline and comes back as a polygon.
        let o = outline("rect", &[[0.0, 0.0], [2.0, 3.0]]).unwrap();
        assert_eq!(from_outline(&o).0, "polygon");
        assert!(
            outline("cobb", &[[0.0, 0.0]; 4]).is_none(),
            "Ruthos's own record carries it"
        );
    }
}
