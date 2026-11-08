//! Measurement reports as DICOM Structured Reports (TID 1500), and key images
//! as Key Object Selection documents (TID 2010), written and read back.
//!
//! A report holds one measurement group a value: its name, its coded
//! concept, the value with a UCUM unit, and the image it was taken on, with
//! its outline as spatial coordinates where it has one. A Ruthos report also
//! carries each measurement's exact tool and points in a private text item, so
//! it comes back as it was drawn; other software reads the outline and value.
//! Findings and the conclusion go in Qualitative Evaluations.

use std::path::{Path, PathBuf};

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{DataElement, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use crate::open::open_any;
use crate::sc::new_uid;
use crate::seg::write::{add_required_empty, COPIED};
use crate::seg::Code;
use crate::{DicomError, Result};

/// Comprehensive SR, which allows spatial coordinates.
pub const COMPREHENSIVE_SR: &str = "1.2.840.10008.5.1.4.1.1.88.33";
pub const KEY_OBJECT_SELECTION: &str = "1.2.840.10008.5.1.4.1.1.88.59";
/// SR classes a measurement report may come in.
pub const SR_CLASSES: &[&str] = &[
    "1.2.840.10008.5.1.4.1.1.88.11",
    "1.2.840.10008.5.1.4.1.1.88.22",
    COMPREHENSIVE_SR,
    "1.2.840.10008.5.1.4.1.1.88.34",
];

/// Ruthos's own codes, for what no public scheme names.
pub const SCHEME: &str = "99RUTHOS";
const DRAWN: &str = "drawn";

type Obj = InMemDicomObject;

fn code(value: &str, scheme: &str, meaning: &str) -> Code {
    Code {
        value: value.into(),
        scheme: scheme.into(),
        meaning: meaning.into(),
    }
}

fn dcm(value: &str, meaning: &str) -> Code {
    code(value, "DCM", meaning)
}

/// An outline on an image, in pixels from the top-left corner of the image.
#[derive(Debug, Clone, PartialEq)]
pub enum Outline {
    Point([f32; 2]),
    /// Open, or closed when its last point repeats its first.
    Polyline(Vec<[f32; 2]>),
    /// The ends of the major axis, then of the minor.
    Ellipse([[f32; 2]; 4]),
}

/// An image a measurement was taken on.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub sop_class: String,
    pub sop_instance: String,
    pub series_uid: String,
    /// From 1, for a multi-frame image.
    pub frame: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SrMeasurement {
    /// What the person named it, also its tracking identifier.
    pub name: String,
    pub concept: Code,
    pub value: f64,
    /// UCUM.
    pub unit: String,
    pub image: Option<ImageRef>,
    pub outline: Option<Outline>,
    /// Ruthos's tool and points, as JSON, so a Ruthos report comes back as drawn.
    pub drawn: Option<String>,
    /// A calculation from other values, with no image of its own.
    pub calculated: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SrReport {
    pub measurements: Vec<SrMeasurement>,
    pub findings: String,
    pub conclusion: String,
}

// ------------------------------------------------------------ building

fn string(obj: &mut Obj, tag: Tag, vr: VR, value: &str) {
    obj.put(DataElement::new(tag, vr, PrimitiveValue::from(value)));
}

fn sequence(tag: Tag, items: Vec<Obj>) -> DataElement<InMemDicomObject> {
    DataElement::new(
        tag,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED)),
    )
}

fn code_item(c: &Code) -> Obj {
    let mut o = Obj::new_empty();
    string(&mut o, tags::CODE_VALUE, VR::SH, &c.value);
    string(&mut o, tags::CODING_SCHEME_DESIGNATOR, VR::SH, &c.scheme);
    string(&mut o, tags::CODE_MEANING, VR::LO, &c.meaning);
    o
}

/// A content item: how it relates to its parent, its type, and its name.
fn item(relation: &str, value_type: &str, concept: Option<&Code>) -> Obj {
    let mut o = Obj::new_empty();
    string(&mut o, tags::RELATIONSHIP_TYPE, VR::CS, relation);
    string(&mut o, tags::VALUE_TYPE, VR::CS, value_type);
    if let Some(c) = concept {
        o.put(sequence(tags::CONCEPT_NAME_CODE_SEQUENCE, vec![code_item(c)]));
    }
    o
}

fn with_children(mut o: Obj, children: Vec<Obj>) -> Obj {
    if !children.is_empty() {
        o.put(sequence(tags::CONTENT_SEQUENCE, children));
    }
    o
}

fn container(relation: &str, concept: &Code, children: Vec<Obj>) -> Obj {
    let mut o = item(relation, "CONTAINER", Some(concept));
    string(&mut o, tags::CONTINUITY_OF_CONTENT, VR::CS, "SEPARATE");
    with_children(o, children)
}

fn text(relation: &str, concept: &Code, value: &str) -> Obj {
    let mut o = item(relation, "TEXT", Some(concept));
    string(&mut o, tags::TEXT_VALUE, VR::UT, value);
    o
}

fn coded(relation: &str, concept: &Code, value: &Code) -> Obj {
    let mut o = item(relation, "CODE", Some(concept));
    o.put(sequence(tags::CONCEPT_CODE_SEQUENCE, vec![code_item(value)]));
    o
}

fn uid_item(relation: &str, concept: &Code, uid: &str) -> Obj {
    let mut o = item(relation, "UIDREF", Some(concept));
    string(&mut o, tags::UID, VR::UI, uid);
    o
}

fn image_item(relation: &str, concept: Option<&Code>, r: &ImageRef) -> Obj {
    let mut o = item(relation, "IMAGE", concept);
    let mut sop = Obj::new_empty();
    string(&mut sop, tags::REFERENCED_SOP_CLASS_UID, VR::UI, &r.sop_class);
    string(&mut sop, tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, &r.sop_instance);
    if let Some(f) = r.frame {
        string(&mut sop, tags::REFERENCED_FRAME_NUMBER, VR::IS, &f.to_string());
    }
    o.put(sequence(tags::REFERENCED_SOP_SEQUENCE, vec![sop]));
    o
}

fn num(relation: &str, concept: &Code, value: f64, unit: &str, children: Vec<Obj>) -> Obj {
    let mut o = item(relation, "NUM", Some(concept));
    let mut measured = Obj::new_empty();
    string(&mut measured, tags::NUMERIC_VALUE, VR::DS, &decimal(value));
    measured.put(sequence(
        tags::MEASUREMENT_UNITS_CODE_SEQUENCE,
        vec![code_item(&code(unit, "UCUM", unit))],
    ));
    o.put(sequence(tags::MEASURED_VALUE_SEQUENCE, vec![measured]));
    with_children(o, children)
}

/// A value as a plain decimal of at most 16 characters, as a DS holds,
/// to six places; an exponent only for numbers too large or small for that.
pub fn decimal(value: f64) -> String {
    let plain = format!("{value:.6}");
    let plain = plain.trim_end_matches('0').trim_end_matches('.');
    let plain = if plain == "-0" { "0" } else { plain };
    if plain.len() <= 16 && (value == 0.0 || plain.trim_start_matches(['-', '0', '.']).len() >= 2) {
        plain.to_string()
    } else {
        format!("{value:.6e}")
    }
}

fn outline_item(outline: &Outline, image: &ImageRef) -> Obj {
    let (kind, points): (&str, Vec<[f32; 2]>) = match outline {
        Outline::Point(p) => ("POINT", vec![*p]),
        Outline::Polyline(p) => ("POLYLINE", p.clone()),
        Outline::Ellipse(p) => ("ELLIPSE", p.to_vec()),
    };
    let mut o = item("INFERRED FROM", "SCOORD", Some(&dcm("111030", "Image Region")));
    o.put(DataElement::new(
        tags::GRAPHIC_DATA,
        VR::FL,
        PrimitiveValue::F32(points.iter().flatten().copied().collect()),
    ));
    string(&mut o, tags::GRAPHIC_TYPE, VR::CS, kind);
    with_children(o, vec![image_item("SELECTED FROM", None, image)])
}

fn measurement_group(m: &SrMeasurement) -> Obj {
    let mut evidence = Vec::new();
    match (&m.outline, &m.image) {
        (Some(outline), Some(image)) => evidence.push(outline_item(outline, image)),
        (None, Some(image)) => evidence.push(image_item(
            "INFERRED FROM",
            Some(&dcm("121112", "Source of Measurement")),
            image,
        )),
        _ => {}
    }
    let mut children = vec![
        text("HAS OBS CONTEXT", &dcm("112039", "Tracking Identifier"), &m.name),
        uid_item(
            "HAS OBS CONTEXT",
            &dcm("112040", "Tracking Unique Identifier"),
            &new_uid(),
        ),
    ];
    if m.calculated {
        children.push(coded(
            "HAS CONCEPT MOD",
            &dcm("121401", "Derivation"),
            &code("calculated", SCHEME, "Calculated from other measurements"),
        ));
    }
    children.push(num("CONTAINS", &m.concept, m.value, &m.unit, evidence));
    if let Some(drawn) = &m.drawn {
        children.push(text(
            "CONTAINS",
            &code(DRAWN, SCHEME, "Ruthos measurement as drawn"),
            drawn,
        ));
    }
    container("CONTAINS", &dcm("125007", "Measurement Group"), children)
}

/// The study's references, as Current Requested Procedure Evidence wants them.
fn evidence(study_uid: &str, images: &[&ImageRef]) -> Option<DataElement<InMemDicomObject>> {
    let mut series: Vec<(String, Vec<Obj>)> = Vec::new();
    for r in images {
        let mut sop = Obj::new_empty();
        string(&mut sop, tags::REFERENCED_SOP_CLASS_UID, VR::UI, &r.sop_class);
        string(&mut sop, tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, &r.sop_instance);
        let i = match series.iter().position(|(uid, _)| *uid == r.series_uid) {
            Some(i) => i,
            None => {
                series.push((r.series_uid.clone(), Vec::new()));
                series.len() - 1
            }
        };
        if !series[i].1.iter().any(|o| o == &sop) {
            series[i].1.push(sop);
        }
    }
    if series.is_empty() {
        return None;
    }
    let series_items = series
        .into_iter()
        .map(|(uid, sops)| {
            let mut s = Obj::new_empty();
            string(&mut s, tags::SERIES_INSTANCE_UID, VR::UI, &uid);
            s.put(sequence(tags::REFERENCED_SOP_SEQUENCE, sops));
            s
        })
        .collect();
    let mut study = Obj::new_empty();
    string(&mut study, tags::STUDY_INSTANCE_UID, VR::UI, study_uid);
    study.put(sequence(tags::REFERENCED_SERIES_SEQUENCE, series_items));
    Some(sequence(
        tags::CURRENT_REQUESTED_PROCEDURE_EVIDENCE_SEQUENCE,
        vec![study],
    ))
}

/// The patient, study and document headers both kinds of document share.
fn document(
    template: &Path,
    sop_class: &str,
    modality: &str,
    series_number: &str,
    description: &str,
) -> Result<(Obj, String)> {
    let source: Obj = open_any(template, Some(tags::PIXEL_DATA))?.into_inner();
    let mut obj = Obj::new_empty();
    string(&mut obj, tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192");
    for tag in COPIED {
        if let Some(element) = source.get(*tag) {
            obj.put(element.clone());
        }
    }
    add_required_empty(&mut obj);
    // Image-only attributes a document does not have.
    obj.remove_element(tags::FRAME_OF_REFERENCE_UID);
    obj.remove_element(tags::POSITION_REFERENCE_INDICATOR);
    let now = chrono::Local::now();
    let (date, time) = (now.format("%Y%m%d").to_string(), now.format("%H%M%S").to_string());
    let sop = new_uid();
    string(&mut obj, tags::SOP_CLASS_UID, VR::UI, sop_class);
    string(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, &sop);
    string(&mut obj, tags::MODALITY, VR::CS, modality);
    string(&mut obj, tags::SERIES_INSTANCE_UID, VR::UI, &new_uid());
    string(&mut obj, tags::SERIES_NUMBER, VR::IS, series_number);
    string(&mut obj, tags::SERIES_DESCRIPTION, VR::LO, description);
    string(&mut obj, tags::SERIES_DATE, VR::DA, &date);
    string(&mut obj, tags::SERIES_TIME, VR::TM, &time);
    string(&mut obj, tags::INSTANCE_NUMBER, VR::IS, "1");
    string(&mut obj, tags::CONTENT_DATE, VR::DA, &date);
    string(&mut obj, tags::CONTENT_TIME, VR::TM, &time);
    string(&mut obj, tags::MANUFACTURER, VR::LO, "Ruthos");
    string(&mut obj, tags::SOFTWARE_VERSIONS, VR::LO, env!("CARGO_PKG_VERSION"));
    obj.put(sequence(tags::REFERENCED_PERFORMED_PROCEDURE_STEP_SEQUENCE, Vec::new()));
    Ok((obj, sop))
}

fn write(obj: Obj, sop_class: &str, sop: &str, folder: &Path) -> Result<PathBuf> {
    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(sop_class)
        .media_storage_sop_instance_uid(sop)
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .implementation_version_name("RUTHOS_0_1")
        .build()
        .map_err(|e| DicomError::Pixels(e.to_string()))?;
    std::fs::create_dir_all(folder).map_err(|e| DicomError::Pixels(e.to_string()))?;
    let file = folder.join(format!("{sop}.dcm"));
    obj.with_exact_meta(meta)
        .write_to_file(&file)
        .map_err(|e| DicomError::Pixels(e.to_string()))?;
    Ok(file)
}

/// Write a measurement report for the study `template` belongs to.
pub fn write_sr(template: &Path, report: &SrReport, folder: &Path) -> Result<PathBuf> {
    let (mut obj, sop) = document(template, COMPREHENSIVE_SR, "SR", "9600", "Ruthos measurement report")?;
    let study_uid = obj
        .get(tags::STUDY_INSTANCE_UID)
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim_end_matches('\0').to_string())
        .unwrap_or_default();
    string(&mut obj, tags::COMPLETION_FLAG, VR::CS, "COMPLETE");
    string(&mut obj, tags::VERIFICATION_FLAG, VR::CS, "UNVERIFIED");
    obj.put(sequence(tags::PERFORMED_PROCEDURE_CODE_SEQUENCE, Vec::new()));
    // Ruthos's own codes, declared as a private coding scheme.
    let mut scheme = Obj::new_empty();
    string(&mut scheme, tags::CODING_SCHEME_DESIGNATOR, VR::SH, SCHEME);
    string(
        &mut scheme,
        tags::CODING_SCHEME_NAME,
        VR::ST,
        "Ruthos measurement definitions",
    );
    string(
        &mut scheme,
        tags::CODING_SCHEME_RESPONSIBLE_ORGANIZATION,
        VR::ST,
        "Ruthos project",
    );
    obj.put(sequence(tags::CODING_SCHEME_IDENTIFICATION_SEQUENCE, vec![scheme]));
    let images: Vec<&ImageRef> = report.measurements.iter().filter_map(|m| m.image.as_ref()).collect();
    if let Some(e) = evidence(&study_uid, &images) {
        obj.put(e);
    }

    let device = new_uid();
    let mut library_group = Vec::new();
    for r in &images {
        let entry = image_item("CONTAINS", None, r);
        if !library_group.contains(&entry) {
            library_group.push(entry);
        }
    }
    let mut content = vec![
        coded(
            "HAS CONCEPT MOD",
            &dcm("121049", "Language of Content Item and Descendants"),
            &code("en", "RFC5646", "English"),
        ),
        coded(
            "HAS OBS CONTEXT",
            &dcm("121005", "Observer Type"),
            &dcm("121007", "Device"),
        ),
        uid_item("HAS OBS CONTEXT", &dcm("121012", "Device Observer UID"), &device),
        text("HAS OBS CONTEXT", &dcm("121013", "Device Observer Name"), "Ruthos"),
        coded(
            "HAS CONCEPT MOD",
            &dcm("121058", "Procedure reported"),
            &code("363679005", "SCT", "Imaging procedure"),
        ),
    ];
    if !library_group.is_empty() {
        content.push(container(
            "CONTAINS",
            &dcm("111028", "Image Library"),
            vec![container(
                "CONTAINS",
                &dcm("126200", "Image Library Group"),
                library_group,
            )],
        ));
    }
    content.push(container(
        "CONTAINS",
        &dcm("126010", "Imaging Measurements"),
        report.measurements.iter().map(measurement_group).collect(),
    ));
    let mut evaluations = Vec::new();
    if !report.findings.trim().is_empty() {
        evaluations.push(text("CONTAINS", &dcm("121071", "Finding"), report.findings.trim()));
    }
    if !report.conclusion.trim().is_empty() {
        evaluations.push(text("CONTAINS", &dcm("121077", "Conclusion"), report.conclusion.trim()));
    }
    if !evaluations.is_empty() {
        content.push(container(
            "CONTAINS",
            &code("C0034375", "UMLS", "Qualitative Evaluations"),
            evaluations,
        ));
    }

    string(&mut obj, tags::VALUE_TYPE, VR::CS, "CONTAINER");
    obj.put(sequence(
        tags::CONCEPT_NAME_CODE_SEQUENCE,
        vec![code_item(&dcm("126000", "Imaging Measurement Report"))],
    ));
    string(&mut obj, tags::CONTINUITY_OF_CONTENT, VR::CS, "SEPARATE");
    let mut template_id = Obj::new_empty();
    string(&mut template_id, tags::MAPPING_RESOURCE, VR::CS, "DCMR");
    string(&mut template_id, tags::TEMPLATE_IDENTIFIER, VR::CS, "1500");
    obj.put(sequence(tags::CONTENT_TEMPLATE_SEQUENCE, vec![template_id]));
    obj.put(sequence(tags::CONTENT_SEQUENCE, content));
    write(obj, COMPREHENSIVE_SR, &sop, folder)
}

/// Write a Key Object Selection document naming the key images.
pub fn write_kos(template: &Path, images: &[ImageRef], folder: &Path) -> Result<PathBuf> {
    let (mut obj, sop) = document(template, KEY_OBJECT_SELECTION, "KO", "9601", "Ruthos key images")?;
    let study_uid = obj
        .get(tags::STUDY_INSTANCE_UID)
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim_end_matches('\0').to_string())
        .unwrap_or_default();
    let refs: Vec<&ImageRef> = images.iter().collect();
    if let Some(e) = evidence(&study_uid, &refs) {
        obj.put(e);
    }
    string(&mut obj, tags::VALUE_TYPE, VR::CS, "CONTAINER");
    obj.put(sequence(
        tags::CONCEPT_NAME_CODE_SEQUENCE,
        vec![code_item(&dcm("113000", "Of Interest"))],
    ));
    string(&mut obj, tags::CONTINUITY_OF_CONTENT, VR::CS, "SEPARATE");
    let mut template_id = Obj::new_empty();
    string(&mut template_id, tags::MAPPING_RESOURCE, VR::CS, "DCMR");
    string(&mut template_id, tags::TEMPLATE_IDENTIFIER, VR::CS, "2010");
    obj.put(sequence(tags::CONTENT_TEMPLATE_SEQUENCE, vec![template_id]));
    let mut content = vec![text(
        "CONTAINS",
        &dcm("113012", "Key Object Description"),
        "Key images chosen in Ruthos",
    )];
    content.extend(images.iter().map(|r| image_item("CONTAINS", None, r)));
    obj.put(sequence(tags::CONTENT_SEQUENCE, content));
    write(obj, KEY_OBJECT_SELECTION, &sop, folder)
}

// ------------------------------------------------------------- reading

fn text_of(o: &Obj, tag: Tag) -> String {
    o.get(tag)
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim_end_matches(['\0', ' ']).to_string())
        .unwrap_or_default()
}

fn items(o: &Obj, tag: Tag) -> Vec<Obj> {
    o.get(tag)
        .and_then(|e| e.items())
        .map(|i| i.to_vec())
        .unwrap_or_default()
}

fn concept_of(o: &Obj, tag: Tag) -> Option<Code> {
    let c = items(o, tag).into_iter().next()?;
    Some(code(
        &text_of(&c, tags::CODE_VALUE),
        &text_of(&c, tags::CODING_SCHEME_DESIGNATOR),
        &text_of(&c, tags::CODE_MEANING),
    ))
}

fn image_of(o: &Obj) -> Option<ImageRef> {
    let sop = items(o, tags::REFERENCED_SOP_SEQUENCE).into_iter().next()?;
    Some(ImageRef {
        sop_class: text_of(&sop, tags::REFERENCED_SOP_CLASS_UID),
        sop_instance: text_of(&sop, tags::REFERENCED_SOP_INSTANCE_UID),
        series_uid: String::new(),
        frame: text_of(&sop, tags::REFERENCED_FRAME_NUMBER).trim().parse().ok(),
    })
}

fn outline_of(o: &Obj) -> Option<Outline> {
    let data: Vec<f32> = o.get(tags::GRAPHIC_DATA)?.to_multi_float32().ok()?;
    let points: Vec<[f32; 2]> = data.as_chunks::<2>().0.to_vec();
    match text_of(o, tags::GRAPHIC_TYPE).as_str() {
        "POINT" => points.first().map(|p| Outline::Point(*p)),
        "POLYLINE" if points.len() >= 2 => Some(Outline::Polyline(points)),
        "POLYGON" if points.len() >= 3 => {
            let mut closed = points.clone();
            closed.push(points[0]);
            Some(Outline::Polyline(closed))
        }
        "ELLIPSE" if points.len() == 4 => Some(Outline::Ellipse([points[0], points[1], points[2], points[3]])),
        _ => None,
    }
}

/// Every content item below `o`, depth first.
fn walk(o: &Obj, out: &mut Vec<Obj>) {
    for child in items(o, tags::CONTENT_SEQUENCE) {
        out.push(child.clone());
        walk(&child, out);
    }
}

/// One measurement group's measurements: each NUM, with the group's tracking
/// identifier as its name, and its image and outline.
fn read_group(group: &Obj, out: &mut Vec<SrMeasurement>) {
    let children = items(group, tags::CONTENT_SEQUENCE);
    let concept_is =
        |o: &Obj, value: &str| concept_of(o, tags::CONCEPT_NAME_CODE_SEQUENCE).is_some_and(|c| c.value == value);
    let tracking = children
        .iter()
        .find(|c| concept_is(c, "112039"))
        .map(|c| text_of(c, tags::TEXT_VALUE));
    let drawn = children
        .iter()
        .find(|c| {
            concept_of(c, tags::CONCEPT_NAME_CODE_SEQUENCE).is_some_and(|k| k.value == DRAWN && k.scheme == SCHEME)
        })
        .map(|c| text_of(c, tags::TEXT_VALUE));
    let calculated = children.iter().any(|c| concept_is(c, "121401"));
    for n in children.iter().filter(|c| text_of(c, tags::VALUE_TYPE) == "NUM") {
        let Some(concept) = concept_of(n, tags::CONCEPT_NAME_CODE_SEQUENCE) else {
            continue;
        };
        let Some(measured) = items(n, tags::MEASURED_VALUE_SEQUENCE).into_iter().next() else {
            continue;
        };
        let Ok(value) = text_of(&measured, tags::NUMERIC_VALUE).trim().parse::<f64>() else {
            continue;
        };
        let unit = concept_of(&measured, tags::MEASUREMENT_UNITS_CODE_SEQUENCE)
            .map(|u| u.value)
            .unwrap_or_default();
        let mut below = Vec::new();
        walk(n, &mut below);
        let scoord = below.iter().find(|c| text_of(c, tags::VALUE_TYPE) == "SCOORD");
        let image = scoord
            .and_then(|s| items(s, tags::CONTENT_SEQUENCE).iter().find_map(image_of))
            .or_else(|| {
                below
                    .iter()
                    .filter(|c| text_of(c, tags::VALUE_TYPE) == "IMAGE")
                    .find_map(image_of)
            });
        out.push(SrMeasurement {
            name: tracking
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| concept.meaning.clone()),
            concept,
            value,
            unit,
            image,
            outline: scoord.and_then(outline_of),
            drawn: drawn.clone(),
            calculated,
        });
    }
}

/// Read a measurement report: TID 1500 measurement groups, and Findings and
/// Conclusion texts wherever they are.
pub fn read_sr(path: &Path) -> Result<SrReport> {
    let obj: Obj = open_any(path, None)?.into_inner();
    let mut all = Vec::new();
    walk(&obj, &mut all);
    let mut report = SrReport::default();
    for o in &all {
        let concept = concept_of(o, tags::CONCEPT_NAME_CODE_SEQUENCE);
        match (
            text_of(o, tags::VALUE_TYPE).as_str(),
            concept.as_ref().map(|c| c.value.as_str()),
        ) {
            ("CONTAINER", Some("125007")) => read_group(o, &mut report.measurements),
            ("TEXT", Some("121071")) => report.findings = text_of(o, tags::TEXT_VALUE),
            ("TEXT", Some("121077")) => report.conclusion = text_of(o, tags::TEXT_VALUE),
            _ => {}
        }
    }
    Ok(report)
}

/// The SOP Instance UIDs a Key Object Selection document names.
pub fn read_kos(path: &Path) -> Result<Vec<String>> {
    let obj: Obj = open_any(path, None)?.into_inner();
    let mut all = Vec::new();
    walk(&obj, &mut all);
    Ok(all
        .iter()
        .filter(|o| text_of(o, tags::VALUE_TYPE) == "IMAGE")
        .filter_map(image_of)
        .map(|r| r.sop_instance)
        .collect())
}

/// A coded concept from a definition's `LN:...`, `SCT:...` or `DCM:...`, or
/// Ruthos's own code from its key.
pub fn concept_for(concept: Option<&str>, key: &str, label: &str) -> Code {
    let scheme = |s: &str| match s {
        "LN" => "LN",
        "SCT" => "SCT",
        "DCM" => "DCM",
        _ => "",
    };
    match concept.and_then(|c| c.split_once(':')) {
        Some((s, v)) if !scheme(s).is_empty() => code(v, scheme(s), label),
        _ => code(if key.is_empty() { label } else { key }, SCHEME, label),
    }
}

/// A Ruthos unit as UCUM, or nothing for one that is not a true size.
pub fn ucum(unit: &str) -> Option<&'static str> {
    Some(match unit {
        "mm" => "mm",
        "cm" => "cm",
        "mm2" => "mm2",
        "cm2" => "cm2",
        "ml" | "mL" => "mL",
        "deg" | "\u{b0}" => "deg",
        "%" => "%",
        "s" => "s",
        "ms" => "ms",
        "cm/s" => "cm/s",
        "m/s" => "m/s",
        "mmHg" => "mm[Hg]",
        "bpm" | "/min" => "/min",
        "cm/s2" => "cm/s2",
        "" => "1",
        _ => return None,
    })
}

/// A UCUM unit back as Ruthos writes it.
pub fn from_ucum(unit: &str) -> String {
    match unit {
        "mm[Hg]" => "mmHg".into(),
        "/min" => "bpm".into(),
        "1" => String::new(),
        "mL" => "ml".into(),
        u => u.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom/CT_small.dcm")
    }

    /// The second is a frame of a loop: only a multi-frame image names a frame.
    fn image(n: u32) -> ImageRef {
        ImageRef {
            sop_class: if n == 2 {
                uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE.into()
            } else {
                uids::CT_IMAGE_STORAGE.into()
            },
            sop_instance: format!("1.2.826.0.1.3680043.10.1465.{n}"),
            series_uid: "1.2.826.0.1.3680043.10.1465.500".into(),
            frame: if n == 2 { Some(3) } else { None },
        }
    }

    fn report() -> SrReport {
        SrReport {
            measurements: vec![
                SrMeasurement {
                    name: "LVIDd".into(),
                    concept: concept_for(Some("LN:29436-3"), "lv.lvidd", "LVIDd"),
                    value: 41.5,
                    unit: "mm".into(),
                    image: Some(image(1)),
                    outline: Some(Outline::Polyline(vec![[10.0, 20.0], [10.0, 61.5]])),
                    drawn: Some("{\"kind\":\"length\"}".into()),
                    calculated: false,
                },
                SrMeasurement {
                    name: "Area".into(),
                    concept: concept_for(None, "", "Area"),
                    value: 120.25,
                    unit: "mm2".into(),
                    image: Some(image(2)),
                    outline: Some(Outline::Ellipse([[0.0, 5.0], [10.0, 5.0], [5.0, 2.0], [5.0, 8.0]])),
                    drawn: None,
                    calculated: false,
                },
                SrMeasurement {
                    name: "Fractional shortening".into(),
                    concept: concept_for(None, "lv.fs", "Fractional shortening"),
                    value: 33.3,
                    unit: "%".into(),
                    image: None,
                    outline: None,
                    drawn: None,
                    calculated: true,
                },
            ],
            findings: "Normal size.".into(),
            conclusion: "No change since last time.".into(),
        }
    }

    #[test]
    fn a_report_comes_back_as_it_went_in() {
        let folder = std::env::temp_dir().join(format!("ruthos-sr-{}", std::process::id()));
        let file = write_sr(&sample(), &report(), &folder).unwrap();
        let back = read_sr(&file).unwrap();
        let want = report();
        assert_eq!(back.findings, want.findings);
        assert_eq!(back.conclusion, want.conclusion);
        assert_eq!(back.measurements.len(), 3);
        for (b, w) in back.measurements.iter().zip(&want.measurements) {
            assert_eq!(b.name, w.name);
            assert_eq!(b.concept, w.concept);
            assert!((b.value - w.value).abs() < 1e-9);
            assert_eq!(b.unit, w.unit);
            assert_eq!(b.outline, w.outline);
            assert_eq!(b.drawn, w.drawn);
            assert_eq!(b.calculated, w.calculated);
            assert_eq!(
                b.image.as_ref().map(|i| (&i.sop_instance, i.frame)),
                w.image.as_ref().map(|i| (&i.sop_instance, i.frame))
            );
        }
        assert_eq!(back.measurements[0].concept.scheme, "LN");
        assert_eq!(back.measurements[1].concept.scheme, SCHEME);
        // For `dciodvfy` in CI.
        if let Some(interop) = std::env::var_os("RUTHOS_SEG_INTEROP") {
            std::fs::copy(&file, PathBuf::from(interop).join("ruthos-sr.dcm")).unwrap();
        }
        std::fs::remove_dir_all(folder).ok();
    }

    #[test]
    fn key_images_come_back() {
        let folder = std::env::temp_dir().join(format!("ruthos-kos-{}", std::process::id()));
        let whole = |n| ImageRef {
            frame: None,
            ..image(n)
        };
        let file = write_kos(&sample(), &[whole(1), whole(2)], &folder).unwrap();
        assert_eq!(
            read_kos(&file).unwrap(),
            vec![image(1).sop_instance, image(2).sop_instance]
        );
        let meta = crate::read_meta(&file).unwrap();
        assert_eq!(meta.modality, "KO");
        if let Some(interop) = std::env::var_os("RUTHOS_SEG_INTEROP") {
            std::fs::copy(&file, PathBuf::from(interop).join("ruthos-kos.dcm")).unwrap();
        }
        std::fs::remove_dir_all(folder).ok();
    }

    #[test]
    fn values_are_plain_decimals() {
        assert_eq!(decimal(4.988295000000001), "4.988295");
        assert_eq!(decimal(50.856), "50.856");
        assert_eq!(decimal(40.0), "40");
        assert_eq!(decimal(-0.0000001), "-1.000000e-7");
        assert_eq!(decimal(0.0), "0");
        assert!(decimal(1.0e20).len() <= 16);
    }

    #[test]
    fn units_go_to_ucum_and_back() {
        for u in ["mm", "cm2", "mmHg", "bpm", "%", "ml", ""] {
            assert_eq!(from_ucum(ucum(u).unwrap()), u);
        }
        assert_eq!(ucum("px"), None, "pixels are not a true size");
    }
}
