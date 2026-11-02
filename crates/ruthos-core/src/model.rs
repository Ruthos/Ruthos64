//! Row types returned by the database.
//!
//! The field set follows the Horos Core Data model (DicomStudy, DicomSeries,
//! DicomImage, DicomAlbum) so a Horos database can be migrated one to one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct StudyRow {
    pub id: i64,
    pub study_instance_uid: String,
    pub patient_name: String,
    pub patient_id: String,
    pub patient_birth_date: Option<String>,
    pub patient_sex: String,
    pub patient_species: Option<String>,
    pub patient_breed: Option<String>,
    pub responsible_person: Option<String>,
    /// ISO date `YYYY-MM-DD` when known.
    pub study_date: Option<String>,
    /// `HH:MM:SS` when known.
    pub study_time: Option<String>,
    pub study_description: String,
    pub accession_number: String,
    pub referring_physician: String,
    pub institution_name: String,
    /// Backslash-separated modalities, e.g. `CR\DX`.
    pub modalities: String,
    /// Unix seconds.
    pub date_added: i64,
    pub date_opened: Option<i64>,
    pub comment: String,
    /// Horos "state": 0 empty, 1 unread, 2 reviewed, 3 dictated, 4 validated.
    pub state: i64,
    pub number_of_series: i64,
    pub number_of_images: i64,
}

impl StudyRow {
    /// Age at study date, formatted the way Horos shows it ("4Y", "7M", "12D").
    pub fn age_at_study(&self) -> Option<String> {
        let dob = parse_iso_or_dicom(self.patient_birth_date.as_deref()?)?;
        let study = parse_iso_or_dicom(self.study_date.as_deref()?)?;
        let days = (study - dob).num_days();
        if days < 0 {
            return None;
        }
        if days >= 730 {
            Some(format!("{}Y", days / 365))
        } else if days >= 60 {
            Some(format!("{}M", days / 30))
        } else {
            Some(format!("{days}D"))
        }
    }
}

fn parse_iso_or_dicom(s: &str) -> Option<chrono::NaiveDate> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(
        digits[0..4].parse().ok()?,
        digits[4..6].parse().ok()?,
        digits[6..8].parse().ok()?,
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesRow {
    pub id: i64,
    pub study_id: i64,
    pub series_instance_uid: String,
    pub series_number: Option<i64>,
    pub modality: String,
    pub series_description: String,
    pub series_date: Option<String>,
    pub body_part_examined: String,
    pub view_position: String,
    pub laterality: String,
    pub number_of_images: i64,
    pub number_of_key_images: i64,
    /// Per-series display state Horos remembers between sessions.
    pub window_center: Option<f64>,
    pub window_width: Option<f64>,
    pub rotation_angle: f64,
    pub x_flipped: bool,
    pub y_flipped: bool,
    pub comment: String,
    pub date_added: i64,
}

impl SeriesRow {
    /// What the thumbnail strip and the viewer drawer label a series with.
    pub fn display_name(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.series_description.is_empty() {
            parts.push(self.series_description.clone());
        } else if !self.body_part_examined.is_empty() || !self.view_position.is_empty() {
            let s = format!("{} {}", self.body_part_examined, self.view_position);
            parts.push(s.trim().to_string());
        }
        if parts.is_empty() {
            match self.series_number {
                Some(n) => parts.push(format!("Series {n}")),
                None => parts.push("Series".to_string()),
            }
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRow {
    pub id: i64,
    pub series_id: i64,
    pub sop_instance_uid: String,
    pub sop_class_uid: String,
    pub instance_number: Option<i64>,
    pub number_of_frames: i64,
    pub rows: i64,
    pub columns: i64,
    /// Path as stored: relative to the data root for copied files, absolute
    /// for linked ones.
    pub path: String,
    pub stored_in_database_folder: bool,
    pub file_size: i64,
    pub transfer_syntax: String,
    pub is_key_image: bool,
    pub slice_location: Option<f64>,
    pub comment: String,
    pub date_added: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumRow {
    pub id: i64,
    pub name: String,
    pub is_smart: bool,
    /// For smart albums, a filter expression (see [`crate::db::StudyFilter`]).
    pub predicate: Option<String>,
    pub index: i64,
    pub number_of_studies: i64,
}

/// The date filter popup in the Horos database toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DateFilter {
    #[default]
    Any,
    Today,
    Yesterday,
    LastTwoDays,
    LastWeek,
    LastMonth,
    LastTwoMonths,
    LastThreeMonths,
    LastYear,
}

impl DateFilter {
    pub const ALL: [DateFilter; 9] = [
        DateFilter::Any,
        DateFilter::Today,
        DateFilter::Yesterday,
        DateFilter::LastTwoDays,
        DateFilter::LastWeek,
        DateFilter::LastMonth,
        DateFilter::LastTwoMonths,
        DateFilter::LastThreeMonths,
        DateFilter::LastYear,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DateFilter::Any => "Any Date",
            DateFilter::Today => "Today",
            DateFilter::Yesterday => "Yesterday",
            DateFilter::LastTwoDays => "Last 2 Days",
            DateFilter::LastWeek => "Last 7 Days",
            DateFilter::LastMonth => "Last Month",
            DateFilter::LastTwoMonths => "Last 2 Months",
            DateFilter::LastThreeMonths => "Last 3 Months",
            DateFilter::LastYear => "Last Year",
        }
    }

    /// Inclusive ISO date bounds for today's date.
    pub fn bounds(self, today: chrono::NaiveDate) -> Option<(String, String)> {
        let d = |n: i64| today - chrono::Duration::days(n);
        let iso = |x: chrono::NaiveDate| x.format("%Y-%m-%d").to_string();
        match self {
            DateFilter::Any => None,
            DateFilter::Today => Some((iso(today), iso(today))),
            DateFilter::Yesterday => Some((iso(d(1)), iso(d(1)))),
            DateFilter::LastTwoDays => Some((iso(d(1)), iso(today))),
            DateFilter::LastWeek => Some((iso(d(6)), iso(today))),
            DateFilter::LastMonth => Some((iso(d(30)), iso(today))),
            DateFilter::LastTwoMonths => Some((iso(d(61)), iso(today))),
            DateFilter::LastThreeMonths => Some((iso(d(92)), iso(today))),
            DateFilter::LastYear => Some((iso(d(365)), iso(today))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_formatting() {
        let mut s = StudyRow {
            id: 0,
            study_instance_uid: String::new(),
            patient_name: String::new(),
            patient_id: String::new(),
            patient_birth_date: Some("2020-01-01".into()),
            patient_sex: String::new(),
            patient_species: None,
            patient_breed: None,
            responsible_person: None,
            study_date: Some("2024-06-01".into()),
            study_time: None,
            study_description: String::new(),
            accession_number: String::new(),
            referring_physician: String::new(),
            institution_name: String::new(),
            modalities: String::new(),
            date_added: 0,
            date_opened: None,
            comment: String::new(),
            state: 0,
            number_of_series: 0,
            number_of_images: 0,
        };
        assert_eq!(s.age_at_study().as_deref(), Some("4Y"));
        s.study_date = Some("2020-04-15".into());
        assert_eq!(s.age_at_study().as_deref(), Some("3M"));
        s.study_date = Some("2020-01-20".into());
        assert_eq!(s.age_at_study().as_deref(), Some("19D"));
    }

    #[test]
    fn date_filter_bounds() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        assert_eq!(
            DateFilter::LastWeek.bounds(today),
            Some(("2026-10-27".to_string(), "2026-11-02".to_string()))
        );
        assert_eq!(DateFilter::Any.bounds(today), None);
    }
}

/// A stored ROI or measurement on one frame. `points` are image pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoiRecord {
    pub id: i64,
    pub image_id: i64,
    pub frame: i64,
    /// `length`, `angle`, `rect`, `oval`, `arrow` or `point`.
    pub kind: String,
    pub points: Vec<[f32; 2]>,
    /// The named measurement this ROI was assigned to, e.g. `LVIDd`.
    pub name: Option<String>,
    /// The measured value at the time it was stored, in `unit`.
    pub value: Option<f64>,
    pub unit: String,
}

/// A named measurement with where it was taken, for reports.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementRow {
    pub roi: RoiRecord,
    pub series_id: i64,
    pub series_label: String,
    pub instance_number: Option<i64>,
}

impl MeasurementRow {
    /// "CR 2, image 3" style location text.
    pub fn location(&self) -> String {
        match self.instance_number {
            Some(n) => format!("{}, image {n}", self.series_label),
            None => self.series_label.clone(),
        }
    }
}

/// The text of a study's report.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReportRow {
    pub study_id: i64,
    pub findings: String,
    pub conclusion: String,
    pub date_modified: i64,
}
