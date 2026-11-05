//! Date shifting and age computation.
//!
//! One offset per case is subtracted from every date, including the birth
//! date, so ages and intervals survive while calendar dates become
//! fictitious. Times of day are kept. With "preserve season" the offset is a
//! whole number of years, so day and month stay real.

use chrono::{Datelike, Days, Months, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateShift {
    /// Days subtracted from every date (positive).
    pub offset_days: i64,
    /// Shift by whole years only, keeping the day and month.
    pub preserve_season: bool,
}

impl DateShift {
    pub fn new(offset_days: i64, preserve_season: bool) -> Self {
        Self {
            offset_days: offset_days.abs().max(1),
            preserve_season,
        }
    }

    /// Years subtracted when preserving the season: at least one.
    pub fn years(&self) -> i64 {
        (self.offset_days / 365).max(1)
    }

    /// Shift a DA value (`YYYYMMDD`). Values that are not a date come back
    /// as `None` and must not be copied.
    pub fn shift(&self, da: &str) -> Option<String> {
        let date = parse_da(da)?;
        let shifted = if self.preserve_season {
            date.checked_sub_months(Months::new((self.years() * 12) as u32))?
        } else {
            date.checked_sub_days(Days::new(self.offset_days as u64))?
        };
        Some(shifted.format("%Y%m%d").to_string())
    }

    /// Shift the date part of a DT value (`YYYYMMDDHHMMSS.FFFFFF&ZZXX`),
    /// keeping the time and dropping any timezone suffix.
    pub fn shift_dt(&self, dt: &str) -> Option<String> {
        let dt = dt.trim();
        // A value from a file need not be ASCII, so byte 8 need not be a
        // character boundary.
        let date = self.shift(dt.get(..8)?)?;
        let rest = dt.get(8..)?;
        let rest = match rest.find(['+', '-']) {
            Some(i) => &rest[..i],
            None => rest,
        };
        Some(format!("{date}{rest}"))
    }
}

/// Parse the first eight digits of a DA value.
pub fn parse_da(s: &str) -> Option<NaiveDate> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
    if digits.len() != 8 {
        return None;
    }
    NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

/// Age at `study` for an animal born on `birth`, in the DICOM AS form:
/// years from two years up, months from two months up, else days.
pub fn age_string(birth: &str, study: &str) -> Option<String> {
    let b = parse_da(birth)?;
    let s = parse_da(study)?;
    if s < b {
        return None;
    }
    let mut years = s.year() - b.year();
    if (s.month(), s.day()) < (b.month(), b.day()) {
        years -= 1;
    }
    if years >= 2 {
        return Some(format!("{:03}Y", years.min(999)));
    }
    let mut months = (s.year() - b.year()) * 12 + s.month() as i32 - b.month() as i32;
    if s.day() < b.day() {
        months -= 1;
    }
    if months >= 2 {
        return Some(format!("{:03}M", months));
    }
    let days = (s - b).num_days();
    Some(format!("{:03}D", days.min(999)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_by_days_and_keeps_intervals() {
        let d = DateShift::new(400, false);
        let a = d.shift("20240310").unwrap();
        let b = d.shift("20240410").unwrap();
        let (pa, pb) = (parse_da(&a).unwrap(), parse_da(&b).unwrap());
        assert_eq!((pb - pa).num_days(), 31);
        assert_ne!(a, "20240310");
    }

    #[test]
    fn preserve_season_keeps_day_and_month() {
        let d = DateShift::new(1000, true);
        assert_eq!(d.years(), 2);
        assert_eq!(d.shift("20240310").unwrap(), "20220310");
    }

    #[test]
    fn datetime_keeps_time_and_drops_zone() {
        let d = DateShift::new(365, false);
        assert_eq!(
            d.shift_dt("20240310121530.000000+0100").unwrap(),
            "20230311121530.000000"
        );
    }

    #[test]
    fn ages() {
        assert_eq!(age_string("20100615", "20240310").unwrap(), "013Y");
        assert_eq!(age_string("20240101", "20240310").unwrap(), "002M");
        assert_eq!(age_string("20240301", "20240310").unwrap(), "009D");
        assert!(age_string("garbage", "20240310").is_none());
    }
}
