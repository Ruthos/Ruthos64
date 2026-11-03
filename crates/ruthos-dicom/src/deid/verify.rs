//! The independent verification pass.
//!
//! Reads a finished teaching copy back from disk through a fresh parse and
//! refuses it if any identifying value from the source appears anywhere:
//! an attribute outside the allowlist, a private, overlay or curve
//! attribute, a sensitive token in any text at any depth, an original UID,
//! or an original date.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use dicom_core::value::Value;
use dicom_core::{Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;

use super::allowlist::{is_allowed, is_type_uid, OTHER_PATIENT_IDS, OTHER_PATIENT_IDS_SEQUENCE};

/// What the pass checks against, collected from the source study.
#[derive(Debug, Clone, Default)]
pub struct VerifyContext {
    /// Sensitive tokens: name components, IDs, institution, station, staff.
    pub tokens: Vec<String>,
    /// Every instance-type UID of the source.
    pub original_uids: HashSet<String>,
    /// Every DA value of the source (`YYYYMMDD`).
    pub original_dates: HashSet<String>,
    /// The original dates of each date attribute (DA, and the date part
    /// of DT), so a correctly shifted value that happens to equal some
    /// other original date is not mistaken for a leak.
    pub dates_by_tag: HashMap<Tag, HashSet<String>>,
    /// The shift the copy applies, when known.
    pub date_shift: Option<super::DateShift>,
}

impl VerifyContext {
    /// Merge another source dataset's values in.
    pub fn absorb(&mut self, other: VerifyContext) {
        for t in other.tokens {
            if !self.tokens.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                self.tokens.push(t);
            }
        }
        self.original_uids.extend(other.original_uids);
        self.original_dates.extend(other.original_dates);
        for (tag, dates) in other.dates_by_tag {
            self.dates_by_tag.entry(tag).or_default().extend(dates);
        }
        if self.date_shift.is_none() {
            self.date_shift = other.date_shift;
        }
    }

    /// Whether `digits` in attribute `tag` is exactly the shifted value of
    /// one of that attribute's own original dates: the expected output,
    /// even when it coincides with another original date of the study.
    fn is_expected_shift(&self, tag: Tag, digits: &str) -> bool {
        let Some(shift) = self.date_shift else { return false };
        self.dates_by_tag
            .get(&tag)
            .is_some_and(|orig| orig.iter().any(|o| shift.shift(o).is_some_and(|d| d == digits)))
    }
}

/// One thing the pass found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    /// Attribute path such as `(0008,103E)` or `(0008,1140)[0]/(0008,1155)`.
    pub path: String,
    pub what: String,
    /// The matched token, UID or date, if any.
    pub matched: String,
    /// A descriptor hit the user may accept with a reason; never for
    /// identity, UIDs, dates or disallowed attributes.
    pub overridable: bool,
}

const STRING_VRS: [VR; 10] = [
    VR::PN,
    VR::LO,
    VR::SH,
    VR::ST,
    VR::LT,
    VR::UT,
    VR::CS,
    VR::AE,
    VR::UC,
    VR::UR,
];

fn text_of(obj: &InMemDicomObject, tag: Tag) -> Vec<String> {
    obj.get(tag)
        .and_then(|e| e.value().to_multi_str().ok().map(|v| v.to_vec()))
        .unwrap_or_default()
}

/// Collect sensitive tokens, UIDs and dates from a source dataset.
pub fn collect_context(src: &InMemDicomObject) -> VerifyContext {
    let mut ctx = VerifyContext::default();
    fn push_words(tokens: &mut Vec<String>, s: &str) {
        for part in s.split(['^', ' ', ',', '=']) {
            let p = part.trim();
            if !p.is_empty() && !tokens.iter().any(|t| t.eq_ignore_ascii_case(p)) {
                tokens.push(p.to_string());
            }
        }
    }
    for tag in [
        tags::PATIENT_NAME,
        tags::RESPONSIBLE_PERSON,
        tags::OTHER_PATIENT_NAMES,
        tags::REFERRING_PHYSICIAN_NAME,
        tags::PERFORMING_PHYSICIAN_NAME,
        tags::NAME_OF_PHYSICIANS_READING_STUDY,
        tags::OPERATORS_NAME,
        tags::PHYSICIANS_OF_RECORD,
        tags::INSTITUTION_ADDRESS,
    ] {
        for v in text_of(src, tag) {
            push_words(&mut ctx.tokens, &v);
        }
    }
    for tag in [
        tags::PATIENT_ID,
        OTHER_PATIENT_IDS,
        tags::ACCESSION_NUMBER,
        tags::INSTITUTION_NAME,
        tags::STATION_NAME,
        tags::DEVICE_SERIAL_NUMBER,
    ] {
        for v in text_of(src, tag) {
            let v = v.trim();
            if !v.is_empty() && !ctx.tokens.iter().any(|t| t.eq_ignore_ascii_case(v)) {
                ctx.tokens.push(v.to_string());
            }
        }
    }
    if let Some(seq) = src.get(OTHER_PATIENT_IDS_SEQUENCE).and_then(|e| e.value().items()) {
        for item in seq {
            for v in text_of(item, tags::PATIENT_ID) {
                ctx.tokens.push(v.trim().to_string());
            }
        }
    }
    // Manufacturer and model are retained by design, so a token equal to
    // either would fail every copy; drop such tokens (a station named after
    // the vendor, typically).
    let retained: Vec<String> = [tags::MANUFACTURER, tags::MANUFACTURER_MODEL_NAME]
        .iter()
        .flat_map(|t| text_of(src, *t))
        .map(|v| v.trim().to_string())
        .collect();
    ctx.tokens
        .retain(|t| !retained.iter().any(|r| r.eq_ignore_ascii_case(t) || token_hit(t, r)));
    collect_uids_and_dates(src, &mut ctx);
    ctx
}

fn collect_uids_and_dates(obj: &InMemDicomObject, ctx: &mut VerifyContext) {
    for e in obj.iter() {
        let tag = e.header().tag;
        match e.value() {
            Value::Sequence(seq) => {
                for item in seq.items() {
                    collect_uids_and_dates(item, ctx);
                }
            }
            Value::Primitive(_) => match e.vr() {
                VR::UI if !is_type_uid(tag) => {
                    for u in e.value().to_multi_str().map(|v| v.to_vec()).unwrap_or_default() {
                        let u = u.trim_end_matches('\0').trim().to_string();
                        if !u.is_empty() {
                            ctx.original_uids.insert(u);
                        }
                    }
                }
                VR::DA => {
                    for d in e.value().to_multi_str().map(|v| v.to_vec()).unwrap_or_default() {
                        let digits: String = d.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
                        if digits.len() == 8 {
                            ctx.original_dates.insert(digits.clone());
                            ctx.dates_by_tag.entry(tag).or_default().insert(digits);
                        }
                    }
                }
                VR::DT => {
                    for d in e.value().to_multi_str().map(|v| v.to_vec()).unwrap_or_default() {
                        let digits: String = d.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
                        if digits.len() == 8 {
                            ctx.dates_by_tag.entry(tag).or_default().insert(digits);
                        }
                    }
                }
                _ => {}
            },
            Value::PixelSequence(_) => {}
        }
    }
}

/// Whether a sensitive token appears in a text. Tokens of three characters
/// or fewer are skipped; four to six must match as whole words.
pub fn token_hit(token: &str, text: &str) -> bool {
    let n = token.chars().count();
    if n <= 3 {
        return false;
    }
    let hay = text.to_lowercase();
    let needle = token.to_lowercase();
    if n > 6 {
        return hay.contains(&needle);
    }
    hay.split(|c: char| !c.is_alphanumeric()).any(|w| w == needle)
}

/// Check free text (a title, report text, the sidecar) against the context.
pub fn verify_text(label: &str, text: &str, ctx: &VerifyContext, overridable: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in &ctx.tokens {
        if token_hit(t, text) {
            out.push(Finding {
                file: String::new(),
                path: label.to_string(),
                what: "sensitive token".into(),
                matched: t.clone(),
                overridable,
            });
        }
    }
    for u in &ctx.original_uids {
        if text.contains(u.as_str()) {
            out.push(Finding {
                file: String::new(),
                path: label.to_string(),
                what: "original UID".into(),
                matched: u.clone(),
                overridable: false,
            });
        }
    }
    out
}

/// Read a teaching-copy file back and check everything in it.
pub fn verify_file(path: &Path, ctx: &VerifyContext) -> Vec<Finding> {
    let file = path.display().to_string();
    let obj = match crate::open::open_any(path, None) {
        Ok(o) => o,
        Err(e) => {
            return vec![Finding {
                file,
                path: String::new(),
                what: format!("cannot be read back: {e}"),
                matched: String::new(),
                overridable: false,
            }];
        }
    };
    let mut out = Vec::new();
    // File meta information: the media storage SOP instance UID must be derived.
    let meta = obj.meta();
    let msi = meta.media_storage_sop_instance_uid.trim_end_matches('\0').trim();
    if ctx.original_uids.contains(msi) {
        out.push(Finding {
            file: file.clone(),
            path: "(0002,0003)".into(),
            what: "original UID".into(),
            matched: msi.to_string(),
            overridable: false,
        });
    }
    check_dataset(&obj, "", &file, ctx, &mut out);
    out
}

fn check_dataset(obj: &InMemDicomObject, prefix: &str, file: &str, ctx: &VerifyContext, out: &mut Vec<Finding>) {
    for e in obj.iter() {
        let tag = e.header().tag;
        let path = format!("{prefix}({:04X},{:04X})", tag.group(), tag.element());
        if !is_allowed(tag) {
            let what = if tag.group() % 2 == 1 {
                "private attribute present"
            } else if (0x5000..=0x50FF).contains(&tag.group()) {
                "curve attribute present"
            } else if (0x6000..=0x60FF).contains(&tag.group()) {
                "overlay attribute present"
            } else {
                "attribute not on the allowlist"
            };
            out.push(Finding {
                file: file.into(),
                path: path.clone(),
                what: what.into(),
                matched: String::new(),
                overridable: false,
            });
            continue;
        }
        match e.value() {
            Value::Sequence(seq) => {
                for (i, item) in seq.items().iter().enumerate() {
                    check_dataset(item, &format!("{path}[{i}]/"), file, ctx, out);
                }
            }
            Value::PixelSequence(_) => {}
            Value::Primitive(_) => {
                let vr = e.vr();
                let values: Vec<String> = e.value().to_multi_str().map(|v| v.to_vec()).unwrap_or_default();
                if vr == VR::UI {
                    for u in &values {
                        let u = u.trim_end_matches('\0').trim();
                        if ctx.original_uids.contains(u) {
                            out.push(Finding {
                                file: file.into(),
                                path: path.clone(),
                                what: "original UID".into(),
                                matched: u.to_string(),
                                overridable: false,
                            });
                        }
                    }
                } else if vr == VR::DA {
                    for d in &values {
                        let digits: String = d.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
                        if ctx.original_dates.contains(&digits) && !ctx.is_expected_shift(tag, &digits) {
                            out.push(Finding {
                                file: file.into(),
                                path: path.clone(),
                                what: "original date".into(),
                                matched: digits,
                                overridable: false,
                            });
                        }
                    }
                } else if vr == VR::DT {
                    for d in &values {
                        let digits: String = d.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
                        if ctx.original_dates.contains(&digits) && !ctx.is_expected_shift(tag, &digits) {
                            out.push(Finding {
                                file: file.into(),
                                path: path.clone(),
                                what: "original date".into(),
                                matched: digits,
                                overridable: false,
                            });
                        }
                    }
                } else if STRING_VRS.contains(&vr) {
                    let identity = matches!(tag, tags::PATIENT_NAME | tags::PATIENT_ID);
                    for v in &values {
                        for t in &ctx.tokens {
                            if token_hit(t, v) {
                                out.push(Finding {
                                    file: file.into(),
                                    path: path.clone(),
                                    what: "sensitive token".into(),
                                    matched: t.clone(),
                                    overridable: !identity,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shifted study date that lands on the original birth date is the
    /// expected output, not a leak; an unshifted original date still is.
    #[test]
    fn a_shift_landing_on_another_original_date_is_not_a_leak() {
        use dicom_core::{DataElement, PrimitiveValue};
        let el = |tag, v: &str| DataElement::new(tag, VR::DA, PrimitiveValue::from(v));
        let mut src = InMemDicomObject::new_empty();
        src.put(el(tags::SERIES_DATE, "20150401"));
        src.put(el(tags::CONTENT_DATE, "20240607"));
        let mut ctx = collect_context(&src);
        // The offset that moves 2024-06-07 onto 2015-04-01.
        let offset = (365..=3650)
            .find(|d| super::super::DateShift::new(*d, false).shift("20240607").as_deref() == Some("20150401"))
            .unwrap();
        let shift = super::super::DateShift::new(offset, false);
        ctx.date_shift = Some(shift);
        let dates = |out: &[Finding]| out.iter().filter(|f| f.what == "original date").count();

        let mut good = InMemDicomObject::new_empty();
        good.put(el(tags::CONTENT_DATE, "20150401"));
        good.put(el(tags::SERIES_DATE, &shift.shift("20150401").unwrap()));
        let mut out = Vec::new();
        check_dataset(&good, "", "f", &ctx, &mut out);
        assert_eq!(dates(&out), 0, "the content date is correctly shifted: {out:?}");

        let mut leak = InMemDicomObject::new_empty();
        leak.put(el(tags::SERIES_DATE, "20150401"));
        let mut out = Vec::new();
        check_dataset(&leak, "", "f", &ctx, &mut out);
        assert_eq!(dates(&out), 1, "an unshifted series date is still a leak");
    }

    #[test]
    fn token_rules() {
        assert!(!token_hit("Bob", "Bob is here"));
        assert!(token_hit("Ashby", "The Ashby clinic"));
        assert!(!token_hit("Ashby", "Ashbyville"));
        assert!(token_hit("Hughes", "mrs hughes reports"));
        assert!(token_hit("985141001234567", "chip 985141001234567 found"));
    }
}
