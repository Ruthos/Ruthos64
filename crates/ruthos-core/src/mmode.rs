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
/// The same LV line drawn on a 2D image, where diastole and systole are
/// different frames of the loop.
pub const KIND_2D_D: &str = "lv2d-d";
pub const KIND_2D_S: &str = "lv2d-s";

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
    /// Frame of the image the line was drawn on.
    pub frame: i64,
    /// The line's time column, in image pixels (M-mode lines).
    pub x: f32,
    /// Drawn on a 2D image rather than in an M-mode strip.
    pub two_d: bool,
}

impl MmodeRow {
    pub fn location(&self) -> String {
        let mut text = match self.instance_number {
            Some(n) => format!("{}, image {n}", self.series_label),
            None => self.series_label.clone(),
        };
        if self.two_d {
            text.push_str(&format!(", frame {}", self.frame + 1));
        }
        text
    }

    pub fn value(&self, key: &str) -> Option<f64> {
        self.values.iter().find(|v| v.def_key == key).and_then(|v| v.value)
    }
}

/// Fractional shortening in percent: (LVIDd − LVIDs) / LVIDd × 100.
pub fn fractional_shortening(lvidd_mm: f64, lvids_mm: f64) -> Option<f64> {
    (lvidd_mm > 0.0).then(|| (lvidd_mm - lvids_mm) / lvidd_mm * 100.0)
}

/// Pair lines drawn on one frame into cardiac cycles. `lines` are
/// (phase, time column); the result lists, for each set, the index of its
/// diastolic and systolic line. Cycles run left to right, so each systole
/// takes the nearest unpaired diastole to its left, or failing that the
/// nearest unpaired diastole anywhere; lines left over form sets of one.
/// Sets come out in time order.
pub fn pair_on_frame(lines: &[(char, f32)]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|a, b| lines[*a].1.total_cmp(&lines[*b].1));
    let mut used = vec![false; lines.len()];
    let mut sets: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    for &si in order.iter().filter(|&&i| lines[i].0 == 's') {
        let sx = lines[si].1;
        let left = order
            .iter()
            .rev()
            .copied()
            .find(|&d| lines[d].0 != 's' && !used[d] && lines[d].1 <= sx);
        let any = || {
            order
                .iter()
                .copied()
                .filter(|&d| lines[d].0 != 's' && !used[d])
                .min_by(|a, b| (lines[*a].1 - sx).abs().total_cmp(&(lines[*b].1 - sx).abs()))
        };
        let d = left.or_else(any);
        if let Some(d) = d {
            used[d] = true;
        }
        used[si] = true;
        sets.push((d, Some(si)));
    }
    for &i in &order {
        if !used[i] {
            sets.push((Some(i), None));
        }
    }
    let key = |set: &(Option<usize>, Option<usize>)| set.0.or(set.1).map(|i| lines[i].1).unwrap_or(0.0);
    sets.sort_by(|a, b| key(a).total_cmp(&key(b)));
    sets
}

/// One cardiac cycle's measurements: a diastolic line, a systolic line or
/// both, and the fractional shortening when both are there.
#[derive(Debug, Clone, PartialEq)]
pub struct MmodeSet {
    pub series_id: i64,
    pub series_label: String,
    pub diastole: Option<MmodeRow>,
    pub systole: Option<MmodeRow>,
    pub fs_percent: Option<f64>,
}

impl MmodeSet {
    pub fn location(&self) -> String {
        self.diastole
            .as_ref()
            .or(self.systole.as_ref())
            .map(|r| r.location())
            .unwrap_or_default()
    }

    /// The value of a key from whichever line carries it.
    pub fn value(&self, key: &str) -> Option<f64> {
        self.diastole
            .as_ref()
            .and_then(|r| r.value(key))
            .or_else(|| self.systole.as_ref().and_then(|r| r.value(key)))
    }
}

/// Every set of a study or series, ordered by series, image and time.
/// M-mode lines are paired within their frame by time column; 2D lines are
/// paired within their loop by frame number, since end-diastole and
/// end-systole are different frames (`pair_on_frame` either way).
pub fn sets_from_rows(rows: Vec<MmodeRow>) -> Vec<MmodeSet> {
    let mut groups: Vec<((i64, i64), Vec<MmodeRow>)> = Vec::new();
    for mut row in rows {
        if row.two_d {
            row.x = row.frame as f32;
        }
        let k = (row.image_id, if row.two_d { -1 } else { row.frame });
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(row),
            None => groups.push((k, vec![row])),
        }
    }
    groups.sort_by_key(|(_, v)| {
        (
            v[0].series_id,
            v[0].instance_number.unwrap_or(0),
            v[0].image_id,
            v[0].frame,
        )
    });
    let mut out = Vec::new();
    for (_, lines) in groups {
        let keys: Vec<(char, f32)> = lines.iter().map(|l| (l.phase, l.x)).collect();
        for (d, s) in pair_on_frame(&keys) {
            let diastole = d.map(|i| lines[i].clone());
            let systole = s.map(|i| lines[i].clone());
            let first = diastole.as_ref().or(systole.as_ref()).unwrap();
            let fs_percent = match (
                diastole.as_ref().and_then(|r| r.value("lv.lvidd")),
                systole.as_ref().and_then(|r| r.value("lv.lvids")),
            ) {
                (Some(dd), Some(ss)) => fractional_shortening(dd, ss),
                _ => None,
            };
            out.push(MmodeSet {
                series_id: first.series_id,
                series_label: first.series_label.clone(),
                diastole,
                systole,
                fs_percent,
            });
        }
    }
    out
}

/// A mean over several sets: each key's mean and how many sets had it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MmodeMean {
    pub values: Vec<(String, f64, usize)>,
    pub fs_percent: Option<(f64, usize)>,
}

pub fn mean_of_sets(sets: &[MmodeSet]) -> MmodeMean {
    let mut out = MmodeMean::default();
    for key in KEYS_D.iter().chain(KEYS_S.iter()) {
        let vals: Vec<f64> = sets.iter().filter_map(|s| s.value(key)).collect();
        if !vals.is_empty() {
            out.values.push((
                key.to_string(),
                vals.iter().sum::<f64>() / vals.len() as f64,
                vals.len(),
            ));
        }
    }
    let fs: Vec<f64> = sets.iter().filter_map(|s| s.fs_percent).collect();
    if !fs.is_empty() {
        out.fs_percent = Some((fs.iter().sum::<f64>() / fs.len() as f64, fs.len()));
    }
    out
}

/// The latest diastolic and systolic lines of one series (most recently
/// edited), for choosing the phase of a new line on a frame without any.
pub fn latest_per_series(rows: &[MmodeRow], series_id: i64) -> (Option<&MmodeRow>, Option<&MmodeRow>) {
    let newest = |phase: char| {
        rows.iter()
            .filter(|r| r.series_id == series_id && (r.phase == 's') == (phase == 's'))
            .max_by_key(|r| (r.date_modified, r.roi_id))
    };
    (newest('d'), newest('s'))
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

    fn row(id: i64, image: i64, phase: char, x: f32, lvid: f64) -> MmodeRow {
        let keys = if phase == 'd' { KEYS_D } else { KEYS_S };
        MmodeRow {
            roi_id: id,
            image_id: image,
            series_id: 7,
            series_label: "US 7".into(),
            instance_number: Some(image),
            phase,
            date_modified: id,
            frame: 0,
            x,
            two_d: false,
            values: vec![
                RoiValue {
                    def_key: keys[0].into(),
                    value: Some(10.0),
                    unit: "mm".into(),
                    basis: Some("us-region".into()),
                },
                RoiValue {
                    def_key: keys[1].into(),
                    value: Some(lvid),
                    unit: "mm".into(),
                    basis: Some("us-region".into()),
                },
                RoiValue {
                    def_key: keys[2].into(),
                    value: Some(9.0),
                    unit: "mm".into(),
                    basis: Some("us-region".into()),
                },
            ],
        }
    }

    #[test]
    fn cycles_pair_left_to_right_on_a_frame() {
        // Two cycles drawn out of order: D at 100, S at 180, D at 300, S at 390.
        let lines = [('s', 390.0), ('d', 100.0), ('d', 300.0), ('s', 180.0)];
        let sets = pair_on_frame(&lines);
        assert_eq!(sets, vec![(Some(1), Some(3)), (Some(2), Some(0))]);
        // A diastole without its systole is a set of one.
        let sets = pair_on_frame(&[('d', 10.0), ('s', 50.0), ('d', 90.0)]);
        assert_eq!(sets, vec![(Some(0), Some(1)), (Some(2), None)]);
        // A systole left of every diastole still pairs with the nearest.
        assert_eq!(pair_on_frame(&[('d', 200.0), ('s', 50.0)]), vec![(Some(0), Some(1))]);
    }

    #[test]
    fn several_sets_per_image_and_their_mean() {
        let rows = vec![
            row(1, 14, 'd', 100.0, 50.0),
            row(2, 14, 's', 180.0, 30.0),
            row(3, 14, 'd', 300.0, 48.0),
            row(4, 14, 's', 390.0, 32.0),
            row(5, 15, 'd', 120.0, 52.0),
        ];
        let sets = sets_from_rows(rows);
        assert_eq!(sets.len(), 3);
        assert!((sets[0].fs_percent.unwrap() - 40.0).abs() < 1e-9);
        assert!((sets[1].fs_percent.unwrap() - (48.0 - 32.0) / 48.0 * 100.0).abs() < 1e-9);
        assert_eq!(sets[2].fs_percent, None, "a diastole alone has no FS");
        let mean = mean_of_sets(&sets);
        let lvidd = mean.values.iter().find(|(k, _, _)| k == "lv.lvidd").unwrap();
        assert!((lvidd.1 - 50.0).abs() < 1e-9 && lvidd.2 == 3);
        let (fs, n) = mean.fs_percent.unwrap();
        assert_eq!(n, 2);
        assert!((fs - (40.0 + 100.0 / 3.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn two_d_lines_pair_across_frames_of_a_loop() {
        let mut d = row(1, 20, 'd', 0.0, 50.0);
        d.two_d = true;
        d.frame = 3;
        let mut s = row(2, 20, 's', 0.0, 30.0);
        s.two_d = true;
        s.frame = 17;
        let sets = sets_from_rows(vec![s, d]);
        assert_eq!(sets.len(), 1, "diastole on frame 4 pairs with systole on frame 18");
        assert!((sets[0].fs_percent.unwrap() - 40.0).abs() < 1e-9);
        assert_eq!(sets[0].location(), "US 7, image 20, frame 4");
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
