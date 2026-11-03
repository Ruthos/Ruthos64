//! The attribute allowlist and the teaching-copy writer.
//!
//! A removal list fails open: an attribute it does not name survives. An
//! allowlist fails closed: an attribute nobody thought of is not copied.
//! The set Ruthos needs is bounded, and it is all here. The result meets
//! the PS3.15 Basic Application Level Confidentiality Profile with the
//! options declared in [`DEID_METHOD_CODES`].

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{dicom_value, DataElement, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;

use super::dates::{age_string, DateShift};
use super::uid::derive_uid;

/// VOI LUT Function (0028,1056) and VOI LUT Sequence (0028,3010), which the
/// bundled dictionary does not name.
const VOI_LUT_FUNCTION: Tag = Tag(0x0028, 0x1056);
const VOI_LUT_SEQUENCE: Tag = Tag(0x0028, 0x3010);
const LUT_EXPLANATION: Tag = Tag(0x0028, 0x3003);
const SOP_CLASSES_SUPPORTED: Tag = Tag(0x0008, 0x115A);
const CODING_SCHEME_VERSION: Tag = Tag(0x0008, 0x0103);
/// Other Patient IDs (0010,1000), retired in the standard but still written
/// by veterinary systems, often with the microchip number.
pub const OTHER_PATIENT_IDS: Tag = Tag(0x0010, 0x1000);
/// Other Patient IDs Sequence (0010,1002).
pub const OTHER_PATIENT_IDS_SEQUENCE: Tag = Tag(0x0010, 0x1002);

/// De-identification Method Code Sequence entries (DCM scheme).
pub const DEID_METHOD_CODES: [(&str, &str); 5] = [
    ("113100", "Basic Application Confidentiality Profile"),
    ("113101", "Clean Pixel Data Option"),
    ("113105", "Clean Descriptors Option"),
    ("113107", "Retain Longitudinal Temporal Information Modified Dates Option"),
    ("113108", "Retain Patient Characteristics Option"),
];

/// Attributes copied into a teaching copy. Anything not here, not in the
/// ultrasound region group and not a replaced attribute is left behind.
const ALLOWED: &[Tag] = &[
    // SOP common and instance
    tags::SPECIFIC_CHARACTER_SET,
    tags::SOP_CLASS_UID,
    tags::SOP_INSTANCE_UID,
    tags::INSTANCE_CREATION_DATE,
    tags::INSTANCE_CREATION_TIME,
    tags::IMAGE_TYPE,
    tags::INSTANCE_NUMBER,
    tags::CONTENT_DATE,
    tags::CONTENT_TIME,
    tags::ACQUISITION_DATE,
    tags::ACQUISITION_TIME,
    tags::ACQUISITION_DATE_TIME,
    tags::ACQUISITION_NUMBER,
    tags::CONVERSION_TYPE,
    tags::BURNED_IN_ANNOTATION,
    tags::LOSSY_IMAGE_COMPRESSION,
    tags::LOSSY_IMAGE_COMPRESSION_RATIO,
    tags::LOSSY_IMAGE_COMPRESSION_METHOD,
    // Study
    tags::STUDY_INSTANCE_UID,
    tags::STUDY_DATE,
    tags::STUDY_TIME,
    tags::STUDY_DESCRIPTION,
    tags::STUDY_ID,
    tags::ACCESSION_NUMBER,
    tags::MODALITY,
    tags::MODALITIES_IN_STUDY,
    // Series and equipment
    tags::SERIES_INSTANCE_UID,
    tags::SERIES_NUMBER,
    tags::SERIES_DATE,
    tags::SERIES_TIME,
    tags::SERIES_DESCRIPTION,
    tags::PROTOCOL_NAME,
    tags::BODY_PART_EXAMINED,
    tags::VIEW_POSITION,
    tags::LATERALITY,
    tags::IMAGE_LATERALITY,
    tags::PATIENT_POSITION,
    tags::FRAME_OF_REFERENCE_UID,
    tags::POSITION_REFERENCE_INDICATOR,
    tags::MANUFACTURER,
    tags::MANUFACTURER_MODEL_NAME,
    tags::SOFTWARE_VERSIONS,
    // Patient: identity replaced, characteristics retained
    tags::PATIENT_NAME,
    tags::PATIENT_ID,
    tags::PATIENT_BIRTH_DATE,
    tags::PATIENT_AGE,
    tags::PATIENT_SEX,
    tags::PATIENT_SEX_NEUTERED,
    tags::PATIENT_WEIGHT,
    tags::PATIENT_SIZE,
    tags::PATIENT_SPECIES_DESCRIPTION,
    tags::PATIENT_SPECIES_CODE_SEQUENCE,
    tags::PATIENT_BREED_DESCRIPTION,
    tags::PATIENT_BREED_CODE_SEQUENCE,
    // De-identification
    tags::PATIENT_IDENTITY_REMOVED,
    tags::DEIDENTIFICATION_METHOD,
    tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE,
    tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
    // Image pixel
    tags::SAMPLES_PER_PIXEL,
    tags::PHOTOMETRIC_INTERPRETATION,
    tags::PLANAR_CONFIGURATION,
    tags::ROWS,
    tags::COLUMNS,
    tags::BITS_ALLOCATED,
    tags::BITS_STORED,
    tags::HIGH_BIT,
    tags::PIXEL_REPRESENTATION,
    tags::PIXEL_DATA,
    tags::NUMBER_OF_FRAMES,
    tags::PIXEL_ASPECT_RATIO,
    tags::PIXEL_PADDING_VALUE,
    tags::SMALLEST_IMAGE_PIXEL_VALUE,
    tags::LARGEST_IMAGE_PIXEL_VALUE,
    tags::RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
    tags::GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
    tags::BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
    tags::RED_PALETTE_COLOR_LOOKUP_TABLE_DATA,
    tags::GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA,
    tags::BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA,
    tags::ULTRASOUND_COLOR_DATA_PRESENT,
    // Geometry
    tags::PIXEL_SPACING,
    tags::IMAGER_PIXEL_SPACING,
    tags::IMAGE_POSITION_PATIENT,
    tags::IMAGE_ORIENTATION_PATIENT,
    tags::SLICE_THICKNESS,
    tags::SLICE_LOCATION,
    tags::SPACING_BETWEEN_SLICES,
    tags::PATIENT_ORIENTATION,
    // Modality and VOI LUT
    tags::WINDOW_CENTER,
    tags::WINDOW_WIDTH,
    tags::WINDOW_CENTER_WIDTH_EXPLANATION,
    VOI_LUT_FUNCTION,
    VOI_LUT_SEQUENCE,
    tags::LUT_DESCRIPTOR,
    tags::LUT_DATA,
    LUT_EXPLANATION,
    tags::RESCALE_INTERCEPT,
    tags::RESCALE_SLOPE,
    tags::RESCALE_TYPE,
    tags::PRESENTATION_LUT_SHAPE,
    // Ultrasound regions are allowed by group range (0018,60xx)
    tags::SEQUENCE_OF_ULTRASOUND_REGIONS,
    // Multi-frame and cine timing
    tags::FRAME_TIME,
    tags::FRAME_TIME_VECTOR,
    tags::CINE_RATE,
    tags::RECOMMENDED_DISPLAY_FRAME_RATE,
    tags::FRAME_INCREMENT_POINTER,
    tags::START_TRIM,
    tags::STOP_TRIM,
    tags::EFFECTIVE_DURATION,
    tags::ACTUAL_FRAME_DURATION,
    tags::PREFERRED_PLAYBACK_SEQUENCING,
    tags::FRAME_DELAY,
    // Acquisition parameters with teaching value
    tags::KVP,
    tags::EXPOSURE,
    tags::EXPOSURE_TIME,
    tags::X_RAY_TUBE_CURRENT,
    tags::CONVOLUTION_KERNEL,
    tags::ECHO_TIME,
    tags::REPETITION_TIME,
    tags::INVERSION_TIME,
    tags::FLIP_ANGLE,
    tags::MAGNETIC_FIELD_STRENGTH,
    tags::SCANNING_SEQUENCE,
    tags::SEQUENCE_VARIANT,
    tags::NUMBER_OF_AVERAGES,
    tags::ECHO_NUMBERS,
    tags::CONTRAST_BOLUS_AGENT,
    tags::DETECTOR_TYPE,
    // References between instances (UIDs are derived)
    tags::REFERENCED_IMAGE_SEQUENCE,
    tags::SOURCE_IMAGE_SEQUENCE,
    tags::REFERENCED_SOP_CLASS_UID,
    tags::REFERENCED_SOP_INSTANCE_UID,
    tags::REFERENCED_FRAME_NUMBER,
    // Code sequence items
    tags::CODE_VALUE,
    tags::CODING_SCHEME_DESIGNATOR,
    CODING_SCHEME_VERSION,
    tags::CODE_MEANING,
];

/// Attributes whose value is replaced rather than copied.
const REPLACED: &[Tag] = &[tags::PATIENT_NAME, tags::PATIENT_ID, tags::STUDY_DESCRIPTION, tags::ACCESSION_NUMBER, tags::STUDY_ID];

/// UIDs that name a type rather than a thing, and so are copied as they are.
pub fn is_type_uid(tag: Tag) -> bool {
    matches!(
        tag,
        tags::SOP_CLASS_UID
            | tags::REFERENCED_SOP_CLASS_UID
            | tags::TRANSFER_SYNTAX_UID
            | tags::IMPLEMENTATION_CLASS_UID
            | tags::MEDIA_STORAGE_SOP_CLASS_UID
            | tags::CODING_SCHEME_UID
            | SOP_CLASSES_SUPPORTED
    )
}

/// Whether an attribute may appear in a teaching copy (at any depth).
pub fn is_allowed(tag: Tag) -> bool {
    if tag.group() % 2 == 1 {
        return false; // private
    }
    if (0x5000..=0x50FF).contains(&tag.group()) || (0x6000..=0x60FF).contains(&tag.group()) {
        return false; // curves and overlays
    }
    if tag.group() == 0x0018 && (0x6011..=0x60FF).contains(&tag.element()) {
        return true; // ultrasound region attributes
    }
    ALLOWED.contains(&tag) || REPLACED.contains(&tag)
}

/// What the writer needs to know about the case.
pub struct DeidParams<'a> {
    pub teaching_name: &'a str,
    pub reference: &'a str,
    /// Written to Study Description when given.
    pub title: Option<&'a str>,
    pub secret: &'a [u8; 32],
    pub dates: DateShift,
    /// Ruthos version, named in De-identification Method.
    pub software: &'a str,
}

/// Build the teaching dataset from a source dataset.
///
/// Pixel data is copied through unchanged here; masking replaces it
/// afterwards (see `mask.rs`). The file meta information is built by the
/// caller, with the derived SOP Instance UID.
pub fn build_teaching_dataset(src: &InMemDicomObject, p: &DeidParams<'_>) -> InMemDicomObject {
    let mut out = InMemDicomObject::new_empty();
    for elt in src.iter() {
        let tag = elt.header().tag;
        if !is_allowed(tag) {
            continue;
        }
        if let Some(new) = transform(elt, p) {
            out.put(new);
        }
    }

    // Replaced attributes.
    let put_str = |out: &mut InMemDicomObject, tag: Tag, vr: VR, s: &str| {
        out.put(DataElement::new(tag, vr, dicom_value!(Str, s)));
    };
    put_str(&mut out, tags::PATIENT_NAME, VR::PN, p.teaching_name);
    put_str(&mut out, tags::PATIENT_ID, VR::LO, p.reference);
    put_str(&mut out, tags::ACCESSION_NUMBER, VR::SH, "");
    put_str(&mut out, tags::STUDY_ID, VR::SH, "");
    if let Some(title) = p.title.filter(|t| !t.trim().is_empty()) {
        put_str(&mut out, tags::STUDY_DESCRIPTION, VR::LO, title.trim());
    }

    // Age is written explicitly, from the true dates, when the source has none.
    if out.get(tags::PATIENT_AGE).is_none() {
        let birth = src.get(tags::PATIENT_BIRTH_DATE).and_then(|e| e.value().to_str().ok().map(|s| s.to_string()));
        let study = src.get(tags::STUDY_DATE).and_then(|e| e.value().to_str().ok().map(|s| s.to_string()));
        if let (Some(b), Some(s)) = (birth, study) {
            if let Some(age) = age_string(&b, &s) {
                put_str(&mut out, tags::PATIENT_AGE, VR::AS, &age);
            }
        }
    }

    // De-identification declarations.
    put_str(&mut out, tags::PATIENT_IDENTITY_REMOVED, VR::CS, "YES");
    put_str(&mut out, tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED, VR::CS, "MODIFIED");
    put_str(&mut out, tags::DEIDENTIFICATION_METHOD, VR::LO, &format!("Ruthos {} teaching copy", p.software));
    let items: Vec<InMemDicomObject> = DEID_METHOD_CODES
        .iter()
        .map(|(value, meaning)| {
            let mut item = InMemDicomObject::new_empty();
            item.put(DataElement::new(tags::CODE_VALUE, VR::SH, dicom_value!(Str, *value)));
            item.put(DataElement::new(tags::CODING_SCHEME_DESIGNATOR, VR::SH, dicom_value!(Str, "DCM")));
            item.put(DataElement::new(tags::CODE_MEANING, VR::LO, dicom_value!(Str, *meaning)));
            item
        })
        .collect();
    out.put(DataElement::new(
        tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED)),
    ));
    out
}

type Element = dicom_object::mem::InMemElement;

/// Copy one allowed element with UIDs derived and dates shifted, recursing
/// into sequences. Returns `None` when the value cannot be carried.
fn transform(elt: &Element, p: &DeidParams<'_>) -> Option<Element> {
    let tag = elt.header().tag;
    let vr = elt.vr();
    match elt.value() {
        Value::Sequence(seq) => {
            let items: Vec<InMemDicomObject> = seq
                .items()
                .iter()
                .map(|item| {
                    let mut new = InMemDicomObject::new_empty();
                    for e in item.iter() {
                        if is_allowed(e.header().tag) {
                            if let Some(t) = transform(e, p) {
                                new.put(t);
                            }
                        }
                    }
                    new
                })
                .collect();
            Some(DataElement::new(tag, VR::SQ, Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED))))
        }
        Value::PixelSequence(_) => Some(elt.clone()),
        Value::Primitive(_) => match vr {
            VR::UI if !is_type_uid(tag) => {
                let values = elt.value().to_multi_str().ok()?;
                let derived: Vec<String> = values.iter().map(|u| derive_uid(p.secret, u)).collect();
                Some(DataElement::new(tag, VR::UI, PrimitiveValue::Strs(derived.into())))
            }
            VR::DA => {
                let values = elt.value().to_multi_str().ok()?;
                let shifted: Vec<String> = values.iter().filter_map(|d| p.dates.shift(d)).collect();
                if shifted.is_empty() {
                    return None;
                }
                Some(DataElement::new(tag, VR::DA, PrimitiveValue::Strs(shifted.into())))
            }
            VR::DT => {
                let values = elt.value().to_multi_str().ok()?;
                let shifted: Vec<String> = values.iter().filter_map(|d| p.dates.shift_dt(d)).collect();
                if shifted.is_empty() {
                    return None;
                }
                Some(DataElement::new(tag, VR::DT, PrimitiveValue::Strs(shifted.into())))
            }
            _ => Some(elt.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> [u8; 32] {
        [3u8; 32]
    }

    fn source() -> InMemDicomObject {
        let mut o = InMemDicomObject::new_empty();
        let mut s = |tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
        s(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.6.1");
        s(tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4.5");
        s(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.3.4");
        s(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.3.4.1");
        s(tags::PATIENT_NAME, VR::PN, "HUGHES^BERTIE");
        s(tags::PATIENT_ID, VR::LO, "V12345");
        s(OTHER_PATIENT_IDS, VR::LO, "985141001234567");
        s(tags::RESPONSIBLE_PERSON, VR::PN, "HUGHES^MARY");
        s(tags::PATIENT_BIRTH_DATE, VR::DA, "20120515");
        s(tags::PATIENT_SEX, VR::CS, "M");
        s(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, "Equine");
        s(tags::STUDY_DATE, VR::DA, "20240310");
        s(tags::STUDY_TIME, VR::TM, "101500");
        s(tags::ACCESSION_NUMBER, VR::SH, "ACC001");
        s(tags::INSTITUTION_NAME, VR::LO, "Ashby Equine Clinic");
        s(tags::STATION_NAME, VR::SH, "ECHO1");
        s(tags::MODALITY, VR::CS, "US");
        s(tags::SERIES_DESCRIPTION, VR::LO, "LV M-mode");
        o.put(DataElement::new(Tag(0x0009, 0x0010), VR::LO, dicom_value!(Str, "VENDOR PRIVATE")));
        o.put(DataElement::new(tags::ROWS, VR::US, dicom_value!(U16, [4])));
        o
    }

    fn params<'a>(secret: &'a [u8; 32]) -> DeidParams<'a> {
        DeidParams {
            teaching_name: "Hartley^Bramble",
            reference: "RUTHOS-2026-0001",
            title: Some("Aortic regurgitation"),
            secret,
            dates: DateShift::new(500, false),
            software: "0.1.0",
        }
    }

    fn text(o: &InMemDicomObject, tag: Tag) -> String {
        o.get(tag).map(|e| e.value().to_str().unwrap().to_string()).unwrap_or_default()
    }

    #[test]
    fn identifiers_replaced_and_rest_left_behind() {
        let s = secret();
        let out = build_teaching_dataset(&source(), &params(&s));
        assert_eq!(text(&out, tags::PATIENT_NAME), "Hartley^Bramble");
        assert_eq!(text(&out, tags::PATIENT_ID), "RUTHOS-2026-0001");
        assert_eq!(text(&out, tags::STUDY_DESCRIPTION), "Aortic regurgitation");
        assert_eq!(text(&out, tags::ACCESSION_NUMBER), "");
        assert!(out.get(OTHER_PATIENT_IDS).is_none());
        assert!(out.get(tags::RESPONSIBLE_PERSON).is_none());
        assert!(out.get(tags::INSTITUTION_NAME).is_none());
        assert!(out.get(tags::STATION_NAME).is_none());
        assert!(out.get(Tag(0x0009, 0x0010)).is_none());
        assert_eq!(text(&out, tags::PATIENT_SEX), "M");
        assert_eq!(text(&out, tags::PATIENT_SPECIES_DESCRIPTION), "Equine");
        assert_eq!(text(&out, tags::SERIES_DESCRIPTION), "LV M-mode");
        assert_eq!(text(&out, tags::PATIENT_IDENTITY_REMOVED), "YES");
        assert_eq!(text(&out, tags::PATIENT_AGE), "011Y");
        assert_eq!(out.get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE).unwrap().value().items().unwrap().len(), 5);
    }

    #[test]
    fn uids_derived_and_dates_shifted() {
        let s = secret();
        let out = build_teaching_dataset(&source(), &params(&s));
        assert_eq!(text(&out, tags::SOP_CLASS_UID), "1.2.840.10008.5.1.4.1.1.6.1");
        assert_eq!(text(&out, tags::SOP_INSTANCE_UID), derive_uid(&s, "1.2.3.4.5"));
        assert_ne!(text(&out, tags::STUDY_INSTANCE_UID), "1.2.3.4");
        assert_eq!(text(&out, tags::STUDY_DATE), "20221027");
        assert_eq!(text(&out, tags::PATIENT_BIRTH_DATE), "20110101");
        assert_eq!(text(&out, tags::STUDY_TIME), "101500");
    }

    #[test]
    fn allowlist_rejects_private_overlay_and_unknown() {
        assert!(!is_allowed(Tag(0x0009, 0x1001)));
        assert!(!is_allowed(Tag(0x6000, 0x3000)));
        assert!(!is_allowed(tags::INSTITUTION_NAME));
        assert!(!is_allowed(OTHER_PATIENT_IDS));
        assert!(is_allowed(tags::PIXEL_SPACING));
        assert!(is_allowed(Tag(0x0018, 0x602C)));
    }

    /// Everything the viewer reads to display, measure and annotate must be
    /// on the allowlist, or a teaching copy would lose it. Add a tag here
    /// when a display path starts reading it.
    #[test]
    fn display_paths_only_read_allowed_attributes() {
        let display_tags = [
            tags::ROWS, tags::COLUMNS, tags::NUMBER_OF_FRAMES, tags::BITS_ALLOCATED, tags::BITS_STORED,
            tags::PIXEL_REPRESENTATION, tags::SAMPLES_PER_PIXEL, tags::PHOTOMETRIC_INTERPRETATION,
            tags::PIXEL_SPACING, tags::IMAGER_PIXEL_SPACING, tags::SLICE_THICKNESS, tags::SLICE_LOCATION,
            tags::IMAGE_POSITION_PATIENT, tags::IMAGE_ORIENTATION_PATIENT, tags::PATIENT_ORIENTATION,
            tags::WINDOW_CENTER, tags::WINDOW_WIDTH, tags::RESCALE_INTERCEPT, tags::RESCALE_SLOPE,
            tags::PRESENTATION_LUT_SHAPE, tags::KVP, tags::EXPOSURE, tags::EXPOSURE_TIME, tags::MANUFACTURER,
            tags::MANUFACTURER_MODEL_NAME, tags::DETECTOR_TYPE, tags::ECHO_TIME, tags::REPETITION_TIME,
            tags::MAGNETIC_FIELD_STRENGTH, tags::SEQUENCE_OF_ULTRASOUND_REGIONS, tags::FRAME_TIME, tags::CINE_RATE,
            tags::RECOMMENDED_DISPLAY_FRAME_RATE, tags::BURNED_IN_ANNOTATION, tags::MODALITY, tags::STUDY_DATE,
            tags::STUDY_TIME, tags::STUDY_DESCRIPTION, tags::SERIES_NUMBER, tags::SERIES_DESCRIPTION,
            tags::SERIES_DATE, tags::BODY_PART_EXAMINED, tags::VIEW_POSITION, tags::LATERALITY, tags::INSTANCE_NUMBER,
            tags::ACQUISITION_DATE, tags::ACQUISITION_TIME, tags::CONTENT_DATE, tags::CONTENT_TIME,
            tags::PATIENT_NAME, tags::PATIENT_ID, tags::PATIENT_BIRTH_DATE, tags::PATIENT_SEX,
            tags::PATIENT_SPECIES_DESCRIPTION, tags::PATIENT_BREED_DESCRIPTION, tags::SOP_CLASS_UID,
            tags::SOP_INSTANCE_UID, tags::STUDY_INSTANCE_UID, tags::SERIES_INSTANCE_UID,
        ];
        for t in display_tags {
            assert!(is_allowed(t), "display path reads ({:04X},{:04X}) which the allowlist does not copy", t.group(), t.element());
        }
    }
}
