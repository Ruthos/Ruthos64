//! Catalogued drives: every DICOM file on a drive is recorded where it is,
//! with no copy made and nothing written to the drive. Studies are searchable
//! with the drive unplugged, and open when it is connected.
//!
//! A drive may come back under another letter, so its images are looked for on
//! each connected volume, at the same path within the volume and with the same
//! size, and the catalogue follows them.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use rayon::prelude::*;

use crate::db::Database;
use crate::import::{import_file, is_ignorable, summarise, ImportEvent, ImportMode, ImportSummary};
use crate::paths::DataPaths;

/// Reading files in parallel helps, but a spinning disk slows down with many
/// readers seeking at once.
const READERS: usize = 4;
/// How many images are looked for to recognise a drive.
const SAMPLES: usize = 3;

/// Folders with no patient images in them, and a lot of files.
const SKIPPED_FOLDERS: [&str; 10] = [
    "$recycle.bin",
    "system volume information",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "appdata",
    "node_modules",
    "library",
    "lost+found",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Catalogue {
    pub id: i64,
    pub name: String,
    /// Where it is, ending in a separator so that `E:\scans\` is not a match
    /// for `E:\scans2\`.
    pub root: String,
    pub scanned_at: i64,
    pub images: i64,
}

/// A catalogue, and whether it can be reached right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub catalogue: Catalogue,
    pub online: bool,
}

/// The form a root is kept in.
pub fn root_form(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if text.ends_with(std::path::MAIN_SEPARATOR) {
        text
    } else {
        format!("{text}{}", std::path::MAIN_SEPARATOR)
    }
}

/// A name for a new catalogue: the folder's, or the drive's.
pub fn default_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => {
            let text = path.to_string_lossy();
            format!("Drive {}", text.trim_end_matches(['\\', '/']))
        }
    }
}

fn skipped(entry: &walkdir::DirEntry, own_data: &Path) -> bool {
    if !entry.file_type().is_dir() || entry.depth() == 0 {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    name.starts_with('.') || SKIPPED_FOLDERS.contains(&name.as_str()) || entry.path() == own_data
}

fn find_files(root: &Path, own_data: &Path, tx: &Sender<ImportEvent>, cancel: &AtomicBool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !skipped(e, own_data));
    for entry in walk.filter_map(|e| e.ok()) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if entry.file_type().is_file() && !is_ignorable(entry.path()) {
            found.push(entry.path().to_path_buf());
            if found.len().is_multiple_of(500) {
                let _ = tx.send(ImportEvent::Progress {
                    done: found.len(),
                    total: 0,
                    current: "Looking for files".to_string(),
                });
            }
        }
    }
    found
}

/// Record every DICOM file under `root` where it is. What was found before a
/// cancel stays in the catalogue.
pub fn scan(
    db: &Database,
    paths: &DataPaths,
    root: &Path,
    tx: &Sender<ImportEvent>,
    cancel: &AtomicBool,
) -> crate::Result<ImportSummary> {
    db.add_catalogue(&default_name(root), &root_form(root))?;
    let files = find_files(root, &paths.root, tx, cancel);
    let total = files.len();
    let _ = tx.send(ImportEvent::Started { total });
    let done = AtomicUsize::new(0);
    let read = |file: PathBuf| {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let result = import_file(db, paths, &file, ImportMode::Link);
        let _ = tx.send(ImportEvent::Progress {
            done: done.fetch_add(1, Ordering::Relaxed) + 1,
            total,
            current: file
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        });
        Some((file, result))
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(READERS)
        .build()
        .map_err(|e| crate::CoreError::Other(e.to_string()))?;
    let results: Vec<_> = pool.install(|| files.into_par_iter().filter_map(read).collect());
    let mut summary = summarise(results);
    summary.catalogued = true;
    // Routing rules are for images arriving, not for an archive being listed.
    summary.image_ids.clear();
    Ok(summary)
}

/// `scan` on a background thread.
pub fn spawn_scan(
    db: Arc<Database>,
    paths: DataPaths,
    root: PathBuf,
    tx: Sender<ImportEvent>,
    cancel: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ruthos-catalogue".into())
        .spawn(move || {
            let summary = scan(&db, &paths, &root, &tx, &cancel).unwrap_or_else(|e| {
                let mut failed = ImportSummary {
                    catalogued: true,
                    ..Default::default()
                };
                failed.errors.push((root.clone(), e.to_string()));
                failed
            });
            let _ = tx.send(ImportEvent::Finished(summary));
        })
}

/// A catalogue root's path within its volume.
fn within_volume(root: &str) -> PathBuf {
    let parts: Vec<_> = Path::new(root)
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let mounted_at = match parts.first().and_then(|p| p.to_str()) {
        Some("Volumes" | "mnt") => 2,
        Some("media") => 3,
        Some("run") => 4,
        _ => 0,
    };
    parts.into_iter().skip(mounted_at).collect()
}

/// Volumes connected now.
pub fn volumes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        out.extend((b'A'..=b'Z').map(|letter| PathBuf::from(format!("{}:\\", letter as char))));
    }
    let mounts: &[(&str, usize)] = &[("/Volumes", 1), ("/mnt", 1), ("/media", 2), ("/run/media", 2)];
    for (base, depth) in mounts {
        let walk = walkdir::WalkDir::new(base).min_depth(*depth).max_depth(*depth);
        out.extend(
            walk.follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
                .map(|e| e.into_path()),
        );
    }
    out.retain(|p| p.is_dir());
    out
}

fn same_file(path: &Path, size: i64) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() as i64 == size)
}

/// Where a catalogue's images are now, among `volumes`: every sample at the
/// same place within a volume, with the same size.
pub fn locate(root: &str, samples: &[(String, i64)], volumes: &[PathBuf]) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let tail = within_volume(root);
    volumes.iter().find_map(|volume| {
        let moved = root_form(&volume.join(&tail));
        let here = samples.iter().all(|(path, size)| {
            path.strip_prefix(root)
                .is_some_and(|rest| same_file(Path::new(&format!("{moved}{rest}")), *size))
        });
        (here && moved != root).then_some(moved)
    })
}

/// Which catalogues can be reached, following any that have moved. The checks
/// can wait on a slow drive, so call this off the interface thread.
pub fn check(db: &Database) -> crate::Result<Vec<Status>> {
    let connected = volumes();
    let mut out = Vec::new();
    for mut catalogue in db.catalogues()? {
        let samples = db.catalogue_samples(&catalogue.root, SAMPLES)?;
        let mut online = if samples.is_empty() {
            Path::new(&catalogue.root).is_dir()
        } else {
            samples.iter().any(|(path, size)| same_file(Path::new(path), *size))
        };
        if !online {
            if let Some(root) = locate(&catalogue.root, &samples, &connected) {
                tracing::info!("catalogue {} is now at {root}", catalogue.name);
                if db.move_catalogue(catalogue.id, &catalogue.root, &root)? {
                    catalogue.root = root;
                    online = true;
                }
            }
        }
        out.push(Status { catalogue, online });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::StudyFilter;

    fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/pydicom")
            .join(name)
    }

    struct Fixture {
        dir: PathBuf,
        paths: DataPaths,
        db: Arc<Database>,
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("ruthos-cat-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let paths = DataPaths::new(dir.join("data"));
        let db = Database::open(&paths).unwrap();
        Fixture { dir, paths, db }
    }

    /// A drive holding a CT in a folder, and an MR in the recycle bin.
    fn drive(root: &Path) {
        std::fs::create_dir_all(root.join("cases/2024")).unwrap();
        std::fs::create_dir_all(root.join("$RECYCLE.BIN")).unwrap();
        std::fs::copy(sample("CT_small.dcm"), root.join("cases/2024/ct.dcm")).unwrap();
        std::fs::copy(sample("MR_small.dcm"), root.join("$RECYCLE.BIN/mr.dcm")).unwrap();
        std::fs::write(root.join("cases/notes.txt"), "not an image").unwrap();
    }

    fn run(f: &Fixture, root: &Path, cancel: bool) -> ImportSummary {
        let (tx, _rx) = crossbeam_channel::unbounded();
        scan(&f.db, &f.paths, root, &tx, &AtomicBool::new(cancel)).unwrap()
    }

    /// Every file under a folder, with its contents.
    fn contents(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| (e.path().to_path_buf(), std::fs::read(e.path()).unwrap()))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_scan_points_at_files_and_changes_nothing_on_the_drive() {
        let f = fixture("points");
        let root = f.dir.join("drive");
        drive(&root);
        let before = contents(&root);
        let summary = run(&f, &root, false);
        assert_eq!(summary.imported, 1, "the CT, and not what is in the recycle bin");
        assert!(summary.catalogued);
        assert!(summary.image_ids.is_empty(), "no routing rule may fire on a catalogue");
        assert_eq!(contents(&root), before, "the drive is untouched");

        let cats = f.db.catalogues().unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!((cats[0].images, cats[0].name.as_str()), (1, "drive"));
        let (path, stored) =
            f.db.first_image(f.db.list_studies(&StudyFilter::default()).unwrap()[0].id)
                .unwrap()
                .unwrap();
        assert!(!stored, "a pointer, not a copy");
        assert!(path.starts_with(&cats[0].root), "{path}");
        let copies = contents(&f.paths.root)
            .iter()
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "dcm"))
            .count();
        assert_eq!(copies, 0, "no image was copied into the database");
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn scanning_again_adds_only_what_is_new() {
        let f = fixture("again");
        let root = f.dir.join("drive");
        drive(&root);
        run(&f, &root, false);
        std::fs::copy(sample("JPEG2000.dcm"), root.join("cases/new.dcm")).unwrap();
        let summary = run(&f, &root, false);
        assert_eq!((summary.imported, summary.duplicates), (1, 1));
        assert_eq!(f.db.catalogues().unwrap().len(), 1, "the same catalogue");
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn a_cancelled_scan_stops_and_keeps_the_catalogue() {
        let f = fixture("cancel");
        let root = f.dir.join("drive");
        drive(&root);
        assert_eq!(run(&f, &root, true).imported, 0);
        assert_eq!(f.db.catalogues().unwrap().len(), 1);
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn a_catalogue_lists_only_its_own_studies_and_not_a_folder_with_a_longer_name() {
        let f = fixture("prefix");
        let (a, b) = (f.dir.join("scans"), f.dir.join("scans2"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::copy(sample("CT_small.dcm"), a.join("ct.dcm")).unwrap();
        std::fs::copy(sample("MR_small.dcm"), b.join("mr.dcm")).unwrap();
        run(&f, &a, false);
        run(&f, &b, false);
        for cat in f.db.catalogues().unwrap() {
            assert_eq!(cat.images, 1, "{}", cat.name);
            let filter = StudyFilter {
                catalogue_root: Some(cat.root.clone()),
                ..Default::default()
            };
            assert_eq!(f.db.list_studies(&filter).unwrap().len(), 1, "{}", cat.name);
        }
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn a_drive_on_another_letter_is_found_and_its_pointers_follow() {
        let f = fixture("moved");
        let (old_volume, new_volume) = (f.dir.join("E"), f.dir.join("F"));
        let root = old_volume.join("cases");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(sample("CT_small.dcm"), root.join("ct.dcm")).unwrap();
        run(&f, &root, false);
        let cat = &f.db.catalogues().unwrap()[0];
        let samples = f.db.catalogue_samples(&cat.root, SAMPLES).unwrap();
        assert_eq!(samples.len(), 1);

        // Unplugged from one place, plugged in at another. Only the part of
        // the path inside the volume is compared, so tests give the volume.
        let within = within_volume(&cat.root);
        let moved_root = new_volume.join(&within);
        std::fs::create_dir_all(&moved_root).unwrap();
        std::fs::copy(sample("CT_small.dcm"), moved_root.join("ct.dcm")).unwrap();
        std::fs::remove_dir_all(&old_volume).ok();
        let found = locate(&cat.root, &samples, std::slice::from_ref(&new_volume)).unwrap();
        assert_eq!(found, root_form(&moved_root));

        assert!(f.db.move_catalogue(cat.id, &cat.root, &found).unwrap());
        let cat = &f.db.catalogues().unwrap()[0];
        assert_eq!((cat.root.as_str(), cat.images), (found.as_str(), 1));
        let study = f.db.list_studies(&StudyFilter::default()).unwrap()[0].id;
        let (path, _) = f.db.first_image(study).unwrap().unwrap();
        assert!(Path::new(&path).is_file(), "{path}");
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn a_different_drive_with_the_same_layout_is_not_taken_for_it() {
        let f = fixture("other");
        let root = f.dir.join("E").join("cases");
        std::fs::create_dir_all(&root).unwrap();
        let cat_root = root_form(&root);
        let samples = vec![(format!("{cat_root}ct.dcm"), 1000)];
        let other = f.dir.join("F");
        let there = other.join(within_volume(&cat_root));
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join("ct.dcm"), vec![0u8; 999]).unwrap();
        assert_eq!(
            locate(&cat_root, &samples, std::slice::from_ref(&other)),
            None,
            "a different size"
        );
        assert_eq!(
            locate(&cat_root, &[], std::slice::from_ref(&other)),
            None,
            "nothing to recognise it by"
        );
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn forgetting_a_drive_removes_its_records_and_never_its_files() {
        let f = fixture("forget");
        let root = f.dir.join("drive");
        drive(&root);
        run(&f, &root, false);
        let before = contents(&root);
        let cat = f.db.catalogues().unwrap().remove(0);
        assert_eq!(f.db.forget_catalogue(&cat).unwrap().len(), 1);
        assert!(f.db.catalogues().unwrap().is_empty());
        assert!(f.db.list_studies(&StudyFilter::default()).unwrap().is_empty());
        assert_eq!(contents(&root), before, "the drive is untouched");
        std::fs::remove_dir_all(&f.dir).ok();
    }

    #[test]
    fn the_part_of_a_path_inside_its_volume() {
        assert_eq!(within_volume("/Volumes/Backup/cases/"), PathBuf::from("cases"));
        assert_eq!(within_volume("/media/mark/Backup/cases/"), PathBuf::from("cases"));
        assert_eq!(within_volume("/run/media/mark/Backup/cases/"), PathBuf::from("cases"));
        assert_eq!(within_volume("/mnt/backup/"), PathBuf::new());
        assert_eq!(within_volume("/home/mark/cases/"), PathBuf::from("home/mark/cases"));
    }

    #[test]
    fn a_root_always_ends_in_a_separator() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(root_form(Path::new("cases")), format!("cases{sep}"));
        assert_eq!(root_form(Path::new(&format!("cases{sep}"))), format!("cases{sep}"));
        assert_eq!(default_name(Path::new("cases")), "cases");
    }
}
