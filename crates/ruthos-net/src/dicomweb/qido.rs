//! QIDO-RS: searching a node. Results come back in the same types as a C-FIND.

use std::sync::atomic::AtomicBool;

use serde_json::Value;

use super::client::WebClient;
use crate::scu::{SeriesResult, StudyQuery, StudyResult};
use crate::NetError;

/// A search reply is abandoned past this, whatever it claims.
pub const MOST_REPLY_BYTES: usize = 64 * 1024 * 1024;
/// For servers that ignore `limit`.
pub const MOST_RESULTS: usize = 1000;

const DICOM_JSON: &str = "application/dicom+json";

/// The fields the study list shows, for servers without `includefield=all`.
const STUDY_FIELDS: [&str; 7] = [
    "00080030", // Study Time
    "00081030", // Study Description
    "00080090", // Referring Physician
    "00100030", // Birth Date
    "00100040", // Sex
    "00102201", // Species
    "00201206", // Series in Study
];
const INSTANCES_IN_STUDY: &str = "00201208";
const SERIES_FIELDS: [&str; 2] = ["0008103E", "00201209"];

/// What a study search found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub studies: Vec<StudyResult>,
    /// There was more than could be shown: narrow the search.
    pub stopped_early: bool,
}

/// One instance of a series, for finding what is missing.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceResult {
    pub sop_instance_uid: String,
}

fn values<'a>(item: &'a Value, tag: &str) -> Option<&'a Vec<Value>> {
    item.get(tag)?.get("Value")?.as_array()
}

/// The first value of an attribute as text, whatever its VR.
pub(super) fn text(item: &Value, tag: &str) -> String {
    let Some(first) = values(item, tag).and_then(|v| v.first()) else {
        return String::new();
    };
    match first {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Object(person) => person
            .get("Alphabetic")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn number(item: &Value, tag: &str) -> Option<i64> {
    text(item, tag).trim().parse().ok()
}

/// All the values of a text attribute, in the `\` form a C-FIND returns.
fn joined(item: &Value, tag: &str) -> String {
    let Some(all) = values(item, tag) else {
        return String::new();
    };
    let parts: Vec<&str> = all.iter().filter_map(Value::as_str).collect();
    parts.join("\\")
}

fn study_of(item: &Value) -> StudyResult {
    StudyResult {
        study_uid: text(item, "0020000D"),
        patient_name: text(item, "00100010"),
        patient_id: text(item, "00100020"),
        birth_date: text(item, "00100030"),
        sex: text(item, "00100040"),
        species: text(item, "00102201"),
        study_date: text(item, "00080020"),
        study_time: text(item, "00080030"),
        accession: text(item, "00080050"),
        description: text(item, "00081030"),
        modalities: joined(item, "00080061"),
        referring: text(item, "00080090"),
        series: number(item, "00201206").map(|n| n as u32),
        instances: number(item, INSTANCES_IN_STUDY).map(|n| n as u32),
    }
}

/// The window's fields as query parameters.
pub fn study_parameters(q: &StudyQuery, name: &str, id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut put = |key: &str, value: &str| {
        if !value.trim().is_empty() {
            out.push((key.to_string(), value.trim().to_string()));
        }
    };
    put("PatientName", name);
    put("PatientID", id);
    put("AccessionNumber", &q.accession);
    put("StudyDate", &q.study_date);
    put("ModalitiesInStudy", &q.modality);
    put("StudyDescription", &q.description);
    put("PatientSpeciesDescription", &q.species);
    put("PatientBreedDescription", &q.breed);
    put("ResponsiblePerson", &q.responsible);
    put("StudyInstanceUID", &q.study_uid);
    if !name.trim().is_empty() {
        out.push(("fuzzymatching".to_string(), "false".to_string()));
    }
    out
}

impl WebClient {
    fn field_parameters(&self, fields: &[&str]) -> Vec<(String, String)> {
        if self.node.include_all == Some(true) {
            return vec![("includefield".to_string(), "all".to_string())];
        }
        fields
            .iter()
            .map(|f| ("includefield".to_string(), (*f).to_string()))
            .collect()
    }

    /// One QIDO-RS page as JSON items.
    pub(super) async fn page(
        &self,
        url: &str,
        parameters: &[(String, String)],
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, NetError> {
        let response = self
            .send(|http| http.get(url).query(parameters).header("Accept", DICOM_JSON))
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            return Err(NetError::NotDicomWeb(format!("the server answered {status}")));
        }
        let is_json = response
            .headers()
            .get("content-type")
            .and_then(|c| c.to_str().ok())
            .is_some_and(|c| c.to_ascii_lowercase().contains("json"));
        if !is_json {
            return Err(self.not_dicom(&response, DICOM_JSON));
        }
        let body = self.read_limited(response, MOST_REPLY_BYTES, cancel).await?;
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Array(items)) => Ok(items),
            _ => Err(NetError::NotDicomWeb("the reply is not DICOM JSON".into())),
        }
    }

    /// Every page of a search, up to `MOST_RESULTS`.
    async fn all_pages(
        &self,
        url: &str,
        parameters: Vec<(String, String)>,
        key: &str,
        cancel: &AtomicBool,
    ) -> Result<(Vec<Value>, bool), NetError> {
        let limit = self.node.page_size.clamp(1, MOST_RESULTS as u32);
        let mut all: Vec<Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut query = parameters.clone();
            query.push(("limit".to_string(), limit.to_string()));
            query.push(("offset".to_string(), all.len().to_string()));
            let items = self.page(url, &query, cancel).await?;
            let count = items.len();
            let mut fresh = 0;
            for item in items {
                // A server that ignores `offset` sends the same page again.
                if seen.insert(text(&item, key)) {
                    fresh += 1;
                    all.push(item);
                }
            }
            if all.len() > MOST_RESULTS {
                all.truncate(MOST_RESULTS);
                return Ok((all, true));
            }
            if fresh == 0 || count < limit as usize {
                return Ok((all, false));
            }
        }
    }

    /// Search for studies. A patient box is both a name and an ID, as in C-FIND.
    pub fn find_studies(&self, q: &StudyQuery, cancel: &AtomicBool) -> Result<Found, NetError> {
        self.run(async {
            let url = format!("{}/studies", self.node.qido);
            let mut fields = self.field_parameters(&STUDY_FIELDS);
            if self.node.include_all != Some(true) {
                fields.push(("includefield".to_string(), INSTANCES_IN_STUDY.to_string()));
            }
            let search = |name: String, id: String| {
                let mut parameters = study_parameters(q, &name, &id);
                parameters.extend(fields.iter().cloned());
                parameters
            };
            let text_box = q.patient.trim();
            let name = match text_box {
                "" => String::new(),
                t if t.contains('*') => t.to_string(),
                t => format!("*{t}*"),
            };
            let (items, mut stopped) = self
                .all_pages(&url, search(name, String::new()), "0020000D", cancel)
                .await?;
            let mut studies: Vec<StudyResult> = items.iter().map(study_of).collect();
            if !text_box.is_empty() && !text_box.contains(' ') {
                let (by_id, more) = self
                    .all_pages(&url, search(String::new(), text_box.to_string()), "0020000D", cancel)
                    .await?;
                stopped |= more;
                for item in &by_id {
                    let study = study_of(item);
                    if !studies.iter().any(|s| s.study_uid == study.study_uid) {
                        studies.push(study);
                    }
                }
            }
            studies.sort_by(|a, b| {
                (b.study_date.as_str(), b.study_time.as_str()).cmp(&(a.study_date.as_str(), a.study_time.as_str()))
            });
            Ok(Found {
                studies,
                stopped_early: stopped,
            })
        })
    }

    pub(super) async fn series_of(&self, study: &str, cancel: &AtomicBool) -> Result<Vec<SeriesResult>, NetError> {
        let url = format!("{}/studies/{study}/series", self.node.qido);
        let (items, _) = self
            .all_pages(&url, self.field_parameters(&SERIES_FIELDS), "0020000E", cancel)
            .await?;
        let mut series: Vec<SeriesResult> = items
            .iter()
            .map(|item| SeriesResult {
                series_uid: text(item, "0020000E"),
                number: number(item, "00200011"),
                modality: text(item, "00080060"),
                description: text(item, "0008103E"),
                instances: number(item, "00201209").map(|n| n as u32),
            })
            .collect();
        series.sort_by_key(|s| s.number.unwrap_or(i64::MAX));
        Ok(series)
    }

    /// The series of a study, in series order.
    pub fn find_series(&self, study: &str, cancel: &AtomicBool) -> Result<Vec<SeriesResult>, NetError> {
        self.run(self.series_of(study, cancel))
    }

    pub(super) async fn instances_of(
        &self,
        study: &str,
        series: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<InstanceResult>, NetError> {
        let url = format!("{}/studies/{study}/series/{series}/instances", self.node.qido);
        let (items, _) = self.all_pages(&url, Vec::new(), "00080018", cancel).await?;
        Ok(items
            .iter()
            .map(|item| InstanceResult {
                sop_instance_uid: text(item, "00080018"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dicom_json_reads_into_a_study() {
        let item = json!({
            "0020000D": {"vr": "UI", "Value": ["1.2.3"]},
            "00100010": {"vr": "PN", "Value": [{"Alphabetic": "SMITH^Bella"}]},
            "00080020": {"vr": "DA", "Value": ["20261105"]},
            "00080061": {"vr": "CS", "Value": ["CT", "SEG"]},
            "00201206": {"vr": "IS", "Value": [2]},
            "00201208": {"vr": "IS", "Value": ["530"]},
            "00081030": {"vr": "LO"},
        });
        let study = study_of(&item);
        assert_eq!(study.study_uid, "1.2.3");
        assert_eq!(study.patient_name, "SMITH^Bella");
        assert_eq!(study.modalities, "CT\\SEG");
        assert_eq!(study.series, Some(2));
        assert_eq!(study.instances, Some(530));
        assert_eq!(study.description, "");
    }

    #[test]
    fn the_window_becomes_parameters() {
        let q = StudyQuery {
            accession: "A1".into(),
            study_date: "20260101-20261231".into(),
            modality: "CT".into(),
            species: "Canine".into(),
            ..StudyQuery::default()
        };
        let p = study_parameters(&q, "*bella*", "");
        let has = |k: &str, v: &str| p.iter().any(|(a, b)| a == k && b == v);
        assert!(has("PatientName", "*bella*"));
        assert!(has("fuzzymatching", "false"));
        assert!(has("AccessionNumber", "A1"));
        assert!(has("StudyDate", "20260101-20261231"));
        assert!(has("ModalitiesInStudy", "CT"));
        assert!(has("PatientSpeciesDescription", "Canine"));
        assert!(!p.iter().any(|(k, _)| k == "ResponsiblePerson" || k == "PatientID"));
    }
}
