//! Teaching copies: the case register, export orchestration, the sidecar
//! and the export log.
//!
//! A teaching copy is a de-identified copy of a study under a teaching name
//! and reference. The register in the archive database is the only link
//! from a reference back to its source studies, and it never leaves the
//! source archive. Nothing is written to a destination until the
//! verification pass has read the whole copy back and found it clean.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::FileMetaTableBuilder;

use ruthos_dicom::deid::{
    apply_mask, build_teaching_dataset, collect_context, derive_uid, regions_union, verify_file, verify_text,
    DateShift, DeidParams, Finding, MaskPlan, MaskTemplate, Rect, VerifyContext,
};
use ruthos_dicom::open::open_any;
use ruthos_dicom::read_meta;

use crate::db::Database;
use crate::import::{import_file, FileOutcome, ImportMode};
use crate::model::{ImageRow, RoiRecord, SeriesRow, StudyRow};
use crate::paths::DataPaths;
use crate::{CoreError, Result};

pub const SIDECAR_NAME: &str = "ruthos-case.json";
pub const TEACHING_ALBUM: &str = "Teaching";
pub const LOG_NAME: &str = "anonymiser.log";
const SOFTWARE: &str = env!("CARGO_PKG_VERSION");

/// A row of the register.
#[derive(Debug, Clone, PartialEq)]
pub struct TeachingCase {
    pub id: i64,
    pub reference: String,
    pub name: String,
    pub title: String,
    pub secret: [u8; 32],
    pub offset_days: i64,
    pub preserve_season: bool,
    pub note: String,
    pub created: i64,
}

/// A case that does not exist yet; it is written to the register only when
/// an export succeeds.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCase {
    pub reference: String,
    pub name: String,
    pub title: String,
    pub preserve_season: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaseChoice {
    New(NewCase),
    Existing(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Folder(PathBuf),
    Archive,
    Both(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeasurementTreatment {
    Include,
    AnswerKey,
    LeaveOut,
}

/// How a series' pixels are handled, as decided in the dialog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MaskDecision {
    /// Everything outside the ultrasound regions blanked.
    Regions(Vec<Rect>),
    /// Regions kept, and these rectangles blanked as well (a header band the
    /// machine puts inside its region, from a template or a review).
    RegionsReviewed { keep: Vec<Rect>, blank: Vec<Rect> },
    /// A device template's rectangles blanked.
    Template { name: String, rects: Vec<Rect> },
    /// Burned In Annotation says NO on a trusted modality: copied as is.
    Trusted,
    /// A person drew (or approved) these rectangles.
    Reviewed(Vec<Rect>),
}

impl MaskDecision {
    pub fn label(&self) -> &'static str {
        match self {
            MaskDecision::Regions(_) => "regions",
            MaskDecision::RegionsReviewed { .. } => "regions+reviewed",
            MaskDecision::Template { .. } => "template",
            MaskDecision::Trusted => "trusted",
            MaskDecision::Reviewed(_) => "reviewed",
        }
    }

    fn plan(&self) -> Option<MaskPlan> {
        match self {
            MaskDecision::Regions(r) => Some(MaskPlan::KeepOnly(r.clone())),
            MaskDecision::RegionsReviewed { keep, blank } => Some(MaskPlan::Combined {
                keep: keep.clone(),
                blank: blank.clone(),
            }),
            MaskDecision::Template { rects, .. } => Some(MaskPlan::Blank(rects.clone())),
            MaskDecision::Reviewed(r) => Some(MaskPlan::Blank(r.clone())),
            MaskDecision::Trusted => None,
        }
    }
}

/// What the dialog proposes for a series before the user has looked.
#[derive(Debug, Clone, PartialEq)]
pub enum SeriesClassification {
    Regions(Vec<Rect>),
    /// Regions plus a saved device template for this machine.
    RegionsWithTemplate {
        keep: Vec<Rect>,
        name: String,
        blank: Vec<Rect>,
    },
    Template {
        name: String,
        rects: Vec<Rect>,
    },
    Trusted,
    NeedsReview,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportRequest {
    pub study_id: i64,
    pub case: CaseChoice,
    /// Overrides the case title for this export when given.
    pub title: Option<String>,
    pub destination: Destination,
    pub measurements: MeasurementTreatment,
    /// Findings and conclusion the user chose to include.
    pub report_text: Option<(String, String)>,
    pub masks: HashMap<i64, MaskDecision>,
    /// Accepted descriptor hits: (attribute path, reason).
    pub overrides: Vec<(String, String)>,
    /// Top-level descriptors the user chose to blank, as `(gggg,eeee)`.
    pub blank_attributes: Vec<String>,
}

/// Parse `(gggg,eeee)` into a tag.
pub fn parse_tag_path(path: &str) -> Option<dicom_core::Tag> {
    let p = path.trim().trim_start_matches('(').trim_end_matches(')');
    let (g, e) = p.split_once(',')?;
    Some(dicom_core::Tag(
        u16::from_str_radix(g.trim(), 16).ok()?,
        u16::from_str_radix(e.trim(), 16).ok()?,
    ))
}

/// The measurements, calibrations and key images that travel with a copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sidecar {
    pub format: u32,
    pub reference: String,
    pub name: String,
    pub title: String,
    pub answer_key: bool,
    pub measurements: Vec<SidecarMeasurement>,
    pub calibrations: Vec<SidecarCalibration>,
    pub key_images: Vec<String>,
    pub findings: Option<String>,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidecarMeasurement {
    pub sop_instance_uid: String,
    pub frame: i64,
    pub kind: String,
    pub points: Vec<[f32; 2]>,
    pub name: Option<String>,
    pub value: Option<f64>,
    pub unit: String,
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub def_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidecarCalibration {
    pub sop_instance_uid: String,
    pub row_mm: f64,
    pub col_mm: f64,
}

/// A copy written to a temporary folder and verified, awaiting commit.
#[derive(Debug)]
pub struct Prepared {
    pub request: ExportRequest,
    pub study: StudyRow,
    pub case: CaseChoice,
    pub secret: [u8; 32],
    pub offset_days: i64,
    pub preserve_season: bool,
    pub reference: String,
    pub name: String,
    pub title: String,
    pub temp_dir: PathBuf,
    pub files: Vec<PathBuf>,
    pub series_count: usize,
    pub image_count: usize,
    pub findings: Vec<Finding>,
    pub overridden: Vec<(Finding, String)>,
    pub sidecar: Sidecar,
}

impl Prepared {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportResult {
    pub reference: String,
    pub folder: Option<PathBuf>,
    pub archive_study_id: Option<i64>,
    pub series: usize,
    pub images: usize,
    pub log_path: PathBuf,
}

// ------------------------------------------------------------ suggestions

const SURNAMES: &[&str] = &[
    "Hartley",
    "Fenwick",
    "Ashcombe",
    "Calloway",
    "Redmayne",
    "Thackeray",
    "Winslow",
    "Barrow",
    "Pemberton",
    "Oakes",
    "Landry",
    "Marchant",
    "Holloway",
    "Dunmore",
    "Ellery",
    "Faraday",
    "Greaves",
    "Haverford",
    "Kendrick",
    "Lyle",
    "Mortimer",
    "Norbury",
    "Prescott",
    "Quayle",
    "Rowntree",
    "Sedgwick",
    "Tillman",
    "Underhill",
    "Verity",
    "Whitlock",
    "Yardley",
    "Abernathy",
    "Blakeney",
    "Crossley",
    "Dashwood",
    "Everly",
    "Fairfax",
    "Garrick",
    "Hazelwood",
    "Ingram",
];
const HORSE_NAMES: &[&str] = &[
    "Bramble", "Copper", "Dapple", "Ember", "Finch", "Gulliver", "Hazel", "Indigo", "Juniper", "Kestrel", "Larkspur",
    "Merlin", "Nutmeg", "Onyx", "Pippin", "Quill", "Rowan", "Sorrel", "Tansy", "Umber", "Velvet", "Willow", "Yarrow",
    "Zephyr", "Alder", "Biscuit", "Clover", "Dune", "Echo", "Fable",
];
const DOG_NAMES: &[&str] = &[
    "Barnaby",
    "Clementine",
    "Digby",
    "Elsie",
    "Fergus",
    "Gwen",
    "Humphrey",
    "Ivy",
    "Jasper",
    "Kit",
    "Lottie",
    "Monty",
    "Nell",
    "Otis",
    "Peggy",
    "Quincy",
    "Rufus",
    "Sadie",
    "Teddy",
    "Una",
    "Vinnie",
    "Wren",
    "Ziggy",
    "Alfie",
    "Bonnie",
    "Cosmo",
    "Dottie",
    "Ernie",
    "Flora",
    "Gus",
];
const CAT_NAMES: &[&str] = &[
    "Agatha", "Basil", "Clio", "Dorian", "Edith", "Figaro", "Greta", "Hugo", "Isolde", "Jinx", "Kiki", "Lupin",
    "Mabel", "Nico", "Olive", "Pixel", "Quentin", "Rhubarb", "Sylvie", "Toulouse", "Ursula", "Vesper", "Wilbur",
    "Xanthe", "Yoshi", "Zelda", "Amber", "Bertram", "Cleo", "Dexter",
];
const OTHER_NAMES: &[&str] = &[
    "Acorn",
    "Button",
    "Cinnamon",
    "Dandelion",
    "Elm",
    "Fennel",
    "Ginger",
    "Honey",
    "Iris",
    "Jasmine",
    "Kiwi",
    "Lemon",
    "Maple",
    "Nugget",
    "Olive",
    "Pebble",
    "Quince",
    "Rosemary",
    "Saffron",
    "Thistle",
];

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).expect("system randomness");
    u64::from_le_bytes(b)
}

fn random_secret() -> [u8; 32] {
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).expect("system randomness");
    s
}

/// A random offset: 365 to 3650 days, or whole years with preserve season.
pub fn random_offset_days(preserve_season: bool) -> i64 {
    if preserve_season {
        365 * (1 + (random_u64() % 10) as i64)
    } else {
        365 + (random_u64() % (3650 - 365 + 1)) as i64
    }
}

fn animal_names(species: Option<&str>) -> &'static [&'static str] {
    let s = species.unwrap_or("").to_ascii_lowercase();
    if s.contains("equ") || s.contains("horse") || s.contains("pony") {
        HORSE_NAMES
    } else if s.contains("can") || s.contains("dog") {
        DOG_NAMES
    } else if s.contains("fel") || s.contains("cat") {
        CAT_NAMES
    } else {
        OTHER_NAMES
    }
}

/// Characters allowed in a teaching name or reference.
pub fn valid_label(s: &str, allow_caret: bool) -> bool {
    !s.trim().is_empty()
        && s.chars().count() <= 64
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '\'' || (allow_caret && c == '^'))
}

/// Whether a proposed name or reference contains a sensitive token.
pub fn conflicts_with_tokens(label: &str, ctx: &VerifyContext) -> Option<String> {
    verify_text("label", label, ctx, false)
        .into_iter()
        .next()
        .map(|f| f.matched)
}

/// Suggest a fictitious name (`Surname^Animal`) that matches no sensitive
/// token and no name already in the register.
pub fn suggest_name(species: Option<&str>, taken: &[String], ctx: &VerifyContext) -> String {
    let animals = animal_names(species);
    for _ in 0..200 {
        let surname = SURNAMES[(random_u64() % SURNAMES.len() as u64) as usize];
        let animal = animals[(random_u64() % animals.len() as u64) as usize];
        let name = format!("{surname}^{animal}");
        if taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            continue;
        }
        if conflicts_with_tokens(&name, ctx).is_some() {
            continue;
        }
        return name;
    }
    format!("Case^{}", random_u64() % 10000)
}

/// The next reference from the prefix and the running number in the meta table.
pub fn suggest_reference(db: &Database, prefix: &str) -> String {
    let n = db
        .meta_get("teaching_next_number")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1);
    let prefix = if prefix.trim().is_empty() {
        "RUTHOS-2026"
    } else {
        prefix.trim()
    };
    format!("{prefix}-{n:04}")
}

// ------------------------------------------------------------- templates

fn templates_dir(paths: &DataPaths) -> PathBuf {
    paths.root.join("MASKS")
}

pub fn load_templates(paths: &DataPaths) -> Vec<MaskTemplate> {
    let Ok(entries) = std::fs::read_dir(templates_dir(paths)) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<MaskTemplate>(&t).ok())
        .collect()
}

pub fn save_template(paths: &DataPaths, template: &MaskTemplate) -> Result<PathBuf> {
    let dir = templates_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!(
        "{}.json",
        MaskTemplate::key(&template.manufacturer, &template.model, template.columns, template.rows)
    ));
    std::fs::write(
        &file,
        serde_json::to_string_pretty(template).map_err(|e| CoreError::Other(e.to_string()))?,
    )?;
    Ok(file)
}

pub fn find_template(
    paths: &DataPaths,
    manufacturer: &str,
    model: &str,
    columns: u32,
    rows: u32,
) -> Option<MaskTemplate> {
    let key = MaskTemplate::key(manufacturer, model, columns, rows);
    load_templates(paths)
        .into_iter()
        .find(|t| MaskTemplate::key(&t.manufacturer, &t.model, t.columns, t.rows) == key)
}

/// What to propose for a series: regions, a template, trusted, or review.
pub fn classify_series(db: &Database, paths: &DataPaths, series: &SeriesRow) -> SeriesClassification {
    let Ok(Some(image)) = db.representative_image(series.id) else {
        return SeriesClassification::NeedsReview;
    };
    let Ok(meta) = read_meta(&paths.resolve(&image.path)) else {
        return SeriesClassification::NeedsReview;
    };
    let modality = series.modality.trim().to_ascii_uppercase();
    if modality == "US" && !meta.us_regions.is_empty() {
        let keep = regions_union(&meta.us_regions);
        if let Some(t) = find_template(paths, &meta.manufacturer, &meta.model_name, meta.columns, meta.rows) {
            return SeriesClassification::RegionsWithTemplate {
                keep,
                name: MaskTemplate::key(&t.manufacturer, &t.model, t.columns, t.rows),
                blank: t.rects,
            };
        }
        return SeriesClassification::Regions(keep);
    }
    if let Some(t) = find_template(paths, &meta.manufacturer, &meta.model_name, meta.columns, meta.rows) {
        return SeriesClassification::Template {
            name: MaskTemplate::key(&t.manufacturer, &t.model, t.columns, t.rows),
            rects: t.rects,
        };
    }
    let trusted_modality = matches!(modality.as_str(), "CT" | "MR" | "CR" | "DX" | "MG" | "PT" | "NM");
    if trusted_modality && meta.burned_in_annotation.trim().eq_ignore_ascii_case("NO") {
        return SeriesClassification::Trusted;
    }
    SeriesClassification::NeedsReview
}

// --------------------------------------------------------------- export

/// Build the copy in a temporary folder and verify it. Nothing touches the
/// destination; `commit` does that, `abandon` deletes the folder.
pub fn prepare(
    db: &Database,
    paths: &DataPaths,
    request: ExportRequest,
    progress: &dyn Fn(usize, usize, &str),
) -> Result<Prepared> {
    prepare_cancellable(db, paths, request, progress, &std::sync::atomic::AtomicBool::new(false))
}

/// `prepare`, stopping between images when `cancel` is set. A cancelled run
/// leaves nothing behind.
pub fn prepare_cancellable(
    db: &Database,
    paths: &DataPaths,
    request: ExportRequest,
    progress: &dyn Fn(usize, usize, &str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Prepared> {
    let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
    let study = db
        .study(request.study_id)?
        .ok_or_else(|| CoreError::Other("study not found".into()))?;
    let series_list = db.series_for_study(study.id)?;
    let mut images: Vec<(SeriesRow, ImageRow)> = Vec::new();
    for s in &series_list {
        for img in db.images_for_series(s.id)? {
            images.push((s.clone(), img));
        }
    }
    if images.is_empty() {
        return Err(CoreError::Other("the study has no images".into()));
    }
    // A SEG has no burned-in text to mask.
    let segmentation = |s: &SeriesRow| s.modality == crate::seg::MODALITY;
    for s in &series_list {
        if !segmentation(s) && !request.masks.contains_key(&s.id) {
            return Err(CoreError::Other(format!(
                "series {} has no masking decision",
                s.display_name()
            )));
        }
    }

    // Sensitive tokens, UIDs and dates from every source file.
    let mut ctx = VerifyContext::default();
    for (_, img) in &images {
        let obj = open_any(&paths.resolve(&img.path), None)?;
        ctx.absorb(collect_context(&obj));
    }

    // The case: existing from the register, or new and still in memory.
    let (secret, offset_days, preserve_season, reference, name, mut title) = match &request.case {
        CaseChoice::Existing(id) => {
            let c = db
                .teaching_case(*id)?
                .ok_or_else(|| CoreError::Other("teaching case not found".into()))?;
            (c.secret, c.offset_days, c.preserve_season, c.reference, c.name, c.title)
        }
        CaseChoice::New(n) => {
            if !valid_label(&n.reference, false) {
                return Err(CoreError::Other("the teaching reference may use letters, digits, spaces, hyphens and apostrophes, up to 64 characters".into()));
            }
            if !valid_label(&n.name, true) {
                return Err(CoreError::Other("the teaching name may use letters, digits, spaces, hyphens, apostrophes and ^, up to 64 characters".into()));
            }
            if db.teaching_case_by_reference(&n.reference)?.is_some() {
                return Err(CoreError::Other(format!(
                    "reference {} already exists; pick it from the list to add this study to that case",
                    n.reference
                )));
            }
            if db
                .teaching_cases()?
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(&n.name))
            {
                return Err(CoreError::Other(format!(
                    "teaching name {} is already used by another case",
                    n.name
                )));
            }
            (
                random_secret(),
                random_offset_days(n.preserve_season),
                n.preserve_season,
                n.reference.clone(),
                n.name.clone(),
                n.title.clone(),
            )
        }
    };
    if let Some(t) = &request.title {
        title = t.clone();
    }
    if let Some(tok) = conflicts_with_tokens(&name, &ctx) {
        return Err(CoreError::Other(format!(
            "the teaching name contains \"{tok}\", which identifies the source"
        )));
    }
    if let Some(tok) = conflicts_with_tokens(&reference, &ctx) {
        return Err(CoreError::Other(format!(
            "the teaching reference contains \"{tok}\", which identifies the source"
        )));
    }

    // Temporary folder on the destination volume.
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let temp_dir = match &request.destination {
        Destination::Folder(d) | Destination::Both(d) => d.join(format!(".ruthos-export-{stamp}")),
        Destination::Archive => paths.temp_dir().join(format!("export-{stamp}")),
    };
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let dates = DateShift::new(offset_days, preserve_season);
    let params = DeidParams {
        teaching_name: &name,
        reference: &reference,
        title: Some(&title),
        secret: &secret,
        dates,
        software: SOFTWARE,
    };

    let mut files = Vec::new();
    let mut sidecar = Sidecar {
        format: 1,
        reference: reference.clone(),
        name: name.clone(),
        title: title.clone(),
        answer_key: request.measurements == MeasurementTreatment::AnswerKey,
        ..Default::default()
    };
    let total = images.len();
    let result: Result<()> = (|| {
        for (i, (series, img)) in images.iter().enumerate() {
            if cancelled() {
                return Err(CoreError::Other("cancelled".into()));
            }
            progress(i, total, &series.display_name());
            let src_path = paths.resolve(&img.path);
            let obj = open_any(&src_path, None)?;
            let source_ts = obj.meta().transfer_syntax().trim_end_matches('\0').to_string();
            let mut out = build_teaching_dataset(&obj, &params);
            for path in &request.blank_attributes {
                if let Some(tag) = parse_tag_path(path) {
                    out.remove_element(tag);
                }
            }
            let plan = if segmentation(series) {
                None
            } else {
                request.masks[&series.id].plan()
            };
            let ts = match plan {
                Some(plan) => {
                    let masked = apply_mask(&obj, &plan)?;
                    let put_u16 = |out: &mut dicom_object::InMemDicomObject, tag, v: u16| {
                        out.put(DataElement::new(tag, VR::US, dicom_value!(U16, [v])));
                    };
                    put_u16(&mut out, tags::ROWS, masked.rows as u16);
                    put_u16(&mut out, tags::COLUMNS, masked.columns as u16);
                    put_u16(&mut out, tags::SAMPLES_PER_PIXEL, masked.samples_per_pixel);
                    put_u16(&mut out, tags::BITS_ALLOCATED, masked.bits_allocated);
                    put_u16(&mut out, tags::BITS_STORED, masked.bits_stored);
                    put_u16(&mut out, tags::HIGH_BIT, masked.high_bit);
                    put_u16(&mut out, tags::PIXEL_REPRESENTATION, masked.pixel_representation);
                    if masked.samples_per_pixel > 1 {
                        put_u16(&mut out, tags::PLANAR_CONFIGURATION, 0);
                    }
                    out.put(DataElement::new(
                        tags::PHOTOMETRIC_INTERPRETATION,
                        VR::CS,
                        dicom_value!(Str, masked.photometric.as_str()),
                    ));
                    out.put(DataElement::new(
                        tags::NUMBER_OF_FRAMES,
                        VR::IS,
                        dicom_value!(Str, masked.frames.to_string()),
                    ));
                    let vr = if masked.bits_allocated > 8 { VR::OW } else { VR::OB };
                    out.put(DataElement::new(
                        tags::PIXEL_DATA,
                        vr,
                        PrimitiveValue::from(masked.data),
                    ));
                    out.put(DataElement::new(
                        tags::BURNED_IN_ANNOTATION,
                        VR::CS,
                        dicom_value!(Str, "NO"),
                    ));
                    uids::EXPLICIT_VR_LITTLE_ENDIAN.to_string()
                }
                None => source_ts,
            };
            let new_sop = derive_uid(&secret, &img.sop_instance_uid);
            let sop_class = obj
                .get(tags::SOP_CLASS_UID)
                .and_then(|e| e.value().to_str().ok().map(|s| s.to_string()))
                .unwrap_or_default();
            let meta = FileMetaTableBuilder::new()
                .transfer_syntax(ts.trim_end_matches('\0'))
                .media_storage_sop_class_uid(sop_class.trim_end_matches('\0'))
                .media_storage_sop_instance_uid(new_sop.clone())
                .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
                .implementation_version_name("RUTHOS_0_1")
                .build()
                .map_err(|e| CoreError::Other(e.to_string()))?;
            let file = temp_dir.join(format!("{new_sop}.dcm"));
            out.with_exact_meta(meta)
                .write_to_file(&file)
                .map_err(|e| CoreError::Other(e.to_string()))?;
            files.push(file);

            // Measurements, calibrations and key images, re-keyed.
            if request.measurements != MeasurementTreatment::LeaveOut {
                let frames = img.number_of_frames.max(1);
                for frame in 0..frames {
                    for r in db.rois(img.id, frame)? {
                        sidecar.measurements.push(SidecarMeasurement {
                            sop_instance_uid: new_sop.clone(),
                            frame,
                            kind: r.kind,
                            points: r.points,
                            name: r.name,
                            value: r.value,
                            unit: r.unit,
                            basis: r.basis,
                            def_key: r.def_key,
                        });
                    }
                }
            }
            if let Some([row_mm, col_mm]) = db.calibration(img.id)? {
                sidecar.calibrations.push(SidecarCalibration {
                    sop_instance_uid: new_sop.clone(),
                    row_mm,
                    col_mm,
                });
            }
            if img.is_key_image {
                sidecar.key_images.push(new_sop.clone());
            }
        }
        if let Some((findings, conclusion)) = &request.report_text {
            sidecar.findings = Some(findings.clone());
            sidecar.conclusion = Some(conclusion.clone());
        }
        let json = serde_json::to_string_pretty(&sidecar).map_err(|e| CoreError::Other(e.to_string()))?;
        std::fs::write(temp_dir.join(SIDECAR_NAME), json)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(e);
    }

    // The independent pass over what is on disk. It knows the shift, so a
    // correctly shifted date that coincides with another original date of
    // the study is not taken for a leak.
    ctx.date_shift = Some(dates);
    let mut findings = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if cancelled() {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(CoreError::Other("cancelled".into()));
        }
        progress(i, files.len(), &format!("verifying image {} of {}", i + 1, files.len()));
        findings.extend(verify_file(f, &ctx));
    }
    progress(files.len(), files.len(), "verified");
    let sidecar_text = std::fs::read_to_string(temp_dir.join(SIDECAR_NAME)).unwrap_or_default();
    findings.extend(verify_text(SIDECAR_NAME, &sidecar_text, &ctx, false));
    findings.extend(verify_text("teaching name", &name, &ctx, false));
    findings.extend(verify_text("teaching reference", &reference, &ctx, false));
    findings.extend(verify_text("teaching title", &title, &ctx, true));
    if let Some((f, c)) = &request.report_text {
        findings.extend(verify_text("findings", f, &ctx, true));
        findings.extend(verify_text("conclusion", c, &ctx, true));
    }
    // Accepted descriptor hits.
    let mut overridden = Vec::new();
    findings.retain(|f| {
        if !f.overridable {
            return true;
        }
        match request.overrides.iter().find(|(p, _)| *p == f.path) {
            Some((_, reason)) => {
                overridden.push((f.clone(), reason.clone()));
                false
            }
            None => true,
        }
    });

    Ok(Prepared {
        series_count: series_list.len(),
        image_count: images.len(),
        request,
        study,
        case: CaseChoice::Existing(0), // replaced below
        secret,
        offset_days,
        preserve_season,
        reference,
        name,
        title,
        temp_dir,
        files,
        findings,
        overridden,
        sidecar,
    })
    .map(|mut p| {
        p.case = p.request.case.clone();
        p
    })
}

/// Delete the temporary folder of a prepared copy.
pub fn abandon(prepared: Prepared) {
    let _ = std::fs::remove_dir_all(&prepared.temp_dir);
}

/// Move a clean copy into place and record it in the register and the log.
pub fn commit(db: &Database, paths: &DataPaths, prepared: Prepared) -> Result<ExportResult> {
    if !prepared.is_clean() {
        abandon(prepared);
        return Err(CoreError::Other(
            "the verification pass found problems; the copy was not written".into(),
        ));
    }
    let Prepared {
        request,
        study,
        secret,
        offset_days,
        preserve_season,
        reference,
        name,
        title,
        temp_dir,
        files,
        series_count,
        image_count,
        overridden,
        sidecar,
        ..
    } = prepared;

    let mut folder_out = None;
    let mut archive_study = None;
    let result: Result<()> = (|| {
        match &request.destination {
            Destination::Folder(dest) | Destination::Both(dest) => {
                let final_dir = dest.join(&reference);
                if !final_dir.exists() {
                    std::fs::rename(&temp_dir, &final_dir).or_else(|_| copy_dir(&temp_dir, &final_dir))?;
                } else {
                    for f in files.iter().chain(std::iter::once(&temp_dir.join(SIDECAR_NAME))) {
                        let target = final_dir.join(f.file_name().unwrap_or_default());
                        std::fs::rename(f, &target).or_else(|_| std::fs::copy(f, &target).map(|_| ()))?;
                    }
                }
                folder_out = Some(final_dir.clone());
                if matches!(request.destination, Destination::Both(_)) {
                    archive_study = Some(import_copy_into_archive(db, paths, &final_dir, &sidecar)?);
                }
            }
            Destination::Archive => {
                archive_study = Some(import_copy_into_archive(db, paths, &temp_dir, &sidecar)?);
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&temp_dir);
    result?;

    // The register.
    let case_id = match &request.case {
        CaseChoice::Existing(id) => *id,
        CaseChoice::New(n) => {
            let id = db.create_teaching_case(&reference, &name, &title, &secret, offset_days, preserve_season)?;
            if suggest_reference(db, reference_prefix(&n.reference).as_deref().unwrap_or("")) == reference {
                let next = db
                    .meta_get("teaching_next_number")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    + 1;
                db.meta_set("teaching_next_number", &next.to_string())?;
            }
            id
        }
    };
    db.add_teaching_source(case_id, &study.study_instance_uid, Some(study.id))?;
    let destination = match (&folder_out, archive_study) {
        (Some(f), Some(_)) => format!("archive and {}", f.display()),
        (Some(f), None) => f.display().to_string(),
        (None, _) => "archive".to_string(),
    };
    db.record_teaching_export(
        case_id,
        &study.study_instance_uid,
        &destination,
        "clean",
        series_count as i64,
        image_count as i64,
    )?;

    // The log line: the only other place source UIDs appear.
    let log_path = paths.root.join(LOG_NAME);
    let line = serde_json::json!({
        "time": chrono::Local::now().to_rfc3339(),
        "ruthos": SOFTWARE,
        "reference": reference,
        "source_study_uid": study.study_instance_uid,
        "destination": destination,
        "series": series_count,
        "images": image_count,
        "masks": request.masks.iter().map(|(sid, d)| serde_json::json!({"series_id": sid, "decision": d})).collect::<Vec<_>>(),
        "verification": "clean",
        "overrides": overridden.iter().map(|(f, r)| serde_json::json!({"path": f.path, "matched": f.matched, "reason": r})).collect::<Vec<_>>(),
    });
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
        writeln!(f, "{line}")?;
    }
    Ok(ExportResult {
        reference,
        folder: folder_out,
        archive_study_id: archive_study,
        series: series_count,
        images: image_count,
        log_path,
    })
}

fn reference_prefix(reference: &str) -> Option<String> {
    reference.rsplit_once('-').map(|(p, _)| p.to_string())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        std::fs::copy(e.path(), to.join(e.file_name()))?;
    }
    std::fs::remove_dir_all(from)
}

/// Import a written copy into the local archive and apply its sidecar.
fn import_copy_into_archive(db: &Database, paths: &DataPaths, dir: &Path, sidecar: &Sidecar) -> Result<i64> {
    let mut study_ids: Vec<i64> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "dcm") {
            if let FileOutcome::Imported { study_id: s, .. } = import_file(db, paths, &p, ImportMode::Copy)? {
                if !study_ids.contains(&s) {
                    study_ids.push(s);
                }
            }
        }
    }
    if study_ids.is_empty() {
        study_ids.extend(studies_of_folder(db, dir));
    }
    apply_sidecar_data(db, sidecar, &study_ids)?;
    study_ids
        .first()
        .copied()
        .ok_or_else(|| CoreError::Other("the copy did not import".into()))
}

/// The archive studies the files of a folder belong to, by SOP Instance UID.
fn studies_of_folder(db: &Database, dir: &Path) -> Vec<i64> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for p in entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "dcm"))
        {
            if let Ok(meta) = read_meta(&p) {
                if let Ok(Some(img)) = db.image_by_sop_uid(&meta.sop_instance_uid) {
                    if let Ok(Some(s)) = db.series(img.series_id) {
                        if !out.contains(&s.study_id) {
                            out.push(s.study_id);
                        }
                    }
                }
            }
        }
    }
    out
}

fn sidecar_first_image(db: &Database, sidecar: &Sidecar) -> Option<ImageRow> {
    sidecar
        .measurements
        .iter()
        .map(|m| m.sop_instance_uid.clone())
        .chain(sidecar.key_images.iter().cloned())
        .chain(sidecar.calibrations.iter().map(|c| c.sop_instance_uid.clone()))
        .find_map(|uid| db.image_by_sop_uid(&uid).ok().flatten())
}

/// Apply a sidecar found in an imported folder: measurements, calibrations,
/// key images, the teaching reference and the Teaching album.
pub fn apply_sidecar(db: &Database, dir: &Path) -> Result<Option<String>> {
    let file = dir.join(SIDECAR_NAME);
    if !file.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&file)?;
    let sidecar: Sidecar =
        serde_json::from_str(&text).map_err(|e| CoreError::Other(format!("{}: {e}", file.display())))?;
    let studies = studies_of_folder(db, dir);
    apply_sidecar_data(db, &sidecar, &studies)?;
    Ok(Some(sidecar.reference))
}

fn apply_sidecar_data(db: &Database, sidecar: &Sidecar, known_studies: &[i64]) -> Result<()> {
    let mut by_image: HashMap<(i64, i64), Vec<RoiRecord>> = HashMap::new();
    let mut study_ids: Vec<i64> = known_studies.to_vec();
    fn note_study(db: &Database, study_ids: &mut Vec<i64>, image: &ImageRow) {
        if let Ok(Some(s)) = db.series(image.series_id) {
            if !study_ids.contains(&s.study_id) {
                study_ids.push(s.study_id);
            }
        }
    }
    for m in &sidecar.measurements {
        if let Some(img) = db.image_by_sop_uid(&m.sop_instance_uid)? {
            note_study(db, &mut study_ids, &img);
            by_image.entry((img.id, m.frame)).or_default().push(RoiRecord {
                id: 0,
                image_id: img.id,
                frame: m.frame,
                kind: m.kind.clone(),
                points: m.points.clone(),
                name: m.name.clone(),
                value: m.value,
                unit: m.unit.clone(),
                basis: m.basis.clone(),
                def_key: m.def_key.clone(),
            });
        }
    }
    for ((image_id, frame), rois) in by_image {
        db.replace_rois(image_id, frame, &rois)?;
    }
    for c in &sidecar.calibrations {
        if let Some(img) = db.image_by_sop_uid(&c.sop_instance_uid)? {
            note_study(db, &mut study_ids, &img);
            db.set_calibration(img.id, c.row_mm, c.col_mm, "teaching copy")?;
        }
    }
    for uid in &sidecar.key_images {
        if let Some(img) = db.image_by_sop_uid(uid)? {
            note_study(db, &mut study_ids, &img);
            db.set_key_image(img.id, true)?;
        }
    }
    // Any image of the copy identifies the study even without measurements.
    if study_ids.is_empty() {
        if let Some(img) = sidecar_first_image(db, sidecar) {
            note_study(db, &mut study_ids, &img);
        }
    }
    let album = db
        .albums()?
        .into_iter()
        .find(|a| a.name == TEACHING_ALBUM && !a.is_smart);
    let album_id = match album {
        Some(a) => a.id,
        None => db.create_album(TEACHING_ALBUM, None)?,
    };
    for study_id in study_ids {
        db.set_study_teaching(study_id, &sidecar.reference, sidecar.answer_key)?;
        let _ = db.add_study_to_album(album_id, study_id);
        if let (Some(f), Some(c)) = (&sidecar.findings, &sidecar.conclusion) {
            db.set_report(study_id, f, c)?;
        }
    }
    Ok(())
}

/// Remove a case from the register and its lines from the log, severing
/// the link for good.
pub fn delete_case(db: &Database, paths: &DataPaths, case_id: i64) -> Result<()> {
    let Some(case) = db.teaching_case(case_id)? else {
        return Ok(());
    };
    db.delete_teaching_case(case_id)?;
    let log_path = paths.root.join(LOG_NAME);
    if let Ok(text) = std::fs::read_to_string(&log_path) {
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .map(|v| v["reference"] != case.reference)
                    .unwrap_or(true)
            })
            .collect();
        std::fs::write(&log_path, kept.join("\n") + if kept.is_empty() { "" } else { "\n" })?;
    }
    Ok(())
}

/// Export a study that is already a teaching copy, unchanged, under its
/// reference: the files as they are plus a fresh sidecar from the archive.
pub fn reexport_copy(db: &Database, paths: &DataPaths, study_id: i64, dest: &Path) -> Result<PathBuf> {
    let study = db
        .study(study_id)?
        .ok_or_else(|| CoreError::Other("study not found".into()))?;
    let reference = study
        .teaching_reference
        .clone()
        .ok_or_else(|| CoreError::Other("not a teaching copy".into()))?;
    let final_dir = dest.join(&reference);
    std::fs::create_dir_all(&final_dir)?;
    let mut sidecar = Sidecar {
        format: 1,
        reference: reference.clone(),
        name: study.patient_name.clone(),
        title: study.study_description.clone(),
        answer_key: study.teaching_answer_key,
        ..Default::default()
    };
    for series in db.series_for_study(study.id)? {
        for img in db.images_for_series(series.id)? {
            let src = paths.resolve(&img.path);
            std::fs::copy(&src, final_dir.join(format!("{}.dcm", img.sop_instance_uid)))?;
            for frame in 0..img.number_of_frames.max(1) {
                for r in db.rois(img.id, frame)? {
                    sidecar.measurements.push(SidecarMeasurement {
                        sop_instance_uid: img.sop_instance_uid.clone(),
                        frame,
                        kind: r.kind,
                        points: r.points,
                        name: r.name,
                        value: r.value,
                        unit: r.unit,
                        basis: r.basis,
                        def_key: r.def_key,
                    });
                }
            }
            if let Some([row_mm, col_mm]) = db.calibration(img.id)? {
                sidecar.calibrations.push(SidecarCalibration {
                    sop_instance_uid: img.sop_instance_uid.clone(),
                    row_mm,
                    col_mm,
                });
            }
            if img.is_key_image {
                sidecar.key_images.push(img.sop_instance_uid.clone());
            }
        }
    }
    if let Some(r) = db.report(study.id)? {
        if !r.findings.is_empty() || !r.conclusion.is_empty() {
            sidecar.findings = Some(r.findings);
            sidecar.conclusion = Some(r.conclusion);
        }
    }
    let json = serde_json::to_string_pretty(&sidecar).map_err(|e| CoreError::Other(e.to_string()))?;
    std::fs::write(final_dir.join(SIDECAR_NAME), json)?;
    Ok(final_dir)
}
