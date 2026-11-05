//! Segments kept with the study, as DICOM Segmentation series.
//!
//! Saving writes one SEG holding every segment on the panel. Saving again adds an
//! instance to the same SEG series, so earlier versions remain and the newest is
//! what opens. A SEG refers to the series it was drawn on, which is how it is found
//! again, and it files, sends and retrieves like any other series.

use std::path::{Path, PathBuf};

use ruthos_dicom::seg::read::{peek, Peek};
use ruthos_dicom::seg::{read_seg, source_series, write_seg, SegOptions, SegWritten, SegmentData};

use crate::db::Database;
use crate::import::{import_file, ImportMode};
use crate::paths::DataPaths;
use crate::report::ReportMeasurement;
use crate::{CoreError, Result};

pub const MODALITY: &str = "SEG";
const SERIES_DESCRIPTION: &str = "Ruthos segments";
/// Series number of a new SEG series, clear of what scanners use.
const SERIES_NUMBER: i32 = 9500;

/// A saved SEG found for a series.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub peek: Peek,
}

impl Saved {
    /// A line for a list: when, and what.
    pub fn describe(&self) -> String {
        let p = &self.peek;
        let date = ruthos_dicom::meta::dicom_date_to_iso(&p.content_date).unwrap_or_else(|| p.content_date.clone());
        let time = ruthos_dicom::meta::dicom_time_to_hms(&p.content_time).unwrap_or_default();
        format!("{date} {time}: {}", p.labels.join(", "))
    }
}

/// Every SEG in a study, newest first.
fn saved_in_study(db: &Database, paths: &DataPaths, study_id: i64) -> Result<Vec<Saved>> {
    let mut out = Vec::new();
    for series in db.series_for_study(study_id)? {
        if series.modality != MODALITY {
            continue;
        }
        for image in db.images_for_series(series.id)? {
            let path = paths.resolve(&image.path);
            match peek(&path) {
                Ok(p) => out.push(Saved { path, peek: p }),
                Err(e) => tracing::warn!("{}: {e}", path.display()),
            }
        }
    }
    let when = |s: &Saved| {
        (
            s.peek.content_date.clone(),
            s.peek.content_time.clone(),
            s.peek.instance_number,
        )
    };
    out.sort_by_key(|s| std::cmp::Reverse(when(s)));
    Ok(out)
}

/// The SEGs drawn on a series, newest first.
pub fn saved_for_series(db: &Database, paths: &DataPaths, study_id: i64, series_uid: &str) -> Result<Vec<Saved>> {
    let mut all = saved_in_study(db, paths, study_id)?;
    all.retain(|s| s.peek.referenced_series.iter().any(|r| r == series_uid));
    Ok(all)
}

/// Write `segments`, drawn on a series, into the study. `dims` is the volume they
/// were drawn on, which must be the series as it stands.
pub fn save_segments(
    db: &Database,
    paths: &DataPaths,
    study_id: i64,
    series_id: i64,
    dims: [usize; 3],
    segments: &[SegmentData],
    creator: &str,
) -> Result<SegWritten> {
    let files: Vec<PathBuf> = db
        .images_for_series(series_id)?
        .iter()
        .map(|i| paths.resolve(&i.path))
        .collect();
    let source = source_series(&files)?;
    if source.slices.len() != dims[2] || source.columns as usize != dims[0] || source.rows as usize != dims[1] {
        return Err(CoreError::Other(
            "The series has changed since it was opened, so these segments no longer fit it".into(),
        ));
    }
    let earlier = saved_for_series(db, paths, study_id, &source.series_uid)?;
    let series_number = earlier.first().map_or(SERIES_NUMBER, |s| s.peek.series_number);
    let instance_number = earlier.iter().map(|s| s.peek.instance_number).max().unwrap_or(0) + 1;
    let options = SegOptions {
        series_uid: earlier.first().map(|s| s.peek.series_uid.clone()),
        series_number,
        instance_number,
        series_description: SERIES_DESCRIPTION.to_string(),
        creator: creator.trim().to_string(),
        software_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let folder = paths
        .temp_dir()
        .join(format!("seg-{}", &ruthos_dicom::sc::new_uid()[5..17]));
    let written = write_seg(&folder, &source, segments, &options)?;
    let imported = import_file(db, paths, &written.path, ImportMode::Copy);
    let _ = std::fs::remove_dir_all(&folder);
    imported?;
    Ok(written)
}

/// What a report says of the study's saved segments: for each series, what is in
/// its newest SEG, with its volume and how it was made.
pub fn report_rows(db: &Database, paths: &DataPaths, study_id: i64) -> Vec<ReportMeasurement> {
    let saved = saved_in_study(db, paths, study_id).unwrap_or_default();
    let mut seen: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for s in &saved {
        let Some(series) = s.peek.referenced_series.first() else {
            continue;
        };
        if seen.contains(&series.as_str()) {
            continue;
        }
        seen.push(series);
        rows.extend(rows_of(&s.path));
    }
    rows
}

fn rows_of(path: &Path) -> Vec<ReportMeasurement> {
    let file = match read_seg(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("{}: {e}", path.display());
            return Vec::new();
        }
    };
    file.volumes()
        .into_iter()
        .map(|(segment, millilitres)| ReportMeasurement {
            name: segment.label.clone(),
            value: format!("{millilitres:.2} ml"),
            location: segment.algorithm.name().unwrap_or("Drawn by hand").to_string(),
            basis: String::new(),
            patient_scale: true,
        })
        .collect()
}
