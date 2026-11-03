//! Header metadata extraction.
//!
//! [`read_meta`] reads a file up to (but not including) Pixel Data, so it is
//! cheap enough to run over thousands of files during import.

use std::path::Path;

use dicom_core::Tag;
use dicom_dictionary_std::tags;
use dicom_object::{InMemDicomObject, StandardDataDictionary};

use crate::{DicomError, Result};

type Obj = InMemDicomObject<StandardDataDictionary>;

/// A DICOM person name split into its components, with Horos-style display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatientName {
    pub raw: String,
    pub family: String,
    pub given: String,
    pub middle: String,
    pub prefix: String,
    pub suffix: String,
}

impl PatientName {
    pub fn parse(raw: &str) -> Self {
        // Only the alphabetic component group is used for display.
        let alphabetic = raw.split('=').next().unwrap_or("");
        let mut parts = alphabetic.split('^').map(|s| s.trim().to_string());
        let family = parts.next().unwrap_or_default();
        let given = parts.next().unwrap_or_default();
        let middle = parts.next().unwrap_or_default();
        let prefix = parts.next().unwrap_or_default();
        let suffix = parts.next().unwrap_or_default();
        Self {
            raw: raw.trim().to_string(),
            family,
            given,
            middle,
            prefix,
            suffix,
        }
    }

    /// "FAMILY, Given Middle", the way Horos lists patients.
    pub fn display(&self) -> String {
        let mut out = String::new();
        if !self.family.is_empty() {
            out.push_str(&self.family);
        }
        let rest: Vec<&str> = [self.given.as_str(), self.middle.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !rest.is_empty() {
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&rest.join(" "));
        }
        if out.is_empty() {
            out = self.raw.clone();
        }
        if out.is_empty() {
            out = "(no name)".to_string();
        }
        out
    }
}

/// Everything the archive and the viewer need to know about one instance
/// without touching pixel data.
#[derive(Debug, Clone, Default)]
pub struct InstanceMeta {
    // Patient
    pub patient_name: PatientName,
    pub patient_id: String,
    pub patient_birth_date: Option<String>,
    pub patient_sex: String,
    pub patient_species: Option<String>,
    pub patient_breed: Option<String>,
    pub responsible_person: Option<String>,

    // Study
    pub study_instance_uid: String,
    pub study_date: Option<String>,
    pub study_time: Option<String>,
    pub study_description: String,
    pub accession_number: String,
    pub referring_physician: String,
    pub institution_name: String,
    pub study_id: String,

    // Series
    pub series_instance_uid: String,
    pub series_number: Option<i64>,
    pub series_description: String,
    pub series_date: Option<String>,
    pub series_time: Option<String>,
    pub modality: String,
    pub body_part_examined: String,
    pub view_position: String,
    pub laterality: String,
    pub protocol_name: String,

    // Instance
    pub sop_instance_uid: String,
    pub sop_class_uid: String,
    pub transfer_syntax: String,
    pub instance_number: Option<i64>,
    pub acquisition_date: Option<String>,
    pub acquisition_time: Option<String>,
    pub content_date: Option<String>,
    pub content_time: Option<String>,

    // Image geometry
    pub rows: u32,
    pub columns: u32,
    pub number_of_frames: u32,
    /// Time between frames of a multi-frame image in milliseconds, from
    /// Frame Time, Cine Rate or Recommended Display Frame Rate.
    pub frame_time_ms: Option<f64>,
    /// Burned In Annotation (0028,0301): YES, NO or empty.
    pub burned_in_annotation: String,
    pub bits_allocated: u16,
    pub bits_stored: u16,
    pub pixel_representation: u16,
    pub samples_per_pixel: u16,
    pub photometric_interpretation: String,
    /// Row spacing, column spacing in mm (Pixel Spacing, else Imager Pixel Spacing).
    pub pixel_spacing: Option<[f64; 2]>,
    /// Pixel Spacing (0028,0030) as written, before any resolution.
    pub pixel_spacing_tag: Option<[f64; 2]>,
    /// Imager Pixel Spacing (0018,1164): spacing at the detector.
    pub imager_pixel_spacing: Option<[f64; 2]>,
    /// Estimated Radiographic Magnification Factor (0018,1114).
    pub magnification_factor: Option<f64>,
    /// Pixel Spacing Calibration Type (0028,0A02): GEOMETRY, FIDUCIAL or empty.
    pub pixel_spacing_calibration_type: String,
    /// A hand calibration applied by the viewer, possibly scoped to a region.
    pub hand_calibration: Option<crate::scale::HandCalibration>,
    /// Ultrasound calibration regions (0018,6011): each gives its own scale,
    /// and an M-mode region has time across and distance down.
    pub us_regions: Vec<UsRegion>,
    /// The user calibrated this image by hand; that overrides everything.
    pub user_calibrated: bool,
    pub pixel_spacing_is_calibrated: bool,
    pub slice_thickness: Option<f64>,
    pub slice_location: Option<f64>,
    pub image_position: Option<[f64; 3]>,
    pub image_orientation: Option<[f64; 6]>,
    /// Patient Orientation (0020,0020) for projection radiographs, e.g. ["L", "F"].
    pub patient_orientation: Option<[String; 2]>,

    // Display defaults
    pub window_center: Option<f64>,
    pub window_width: Option<f64>,
    pub rescale_intercept: f64,
    pub rescale_slope: f64,
    pub presentation_lut_shape: String,

    // Acquisition (radiography)
    pub kvp: Option<f64>,
    pub exposure_mas: Option<f64>,
    pub exposure_time_ms: Option<f64>,
    pub manufacturer: String,
    pub model_name: String,
    pub station_name: String,
    pub detector_type: String,

    // MRI (used by the lower-right annotation block)
    pub echo_time: Option<f64>,
    pub repetition_time: Option<f64>,
    pub magnetic_field_strength: Option<f64>,
}

impl InstanceMeta {
    pub fn is_multiframe(&self) -> bool {
        self.number_of_frames > 1
    }

    pub fn is_color(&self) -> bool {
        self.samples_per_pixel > 1 || self.photometric_interpretation == "PALETTE COLOR"
    }

    pub fn is_monochrome1(&self) -> bool {
        self.photometric_interpretation == "MONOCHROME1"
    }

    /// Study date as `YYYY-MM-DD` when the DICOM value is complete.
    pub fn study_date_iso(&self) -> Option<String> {
        self.study_date.as_deref().and_then(dicom_date_to_iso)
    }
}

/// Convert a DICOM DA value (`YYYYMMDD`, or old-style `YYYY.MM.DD`) to ISO.
pub fn dicom_date_to_iso(da: &str) -> Option<String> {
    let digits: String = da.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    Some(format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8]))
}

/// Convert a DICOM TM value (`HHMMSS.FFFFFF`) to `HH:MM:SS`.
pub fn dicom_time_to_hms(tm: &str) -> Option<String> {
    let digits: String = tm.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return None;
    }
    let h = &digits[0..2];
    let m = &digits[2..4];
    let s = if digits.len() >= 6 { &digits[4..6] } else { "00" };
    Some(format!("{h}:{m}:{s}"))
}

/// Read header metadata from a file on disk.
pub fn read_meta(path: &Path) -> Result<InstanceMeta> {
    let obj = crate::open::open_any(path, Some(tags::PIXEL_DATA))?;

    let ts = obj.meta().transfer_syntax().to_string();
    let sop_class_meta = obj.meta().media_storage_sop_class_uid().to_string();
    let sop_instance_meta = obj.meta().media_storage_sop_instance_uid().to_string();
    let mut meta = meta_from_object(&obj);
    meta.transfer_syntax = trim_uid(&ts);
    if meta.sop_class_uid.is_empty() {
        meta.sop_class_uid = trim_uid(&sop_class_meta);
    }
    if meta.sop_instance_uid.is_empty() {
        meta.sop_instance_uid = trim_uid(&sop_instance_meta);
    }
    if meta.sop_instance_uid.is_empty() {
        return Err(DicomError::Missing("SOP Instance UID"));
    }
    if meta.study_instance_uid.is_empty() {
        return Err(DicomError::Missing("Study Instance UID"));
    }
    if meta.series_instance_uid.is_empty() {
        return Err(DicomError::Missing("Series Instance UID"));
    }
    Ok(meta)
}

fn trim_uid(s: &str) -> String {
    s.trim_end_matches('\0').trim().to_string()
}

/// Build metadata from an already-open object (used by the import path and
/// by the network receiver, which has the dataset in memory).
pub fn meta_from_object(obj: &Obj) -> InstanceMeta {
    let g = Getter { obj };
    let mut m = InstanceMeta {
        patient_name: PatientName::parse(&g.str(tags::PATIENT_NAME)),
        patient_id: g.str(tags::PATIENT_ID),
        patient_birth_date: g.opt_str(tags::PATIENT_BIRTH_DATE),
        patient_sex: g.str(tags::PATIENT_SEX),
        patient_species: g.opt_str(tags::PATIENT_SPECIES_DESCRIPTION),
        patient_breed: g.opt_str(tags::PATIENT_BREED_DESCRIPTION),
        responsible_person: g.opt_str(tags::RESPONSIBLE_PERSON),

        study_instance_uid: g.str(tags::STUDY_INSTANCE_UID),
        study_date: g.opt_str(tags::STUDY_DATE),
        study_time: g.opt_str(tags::STUDY_TIME),
        study_description: g.str(tags::STUDY_DESCRIPTION),
        accession_number: g.str(tags::ACCESSION_NUMBER),
        referring_physician: PatientName::parse(&g.str(tags::REFERRING_PHYSICIAN_NAME)).display_if_present(),
        institution_name: g.str(tags::INSTITUTION_NAME),
        study_id: g.str(tags::STUDY_ID),

        series_instance_uid: g.str(tags::SERIES_INSTANCE_UID),
        series_number: g.int(tags::SERIES_NUMBER),
        series_description: g.str(tags::SERIES_DESCRIPTION),
        series_date: g.opt_str(tags::SERIES_DATE),
        series_time: g.opt_str(tags::SERIES_TIME),
        modality: g.str(tags::MODALITY),
        body_part_examined: g.str(tags::BODY_PART_EXAMINED),
        view_position: g.str(tags::VIEW_POSITION),
        laterality: {
            let l = g.str(tags::IMAGE_LATERALITY);
            if l.is_empty() {
                g.str(tags::LATERALITY)
            } else {
                l
            }
        },
        protocol_name: g.str(tags::PROTOCOL_NAME),

        sop_instance_uid: g.str(tags::SOP_INSTANCE_UID),
        sop_class_uid: g.str(tags::SOP_CLASS_UID),
        transfer_syntax: String::new(),
        instance_number: g.int(tags::INSTANCE_NUMBER),
        acquisition_date: g.opt_str(tags::ACQUISITION_DATE),
        acquisition_time: g.opt_str(tags::ACQUISITION_TIME),
        content_date: g.opt_str(tags::CONTENT_DATE),
        content_time: g.opt_str(tags::CONTENT_TIME),

        rows: g.int(tags::ROWS).unwrap_or(0) as u32,
        columns: g.int(tags::COLUMNS).unwrap_or(0) as u32,
        number_of_frames: g.int(tags::NUMBER_OF_FRAMES).unwrap_or(1).max(1) as u32,
        burned_in_annotation: g.str(tags::BURNED_IN_ANNOTATION),
        frame_time_ms: g
            .f64(tags::FRAME_TIME)
            .filter(|t| *t > 0.0)
            .or_else(|| g.f64(tags::CINE_RATE).filter(|r| *r > 0.0).map(|r| 1000.0 / r))
            .or_else(|| {
                g.f64(tags::RECOMMENDED_DISPLAY_FRAME_RATE)
                    .filter(|r| *r > 0.0)
                    .map(|r| 1000.0 / r)
            }),
        bits_allocated: g.int(tags::BITS_ALLOCATED).unwrap_or(16) as u16,
        bits_stored: g.int(tags::BITS_STORED).unwrap_or(16) as u16,
        pixel_representation: g.int(tags::PIXEL_REPRESENTATION).unwrap_or(0) as u16,
        samples_per_pixel: g.int(tags::SAMPLES_PER_PIXEL).unwrap_or(1) as u16,
        photometric_interpretation: g.str(tags::PHOTOMETRIC_INTERPRETATION),
        pixel_spacing: None,
        pixel_spacing_tag: g
            .f64s(tags::PIXEL_SPACING)
            .filter(|v| v.len() >= 2)
            .map(|v| [v[0], v[1]]),
        imager_pixel_spacing: g
            .f64s(tags::IMAGER_PIXEL_SPACING)
            .filter(|v| v.len() >= 2)
            .map(|v| [v[0], v[1]]),
        magnification_factor: g
            .f64(tags::ESTIMATED_RADIOGRAPHIC_MAGNIFICATION_FACTOR)
            .filter(|f| *f > 0.0),
        pixel_spacing_calibration_type: g.str(tags::PIXEL_SPACING_CALIBRATION_TYPE),
        hand_calibration: None,
        us_regions: read_us_regions(obj),
        user_calibrated: false,
        pixel_spacing_is_calibrated: false,
        slice_thickness: g.f64(tags::SLICE_THICKNESS),
        slice_location: g.f64(tags::SLICE_LOCATION),
        image_position: g.f64s(tags::IMAGE_POSITION_PATIENT).and_then(|v| v.try_into().ok()),
        image_orientation: g.f64s(tags::IMAGE_ORIENTATION_PATIENT).and_then(|v| v.try_into().ok()),
        patient_orientation: None,

        window_center: g.f64s(tags::WINDOW_CENTER).and_then(|v| v.first().copied()),
        window_width: g.f64s(tags::WINDOW_WIDTH).and_then(|v| v.first().copied()),
        rescale_intercept: g.f64(tags::RESCALE_INTERCEPT).unwrap_or(0.0),
        rescale_slope: g.f64(tags::RESCALE_SLOPE).unwrap_or(1.0),
        presentation_lut_shape: g.str(tags::PRESENTATION_LUT_SHAPE),

        kvp: g.f64(tags::KVP),
        exposure_mas: g
            .f64(tags::EXPOSURE)
            .or_else(|| g.f64(tags::EXPOSURE_INU_AS).map(|v| v / 1000.0)),
        exposure_time_ms: g.f64(tags::EXPOSURE_TIME),
        manufacturer: g.str(tags::MANUFACTURER),
        model_name: g.str(tags::MANUFACTURER_MODEL_NAME),
        station_name: g.str(tags::STATION_NAME),
        detector_type: g.str(tags::DETECTOR_TYPE),

        echo_time: g.f64(tags::ECHO_TIME),
        repetition_time: g.f64(tags::REPETITION_TIME),
        magnetic_field_strength: g.f64(tags::MAGNETIC_FIELD_STRENGTH),
    };

    // Pixel Spacing is calibrated to the patient; Imager Pixel Spacing is at
    // the detector. Horos prefers the former and falls back to the latter.
    if let Some(ps) = g.f64s(tags::PIXEL_SPACING).filter(|v| v.len() >= 2) {
        m.pixel_spacing = Some([ps[0], ps[1]]);
        m.pixel_spacing_is_calibrated = true;
    } else if let Some(ps) = g.f64s(tags::IMAGER_PIXEL_SPACING).filter(|v| v.len() >= 2) {
        m.pixel_spacing = Some([ps[0], ps[1]]);
        m.pixel_spacing_is_calibrated = false;
    } else if let Some(spacing) = m.us_regions.iter().find_map(|r| r.mm_per_pixel()) {
        // Ultrasound: the first 2D region calibrated in centimetres.
        m.pixel_spacing = Some(spacing);
        m.pixel_spacing_is_calibrated = true;
    }

    if let Some(po) = g.strs(tags::PATIENT_ORIENTATION) {
        if po.len() >= 2 {
            m.patient_orientation = Some([po[0].clone(), po[1].clone()]);
        }
    }

    // A window width of zero or less is meaningless; treat it as absent.
    if matches!(m.window_width, Some(w) if w <= 0.0) {
        m.window_width = None;
        m.window_center = None;
    }

    m
}

impl PatientName {
    fn display_if_present(&self) -> String {
        if self.raw.is_empty() {
            String::new()
        } else {
            self.display()
        }
    }
}

/// One item of the Sequence of Ultrasound Regions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsRegion {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
    /// 1 = 2D, 2 = M-mode, 3 = spectral, 4 = waveform, 5 = graphics.
    pub spatial_format: u16,
    pub data_type: u16,
    /// Physical units: 3 = cm, 4 = seconds, 7 = cm/s.
    pub units_x: u16,
    pub units_y: u16,
    /// Physical size of one pixel, in the units above.
    pub delta_x: f64,
    pub delta_y: f64,
    /// Reference pixel (0018,6020) and (0018,6022), relative to the
    /// region's top-left corner: where the physical value is
    /// `ref_value_x` / `ref_value_y` (the Doppler baseline, the time zero).
    pub ref_x0: i64,
    pub ref_y0: i64,
    pub ref_value_x: f64,
    pub ref_value_y: f64,
}

impl UsRegion {
    /// Velocity in cm/s down the region (spectral Doppler).
    pub fn is_velocity_y(&self) -> bool {
        self.units_y == 7 && self.delta_y != 0.0
    }

    /// The physical value on the vertical axis at image row `y`, in the
    /// region's units (cm/s for Doppler, cm for M-mode depth).
    pub fn value_y(&self, y: f64) -> f64 {
        (y - (self.y0 + self.ref_y0) as f64) * self.delta_y + self.ref_value_y
    }

    /// Seconds per pixel across a time-axis region.
    pub fn seconds_per_pixel(&self) -> Option<f64> {
        (self.units_x == 4 && self.delta_x != 0.0).then(|| self.delta_x.abs())
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 as f64 && x <= self.x1 as f64 + 1.0 && y >= self.y0 as f64 && y <= self.y1 as f64 + 1.0
    }

    fn mm(units: u16, delta: f64) -> Option<f64> {
        match units {
            3 if delta > 0.0 => Some(delta.abs() * 10.0),
            _ => None,
        }
    }

    /// Millimetres per pixel across, when x is a distance.
    pub fn col_mm(&self) -> Option<f64> {
        Self::mm(self.units_x, self.delta_x)
    }

    /// Millimetres per pixel down, when y is a distance.
    pub fn row_mm(&self) -> Option<f64> {
        Self::mm(self.units_y, self.delta_y)
    }

    /// `[row, column]` spacing in mm when both axes are distances (2D).
    pub fn mm_per_pixel(&self) -> Option<[f64; 2]> {
        Some([self.row_mm()?, self.col_mm()?])
    }

    /// M-mode and spectral strips: time across, distance (or velocity) down.
    pub fn is_time_across(&self) -> bool {
        self.units_x == 4
    }
}

fn read_us_regions(obj: &Obj) -> Vec<UsRegion> {
    let Some(seq) = obj.get(tags::SEQUENCE_OF_ULTRASOUND_REGIONS) else {
        return Vec::new();
    };
    let Some(items) = seq.items() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let g = Getter { obj: item };
            Some(UsRegion {
                x0: g.int(tags::REGION_LOCATION_MIN_X0)?,
                y0: g.int(tags::REGION_LOCATION_MIN_Y0)?,
                x1: g.int(tags::REGION_LOCATION_MAX_X1)?,
                y1: g.int(tags::REGION_LOCATION_MAX_Y1)?,
                spatial_format: g.int(tags::REGION_SPATIAL_FORMAT).unwrap_or(0) as u16,
                data_type: g.int(tags::REGION_DATA_TYPE).unwrap_or(0) as u16,
                units_x: g.int(tags::PHYSICAL_UNITS_X_DIRECTION).unwrap_or(0) as u16,
                units_y: g.int(tags::PHYSICAL_UNITS_Y_DIRECTION).unwrap_or(0) as u16,
                delta_x: g.f64(tags::PHYSICAL_DELTA_X).unwrap_or(0.0),
                delta_y: g.f64(tags::PHYSICAL_DELTA_Y).unwrap_or(0.0),
                ref_x0: g.int(tags::REFERENCE_PIXEL_X0).unwrap_or(0),
                ref_y0: g.int(tags::REFERENCE_PIXEL_Y0).unwrap_or(0),
                ref_value_x: g.f64(tags::REFERENCE_PIXEL_PHYSICAL_VALUE_X).unwrap_or(0.0),
                ref_value_y: g.f64(tags::REFERENCE_PIXEL_PHYSICAL_VALUE_Y).unwrap_or(0.0),
            })
        })
        .collect()
}

struct Getter<'a> {
    obj: &'a Obj,
}

impl Getter<'_> {
    fn str(&self, tag: Tag) -> String {
        self.opt_str(tag).unwrap_or_default()
    }

    fn opt_str(&self, tag: Tag) -> Option<String> {
        let e = self.obj.get(tag)?;
        let s = e.to_str().ok()?;
        let s = s.trim_end_matches('\0').trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    fn strs(&self, tag: Tag) -> Option<Vec<String>> {
        let e = self.obj.get(tag)?;
        let v = e.to_multi_str().ok()?;
        Some(v.iter().map(|s| s.trim().to_string()).collect())
    }

    fn int(&self, tag: Tag) -> Option<i64> {
        let e = self.obj.get(tag)?;
        e.to_int::<i64>().ok().or_else(|| {
            // Some writers store numbers as text with padding.
            e.to_str().ok().and_then(|s| s.trim().parse::<i64>().ok())
        })
    }

    fn f64(&self, tag: Tag) -> Option<f64> {
        let e = self.obj.get(tag)?;
        e.to_float64()
            .ok()
            .or_else(|| e.to_multi_float64().ok().and_then(|v| v.first().copied()))
    }

    fn f64s(&self, tag: Tag) -> Option<Vec<f64>> {
        let e = self.obj.get(tag)?;
        e.to_multi_float64().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patient_name_display() {
        let n = PatientName::parse("DOE^JOHN^Q");
        assert_eq!(n.display(), "DOE, JOHN Q");
        let n = PatientName::parse("Rex");
        assert_eq!(n.display(), "Rex");
        let n = PatientName::parse("");
        assert_eq!(n.display(), "(no name)");
        let n = PatientName::parse("Smith^Bella=史密斯^贝拉");
        assert_eq!(n.display(), "Smith, Bella");
    }

    #[test]
    fn date_conversion() {
        assert_eq!(dicom_date_to_iso("20240131").as_deref(), Some("2024-01-31"));
        assert_eq!(dicom_date_to_iso("2024.01.31").as_deref(), Some("2024-01-31"));
        assert_eq!(dicom_date_to_iso("2024"), None);
        assert_eq!(dicom_time_to_hms("093015.123").as_deref(), Some("09:30:15"));
        assert_eq!(dicom_time_to_hms("0930").as_deref(), Some("09:30:00"));
    }
}
