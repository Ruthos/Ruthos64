//! Segments saved to a study: filed, found again, reported, and carried into a
//! teaching copy, over a temporary archive.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use dicom_core::value::Value;
use dicom_core::{dicom_value, DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_core::import::{import_file, FileOutcome, ImportMode};
use ruthos_core::seg::{report_rows, save_segments, saved_for_series, MODALITY};
use ruthos_core::teaching::{
    commit, prepare, CaseChoice, Destination, ExportRequest, MaskDecision, MeasurementTreatment, NewCase,
};
use ruthos_core::{DataPaths, Database};
use ruthos_dicom::open::open_any;
use ruthos_dicom::sc::new_uid;
use ruthos_dicom::seg::{read_seg, source_series, Algorithm, Geometry, Segment, SegmentData, Term};

const NAME: &str = "HUGHES^BERTIE";
const DIMS: [usize; 3] = [8, 6, 5];

struct Archive {
    db: std::sync::Arc<Database>,
    paths: DataPaths,
    root: PathBuf,
}

impl Drop for Archive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn archive(tag: &str) -> Archive {
    let root = std::env::temp_dir().join(format!("ruthos-seg-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let paths = DataPaths::new(&root);
    paths.ensure().unwrap();
    let db = Database::open(&paths).unwrap();
    Archive { db, paths, root }
}

/// A CT series in the archive: its study and series ids.
fn ct(a: &Archive) -> (i64, i64) {
    let [w, h, n] = DIMS;
    let dir = a.root.join("source");
    std::fs::create_dir_all(&dir).unwrap();
    let (study, series, frame) = (new_uid(), new_uid(), new_uid());
    let mut study_id = 0;
    for z in 0..n {
        let sop = new_uid();
        let mut o = InMemDicomObject::new_empty();
        let mut s = |tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
        s(tags::SOP_CLASS_UID, VR::UI, uids::CT_IMAGE_STORAGE);
        s(tags::SOP_INSTANCE_UID, VR::UI, &sop);
        s(tags::STUDY_INSTANCE_UID, VR::UI, &study);
        s(tags::SERIES_INSTANCE_UID, VR::UI, &series);
        s(tags::FRAME_OF_REFERENCE_UID, VR::UI, &frame);
        s(tags::PATIENT_NAME, VR::PN, NAME);
        s(tags::PATIENT_ID, VR::LO, "V12345");
        s(tags::RESPONSIBLE_PERSON, VR::PN, "HUGHES^MARY");
        s(tags::INSTITUTION_NAME, VR::LO, "Ashby Equine Clinic");
        s(tags::MODALITY, VR::CS, "CT");
        s(tags::STUDY_DATE, VR::DA, "20240310");
        s(tags::SERIES_DESCRIPTION, VR::LO, "Head");
        s(tags::SLICE_THICKNESS, VR::DS, "2");
        s(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
        s(tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
        s(tags::IMAGE_ORIENTATION_PATIENT, VR::DS, "1\\0\\0\\0\\1\\0");
        s(tags::PIXEL_SPACING, VR::DS, "0.5\\0.5");
        s(tags::IMAGE_POSITION_PATIENT, VR::DS, &format!("0\\0\\{}", z * 2));
        for (tag, v) in [
            (tags::ROWS, h as u16),
            (tags::COLUMNS, w as u16),
            (tags::SAMPLES_PER_PIXEL, 1),
            (tags::BITS_ALLOCATED, 16),
            (tags::BITS_STORED, 16),
            (tags::HIGH_BIT, 15),
            (tags::PIXEL_REPRESENTATION, 1),
        ] {
            o.put(DataElement::new(tag, VR::US, PrimitiveValue::from(v)));
        }
        o.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::U16(vec![z as u16; w * h].into()),
        ));
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(uids::CT_IMAGE_STORAGE)
            .media_storage_sop_instance_uid(sop.clone())
            .build()
            .unwrap();
        let path = dir.join(format!("{sop}.dcm"));
        o.with_exact_meta(meta).write_to_file(&path).unwrap();
        match import_file(&a.db, &a.paths, &path, ImportMode::Copy).unwrap() {
            FileOutcome::Imported { study_id: id, .. } => study_id = id,
            other => panic!("{other:?}"),
        }
    }
    let series_id = a.db.series_for_study(study_id).unwrap()[0].id;
    (study_id, series_id)
}

fn segments(label: &str) -> Vec<SegmentData> {
    let t = Term::by_label("Bone").unwrap();
    let side = DIMS[0] * DIMS[1];
    let mut slices = BTreeMap::new();
    let mut mask = vec![false; side];
    mask[3] = true;
    mask[10] = true;
    slices.insert(2usize, mask);
    vec![SegmentData {
        segment: Segment {
            number: 1,
            label: label.to_string(),
            colour: [240, 200, 120],
            algorithm: Algorithm::SemiAutomatic("SAM 2.1 Tiny with Ruthos edge refinement".to_string()),
            category: t.category(),
            kind: t.kind(),
        },
        slices,
    }]
}

fn geometry(files: &[PathBuf]) -> Geometry {
    let s = source_series(files).unwrap();
    Geometry {
        dims: DIMS,
        spacing: [0.5, 0.5, 2.0],
        origin: s.slices[0].position,
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn series_uid(a: &Archive, series: i64) -> String {
    a.db.series(series).unwrap().unwrap().series_instance_uid
}

/// Saving again adds a version to the same series; the newest comes first.
#[test]
fn a_second_save_is_a_new_instance_of_the_same_series() {
    let a = archive("twice");
    let (study, series) = ct(&a);
    let first = save_segments(&a.db, &a.paths, study, series, DIMS, &segments("First"), "BOWEN^MARK").unwrap();
    let second = save_segments(&a.db, &a.paths, study, series, DIMS, &segments("Second"), "").unwrap();
    assert_eq!(first.series_uid, second.series_uid);
    assert_ne!(first.sop_instance_uid, second.sop_instance_uid);

    let rows = a.db.series_for_study(study).unwrap();
    let seg: Vec<_> = rows.iter().filter(|s| s.modality == MODALITY).collect();
    assert_eq!(seg.len(), 1, "{rows:?}");
    assert_eq!(a.db.images_for_series(seg[0].id).unwrap().len(), 2);
    assert_eq!(seg[0].series_description, "Ruthos segments", "not a cine loop");

    let saved = saved_for_series(&a.db, &a.paths, study, &series_uid(&a, series)).unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].peek.labels, ["Second"]);
    assert_eq!(saved[1].peek.labels, ["First"]);
    assert!(saved[0].peek.instance_number > saved[1].peek.instance_number);
}

/// Acceptance 4 through the archive: closed and reopened, the same voxels.
#[test]
fn saved_segments_come_back_from_the_archive() {
    let a = archive("back");
    let (study, series) = ct(&a);
    save_segments(&a.db, &a.paths, study, series, DIMS, &segments("Maxilla"), "BOWEN^MARK").unwrap();
    let files: Vec<PathBuf> =
        a.db.images_for_series(series)
            .unwrap()
            .iter()
            .map(|i| a.paths.resolve(&i.path))
            .collect();
    let saved = saved_for_series(&a.db, &a.paths, study, &series_uid(&a, series)).unwrap();
    let file = read_seg(&saved[0].path).unwrap();
    let placed = file.place(&geometry(&files)).unwrap();
    let plane = DIMS[0] * DIMS[1];
    assert_eq!(placed[0].voxels, [2 * plane + 3, 2 * plane + 10]);
    assert_eq!(placed[0].segment.label, "Maxilla");
    assert_eq!(file.creator, "BOWEN^MARK");
}

#[test]
fn another_series_does_not_see_them() {
    let a = archive("other");
    let (study, series) = ct(&a);
    save_segments(&a.db, &a.paths, study, series, DIMS, &segments("A"), "").unwrap();
    assert!(saved_for_series(&a.db, &a.paths, study, "1.2.3").unwrap().is_empty());
}

#[test]
fn a_series_that_has_changed_is_not_saved_to() {
    let a = archive("changed");
    let (study, series) = ct(&a);
    let e = save_segments(&a.db, &a.paths, study, series, [8, 6, 9], &segments("A"), "").unwrap_err();
    assert!(e.to_string().contains("changed"), "{e}");
}

/// A report states the volume and whether it came from an AI tool.
#[test]
fn a_report_lists_the_newest_segments_with_volume_and_algorithm() {
    let a = archive("report");
    let (study, series) = ct(&a);
    save_segments(&a.db, &a.paths, study, series, DIMS, &segments("Old"), "").unwrap();
    save_segments(&a.db, &a.paths, study, series, DIMS, &segments("Maxilla"), "").unwrap();
    let rows = report_rows(&a.db, &a.paths, study);
    assert_eq!(rows.len(), 1, "the newest only");
    assert_eq!(rows[0].name, "Maxilla");
    // Two voxels of 0.5 x 0.5 x 2 mm.
    assert_eq!(rows[0].value, "0.00 ml", "2.5 mm3 of it");
    assert_eq!(rows[0].location, "SAM 2.1 Tiny with Ruthos edge refinement");
}

fn all_uids(obj: &InMemDicomObject, out: &mut Vec<String>) {
    for e in obj.iter() {
        match e.value() {
            Value::Sequence(s) => s.items().iter().for_each(|i| all_uids(i, out)),
            _ if e.vr() == VR::UI => {
                if let Ok(v) = e.value().to_str() {
                    out.push(v.trim_end_matches('\0').to_string());
                }
            }
            _ => {}
        }
    }
}

fn objects_in(dir: &Path) -> Vec<InMemDicomObject> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dcm"))
        .collect();
    paths.sort();
    paths.iter().map(|p| open_any(p, None).unwrap().into_inner()).collect()
}

/// Acceptance 5: the teaching copy's SEG opens aligned on its own copied images,
/// through its derived references, and holds no original UID or name.
#[test]
fn a_teaching_copy_keeps_its_segments_aligned_and_clean() {
    let a = archive("teach");
    let (study, series) = ct(&a);
    save_segments(&a.db, &a.paths, study, series, DIMS, &segments("Maxilla"), "BOWEN^MARK").unwrap();
    let original = saved_for_series(&a.db, &a.paths, study, &series_uid(&a, series)).unwrap();
    let source: InMemDicomObject = open_any(&original[0].path, None).unwrap().into_inner();
    let mut originals = Vec::new();
    all_uids(&source, &mut originals);
    for image in a.db.images_for_series(series).unwrap() {
        let obj: InMemDicomObject = open_any(&a.paths.resolve(&image.path), None).unwrap().into_inner();
        all_uids(&obj, &mut originals);
    }
    let keep: Vec<&str> = vec![uids::CT_IMAGE_STORAGE, ruthos_dicom::seg::SOP_CLASS];

    let dest = a.root.join("teaching");
    std::fs::create_dir_all(&dest).unwrap();
    // The dialog makes no decision for a SEG, so none is given.
    let masks: HashMap<i64, MaskDecision> = [(series, MaskDecision::Trusted)].into();
    let request = ExportRequest {
        study_id: study,
        case: CaseChoice::New(NewCase {
            reference: "T-0100".into(),
            name: "Hartley^Case".into(),
            title: "Maxilla".into(),
            preserve_season: false,
        }),
        title: None,
        destination: Destination::Folder(dest.clone()),
        measurements: MeasurementTreatment::Include,
        report_text: None,
        masks,
        overrides: Vec::new(),
        blank_attributes: Vec::new(),
    };
    let prepared = prepare(&a.db, &a.paths, request, &|_, _, _| {}).unwrap();
    assert!(prepared.is_clean(), "{:?}", prepared.findings);
    let result = commit(&a.db, &a.paths, prepared).unwrap();
    let folder = result.folder.clone().unwrap();

    let mut seg_path = None;
    let mut ct_files = Vec::new();
    for e in std::fs::read_dir(&folder).unwrap().flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "dcm") {
            continue;
        }
        let obj = open_any(&path, Some(tags::PIXEL_DATA)).unwrap();
        let class = obj.element(tags::SOP_CLASS_UID).unwrap().to_str().unwrap().to_string();
        if class.trim_end_matches('\0') == ruthos_dicom::seg::SOP_CLASS {
            seg_path = Some(path);
        } else {
            ct_files.push(path);
        }
    }
    let seg_path = seg_path.expect("the SEG is in the copy");
    assert_eq!(ct_files.len(), DIMS[2]);

    // It finds its copied series by the derived references, and lines up.
    let copied = read_seg(&seg_path).unwrap();
    let copied_source = source_series(&ct_files).unwrap();
    assert!(copied.refers_to(&copied_source.series_uid));
    for frame in &copied.frames {
        let instance = frame.source_instance.as_deref().unwrap();
        assert!(copied_source.slices.iter().any(|s| s.sop_instance == instance));
    }
    let placed = copied.place(&geometry(&ct_files)).unwrap();
    let plane = DIMS[0] * DIMS[1];
    assert_eq!(placed[0].voxels, [2 * plane + 3, 2 * plane + 10]);
    assert_eq!(placed[0].segment.label, "Maxilla");
    assert_eq!(copied.creator, "", "the creator's name goes");

    // Nothing of the original remains, anywhere in the file.
    let obj: InMemDicomObject = open_any(&seg_path, None).unwrap().into_inner();
    let mut left = Vec::new();
    all_uids(&obj, &mut left);
    for uid in left.iter().filter(|u| !keep.contains(&u.as_str())) {
        assert!(!originals.contains(uid), "{uid} survived");
    }
    let bytes = std::fs::read(&seg_path).unwrap();
    for word in ["HUGHES", "BERTIE", "BOWEN", "Ashby", "V12345"] {
        assert!(
            !bytes.windows(word.len()).any(|w| w == word.as_bytes()),
            "{word} survived"
        );
    }
    assert!(objects_in(&folder).len() > DIMS[2]);
}
