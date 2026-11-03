//! Acceptance tests for the anonymiser, over a temporary archive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{dicom_value, DataElement, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_core::import::{import_file, FileOutcome, ImportMode};
use ruthos_core::teaching::{
    abandon, apply_sidecar, commit, delete_case, prepare, CaseChoice, Destination, ExportRequest, MaskDecision,
    MeasurementTreatment, NewCase, SIDECAR_NAME,
};
use ruthos_core::{Database, DataPaths, RoiRecord};
use ruthos_dicom::deid::Rect;
use ruthos_dicom::open::open_any;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom")
}

struct Archive {
    db: std::sync::Arc<Database>,
    paths: DataPaths,
    _root: PathBuf,
}

fn archive(tag: &str) -> Archive {
    let root = std::env::temp_dir().join(format!("ruthos-teaching-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let paths = DataPaths::new(&root);
    paths.ensure().unwrap();
    let db = Database::open(&paths).unwrap();
    Archive { db, paths, _root: root }
}

fn import(a: &Archive, file: &Path) -> i64 {
    match import_file(&a.db, &a.paths, file, ImportMode::Copy).unwrap() {
        FileOutcome::Imported { study_id, .. } => study_id,
        other => panic!("{} did not import: {other:?}", file.display()),
    }
}

fn masks_for(a: &Archive, study_id: i64, decision: MaskDecision) -> HashMap<i64, MaskDecision> {
    a.db.series_for_study(study_id).unwrap().into_iter().map(|s| (s.id, decision.clone())).collect()
}

fn request(study_id: i64, case: CaseChoice, dest: Destination, masks: HashMap<i64, MaskDecision>) -> ExportRequest {
    ExportRequest {
        study_id,
        case,
        title: None,
        destination: dest,
        measurements: MeasurementTreatment::Include,
        report_text: None,
        masks,
        overrides: Vec::new(),
        blank_attributes: Vec::new(),
    }
}

fn new_case(reference: &str, name: &str) -> CaseChoice {
    CaseChoice::New(NewCase { reference: reference.into(), name: name.into(), title: "Teaching case".into(), preserve_season: false })
}

fn out_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruthos-teaching-out-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn uids_of(file: &Path) -> Vec<String> {
    let obj = open_any(file, None).unwrap();
    let mut v = Vec::new();
    for tag in [tags::STUDY_INSTANCE_UID, tags::SERIES_INSTANCE_UID, tags::SOP_INSTANCE_UID] {
        v.push(obj.element(tag).unwrap().value().to_str().unwrap().to_string());
    }
    v
}

fn text(file: &Path, tag: Tag) -> String {
    let obj = open_any(file, None).unwrap();
    obj.get(tag).map(|e| e.value().to_str().unwrap().to_string()).unwrap_or_default()
}

fn dcm_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dcm"))
        .collect();
    v.sort();
    v
}

/// A synthetic ultrasound still with a header band: SMITH^BERTIE and a
/// practice name live in the band (rows 0..30), the region is rows 40..100.
fn write_synthetic_us(dir: &Path, sop: &str, study_uid: &str, study_date: &str, with_planted: bool) -> PathBuf {
    let (w, h) = (120u16, 100u16);
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        for _x in 0..w {
            let v: u8 = if y < 30 { 200 } else { 100 }; // 200 = "text" band, 100 = image
            pixels.extend_from_slice(&[v, v, v]);
        }
    }
    let mut o = InMemDicomObject::new_empty();
    let mut s = |tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
    s(tags::SOP_CLASS_UID, VR::UI, uids::ULTRASOUND_IMAGE_STORAGE);
    s(tags::SOP_INSTANCE_UID, VR::UI, sop);
    s(tags::STUDY_INSTANCE_UID, VR::UI, study_uid);
    s(tags::SERIES_INSTANCE_UID, VR::UI, &format!("{study_uid}.1"));
    s(tags::MODALITY, VR::CS, "US");
    s(tags::PATIENT_NAME, VR::PN, "SMITH^BERTIE");
    s(tags::PATIENT_ID, VR::LO, "V778899");
    s(tags::PATIENT_BIRTH_DATE, VR::DA, "20150401");
    s(tags::PATIENT_SEX, VR::CS, "M");
    s(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, "Equine");
    s(tags::RESPONSIBLE_PERSON, VR::PN, "SMITH^ALISON");
    s(tags::INSTITUTION_NAME, VR::LO, "Ashby Equine Clinic");
    s(tags::STATION_NAME, VR::SH, "ECHO1");
    s(tags::STUDY_DATE, VR::DA, study_date);
    s(tags::STUDY_TIME, VR::TM, "093000");
    s(tags::SERIES_NUMBER, VR::IS, "1");
    s(tags::INSTANCE_NUMBER, VR::IS, "1");
    s(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "RGB");
    s(tags::BURNED_IN_ANNOTATION, VR::CS, "YES");
    if with_planted {
        s(tags::SERIES_DESCRIPTION, VR::LO, "Echo for BERTIE");
        o.put(DataElement::new(Tag(0x0010, 0x1000), VR::LO, dicom_value!(Str, "985141001234567")));
        o.put(DataElement::new(Tag(0x0009, 0x1010), VR::LO, dicom_value!(Str, "SMITH private")));
        let mut item = InMemDicomObject::new_empty();
        item.put(DataElement::new(tags::CODE_MEANING, VR::LO, dicom_value!(Str, "Ashby nested")));
        o.put(DataElement::new(
            Tag(0x0008, 0x1032),
            VR::SQ,
            Value::Sequence(DataSetSequence::new(vec![item], Length::UNDEFINED)),
        ));
    }
    o.put(DataElement::new(tags::SAMPLES_PER_PIXEL, VR::US, dicom_value!(U16, [3])));
    o.put(DataElement::new(tags::PLANAR_CONFIGURATION, VR::US, dicom_value!(U16, [0])));
    o.put(DataElement::new(tags::ROWS, VR::US, dicom_value!(U16, [h])));
    o.put(DataElement::new(tags::COLUMNS, VR::US, dicom_value!(U16, [w])));
    o.put(DataElement::new(tags::BITS_ALLOCATED, VR::US, dicom_value!(U16, [8])));
    o.put(DataElement::new(tags::BITS_STORED, VR::US, dicom_value!(U16, [8])));
    o.put(DataElement::new(tags::HIGH_BIT, VR::US, dicom_value!(U16, [7])));
    o.put(DataElement::new(tags::PIXEL_REPRESENTATION, VR::US, dicom_value!(U16, [0])));
    // One 2D region: x 10..109, y 40..99, in cm.
    let mut region = InMemDicomObject::new_empty();
    region.put(DataElement::new(Tag(0x0018, 0x6012), VR::US, dicom_value!(U16, [1])));
    region.put(DataElement::new(Tag(0x0018, 0x6014), VR::US, dicom_value!(U16, [1])));
    region.put(DataElement::new(Tag(0x0018, 0x6018), VR::UL, dicom_value!(U32, [10])));
    region.put(DataElement::new(Tag(0x0018, 0x601A), VR::UL, dicom_value!(U32, [40])));
    region.put(DataElement::new(Tag(0x0018, 0x601C), VR::UL, dicom_value!(U32, [109])));
    region.put(DataElement::new(Tag(0x0018, 0x601E), VR::UL, dicom_value!(U32, [99])));
    region.put(DataElement::new(Tag(0x0018, 0x6024), VR::US, dicom_value!(U16, [3])));
    region.put(DataElement::new(Tag(0x0018, 0x6026), VR::US, dicom_value!(U16, [3])));
    region.put(DataElement::new(Tag(0x0018, 0x602C), VR::FD, dicom_value!(F64, [0.05])));
    region.put(DataElement::new(Tag(0x0018, 0x602E), VR::FD, dicom_value!(F64, [0.05])));
    o.put(DataElement::new(
        tags::SEQUENCE_OF_ULTRASOUND_REGIONS,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(vec![region], Length::UNDEFINED)),
    ));
    o.put(DataElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::from(pixels)));
    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(uids::ULTRASOUND_IMAGE_STORAGE)
        .media_storage_sop_instance_uid(sop)
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .build()
        .unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{sop}.dcm"));
    o.with_exact_meta(meta).write_to_file(&path).unwrap();
    path
}

#[test]
fn ct_and_mr_export_clean_with_identical_pixels() {
    let a = archive("ctmr");
    for (file, reference) in [("CT_small.dcm", "T-0001"), ("MR_small.dcm", "T-0002")] {
        let src = testdata().join(file);
        let study = import(&a, &src);
        let dest = out_dir(&format!("ctmr-{reference}"));
        let req = request(study, new_case(reference, &format!("Hartley^{reference}")), Destination::Folder(dest.clone()), masks_for(&a, study, MaskDecision::Trusted));
        let prepared = prepare(&a.db, &a.paths, req, &|_, _, _| {}).unwrap();
        assert!(prepared.is_clean(), "{file}: {:?}", prepared.findings);
        let result = commit(&a.db, &a.paths, prepared).unwrap();
        let out = dcm_files(&result.folder.clone().unwrap());
        assert_eq!(out.len(), 1);
        let src_uids = uids_of(&src);
        let out_uids = uids_of(&out[0]);
        for u in &src_uids {
            assert!(!out_uids.contains(u), "{file}: original UID survived");
        }
        let src_px = open_any(&src, None).unwrap().element(tags::PIXEL_DATA).unwrap().value().to_bytes().unwrap().to_vec();
        let out_px = open_any(&out[0], None).unwrap().element(tags::PIXEL_DATA).unwrap().value().to_bytes().unwrap().to_vec();
        assert_eq!(src_px, out_px, "{file}: pixel data changed on passthrough");
        assert_eq!(text(&out[0], tags::PATIENT_IDENTITY_REMOVED), "YES");
        assert_eq!(text(&out[0], tags::PATIENT_ID), reference);
        assert!(!result.log_path.exists() || std::fs::read_to_string(&result.log_path).unwrap().contains(reference));
    }
}

#[test]
fn ultrasound_regions_mask_outside_and_keep_inside() {
    let a = archive("us");
    let src = write_synthetic_us(&std::env::temp_dir().join(format!("ruthos-us-src-{}", std::process::id())), "1.2.3.100", "1.2.3", "20240310", false);
    let study = import(&a, &src);
    let series = a.db.series_for_study(study).unwrap();
    let decision = MaskDecision::Regions(vec![Rect { x0: 10, y0: 40, x1: 109, y1: 99 }]);
    let masks: HashMap<i64, MaskDecision> = series.iter().map(|s| (s.id, decision.clone())).collect();
    let dest = out_dir("us");
    let req = request(study, new_case("US-0001", "Fenwick^Copper"), Destination::Folder(dest.clone()), masks);
    let prepared = prepare(&a.db, &a.paths, req, &|_, _, _| {}).unwrap();
    assert!(prepared.is_clean(), "{:?}", prepared.findings);
    let result = commit(&a.db, &a.paths, prepared).unwrap();
    let out = dcm_files(&result.folder.unwrap());
    let obj = open_any(&out[0], None).unwrap();
    let px = obj.element(tags::PIXEL_DATA).unwrap().value().to_bytes().unwrap().to_vec();
    let w = 120usize;
    let at = |x: usize, y: usize| px[(y * w + x) * 3];
    assert_eq!(at(5, 5), 0, "band outside the region must be black");
    assert_eq!(at(60, 10), 0);
    assert_eq!(at(5, 50), 0, "left of the region must be black");
    assert_eq!(at(60, 70), 100, "inside the region must be unchanged");
    assert_eq!(at(109, 99), 100);
    assert_eq!(at(110, 99), 0);
    assert_eq!(text(&out[0], tags::BURNED_IN_ANNOTATION), "NO");
    assert!(obj.get(Tag(0x0018, 0x602C)).is_some() || obj.get(tags::SEQUENCE_OF_ULTRASOUND_REGIONS).is_some(), "regions are carried");
}

#[test]
fn uids_stable_within_a_case_and_disjoint_across_cases() {
    let a = archive("uids");
    let study = import(&a, &testdata().join("CT_small.dcm"));
    let d1 = out_dir("uids-1");
    let r1 = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, new_case("C-1", "Oakes^Finch"), Destination::Folder(d1.clone()), masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap()).unwrap();
    let case = a.db.teaching_case_by_reference("C-1").unwrap().unwrap();
    let d2 = out_dir("uids-2");
    let r2 = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, CaseChoice::Existing(case.id), Destination::Folder(d2.clone()), masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap()).unwrap();
    let d3 = out_dir("uids-3");
    let r3 = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, new_case("C-2", "Lyle^Hazel"), Destination::Folder(d3.clone()), masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap()).unwrap();
    let u1 = uids_of(&dcm_files(&r1.folder.unwrap())[0]);
    let u2 = uids_of(&dcm_files(&r2.folder.unwrap())[0]);
    let u3 = uids_of(&dcm_files(&r3.folder.unwrap())[0]);
    assert_eq!(u1, u2, "same case must reproduce the same UIDs");
    for u in &u1 {
        assert!(!u3.contains(u), "different cases must not share UIDs");
    }
}

#[test]
fn two_studies_in_one_case_keep_interval_and_age() {
    let a = archive("dates");
    let dir = std::env::temp_dir().join(format!("ruthos-dates-src-{}", std::process::id()));
    let s1 = import(&a, &write_synthetic_us(&dir, "1.2.4.100", "1.2.4", "20240310", false));
    let s2 = import(&a, &write_synthetic_us(&dir, "1.2.5.100", "1.2.5", "20240410", false));
    let region = MaskDecision::Regions(vec![Rect { x0: 10, y0: 40, x1: 109, y1: 99 }]);
    let dest = out_dir("dates");
    let r1 = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(s1, new_case("D-1", "Barrow^Juniper"), Destination::Folder(dest.clone()), masks_for(&a, s1, region.clone())), &|_, _, _| {}).unwrap()).unwrap();
    let case = a.db.teaching_case_by_reference("D-1").unwrap().unwrap();
    let r2 = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(s2, CaseChoice::Existing(case.id), Destination::Folder(dest.clone()), masks_for(&a, s2, region)), &|_, _, _| {}).unwrap()).unwrap();
    assert_eq!(r1.folder, r2.folder, "a case's studies share one folder");
    let files = dcm_files(&r1.folder.unwrap());
    assert_eq!(files.len(), 2);
    let mut dates: Vec<chrono::NaiveDate> = files.iter().map(|f| chrono::NaiveDate::parse_from_str(&text(f, tags::STUDY_DATE), "%Y%m%d").unwrap()).collect();
    dates.sort();
    assert_eq!((dates[1] - dates[0]).num_days(), 31, "the interval survives");
    let mut ages: Vec<String> = files.iter().map(|f| text(f, tags::PATIENT_AGE)).collect();
    ages.sort();
    assert_eq!(ages, vec!["008Y".to_string(), "009Y".to_string()], "born 2015-04-01: 8 in March, 9 in April");
    for f in &files {
        let d = text(f, tags::STUDY_DATE);
        assert!(d != "20240310" && d != "20240410", "original date survived");
        assert_ne!(text(f, tags::PATIENT_BIRTH_DATE), "20150401");
    }
}

#[test]
fn planted_tokens_are_absent_or_reported() {
    let a = archive("tokens");
    let src = write_synthetic_us(&std::env::temp_dir().join(format!("ruthos-tokens-src-{}", std::process::id())), "1.2.6.100", "1.2.6", "20240310", true);
    let study = import(&a, &src);
    let region = MaskDecision::Regions(vec![Rect { x0: 10, y0: 40, x1: 109, y1: 99 }]);
    let dest = out_dir("tokens");
    let req = request(study, new_case("TK-1", "Marchant^Rowan"), Destination::Folder(dest.clone()), masks_for(&a, study, region.clone()));
    let prepared = prepare(&a.db, &a.paths, req, &|_, _, _| {}).unwrap();
    // The series description carries the animal's name: reported, blocks the write.
    assert!(!prepared.is_clean());
    assert!(prepared.findings.iter().any(|f| f.path.contains("(0008,103E)") && f.matched.eq_ignore_ascii_case("BERTIE")), "{:?}", prepared.findings);
    assert!(prepared.findings.iter().all(|f| f.overridable), "only the descriptor hit is expected: {:?}", prepared.findings);
    assert!(commit(&a.db, &a.paths, prepared).is_err());
    assert!(!dest.join("TK-1").exists(), "nothing written after a failed pass");
    // Accepting the descriptor hit with a reason lets it through; the other
    // planted tokens never reached the copy.
    let mut req = request(study, new_case("TK-1", "Marchant^Rowan"), Destination::Folder(dest.clone()), masks_for(&a, study, region));
    req.overrides.push(("(0008,103E)".into(), "the horse is called after its breed".into()));
    let prepared = prepare(&a.db, &a.paths, req, &|_, _, _| {}).unwrap();
    assert!(prepared.is_clean(), "{:?}", prepared.findings);
    assert_eq!(prepared.overridden.len(), 1);
    let result = commit(&a.db, &a.paths, prepared).unwrap();
    let out = dcm_files(&result.folder.unwrap());
    let obj = open_any(&out[0], None).unwrap();
    assert!(obj.get(Tag(0x0010, 0x1000)).is_none());
    assert!(obj.get(Tag(0x0009, 0x1010)).is_none());
    assert!(obj.get(Tag(0x0008, 0x1032)).is_none());
    let bytes = std::fs::read(&out[0]).unwrap();
    for token in ["985141001234567", "Ashby", "SMITH", "ECHO1", "V778899"] {
        assert!(!String::from_utf8_lossy(&bytes).contains(token), "{token} in output");
    }
    let log = std::fs::read_to_string(&result.log_path).unwrap();
    assert!(log.contains("the horse is called after its breed"));
}

#[test]
fn failed_write_leaves_destination_untouched() {
    let a = archive("fail");
    let study = import(&a, &testdata().join("CT_small.dcm"));
    let dest = std::env::temp_dir().join(format!("ruthos-fail-dest-{}", std::process::id()));
    let _ = std::fs::remove_file(&dest);
    std::fs::write(&dest, b"not a folder").unwrap();
    let req = request(study, new_case("F-1", "Greaves^Ember"), Destination::Folder(dest.clone()), masks_for(&a, study, MaskDecision::Trusted));
    assert!(prepare(&a.db, &a.paths, req, &|_, _, _| {}).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"not a folder");
    assert!(a.db.teaching_case_by_reference("F-1").unwrap().is_none(), "no case recorded for a failed export");
    // A prepared copy that is abandoned leaves nothing behind either.
    let dest2 = out_dir("fail-2");
    let prepared = prepare(&a.db, &a.paths, request(study, new_case("F-2", "Greaves^Ember"), Destination::Folder(dest2.clone()), masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap();
    let temp = prepared.temp_dir.clone();
    abandon(prepared);
    assert!(!temp.exists());
    assert!(dcm_files(&dest2).is_empty());
}

#[test]
fn measurements_calibrations_and_key_images_round_trip_through_a_folder() {
    let a = archive("sidecar");
    let src = write_synthetic_us(&std::env::temp_dir().join(format!("ruthos-sidecar-src-{}", std::process::id())), "1.2.7.100", "1.2.7", "20240310", false);
    let study = import(&a, &src);
    let series = a.db.series_for_study(study).unwrap();
    let image = a.db.images_for_series(series[0].id).unwrap()[0].clone();
    a.db.replace_rois(
        image.id,
        0,
        &[RoiRecord { id: 0, image_id: image.id, frame: 0, kind: "length".into(), points: vec![[20.0, 50.0], [60.0, 50.0]], name: Some("LVIDd".into()), value: Some(20.0), unit: "mm".into() }],
    )
    .unwrap();
    a.db.set_calibration(image.id, 0.5, 0.5, "ruler").unwrap();
    a.db.set_key_image(image.id, true).unwrap();
    let region = MaskDecision::Regions(vec![Rect { x0: 10, y0: 40, x1: 109, y1: 99 }]);
    let dest = out_dir("sidecar");
    let result = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, new_case("SC-1", "Holloway^Sorrel"), Destination::Folder(dest.clone()), masks_for(&a, study, region)), &|_, _, _| {}).unwrap()).unwrap();
    let folder = result.folder.unwrap();
    assert!(folder.join(SIDECAR_NAME).is_file());

    // Into an empty archive, as the importer does.
    let b = archive("sidecar-b");
    for f in dcm_files(&folder) {
        import(&b, &f);
    }
    assert_eq!(apply_sidecar(&b.db, &folder).unwrap().as_deref(), Some("SC-1"));
    let studies = b.db.list_studies(&Default::default()).unwrap();
    assert_eq!(studies.len(), 1);
    assert_eq!(studies[0].teaching_reference.as_deref(), Some("SC-1"));
    assert_eq!(studies[0].patient_name, "Holloway, Sorrel");
    let bseries = b.db.series_for_study(studies[0].id).unwrap();
    let bimage = b.db.images_for_series(bseries[0].id).unwrap()[0].clone();
    let rois = b.db.rois(bimage.id, 0).unwrap();
    assert_eq!(rois.len(), 1);
    assert_eq!(rois[0].name.as_deref(), Some("LVIDd"));
    assert_eq!(rois[0].points, vec![[20.0, 50.0], [60.0, 50.0]]);
    assert_eq!(b.db.calibration(bimage.id).unwrap(), Some([0.5, 0.5]));
    assert!(bimage.is_key_image);
    assert!(b.db.albums().unwrap().iter().any(|al| al.name == "Teaching"));
}

#[test]
fn deleting_a_case_removes_register_rows_and_log_lines() {
    let a = archive("delete");
    let study = import(&a, &testdata().join("CT_small.dcm"));
    let dest = out_dir("delete");
    let result = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, new_case("DEL-1", "Prescott^Tansy"), Destination::Folder(dest), masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap()).unwrap();
    let case = a.db.teaching_case_by_reference("DEL-1").unwrap().unwrap();
    assert_eq!(a.db.teaching_sources(case.id).unwrap().len(), 1);
    assert!(std::fs::read_to_string(&result.log_path).unwrap().contains("DEL-1"));
    delete_case(&a.db, &a.paths, case.id).unwrap();
    assert!(a.db.teaching_case_by_reference("DEL-1").unwrap().is_none());
    assert!(a.db.teaching_sources(case.id).unwrap().is_empty());
    assert!(!std::fs::read_to_string(&result.log_path).unwrap().contains("DEL-1"));
}

#[test]
fn export_into_the_archive_makes_a_flagged_study_in_the_teaching_album() {
    let a = archive("archive-dest");
    let study = import(&a, &testdata().join("CT_small.dcm"));
    let result = commit(&a.db, &a.paths, prepare(&a.db, &a.paths, request(study, new_case("AR-1", "Dunmore^Willow"), Destination::Archive, masks_for(&a, study, MaskDecision::Trusted)), &|_, _, _| {}).unwrap()).unwrap();
    let copy_id = result.archive_study_id.unwrap();
    assert_ne!(copy_id, study);
    let copy = a.db.study(copy_id).unwrap().unwrap();
    assert_eq!(copy.teaching_reference.as_deref(), Some("AR-1"));
    assert_eq!(copy.patient_name, "Dunmore, Willow");
    let source = a.db.study(study).unwrap().unwrap();
    assert!(source.teaching_reference.is_none());
    assert_eq!(a.db.teaching_cases_for_study(&source.study_instance_uid).unwrap().len(), 1);
}
