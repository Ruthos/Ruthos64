//! Moving a Horos library into Ruthos, over a small library built the way
//! Horos lays one out: `Horos Data/Database.sql` (Core Data tables) and
//! numbered files under `DATABASE.noindex/10000/`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use rusqlite::{params, Connection};

use ruthos_core::horos::{locate, migrate, read_catalog, MigrateMode};
use ruthos_core::{DataPaths, Database};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/pydicom")
        .join(name)
}

/// Horos's packed form of a UID: two characters to a byte.
fn encode_uid(uid: &str) -> Vec<u8> {
    let nib = |c: Option<char>| match c {
        Some('.') => 11u8,
        Some(d @ '0'..='9') => d as u8 - b'0' + 1,
        _ => 0,
    };
    let chars: Vec<char> = uid.chars().collect();
    chars
        .chunks(2)
        .map(|p| (nib(p.first().copied()) << 4) | nib(p.get(1).copied()))
        .collect()
}

struct Fixture {
    root: PathBuf,
    horos: PathBuf,
    ct_study: String,
    added: f64,
}

fn horos_library(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("ruthos-horos-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let horos = root.join("Horos Data");
    let bucket = horos.join("DATABASE.noindex").join("10000");
    std::fs::create_dir_all(&bucket).unwrap();
    std::fs::copy(testdata("CT_small.dcm"), bucket.join("1.dcm")).unwrap();
    std::fs::copy(testdata("MR_small.dcm"), bucket.join("2.dcm")).unwrap();
    let ct = ruthos_dicom::read_meta(&bucket.join("1.dcm")).unwrap();
    let mr = ruthos_dicom::read_meta(&bucket.join("2.dcm")).unwrap();

    let conn = Connection::open(horos.join("Database.sql")).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZSTUDY (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZSTUDYINSTANCEUID VARCHAR, ZNAME VARCHAR,
            ZCOMMENT VARCHAR, ZCOMMENT2 VARCHAR, ZSTATETEXT INTEGER, ZDATEADDED TIMESTAMP, ZDATEOPENED TIMESTAMP,
            ZREPORTURL VARCHAR);
         CREATE TABLE ZSERIES (Z_PK INTEGER PRIMARY KEY, ZSTUDY INTEGER, ZNAME VARCHAR, ZMODALITY VARCHAR);
         CREATE TABLE ZIMAGE (Z_PK INTEGER PRIMARY KEY, ZSERIES INTEGER, ZPATHNUMBER INTEGER, ZPATHSTRING VARCHAR,
            ZCOMPRESSEDSOPINSTANCEUID BLOB, ZSTOREDISKEYIMAGE INTEGER, ZCOMMENT VARCHAR);
         CREATE TABLE ZALBUM (Z_PK INTEGER PRIMARY KEY, ZINDEX INTEGER, ZNAME VARCHAR, ZSMARTALBUM INTEGER,
            ZPREDICATESTRING VARCHAR);
         CREATE TABLE Z_1STUDIES (Z_1ALBUMS INTEGER, Z_3STUDIES INTEGER);",
    )
    .unwrap();
    // 1 January 2020 in Core Data time.
    let added = 599_616_000.0_f64;
    conn.execute(
        "INSERT INTO ZSTUDY VALUES (1, 3, ?1, 'BRS^CORNELIA', 'Grade 3 murmur', 'recheck', 2, ?2, NULL, '/Users/v/r.doc')",
        params![ct.study_instance_uid, added],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ZSTUDY VALUES (2, 3, ?1, 'MR^PATIENT', NULL, NULL, 0, ?2, NULL, NULL)",
        params![mr.study_instance_uid, added],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO ZSERIES VALUES (10, 1, 'CT', 'CT');
         INSERT INTO ZSERIES VALUES (11, 2, 'MR', 'MR');
         INSERT INTO ZSERIES VALUES (12, 1, 'OsiriX ROI SR', 'SR');",
    )
    .unwrap();
    let ins = "INSERT INTO ZIMAGE VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    conn.execute(
        ins,
        params![
            100,
            10,
            1,
            None::<String>,
            encode_uid(&ct.sop_instance_uid),
            1,
            "best view"
        ],
    )
    .unwrap();
    conn.execute(
        ins,
        params![
            101,
            11,
            2,
            None::<String>,
            encode_uid(&mr.sop_instance_uid),
            0,
            None::<String>
        ],
    )
    .unwrap();
    // A file that is not on this computer, and Horos's own measurement object.
    conn.execute(
        ins,
        params![
            102,
            11,
            None::<i64>,
            "/Volumes/USB/elsewhere.dcm",
            None::<Vec<u8>>,
            0,
            None::<String>
        ],
    )
    .unwrap();
    conn.execute(
        ins,
        params![103, 12, 3, None::<String>, None::<Vec<u8>>, 0, None::<String>],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO ZALBUM VALUES (1, 1, 'Cardiology', 0, NULL);
         INSERT INTO ZALBUM VALUES (2, 2, 'Ultrasound', 1, '(modality CONTAINS[cd] \"US\")');
         INSERT INTO ZALBUM VALUES (3, 3, 'CT or MR', 1, '(modality == \"CT\") OR (modality == \"MR\")');
         INSERT INTO ZALBUM VALUES (4, 0, 'Just Added', 1, 'dateAdded >= CAST($NSDATE_LASTHOUR, \"NSDate\")');
         INSERT INTO Z_1STUDIES VALUES (1, 1);",
    )
    .unwrap();
    Fixture {
        root,
        horos,
        ct_study: ct.study_instance_uid,
        added,
    }
}

#[test]
fn a_horos_library_comes_over_with_its_albums_comments_and_key_images() {
    let fx = horos_library("full");
    let paths = DataPaths::new(fx.root.join("ruthos"));
    paths.ensure().unwrap();
    let db = Database::open(&paths).unwrap();

    // Found from the folder above the library, too.
    let lib = locate(&fx.root).expect("library found from its parent folder");
    assert_eq!(lib.data_dir, fx.horos);
    let catalog = read_catalog(&lib, &paths).unwrap();
    assert_eq!(catalog.studies.len(), 2);
    assert_eq!(catalog.importable(), 2);
    assert_eq!(catalog.missing(), 1);
    assert_eq!(catalog.key_images(), 1);

    let s = migrate(
        &db,
        &paths,
        &catalog,
        MigrateMode::Copy,
        &|_, _, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(s.images_imported, 2, "{s:?}");
    assert_eq!(s.images_missing, 1);
    assert_eq!(s.horos_private_skipped, 1);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert_eq!((s.new_studies, s.comments, s.states, s.key_images), (2, 1, 1, 1));
    assert_eq!((s.albums_created, s.smart_albums_created), (1, 1));
    assert_eq!(s.smart_albums_skipped.len(), 1, "the OR rule is reported");
    assert_eq!(s.smart_albums_skipped[0].0, "CT or MR");
    assert_eq!(s.reports_not_copied, 1);

    let ct_id = db.study_id_for_uid(&fx.ct_study).unwrap().unwrap();
    let ct = db.study(ct_id).unwrap().unwrap();
    assert_eq!(ct.comment, "Grade 3 murmur · recheck");
    assert_eq!(ct.state, 2);
    assert_eq!(
        ct.date_added,
        (fx.added + 978_307_200.0) as i64,
        "Horos's date added is kept"
    );
    let keys = db.key_images_for_study(ct_id).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].comment, "best view");

    let albums = db.albums().unwrap();
    let cardio = albums.iter().find(|a| a.name == "Cardiology").unwrap();
    assert_eq!(cardio.number_of_studies, 1);
    let us = albums.iter().find(|a| a.name == "Ultrasound").unwrap();
    assert_eq!(Database::smart_album_filter(us).unwrap().modality, "US");
    assert!(
        !albums.iter().any(|a| a.name == "Just Added"),
        "built-in albums are not duplicated"
    );

    // Horos's files are left where they were.
    assert!(fx.horos.join("DATABASE.noindex/10000/1.dcm").is_file());

    // Running it again changes nothing.
    let again = migrate(
        &db,
        &paths,
        &catalog,
        MigrateMode::Copy,
        &|_, _, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!((again.images_imported, again.images_present), (0, 2), "{again:?}");
    assert_eq!(
        (again.albums_created, again.albums_merged, again.smart_albums_created),
        (0, 1, 0)
    );
    assert_eq!(again.comments, 0);
    assert_eq!(db.albums().unwrap().len(), albums.len());
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn a_folder_that_is_not_a_horos_library_is_refused() {
    let dir = std::env::temp_dir().join(format!("ruthos-horos-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(locate(&dir).is_none());
    // A Database.sql that is some other SQLite file.
    Connection::open(dir.join("Database.sql"))
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    let lib = locate(&dir).unwrap();
    let paths = DataPaths::new(dir.join("ruthos"));
    paths.ensure().unwrap();
    let err = read_catalog(&lib, &paths).unwrap_err().to_string();
    assert!(err.contains("not a Horos"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
