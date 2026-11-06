//! A package, from the archive to someone else's computer.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ruthos_core::import::{import_file, ImportMode};
use ruthos_core::package::{self, Spec, NOTES_NAME, PROGRAM_NAME, READ_ME_NAME};
use ruthos_core::{DataPaths, Database, RoiRecord};
use ruthos_dicom::dicomdir;

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/pydicom")
        .join(name)
}

struct Fixture {
    dir: PathBuf,
    paths: DataPaths,
    db: Arc<Database>,
    study: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn no_progress(_: usize, _: usize) {}

fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("ruthos-pkg-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let paths = DataPaths::new(dir.join("data"));
    let db = Database::open(&paths).unwrap();
    import_file(&db, &paths, &sample("CT_small.dcm"), ImportMode::Copy).unwrap();
    let study = db.list_studies(&Default::default()).unwrap()[0].id;
    Fixture { dir, paths, db, study }
}

fn spec(f: &Fixture) -> Spec {
    let program = f.dir.join("ruthos.exe");
    std::fs::write(&program, b"the program").unwrap();
    Spec {
        studies: vec![f.study],
        title: "Bella CT 5 Nov 2026".into(),
        program: Some(program),
        ..Spec::default()
    }
}

/// Everything in a folder, with its bytes.
fn contents(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_path_buf(), std::fs::read(e.path()).unwrap()))
        .collect();
    out.sort();
    out
}

/// Open a package as a viewer run does: read in place, in a new memory database.
fn open_as_recipient(folder: &Path, scratch: &Path) -> Arc<Database> {
    let paths = DataPaths::new(scratch);
    let db = Database::open_in_memory(&paths).unwrap();
    let dir = dicomdir::open(&folder.join("DICOMDIR")).unwrap();
    let (files, missing) = dir.locate_all(folder);
    assert_eq!(missing, 0);
    for file in files {
        import_file(&db, &paths, &file, ImportMode::Link).unwrap();
    }
    package::apply_notes(&db, folder).unwrap();
    db
}

#[test]
fn a_package_opens_elsewhere_with_its_measurements_and_changes_nothing() {
    let f = fixture("open");
    let series = f.db.series_for_study(f.study).unwrap();
    let image = f.db.images_for_series(series[0].id).unwrap().remove(0);
    let original = std::fs::read(f.paths.resolve(&image.path)).unwrap();
    f.db.replace_rois(
        image.id,
        0,
        &[RoiRecord {
            id: 0,
            image_id: image.id,
            frame: 0,
            kind: "line".into(),
            points: vec![[1.0, 2.0], [3.0, 4.0]],
            name: Some("Width".into()),
            value: Some(12.5),
            unit: "mm".into(),
            basis: None,
            def_key: None,
        }],
    )
    .unwrap();
    f.db.set_calibration(image.id, 0.5, 0.5, "test").unwrap();
    f.db.set_key_image(image.id, true).unwrap();

    let out = f.dir.join("stick");
    std::fs::create_dir_all(&out).unwrap();
    let folder =
        package::write_folder(&f.db, &f.paths, &spec(&f), &out, &no_progress, &AtomicBool::new(false)).unwrap();
    assert_eq!(folder.file_name().unwrap(), "Bella CT 5 Nov 2026");
    for name in [PROGRAM_NAME, READ_ME_NAME, NOTES_NAME, "ruthos.ini", "DICOMDIR"] {
        assert!(folder.join(name).is_file(), "{name}");
    }
    assert_eq!(std::fs::read(folder.join(PROGRAM_NAME)).unwrap(), b"the program");
    let ini = std::fs::read_to_string(folder.join("ruthos.ini")).unwrap();
    assert!(ini.contains("[viewer]") && ini.contains("open = DICOMDIR"));

    let copied = std::fs::read(folder.join("DICOM/PT000001/ST000001/SE000001/IM000001")).unwrap();
    assert_eq!(copied, original, "images are copied byte for byte");

    let before = contents(&folder);
    let there = open_as_recipient(&folder, &f.dir.join("recipient"));
    assert_eq!(contents(&folder), before, "opening a package writes nothing to it");

    let studies = there.list_studies(&Default::default()).unwrap();
    assert_eq!(studies.len(), 1);
    let series = there.series_for_study(studies[0].id).unwrap();
    let opened = there.images_for_series(series[0].id).unwrap().remove(0);
    assert!(opened.is_key_image);
    let rois = there.rois(opened.id, 0).unwrap();
    assert_eq!(rois.len(), 1);
    assert_eq!(rois[0].value, Some(12.5));
    assert_eq!(there.calibration(opened.id).unwrap(), Some([0.5, 0.5]));

    // A second package beside it does not disturb the first.
    let second =
        package::write_folder(&f.db, &f.paths, &spec(&f), &out, &no_progress, &AtomicBool::new(false)).unwrap();
    assert_eq!(second.file_name().unwrap(), "Bella CT 5 Nov 2026 (2)");

    if let Some(keep) = std::env::var_os("RUTHOS_PACKAGE_KEEP") {
        let keep = PathBuf::from(keep);
        for (path, bytes) in contents(&folder) {
            let to = keep.join(path.strip_prefix(&folder).unwrap());
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, bytes).unwrap();
        }
    }
}

#[test]
fn a_plain_export_has_no_program_and_nothing_that_would_start_one() {
    let f = fixture("plain");
    let mut s = spec(&f);
    s.program = None;
    let out = f.dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let folder = package::write_folder(&f.db, &f.paths, &s, &out, &no_progress, &AtomicBool::new(false)).unwrap();
    assert!(!folder.join(PROGRAM_NAME).exists());
    assert!(!folder.join("ruthos.ini").exists());
    assert!(folder.join("DICOMDIR").is_file());
}

#[test]
fn the_owner_can_be_left_out() {
    let f = fixture("owner");
    let series = f.db.series_for_study(f.study).unwrap();
    let image = f.db.images_for_series(series[0].id).unwrap().remove(0);
    let stored = f.paths.resolve(&image.path);
    // Give the stored file an owner, and a name carrying the owner's surname.
    let mut object = ruthos_dicom::open::open_any(&stored, None).unwrap();
    use dicom_core::{DataElement, PrimitiveValue, VR};
    use dicom_dictionary_std::tags;
    object.put(DataElement::new(
        tags::PATIENT_NAME,
        VR::PN,
        PrimitiveValue::from("SMITH^Bella"),
    ));
    object.put(DataElement::new(
        tags::RESPONSIBLE_PERSON,
        VR::PN,
        PrimitiveValue::from("SMITH^Jo"),
    ));
    object.put(DataElement::new(
        tags::PATIENT_ADDRESS,
        VR::LO,
        PrimitiveValue::from("1 High St"),
    ));
    object.write_to_file(&stored).unwrap();
    let original_pixels = object.element(tags::PIXEL_DATA).unwrap().to_multi_int::<u16>().unwrap();

    let mut s = spec(&f);
    s.remove_owner = true;
    let out = f.dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let folder = package::write_folder(&f.db, &f.paths, &s, &out, &no_progress, &AtomicBool::new(false)).unwrap();

    let copy = folder.join("DICOM/PT000001/ST000001/SE000001/IM000001");
    let bytes = std::fs::read(&copy).unwrap();
    for text in [&b"SMITH"[..], b"High St"] {
        assert!(!bytes.windows(text.len()).any(|w| w == text));
    }
    let back = ruthos_dicom::open::open_any(&copy, None).unwrap();
    assert_eq!(
        back.element(tags::PATIENT_NAME).unwrap().to_str().unwrap().trim(),
        "Bella"
    );
    assert_eq!(
        back.element(tags::PIXEL_DATA).unwrap().to_multi_int::<u16>().unwrap(),
        original_pixels
    );
    let dir = std::fs::read(folder.join("DICOMDIR")).unwrap();
    assert!(!dir.windows(5).any(|w| w == b"SMITH"), "nor in the index");
}

#[test]
fn a_missing_image_stops_the_export_and_leaves_no_folder() {
    let f = fixture("missing");
    let series = f.db.series_for_study(f.study).unwrap();
    let image = f.db.images_for_series(series[0].id).unwrap().remove(0);
    std::fs::remove_file(f.paths.resolve(&image.path)).unwrap();
    let out = f.dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let error = package::write_folder(&f.db, &f.paths, &spec(&f), &out, &no_progress, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot be found"), "{error}");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn stopping_leaves_nothing_behind() {
    let f = fixture("stop");
    let out = f.dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let stop = AtomicBool::new(true);
    assert!(package::write_folder(&f.db, &f.paths, &spec(&f), &out, &no_progress, &stop).is_err());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    let zip = out.join("x.zip");
    assert!(package::write_zip(&f.db, &f.paths, &spec(&f), &zip, &no_progress, &stop).is_err());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn a_zip_holds_one_folder_that_opens_when_extracted() {
    let f = fixture("zip");
    let out = f.dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("Bella.zip");
    package::write_zip(&f.db, &f.paths, &spec(&f), &path, &no_progress, &AtomicBool::new(false)).unwrap();

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let extracted = f.dir.join("extracted");
    archive.extract(&extracted).unwrap();
    let tops: Vec<_> = std::fs::read_dir(&extracted).unwrap().flatten().collect();
    assert_eq!(tops.len(), 1, "one folder, so Extract All leaves no mess");
    let there = open_as_recipient(&tops[0].path(), &f.dir.join("recipient"));
    assert_eq!(there.list_studies(&Default::default()).unwrap().len(), 1);
}
