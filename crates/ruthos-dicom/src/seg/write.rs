//! Writing a Segmentation object.

use std::path::{Path, PathBuf};

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{dicom_value, DataElement, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use super::{to_cielab, Code, SegmentData, SOP_CLASS};
use crate::open::open_any;
use crate::sc::new_uid;
use crate::{DicomError, Result};

const SEGMENT_IDENTIFICATION_SEQUENCE: Tag = Tag(0x0062, 0x000A);
const REFERENCED_SEGMENT_NUMBER: Tag = Tag(0x0062, 0x000B);
const RECOMMENDED_DISPLAY_CIELAB: Tag = Tag(0x0062, 0x000D);
const SEGMENT_ALGORITHM_NAME: Tag = Tag(0x0062, 0x0009);

/// What a study's objects share, copied from a source image so that a SEG files
/// under the same patient and study.
const COPIED: &[Tag] = &[
    tags::PATIENT_NAME,
    tags::PATIENT_ID,
    tags::PATIENT_BIRTH_DATE,
    tags::PATIENT_SEX,
    tags::PATIENT_AGE,
    tags::PATIENT_WEIGHT,
    tags::PATIENT_COMMENTS,
    tags::PATIENT_SPECIES_DESCRIPTION,
    tags::PATIENT_SPECIES_CODE_SEQUENCE,
    tags::PATIENT_BREED_DESCRIPTION,
    tags::PATIENT_BREED_CODE_SEQUENCE,
    tags::BREED_REGISTRATION_SEQUENCE,
    tags::RESPONSIBLE_PERSON,
    tags::RESPONSIBLE_PERSON_ROLE,
    tags::RESPONSIBLE_ORGANIZATION,
    tags::PATIENT_SEX_NEUTERED,
    tags::STUDY_INSTANCE_UID,
    tags::STUDY_DATE,
    tags::STUDY_TIME,
    tags::STUDY_ID,
    tags::STUDY_DESCRIPTION,
    tags::ACCESSION_NUMBER,
    tags::REFERRING_PHYSICIAN_NAME,
    tags::FRAME_OF_REFERENCE_UID,
    tags::POSITION_REFERENCE_INDICATOR,
];

/// One slice of the series a segmentation was drawn on.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSlice {
    pub sop_class: String,
    pub sop_instance: String,
    pub position: [f64; 3],
}

/// The series a segmentation was drawn on, with slices in the volume's order.
#[derive(Debug, Clone)]
pub struct SourceSeries {
    pub series_uid: String,
    pub rows: u16,
    pub columns: u16,
    pub orientation: [f64; 6],
    /// Between rows, then between columns.
    pub pixel_spacing: [f64; 2],
    pub slice_thickness: Option<f64>,
    pub spacing_between_slices: f64,
    pub slices: Vec<SourceSlice>,
    /// The attributes in [`COPIED`], taken from the first slice.
    shared: InMemDicomObject,
}

fn text(obj: &InMemDicomObject, tag: Tag) -> Option<String> {
    let s = obj.get(tag)?.to_str().ok()?;
    let s = s.trim_end_matches(['\0', ' ']).to_string();
    (!s.is_empty()).then_some(s)
}

fn numbers<const N: usize>(obj: &InMemDicomObject, tag: Tag) -> Option<[f64; N]> {
    let v = obj.get(tag)?.to_multi_float64().ok()?;
    (v.len() >= N).then(|| std::array::from_fn(|i| v[i]))
}

fn missing(file: &Path, what: &str) -> DicomError {
    DicomError::Read(format!("{}: {what}", file.display()))
}

/// Read the slices of a series and order them as a volume does: along the normal,
/// a second slice at the same place dropped.
pub fn source_series(files: &[PathBuf]) -> Result<SourceSeries> {
    let first = files
        .first()
        .ok_or_else(|| DicomError::Read("The series has no images".to_string()))?;
    let mut slices = Vec::with_capacity(files.len());
    let mut template: Option<InMemDicomObject> = None;
    for file in files {
        let obj: InMemDicomObject = open_any(file, Some(tags::PIXEL_DATA))?.into_inner();
        let position = numbers::<3>(&obj, tags::IMAGE_POSITION_PATIENT)
            .ok_or_else(|| missing(file, "this image has no position, so it cannot be segmented in place"))?;
        slices.push(SourceSlice {
            sop_class: text(&obj, tags::SOP_CLASS_UID).ok_or_else(|| missing(file, "no SOP class"))?,
            sop_instance: text(&obj, tags::SOP_INSTANCE_UID).ok_or_else(|| missing(file, "no SOP instance"))?,
            position,
        });
        template.get_or_insert(obj);
    }
    let template = template.ok_or_else(|| missing(first, "no images"))?;
    let orientation = numbers::<6>(&template, tags::IMAGE_ORIENTATION_PATIENT)
        .ok_or_else(|| missing(first, "no image orientation"))?;
    let pixel_spacing =
        numbers::<2>(&template, tags::PIXEL_SPACING).ok_or_else(|| missing(first, "no pixel spacing"))?;
    let rows = template.get(tags::ROWS).and_then(|e| e.to_int::<u16>().ok());
    let columns = template.get(tags::COLUMNS).and_then(|e| e.to_int::<u16>().ok());
    let (Some(rows), Some(columns)) = (rows, columns) else {
        return Err(missing(first, "no image size"));
    };
    let normal = [
        orientation[1] * orientation[5] - orientation[2] * orientation[4],
        orientation[2] * orientation[3] - orientation[0] * orientation[5],
        orientation[0] * orientation[4] - orientation[1] * orientation[3],
    ];
    let along = |s: &SourceSlice| (0..3).map(|k| s.position[k] * normal[k]).sum::<f64>();
    slices.sort_by(|a, b| along(a).total_cmp(&along(b)));
    slices.dedup_by(|b, a| (along(a) - along(b)).abs() < 1e-4);
    let mut gaps: Vec<f64> = slices.windows(2).map(|w| along(&w[1]) - along(&w[0])).collect();
    gaps.sort_by(f64::total_cmp);
    let spacing_between_slices = gaps.get(gaps.len() / 2).copied().unwrap_or(1.0);
    let mut shared = InMemDicomObject::new_empty();
    for tag in COPIED {
        if let Some(e) = template.get(*tag) {
            shared.put(e.clone());
        }
    }
    Ok(SourceSeries {
        series_uid: text(&template, tags::SERIES_INSTANCE_UID).ok_or_else(|| missing(first, "no series UID"))?,
        rows,
        columns,
        orientation,
        pixel_spacing,
        slice_thickness: numbers::<1>(&template, tags::SLICE_THICKNESS).map(|v| v[0]),
        spacing_between_slices,
        slices,
        shared,
    })
}

/// What is decided for this particular save.
#[derive(Debug, Clone, Default)]
pub struct SegOptions {
    /// The SEG series to add to; a new one if None.
    pub series_uid: Option<String>,
    pub series_number: i32,
    pub instance_number: i32,
    pub series_description: String,
    pub creator: String,
    pub software_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SegWritten {
    pub path: PathBuf,
    pub sop_instance_uid: String,
    pub series_uid: String,
    pub frames: usize,
}

fn string(obj: &mut InMemDicomObject, tag: Tag, vr: VR, value: &str) {
    obj.put(DataElement::new(tag, vr, dicom_value!(Str, value)));
}

fn int(obj: &mut InMemDicomObject, tag: Tag, value: i32) {
    string(obj, tag, VR::IS, &value.to_string());
}

fn decimals(values: &[f64]) -> Vec<String> {
    values.iter().map(|v| decimal(*v)).collect()
}

/// A number as a Decimal String holds it, in at most 16 characters.
fn decimal(v: f64) -> String {
    let mut text = format!("{v}");
    if text.len() > 16 {
        text = format!("{v:.*}", 16usize.saturating_sub(format!("{v:.0}").len() + 1));
    }
    text.truncate(16);
    text
}

fn ds(obj: &mut InMemDicomObject, tag: Tag, values: &[f64]) {
    let v: Vec<String> = decimals(values);
    obj.put(DataElement::new(tag, VR::DS, PrimitiveValue::Strs(v.into())));
}

fn sequence(obj: &mut InMemDicomObject, tag: Tag, items: Vec<InMemDicomObject>) {
    obj.put(DataElement::new(
        tag,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED)),
    ));
}

fn code_item(code: &Code) -> InMemDicomObject {
    let mut item = InMemDicomObject::new_empty();
    string(&mut item, tags::CODE_VALUE, VR::SH, &code.value);
    string(&mut item, tags::CODING_SCHEME_DESIGNATOR, VR::SH, &code.scheme);
    string(&mut item, tags::CODE_MEANING, VR::LO, &code.meaning);
    item
}

fn dcm(value: &str, meaning: &str) -> InMemDicomObject {
    code_item(&Code {
        value: value.to_string(),
        scheme: "DCM".to_string(),
        meaning: meaning.to_string(),
    })
}

fn segment_item(data: &SegmentData) -> InMemDicomObject {
    let s = &data.segment;
    let mut item = InMemDicomObject::new_empty();
    sequence(
        &mut item,
        tags::SEGMENTED_PROPERTY_CATEGORY_CODE_SEQUENCE,
        vec![code_item(&s.category)],
    );
    item.put(DataElement::new(
        tags::SEGMENT_NUMBER,
        VR::US,
        PrimitiveValue::from(s.number),
    ));
    string(&mut item, tags::SEGMENT_LABEL, VR::LO, &s.label);
    string(&mut item, tags::SEGMENT_ALGORITHM_TYPE, VR::CS, s.algorithm.kind());
    if let Some(name) = s.algorithm.name() {
        string(&mut item, SEGMENT_ALGORITHM_NAME, VR::LO, name);
    }
    sequence(
        &mut item,
        tags::SEGMENTED_PROPERTY_TYPE_CODE_SEQUENCE,
        vec![code_item(&s.kind)],
    );
    let lab: Vec<u16> = to_cielab(s.colour).to_vec();
    item.put(DataElement::new(
        RECOMMENDED_DISPLAY_CIELAB,
        VR::US,
        PrimitiveValue::U16(lab.into()),
    ));
    item
}

fn pointer(tag: Tag) -> PrimitiveValue {
    PrimitiveValue::Tags(vec![tag].into())
}

fn dimension_item(uid: &str, index: Tag, group: Tag, label: &str) -> InMemDicomObject {
    let mut item = InMemDicomObject::new_empty();
    string(&mut item, tags::DIMENSION_ORGANIZATION_UID, VR::UI, uid);
    item.put(DataElement::new(tags::DIMENSION_INDEX_POINTER, VR::AT, pointer(index)));
    item.put(DataElement::new(tags::FUNCTIONAL_GROUP_POINTER, VR::AT, pointer(group)));
    string(&mut item, tags::DIMENSION_DESCRIPTION_LABEL, VR::LO, label);
    item
}

/// One frame's functional groups.
fn frame_item(source: &SourceSlice, segment: u16, dimension: [u32; 2]) -> InMemDicomObject {
    let mut item = InMemDicomObject::new_empty();

    let mut image = InMemDicomObject::new_empty();
    string(&mut image, tags::REFERENCED_SOP_CLASS_UID, VR::UI, &source.sop_class);
    string(
        &mut image,
        tags::REFERENCED_SOP_INSTANCE_UID,
        VR::UI,
        &source.sop_instance,
    );
    sequence(
        &mut image,
        tags::PURPOSE_OF_REFERENCE_CODE_SEQUENCE,
        vec![dcm("121322", "Source image for image processing operation")],
    );
    let mut derivation = InMemDicomObject::new_empty();
    sequence(
        &mut derivation,
        tags::DERIVATION_CODE_SEQUENCE,
        vec![dcm("113076", "Segmentation")],
    );
    sequence(&mut derivation, tags::SOURCE_IMAGE_SEQUENCE, vec![image]);
    sequence(&mut item, tags::DERIVATION_IMAGE_SEQUENCE, vec![derivation]);

    let mut content = InMemDicomObject::new_empty();
    content.put(DataElement::new(
        tags::DIMENSION_INDEX_VALUES,
        VR::UL,
        PrimitiveValue::U32(dimension.to_vec().into()),
    ));
    sequence(&mut item, tags::FRAME_CONTENT_SEQUENCE, vec![content]);

    let mut plane = InMemDicomObject::new_empty();
    ds(&mut plane, tags::IMAGE_POSITION_PATIENT, &source.position);
    sequence(&mut item, tags::PLANE_POSITION_SEQUENCE, vec![plane]);

    let mut identification = InMemDicomObject::new_empty();
    identification.put(DataElement::new(
        REFERENCED_SEGMENT_NUMBER,
        VR::US,
        PrimitiveValue::from(segment),
    ));
    sequence(&mut item, SEGMENT_IDENTIFICATION_SEQUENCE, vec![identification]);
    item
}

fn shared_groups(source: &SourceSeries) -> InMemDicomObject {
    let mut measures = InMemDicomObject::new_empty();
    ds(&mut measures, tags::PIXEL_SPACING, &source.pixel_spacing);
    ds(
        &mut measures,
        tags::SLICE_THICKNESS,
        &[source.slice_thickness.unwrap_or(source.spacing_between_slices)],
    );
    ds(
        &mut measures,
        tags::SPACING_BETWEEN_SLICES,
        &[source.spacing_between_slices],
    );
    let mut orientation = InMemDicomObject::new_empty();
    ds(&mut orientation, tags::IMAGE_ORIENTATION_PATIENT, &source.orientation);
    let mut shared = InMemDicomObject::new_empty();
    sequence(&mut shared, tags::PIXEL_MEASURES_SEQUENCE, vec![measures]);
    sequence(&mut shared, tags::PLANE_ORIENTATION_SEQUENCE, vec![orientation]);
    shared
}

/// Frames run on with no gap between them, each pixel a bit, the first in the
/// lowest bit of the first byte, and the whole padded to an even length.
fn pack(frames: &[&[bool]]) -> Vec<u8> {
    let bits: usize = frames.iter().map(|f| f.len()).sum();
    let mut out = vec![0u8; bits.div_ceil(8)];
    let mut at = 0usize;
    for frame in frames {
        for on in *frame {
            if *on {
                out[at / 8] |= 1 << (at % 8);
            }
            at += 1;
        }
    }
    if !out.len().is_multiple_of(2) {
        out.push(0);
    }
    out
}

/// Write one Segmentation object into `folder`, with every segment in `segments`.
/// Only frames holding part of a segment are written.
pub fn write_seg(
    folder: &Path,
    source: &SourceSeries,
    segments: &[SegmentData],
    options: &SegOptions,
) -> Result<SegWritten> {
    let (rows, columns) = (source.rows as usize, source.columns as usize);
    let side = rows * columns;
    // Frames sorted by segment, then along the stack.
    let mut frames: Vec<(u16, usize, &[bool])> = Vec::new();
    let mut ordered: Vec<&SegmentData> = segments.iter().collect();
    ordered.sort_by_key(|s| s.segment.number);
    for data in &ordered {
        for (z, mask) in &data.slices {
            if mask.len() != side {
                return Err(DicomError::Read(
                    "A segment's slice is not the size of the image".into(),
                ));
            }
            if *z >= source.slices.len() {
                return Err(DicomError::Read("A segment lies beyond the end of the series".into()));
            }
            if mask.iter().any(|v| *v) {
                frames.push((data.segment.number, *z, mask));
            }
        }
    }
    if frames.is_empty() {
        return Err(DicomError::Read(
            "There is nothing to save: every segment is empty".into(),
        ));
    }
    let mut used: Vec<usize> = frames.iter().map(|f| f.1).collect();
    used.sort_unstable();
    used.dedup();

    let mut obj = InMemDicomObject::new_empty();
    string(&mut obj, tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192");
    for element in source.shared.iter() {
        obj.put(element.clone());
    }
    // Required even when empty, whatever the source left out.
    for (tag, vr) in [
        (tags::PATIENT_NAME, VR::PN),
        (tags::PATIENT_ID, VR::LO),
        (tags::PATIENT_BIRTH_DATE, VR::DA),
        (tags::PATIENT_SEX, VR::CS),
        (tags::STUDY_DATE, VR::DA),
        (tags::STUDY_TIME, VR::TM),
        (tags::STUDY_ID, VR::SH),
        (tags::ACCESSION_NUMBER, VR::SH),
        (tags::REFERRING_PHYSICIAN_NAME, VR::PN),
        (tags::POSITION_REFERENCE_INDICATOR, VR::LO),
    ] {
        if obj.get(tag).is_none() {
            string(&mut obj, tag, vr, "");
        }
    }
    if obj.get(tags::FRAME_OF_REFERENCE_UID).is_none() {
        // A series with none still has one, shared with the SEG only through
        // the references, so a new one says no more than is known.
        string(&mut obj, tags::FRAME_OF_REFERENCE_UID, VR::UI, &new_uid());
    }

    let now = chrono::Local::now();
    let (date, time) = (now.format("%Y%m%d").to_string(), now.format("%H%M%S").to_string());
    let sop_uid = new_uid();
    let series_uid = options.series_uid.clone().unwrap_or_else(new_uid);

    string(&mut obj, tags::SOP_CLASS_UID, VR::UI, SOP_CLASS);
    string(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, &sop_uid);
    string(&mut obj, tags::SERIES_INSTANCE_UID, VR::UI, &series_uid);
    string(&mut obj, tags::MODALITY, VR::CS, "SEG");
    int(&mut obj, tags::SERIES_NUMBER, options.series_number);
    int(&mut obj, tags::INSTANCE_NUMBER, options.instance_number);
    string(&mut obj, tags::SERIES_DESCRIPTION, VR::LO, &options.series_description);
    string(&mut obj, tags::SERIES_DATE, VR::DA, &date);
    string(&mut obj, tags::SERIES_TIME, VR::TM, &time);
    string(&mut obj, tags::CONTENT_DATE, VR::DA, &date);
    string(&mut obj, tags::CONTENT_TIME, VR::TM, &time);
    string(&mut obj, tags::INSTANCE_CREATION_DATE, VR::DA, &date);
    string(&mut obj, tags::INSTANCE_CREATION_TIME, VR::TM, &time);

    string(&mut obj, tags::MANUFACTURER, VR::LO, "Ruthos");
    string(&mut obj, tags::MANUFACTURER_MODEL_NAME, VR::LO, "Ruthos");
    string(&mut obj, tags::DEVICE_SERIAL_NUMBER, VR::LO, "1");
    string(&mut obj, tags::SOFTWARE_VERSIONS, VR::LO, &options.software_version);

    obj.put(DataElement::new(
        tags::IMAGE_TYPE,
        VR::CS,
        PrimitiveValue::Strs(vec!["DERIVED".to_string(), "PRIMARY".to_string()].into()),
    ));
    string(&mut obj, tags::CONTENT_LABEL, VR::CS, "SEGMENTS");
    string(
        &mut obj,
        tags::CONTENT_DESCRIPTION,
        VR::LO,
        "Segments saved from Ruthos",
    );
    string(&mut obj, tags::CONTENT_CREATOR_NAME, VR::PN, &options.creator);
    string(&mut obj, tags::SEGMENTATION_TYPE, VR::CS, "BINARY");
    string(&mut obj, tags::LOSSY_IMAGE_COMPRESSION, VR::CS, "00");
    sequence(
        &mut obj,
        tags::SEGMENT_SEQUENCE,
        ordered.iter().map(|d| segment_item(d)).collect(),
    );

    let u16_element = |obj: &mut InMemDicomObject, tag: Tag, value: u16| {
        obj.put(DataElement::new(tag, VR::US, PrimitiveValue::from(value)));
    };
    u16_element(&mut obj, tags::SAMPLES_PER_PIXEL, 1);
    string(&mut obj, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    u16_element(&mut obj, tags::ROWS, source.rows);
    u16_element(&mut obj, tags::COLUMNS, source.columns);
    u16_element(&mut obj, tags::BITS_ALLOCATED, 1);
    u16_element(&mut obj, tags::BITS_STORED, 1);
    u16_element(&mut obj, tags::HIGH_BIT, 0);
    u16_element(&mut obj, tags::PIXEL_REPRESENTATION, 0);
    int(&mut obj, tags::NUMBER_OF_FRAMES, frames.len() as i32);

    let organization = new_uid();
    let mut organized = InMemDicomObject::new_empty();
    string(&mut organized, tags::DIMENSION_ORGANIZATION_UID, VR::UI, &organization);
    sequence(&mut obj, tags::DIMENSION_ORGANIZATION_SEQUENCE, vec![organized]);
    sequence(
        &mut obj,
        tags::DIMENSION_INDEX_SEQUENCE,
        vec![
            dimension_item(
                &organization,
                REFERENCED_SEGMENT_NUMBER,
                SEGMENT_IDENTIFICATION_SEQUENCE,
                "ReferencedSegmentNumber",
            ),
            dimension_item(
                &organization,
                tags::IMAGE_POSITION_PATIENT,
                tags::PLANE_POSITION_SEQUENCE,
                "ImagePositionPatient",
            ),
        ],
    );
    sequence(
        &mut obj,
        tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
        vec![shared_groups(source)],
    );
    let per_frame: Vec<InMemDicomObject> = frames
        .iter()
        .map(|(number, z, _)| {
            let place = used.binary_search(z).unwrap_or(0) as u32 + 1;
            frame_item(&source.slices[*z], *number, [*number as u32, place])
        })
        .collect();
    sequence(&mut obj, tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE, per_frame);

    let referenced: Vec<InMemDicomObject> = used
        .iter()
        .map(|z| {
            let mut item = InMemDicomObject::new_empty();
            let slice = &source.slices[*z];
            string(&mut item, tags::REFERENCED_SOP_CLASS_UID, VR::UI, &slice.sop_class);
            string(
                &mut item,
                tags::REFERENCED_SOP_INSTANCE_UID,
                VR::UI,
                &slice.sop_instance,
            );
            item
        })
        .collect();
    let mut series = InMemDicomObject::new_empty();
    string(&mut series, tags::SERIES_INSTANCE_UID, VR::UI, &source.series_uid);
    sequence(&mut series, tags::REFERENCED_INSTANCE_SEQUENCE, referenced);
    sequence(&mut obj, tags::REFERENCED_SERIES_SEQUENCE, vec![series]);

    let masks: Vec<&[bool]> = frames.iter().map(|f| f.2).collect();
    obj.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::U8(pack(&masks).into()),
    ));

    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(SOP_CLASS)
        .media_storage_sop_instance_uid(sop_uid.clone())
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .implementation_version_name("RUTHOS_0_1")
        .build()
        .map_err(|e| DicomError::Read(e.to_string()))?;
    std::fs::create_dir_all(folder)?;
    let path = folder.join(format!("{sop_uid}.dcm"));
    obj.with_exact_meta(meta)
        .write_to_file(&path)
        .map_err(|e| DicomError::Read(e.to_string()))?;
    Ok(SegWritten {
        path,
        sop_instance_uid: sop_uid,
        series_uid,
        frames: frames.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_run_on_across_frames_from_the_low_bit() {
        let a = [true, false, false, false, false];
        let b = [false, true, true, false, true];
        // 1 0 0 0 0 | 0 1 1 | 0 1
        assert_eq!(pack(&[&a, &b]), vec![0b1100_0001, 0b0000_0010]);
        // Padded to an even length.
        assert_eq!(pack(&[&a]), vec![1, 0]);
    }

    #[test]
    fn a_decimal_string_fits_its_sixteen_characters() {
        for v in [
            0.1,
            -123.456_789_012_345,
            1.0 / 3.0,
            100000.123456789,
            -0.000123456789012,
        ] {
            let s = decimal(v);
            assert!(s.len() <= 16, "{s}");
            let back: f64 = s.parse().unwrap();
            assert!((back - v).abs() < 1e-6 * v.abs().max(1.0), "{s}");
        }
    }
}
