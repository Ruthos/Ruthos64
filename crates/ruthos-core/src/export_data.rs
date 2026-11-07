//! Measurements, calculations and reports as CSV or JSON, for research, audit
//! and teaching. One row for each value, so a spreadsheet can filter and pivot
//! it without knowing Ruthos.
//!
//! Without identifiers the export leaves out names, IDs, dates, UIDs and the
//! report's free text, and keeps species, breed, sex and age in days. Study and
//! series descriptions are kept as written.

use std::path::Path;

use serde::Serialize;

use crate::db::Database;
use crate::measure::{study_calculations, Library};
use crate::Result;

/// A measurement as stored, with where it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedRoi {
    pub roi_id: i64,
    pub kind: String,
    pub name: Option<String>,
    pub def_key: Option<String>,
    pub value: Option<f64>,
    pub unit: String,
    pub basis: Option<String>,
    pub frame: i64,
    pub instance_number: Option<i64>,
    pub modality: String,
    pub series_number: Option<i64>,
    pub series_description: String,
    pub series_uid: String,
    pub sop_instance_uid: String,
    /// Values worked out from it, such as the LV dimensions of one line.
    pub derived: Vec<crate::model::RoiValue>,
}

/// Everything written, as JSON gives it.
#[derive(Debug, Clone, Serialize)]
pub struct DataExport {
    pub software: String,
    pub exported: String,
    /// False when names, IDs, dates, UIDs and report text were left out.
    pub identifiers: bool,
    pub studies: Vec<StudyExport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StudyExport {
    /// Numbered from 1 in this export, to tell studies apart without identifiers.
    pub case: usize,
    pub patient: Patient,
    pub study: Study,
    pub values: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ReportText>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Patient {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub birth_date: Option<String>,
    pub species: String,
    pub breed: String,
    pub sex: String,
    pub age_days: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Study {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accession: Option<String>,
    pub description: String,
    pub modalities: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportText {
    pub findings: String,
    pub conclusion: String,
}

/// One number: a measurement, a value derived from one, or a calculation.
#[derive(Debug, Clone, Serialize)]
pub struct Value {
    /// `measurement`, `derived` or `calculation`.
    pub kind: &'static str,
    pub name: String,
    pub key: String,
    pub value: Option<f64>,
    pub unit: String,
    /// What the scale came from, in words.
    pub basis: String,
    /// False when the scale is not the patient's, so the value is not true size.
    pub patient_scale: bool,
    /// For a calculation, the definitions it used, each with how many values were averaged.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub inputs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Place>,
}

/// Where a measurement was taken.
#[derive(Debug, Clone, Serialize)]
pub struct Place {
    pub roi_id: i64,
    pub tool: String,
    pub modality: String,
    pub series_number: Option<i64>,
    pub series_description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sop_instance_uid: Option<String>,
    pub instance_number: Option<i64>,
    pub frame: i64,
}

fn basis_words(basis: Option<&str>) -> (String, bool) {
    match basis.and_then(ruthos_dicom::SpacingBasis::from_key) {
        Some(b) => (b.label().to_string(), b.is_patient_scale()),
        None => (String::new(), true),
    }
}

/// Gather the given studies, in that order.
pub fn gather(db: &Database, library: &Library, study_ids: &[i64], identifiers: bool) -> Result<DataExport> {
    let keep = |value: &str| (identifiers && !value.is_empty()).then(|| value.to_string());
    let mut studies = Vec::new();
    for &study_id in study_ids {
        let Some(s) = db.study(study_id)? else { continue };
        let mut values = Vec::new();
        for roi in db.export_rois(study_id)? {
            let place = Place {
                roi_id: roi.roi_id,
                tool: roi.kind.clone(),
                modality: roi.modality.clone(),
                series_number: roi.series_number,
                series_description: roi.series_description.clone(),
                series_uid: keep(&roi.series_uid),
                sop_instance_uid: keep(&roi.sop_instance_uid),
                instance_number: roi.instance_number,
                frame: roi.frame,
            };
            if roi.value.is_some() {
                let (basis, patient_scale) = basis_words(roi.basis.as_deref());
                values.push(Value {
                    kind: "measurement",
                    name: roi.name.clone().unwrap_or_default(),
                    key: roi.def_key.clone().unwrap_or_default(),
                    value: roi.value,
                    unit: roi.unit.clone(),
                    basis,
                    patient_scale,
                    inputs: String::new(),
                    at: Some(place.clone()),
                });
            }
            for d in &roi.derived {
                let (basis, patient_scale) = basis_words(d.basis.as_deref());
                values.push(Value {
                    kind: "derived",
                    name: library.label(&d.def_key),
                    key: d.def_key.clone(),
                    value: d.value,
                    unit: d.unit.clone(),
                    basis,
                    patient_scale,
                    inputs: String::new(),
                    at: Some(place.clone()),
                });
            }
        }
        let inputs = db.measurement_inputs_for_study(study_id)?;
        let has_mmode = !db.mmode_rows_for_study(study_id)?.is_empty();
        let (results, counts) = study_calculations(library, &inputs, has_mmode);
        for r in results {
            values.push(Value {
                kind: "calculation",
                name: r.label.clone(),
                key: r.key.clone(),
                value: Some(r.value),
                unit: r.unit.clone(),
                basis: if r.scale_independent {
                    "Scale independent".into()
                } else {
                    String::new()
                },
                patient_scale: r.trustworthy_scale,
                inputs: r
                    .inputs
                    .iter()
                    .map(|k| format!("{k} x{}", counts.get(k).copied().unwrap_or(1)))
                    .collect::<Vec<_>>()
                    .join("; "),
                at: None,
            });
        }
        let report = db
            .report(study_id)?
            .filter(|r| identifiers && !(r.findings.trim().is_empty() && r.conclusion.trim().is_empty()))
            .map(|r| ReportText {
                findings: r.findings,
                conclusion: r.conclusion,
            });
        studies.push(StudyExport {
            case: studies.len() + 1,
            patient: Patient {
                name: keep(&s.patient_name),
                id: keep(&s.patient_id),
                birth_date: s.patient_birth_date.as_deref().and_then(keep),
                species: s.patient_species.clone().unwrap_or_default(),
                breed: s.patient_breed.clone().unwrap_or_default(),
                sex: s.patient_sex.clone(),
                age_days: s.age_in_days(),
            },
            study: Study {
                uid: keep(&s.study_instance_uid),
                date: s.study_date.as_deref().and_then(keep),
                accession: keep(&s.accession_number),
                description: s.study_description.clone(),
                modalities: s.modalities.replace('\\', " "),
            },
            values,
            report,
        });
    }
    Ok(DataExport {
        software: format!("Ruthos {}", env!("CARGO_PKG_VERSION")),
        exported: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        identifiers,
        studies,
    })
}

impl DataExport {
    pub fn value_count(&self) -> usize {
        self.studies.iter().map(|s| s.values.len()).sum()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// One row a value, and one a report section, under a header row.
    pub fn to_csv(&self) -> String {
        const HEADER: [&str; 27] = [
            "case",
            "patient_name",
            "patient_id",
            "birth_date",
            "species",
            "breed",
            "sex",
            "age_days",
            "study_uid",
            "study_date",
            "accession",
            "study_description",
            "modality",
            "series_number",
            "series_description",
            "series_uid",
            "sop_instance_uid",
            "instance_number",
            "frame",
            "kind",
            "name",
            "key",
            "value",
            "unit",
            "basis",
            "patient_scale",
            "inputs_or_text",
        ];
        // A byte order mark, so Excel reads the file as UTF-8.
        let mut out = String::from("\u{feff}");
        out.push_str(&HEADER.join(","));
        out.push_str("\r\n");
        let number = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
        for s in &self.studies {
            let p = &s.patient;
            let start = [
                s.case.to_string(),
                cell(p.name.as_deref().unwrap_or("")),
                cell(p.id.as_deref().unwrap_or("")),
                cell(p.birth_date.as_deref().unwrap_or("")),
                cell(&p.species),
                cell(&p.breed),
                cell(&p.sex),
                number(p.age_days),
                cell(s.study.uid.as_deref().unwrap_or("")),
                cell(s.study.date.as_deref().unwrap_or("")),
                cell(s.study.accession.as_deref().unwrap_or("")),
                cell(&s.study.description),
            ];
            for v in &s.values {
                let at = v.at.as_ref();
                let row: Vec<String> = start
                    .iter()
                    .cloned()
                    .chain([
                        cell(at.map(|a| a.modality.as_str()).unwrap_or("")),
                        number(at.and_then(|a| a.series_number)),
                        cell(at.map(|a| a.series_description.as_str()).unwrap_or("")),
                        cell(at.and_then(|a| a.series_uid.as_deref()).unwrap_or("")),
                        cell(at.and_then(|a| a.sop_instance_uid.as_deref()).unwrap_or("")),
                        number(at.and_then(|a| a.instance_number)),
                        number(at.map(|a| a.frame)),
                        v.kind.to_string(),
                        cell(&v.name),
                        cell(&v.key),
                        v.value.map(|x| x.to_string()).unwrap_or_default(),
                        cell(&v.unit),
                        cell(&v.basis),
                        v.patient_scale.to_string(),
                        cell(&v.inputs),
                    ])
                    .collect();
                out.push_str(&row.join(","));
                out.push_str("\r\n");
            }
            if let Some(report) = &s.report {
                for (kind, text) in [("findings", &report.findings), ("conclusion", &report.conclusion)] {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let mut row: Vec<String> = start.to_vec();
                    row.extend(std::iter::repeat_n(String::new(), 7));
                    row.push(kind.to_string());
                    row.extend(std::iter::repeat_n(String::new(), 6));
                    row.push(cell(text));
                    out.push_str(&row.join(","));
                    out.push_str("\r\n");
                }
            }
        }
        out
    }

    /// Write as CSV, or as JSON when the file name ends `.json`.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        std::fs::write(path, if json { self.to_json() } else { self.to_csv() })
    }
}

/// A text cell: quoted when it must be, and never read as a formula.
fn cell(text: &str) -> String {
    let mut text = text.to_string();
    // A spreadsheet runs a cell that starts like this as a formula.
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        text.insert(0, '\'');
    }
    if text.contains([',', '"', '\n', '\r']) || text.starts_with(' ') || text.ends_with(' ') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_is_quoted_and_never_a_formula() {
        assert_eq!(cell("plain"), "plain");
        assert_eq!(cell("a, b"), "\"a, b\"");
        assert_eq!(cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(cell("two\nlines"), "\"two\nlines\"");
        assert_eq!(cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(cell("+1"), "'+1");
        assert_eq!(cell("@SUM"), "'@SUM");
    }
}
