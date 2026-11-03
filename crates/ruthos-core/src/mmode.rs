//! Left-ventricular M-mode: the two lines (diastole and systole), the six
//! wall and cavity values they give, and fractional shortening from the
//! pair. The viewer stores each line as a `roi` row of kind `lvmmode-d` or
//! `lvmmode-s` with its three values in `roi_value`; this module pairs them
//! for display and reports.

use crate::model::RoiValue;

/// Definition keys of the diastolic values, in depth order.
pub const KEYS_D: [&str; 3] = ["lv.ivsd", "lv.lvidd", "lv.lvfwd"];
/// Definition keys of the systolic values, in depth order.
pub const KEYS_S: [&str; 3] = ["lv.ivss", "lv.lvids", "lv.lvfws"];
pub const LABELS_D: [&str; 3] = ["IVSd", "LVIDd", "LVFWd"];
pub const LABELS_S: [&str; 3] = ["IVSs", "LVIDs", "LVFWs"];
pub const KIND_D: &str = "lvmmode-d";
pub const KIND_S: &str = "lvmmode-s";

/// One stored M-mode line.
#[derive(Debug, Clone, PartialEq)]
pub struct MmodeRow {
    pub roi_id: i64,
    pub image_id: i64,
    pub series_id: i64,
    pub series_label: String,
    pub instance_number: Option<i64>,
    /// 'd' or 's'.
    pub phase: char,
    pub date_modified: i64,
    pub values: Vec<RoiValue>,
}

impl MmodeRow {
    pub fn location(&self) -> String {
        match self.instance_number {
            Some(n) => format!("{}, image {n}", self.series_label),
            None => self.series_label.clone(),
        }
    }

    pub fn value(&self, key: &str) -> Option<f64> {
        self.values.iter().find(|v| v.def_key == key).and_then(|v| v.value)
    }
}

/// The latest diastolic and systolic lines of one series, and the
/// fractional shortening they give.
#[derive(Debug, Clone, PartialEq)]
pub struct MmodePair {
    pub series_id: i64,
    pub series_label: String,
    pub diastole: Option<MmodeRow>,
    pub systole: Option<MmodeRow>,
    pub fs_percent: Option<f64>,
}

/// Fractional shortening in percent: (LVIDd − LVIDs) / LVIDd × 100.
pub fn fractional_shortening(lvidd_mm: f64, lvids_mm: f64) -> Option<f64> {
    (lvidd_mm > 0.0).then(|| (lvidd_mm - lvids_mm) / lvidd_mm * 100.0)
}

/// Pair the lines of each series: the most recently edited diastole with
/// the most recently edited systole. Series keep their first-seen order.
pub fn pair_rows(rows: Vec<MmodeRow>) -> Vec<MmodePair> {
    let mut pairs: Vec<MmodePair> = Vec::new();
    for row in rows {
        let pair = match pairs.iter_mut().find(|p| p.series_id == row.series_id) {
            Some(p) => p,
            None => {
                pairs.push(MmodePair { series_id: row.series_id, series_label: row.series_label.clone(), diastole: None, systole: None, fs_percent: None });
                pairs.last_mut().unwrap()
            }
        };
        let slot = if row.phase == 's' { &mut pair.systole } else { &mut pair.diastole };
        let newer = match slot {
            Some(cur) => (row.date_modified, row.roi_id) > (cur.date_modified, cur.roi_id),
            None => true,
        };
        if newer {
            *slot = Some(row);
        }
    }
    for p in &mut pairs {
        p.fs_percent = match (&p.diastole, &p.systole) {
            (Some(d), Some(s)) => match (d.value("lv.lvidd"), s.value("lv.lvids")) {
                (Some(dd), Some(ss)) => fractional_shortening(dd, ss),
                _ => None,
            },
            _ => None,
        };
    }
    pairs
}

/// An echo length in the chosen unit: "4.21 cm" or "42.1 mm".
pub fn echo_value(mm: f64, cm: bool) -> String {
    if cm {
        format!("{:.2} cm", mm / 10.0)
    } else {
        format!("{mm:.1} mm")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, series: i64, phase: char, when: i64, lvid: f64) -> MmodeRow {
        let keys = if phase == 'd' { KEYS_D } else { KEYS_S };
        MmodeRow {
            roi_id: id,
            image_id: 1,
            series_id: series,
            series_label: format!("US {series}"),
            instance_number: Some(1),
            phase,
            date_modified: when,
            values: vec![
                RoiValue { def_key: keys[0].into(), value: Some(10.0), unit: "mm".into(), basis: Some("ultrasound-region".into()) },
                RoiValue { def_key: keys[1].into(), value: Some(lvid), unit: "mm".into(), basis: Some("ultrasound-region".into()) },
                RoiValue { def_key: keys[2].into(), value: Some(9.0), unit: "mm".into(), basis: Some("ultrasound-region".into()) },
            ],
        }
    }

    #[test]
    fn pairs_latest_lines_per_series_and_computes_fs() {
        let rows = vec![row(1, 7, 'd', 100, 48.0), row(2, 7, 's', 110, 30.0), row(3, 7, 'd', 120, 50.0), row(4, 8, 'd', 50, 40.0)];
        let pairs = pair_rows(rows);
        assert_eq!(pairs.len(), 2);
        let p = &pairs[0];
        assert_eq!(p.diastole.as_ref().unwrap().roi_id, 3, "the most recently edited diastole wins");
        assert_eq!(p.systole.as_ref().unwrap().roi_id, 2);
        assert!((p.fs_percent.unwrap() - 40.0).abs() < 1e-9);
        assert_eq!(pairs[1].fs_percent, None, "no systole, no FS");
    }

    #[test]
    fn fs_needs_a_positive_diastolic_diameter() {
        assert_eq!(fractional_shortening(0.0, 1.0), None);
        assert!((fractional_shortening(50.0, 30.0).unwrap() - 40.0).abs() < 1e-9);
    }

    #[test]
    fn echo_units() {
        assert_eq!(echo_value(42.1, false), "42.1 mm");
        assert_eq!(echo_value(42.1, true), "4.21 cm");
    }
}
