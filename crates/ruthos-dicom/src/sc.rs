//! Secondary Capture: turning ordinary pictures into DICOM.
//!
//! Clinic photos, dermatology shots, endoscopy stills and scanned films
//! arrive as JPEG or PNG. Horos imports them as Secondary Capture objects
//! after asking for the patient; this module does the conversion. Pixels are
//! stored as uncompressed 8-bit RGB (EXIF orientation applied), one instance
//! per picture, all in one series.

use std::path::{Path, PathBuf};

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
use image::ImageDecoder;

use crate::{DicomError, Result};

/// What the user fills in before importing pictures.
#[derive(Debug, Clone, Default)]
pub struct PictureInfo {
    /// Free text; "Smith, Rex" or "Rex" both work.
    pub patient_name: String,
    pub patient_id: String,
    /// `YYYY-MM-DD` or `YYYYMMDD`, optional.
    pub birth_date: String,
    /// `M`, `F`, `O` or empty.
    pub sex: String,
    pub neutered: Option<bool>,
    pub species: String,
    pub breed: String,
    pub responsible_person: String,
    pub study_description: String,
    pub series_description: String,
    /// `YYYY-MM-DD` or `YYYYMMDD`; empty means today.
    pub study_date: String,
    /// DICOM modality, e.g. `XC` (external camera), `OT`, `ES`, `GM`.
    pub modality: String,
    /// Reuse an existing study (adds a new series to it) instead of
    /// creating a new one.
    pub existing_study_uid: Option<String>,
    /// Longest edge in pixels; larger pictures are scaled down.
    pub max_edge: Option<u32>,
}

/// A fresh UID in the `2.25.<uuid>` form.
pub fn new_uid() -> String {
    format!("2.25.{}", uuid::Uuid::new_v4().as_u128())
}

/// Convert each picture and write the DICOM files into `out_dir`.
/// Returns the written paths in input order.
pub fn convert_pictures(pictures: &[PathBuf], info: &PictureInfo, out_dir: &Path) -> Result<Vec<PathBuf>> {
    if pictures.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(out_dir)?;
    let study_uid = info.existing_study_uid.clone().unwrap_or_else(new_uid);
    let series_uid = new_uid();
    let now = chrono::Local::now();
    let study_date = normalise_date(&info.study_date).unwrap_or_else(|| now.format("%Y%m%d").to_string());
    let time = now.format("%H%M%S").to_string();

    let mut out = Vec::with_capacity(pictures.len());
    for (i, path) in pictures.iter().enumerate() {
        let rgb = load_rgb(path, info.max_edge)?;
        let sop_uid = new_uid();
        let picture = if is_grayscale(&rgb) { Picture::Gray(to_gray(&rgb)) } else { Picture::Rgb(rgb) };
        let obj = build_object(&picture, info, &study_uid, &series_uid, &sop_uid, &study_date, &time, i as u32 + 1);
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(uids::SECONDARY_CAPTURE_IMAGE_STORAGE)
            .media_storage_sop_instance_uid(sop_uid.clone())
            .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
            .implementation_version_name("RUTHOS_0_1")
            .build()
            .map_err(|e| DicomError::Read(e.to_string()))?;
        let file_path = out_dir.join(format!("{sop_uid}.dcm"));
        obj.with_exact_meta(meta)
            .write_to_file(&file_path)
            .map_err(|e| DicomError::Read(e.to_string()))?;
        out.push(file_path);
    }
    Ok(out)
}

/// Decode a picture to 8-bit RGB with EXIF orientation applied and an
/// optional size cap.
pub fn load_rgb(path: &Path, max_edge: Option<u32>) -> Result<image::RgbImage> {
    let reader = image::ImageReader::open(path)?
        .with_guessed_format()
        .map_err(|e| DicomError::Unsupported(format!("{}: {e}", path.display())))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| DicomError::Unsupported(format!("{}: {e}", path.display())))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| DicomError::Unsupported(format!("{}: {e}", path.display())))?;
    img.apply_orientation(orientation);
    if let Some(max) = max_edge {
        let (w, h) = (img.width(), img.height());
        let long = w.max(h);
        if long > max && long > 0 {
            let scale = max as f64 / long as f64;
            let nw = ((w as f64 * scale).round() as u32).max(1);
            let nh = ((h as f64 * scale).round() as u32).max(1);
            img = img.resize(nw, nh, image::imageops::FilterType::Triangle);
        }
    }
    Ok(img.to_rgb8())
}

/// Pixel payload chosen for a picture.
enum Picture {
    Rgb(image::RgbImage),
    /// Exported radiographs and scanned films are grayscale JPEGs; storing
    /// them as MONOCHROME2 makes window/level and CLUTs work and keeps the
    /// file a third of the size.
    Gray(image::GrayImage),
}

/// True when practically every pixel has equal R, G and B.
pub fn is_grayscale(img: &image::RgbImage) -> bool {
    let n = (img.width() as usize) * (img.height() as usize);
    if n == 0 {
        return false;
    }
    let allowed_off = n / 1000; // 0.1% of pixels may carry JPEG chroma noise
    let mut off = 0usize;
    for p in img.pixels() {
        let [r, g, b] = p.0;
        if r.abs_diff(g) > 6 || g.abs_diff(b) > 6 || r.abs_diff(b) > 6 {
            off += 1;
            if off > allowed_off {
                return false;
            }
        }
    }
    true
}

fn to_gray(img: &image::RgbImage) -> image::GrayImage {
    let mut out = image::GrayImage::new(img.width(), img.height());
    for (src, dst) in img.pixels().zip(out.pixels_mut()) {
        let [r, g, b] = src.0;
        dst.0[0] = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_object(
    picture: &Picture,
    info: &PictureInfo,
    study_uid: &str,
    series_uid: &str,
    sop_uid: &str,
    study_date: &str,
    time: &str,
    instance_number: u32,
) -> InMemDicomObject {
    let (w, h, samples, photometric, mut pixels) = match picture {
        Picture::Rgb(rgb) => {
            let (w, h) = rgb.dimensions();
            (w, h, 3u16, "RGB", rgb.as_raw().clone())
        }
        Picture::Gray(gray) => {
            let (w, h) = gray.dimensions();
            (w, h, 1u16, "MONOCHROME2", gray.as_raw().clone())
        }
    };
    if pixels.len() % 2 == 1 {
        pixels.push(0);
    }
    let mut obj = InMemDicomObject::new_empty();
    let mut put_str = |tag, vr, s: &str| {
        if !s.trim().is_empty() {
            obj.put(DataElement::new(tag, vr, dicom_value!(Str, s.trim())));
        }
    };
    put_str(tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192");
    put_str(tags::SOP_CLASS_UID, VR::UI, uids::SECONDARY_CAPTURE_IMAGE_STORAGE);
    put_str(tags::SOP_INSTANCE_UID, VR::UI, sop_uid);
    put_str(tags::STUDY_DATE, VR::DA, study_date);
    put_str(tags::SERIES_DATE, VR::DA, study_date);
    put_str(tags::CONTENT_DATE, VR::DA, study_date);
    put_str(tags::STUDY_TIME, VR::TM, time);
    put_str(tags::SERIES_TIME, VR::TM, time);
    put_str(tags::CONTENT_TIME, VR::TM, time);
    put_str(tags::MODALITY, VR::CS, if info.modality.trim().is_empty() { "OT" } else { &info.modality });
    put_str(tags::CONVERSION_TYPE, VR::CS, "WSD");
    put_str(tags::MANUFACTURER, VR::LO, "Ruthos");
    put_str(tags::STUDY_DESCRIPTION, VR::LO, &info.study_description);
    put_str(
        tags::SERIES_DESCRIPTION,
        VR::LO,
        if info.series_description.trim().is_empty() { "Pictures" } else { &info.series_description },
    );
    put_str(tags::PATIENT_NAME, VR::PN, &to_person_name(&info.patient_name));
    put_str(tags::PATIENT_ID, VR::LO, &info.patient_id);
    if let Some(d) = normalise_date(&info.birth_date) {
        put_str(tags::PATIENT_BIRTH_DATE, VR::DA, &d);
    }
    put_str(tags::PATIENT_SEX, VR::CS, &info.sex.to_ascii_uppercase());
    if let Some(n) = info.neutered {
        put_str(tags::PATIENT_SEX_NEUTERED, VR::CS, if n { "ALTERED" } else { "UNALTERED" });
    }
    put_str(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, &info.species);
    put_str(tags::PATIENT_BREED_DESCRIPTION, VR::LO, &info.breed);
    put_str(tags::RESPONSIBLE_PERSON, VR::PN, &to_person_name(&info.responsible_person));
    put_str(tags::STUDY_INSTANCE_UID, VR::UI, study_uid);
    put_str(tags::SERIES_INSTANCE_UID, VR::UI, series_uid);
    put_str(tags::STUDY_ID, VR::SH, "1");
    put_str(tags::SERIES_NUMBER, VR::IS, "1");
    put_str(tags::INSTANCE_NUMBER, VR::IS, &instance_number.to_string());
    put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, photometric);
    put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
    if samples == 1 {
        put_str(tags::WINDOW_CENTER, VR::DS, "128");
        put_str(tags::WINDOW_WIDTH, VR::DS, "256");
    }

    obj.put(DataElement::new(tags::IMAGE_TYPE, VR::CS, dicom_value!(Strs, ["DERIVED", "SECONDARY"])));
    obj.put(DataElement::new(tags::SAMPLES_PER_PIXEL, VR::US, dicom_value!(U16, [samples])));
    if samples == 3 {
        obj.put(DataElement::new(tags::PLANAR_CONFIGURATION, VR::US, dicom_value!(U16, [0])));
    }
    obj.put(DataElement::new(tags::ROWS, VR::US, dicom_value!(U16, [h as u16])));
    obj.put(DataElement::new(tags::COLUMNS, VR::US, dicom_value!(U16, [w as u16])));
    obj.put(DataElement::new(tags::BITS_ALLOCATED, VR::US, dicom_value!(U16, [8])));
    obj.put(DataElement::new(tags::BITS_STORED, VR::US, dicom_value!(U16, [8])));
    obj.put(DataElement::new(tags::HIGH_BIT, VR::US, dicom_value!(U16, [7])));
    obj.put(DataElement::new(tags::PIXEL_REPRESENTATION, VR::US, dicom_value!(U16, [0])));
    obj.put(DataElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::from(pixels)));
    obj
}

/// "Smith, Rex" -> "Smith^Rex"; "Rex" -> "Rex"; already-caret names pass through.
pub fn to_person_name(s: &str) -> String {
    let s = s.trim();
    if s.contains('^') {
        return s.to_string();
    }
    match s.split_once(',') {
        Some((family, given)) => format!("{}^{}", family.trim(), given.trim()),
        None => s.to_string(),
    }
}

/// Accepts `YYYY-MM-DD`, `YYYY/MM/DD`, `DD/MM/YYYY` or `YYYYMMDD`; returns DA.
pub fn normalise_date(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 8 {
        return None;
    }
    // Day-first (DD/MM/YYYY) when the last four digits look like a year and
    // the first two cannot be a year prefix.
    let separators = s.chars().filter(|c| !c.is_ascii_digit()).count();
    let (y, m, d) = if separators > 0 && !s.starts_with(|c: char| c.is_ascii_digit() && s[..4].chars().all(|c| c.is_ascii_digit())) {
        (&digits[4..8], &digits[2..4], &digits[0..2])
    } else {
        (&digits[0..4], &digits[4..6], &digits[6..8])
    };
    let month: u32 = m.parse().ok()?;
    let day: u32 = d.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{y}{m}{d}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_dates() {
        assert_eq!(to_person_name("Smith, Rex"), "Smith^Rex");
        assert_eq!(to_person_name("Rex"), "Rex");
        assert_eq!(normalise_date("2024-03-09").as_deref(), Some("20240309"));
        assert_eq!(normalise_date("20240309").as_deref(), Some("20240309"));
        assert_eq!(normalise_date("09/03/2024").as_deref(), Some("20240309"));
        assert_eq!(normalise_date("2024-13-01"), None);
        assert_eq!(normalise_date(""), None);
    }

    #[test]
    fn round_trip_picture() {
        let dir = std::env::temp_dir().join(format!("ruthos-sc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbImage::new(64, 48);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([x as u8 * 4, y as u8 * 5, 128]);
        }
        let png = dir.join("photo.png");
        img.save(&png).unwrap();

        let info = PictureInfo {
            patient_name: "Smith, Rex".into(),
            patient_id: "V42".into(),
            species: "Canine".into(),
            modality: "XC".into(),
            study_description: "Skin lesion".into(),
            ..Default::default()
        };
        let files = convert_pictures(&[png], &info, &dir.join("out")).unwrap();
        assert_eq!(files.len(), 1);

        let meta = crate::read_meta(&files[0]).unwrap();
        assert_eq!(meta.patient_name.display(), "Smith, Rex");
        assert_eq!(meta.modality, "XC");
        assert_eq!((meta.rows, meta.columns), (48, 64));
        assert_eq!(meta.patient_species.as_deref(), Some("Canine"));
        assert_eq!(meta.sop_class_uid, uids::SECONDARY_CAPTURE_IMAGE_STORAGE);

        let frame = crate::decode_frame(&files[0], 0).unwrap();
        assert!(frame.is_color());
        assert_eq!((frame.width, frame.height), (64, 48));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grayscale_picture_becomes_monochrome() {
        let dir = std::env::temp_dir().join(format!("ruthos-sc-gray-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbImage::new(40, 30);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            let v = (x * 6) as u8;
            *p = image::Rgb([v, v, v]);
        }
        assert!(is_grayscale(&img));
        let jpg = dir.join("xray.jpg");
        img.save(&jpg).unwrap();

        let info = PictureInfo {
            patient_name: "Rex".into(),
            modality: "OT".into(),
            ..Default::default()
        };
        let files = convert_pictures(&[jpg], &info, &dir.join("out")).unwrap();
        let meta = crate::read_meta(&files[0]).unwrap();
        assert_eq!(meta.photometric_interpretation, "MONOCHROME2");
        assert_eq!(meta.samples_per_pixel, 1);
        assert_eq!(meta.window_center, Some(128.0));

        let frame = crate::decode_frame(&files[0], 0).unwrap();
        assert!(!frame.is_color());
        assert!(frame.max > frame.min);
        assert!(frame.value_at(39, 0).unwrap() > frame.value_at(0, 0).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
