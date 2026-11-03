//! Watch folders: import new DICOM files that appear in folders the user
//! chose (Horos: "watched folders"), such as a modality's export share, a
//! scanner's output folder or a USB-mounted CR reader.
//!
//! Each folder is scanned every few seconds, including its subfolders. A
//! file is imported once two scans at least a couple of seconds apart have
//! seen the same size and modification time; a copy in progress keeps the
//! source's modification time on Windows, so the time alone is not enough.
//! What was imported, and
//! what was not DICOM, is remembered by path, size and modification time in
//! the database, so a restart does not read every file again and a file
//! written anew is imported anew. The originals are never moved or deleted:
//! they are copied into the archive, or used where they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::import::{import_file, FileOutcome, ImportEvent, ImportMode, ImportSummary};
use crate::paths::DataPaths;

/// A folder to watch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchFolder {
    pub path: PathBuf,
    pub enabled: bool,
    /// Copy files into the archive (true) or use them where they are.
    pub copy: bool,
}

impl Default for WatchFolder {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            enabled: true,
            copy: true,
        }
    }
}

/// What happened at a folder's last scan, for Preferences.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatchStatus {
    /// Local time of the last scan, `HH:MM:SS`.
    pub checked: String,
    pub reachable: bool,
    /// Images imported since Ruthos started.
    pub imported: usize,
    pub last_error: Option<String>,
}

/// The folders to watch, changed by Preferences while the scanner runs, and
/// what each scan found.
#[derive(Default)]
pub struct WatchState {
    pub folders: Mutex<Vec<WatchFolder>>,
    pub status: Mutex<HashMap<PathBuf, WatchStatus>>,
}

/// Why a folder cannot be watched, in words for Preferences.
pub fn refuse(folder: &Path, paths: &DataPaths) -> Option<String> {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (f, root) = (norm(folder), norm(&paths.root));
    if f.starts_with(&root) {
        return Some("This folder is inside Ruthos's own data folder.".into());
    }
    if root.starts_with(&f) {
        return Some("Ruthos's data folder is inside this folder, so it would import its own files.".into());
    }
    None
}

/// Files to look at per scan: a huge folder is worked through over several
/// scans, so imports appear as they go and shutdown stays prompt.
const PER_SCAN: usize = 2000;
/// A file must have been seen unchanged for this long.
const SETTLE: Duration = Duration::from_secs(2);

/// Files seen but not yet settled: size, modification time and when they
/// were first seen like that.
pub type Pending = HashMap<String, (i64, i64, std::time::Instant)>;

pub struct WatchScanner;

impl WatchScanner {
    pub fn spawn(
        db: Arc<Database>,
        paths: DataPaths,
        state: Arc<WatchState>,
        interval: Duration,
        tx: Sender<ImportEvent>,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("ruthos-watch".into())
            .spawn(move || {
                let mut pending = Pending::new();
                while !stop.load(Ordering::Relaxed) {
                    let folders: Vec<WatchFolder> = state.folders.lock().clone();
                    for folder in folders.iter().filter(|f| f.enabled && !f.path.as_os_str().is_empty()) {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let summary = scan_once(&db, &paths, folder, &mut pending, SETTLE, &stop);
                        let mut st = state.status.lock();
                        let entry = st.entry(folder.path.clone()).or_default();
                        entry.checked = chrono::Local::now().format("%H:%M:%S").to_string();
                        match summary {
                            Ok(s) => {
                                entry.reachable = true;
                                entry.imported += s.imported;
                                entry.last_error = s.errors.first().map(|(f, e)| format!("{}: {e}", f.display()));
                                if s.total_files() > 0 && (s.imported > 0 || !s.errors.is_empty()) {
                                    drop(st);
                                    let _ = tx.send(ImportEvent::Finished(s));
                                }
                            }
                            Err(e) => {
                                entry.reachable = false;
                                entry.last_error = Some(e);
                            }
                        }
                    }
                    let mut slept = Duration::ZERO;
                    while slept < interval && !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        slept += Duration::from_millis(200);
                    }
                }
            })
            .expect("spawn watch scanner")
    }
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Import what is new in one folder. `Err` when the folder cannot be read
/// (a disconnected drive or share).
pub fn scan_once(
    db: &Database,
    paths: &DataPaths,
    folder: &WatchFolder,
    pending: &mut Pending,
    settle: Duration,
    stop: &AtomicBool,
) -> Result<ImportSummary, String> {
    if !folder.path.is_dir() {
        return Err("the folder is not reachable".into());
    }
    if let Some(why) = refuse(&folder.path, paths) {
        return Err(why);
    }
    let mode = if folder.copy {
        ImportMode::Copy
    } else {
        ImportMode::Link
    };
    let now = std::time::Instant::now();
    let mut summary = ImportSummary::default();
    let mut looked = 0usize;
    for e in walkdir::WalkDir::new(&folder.path).into_iter().filter_map(|e| e.ok()) {
        if stop.load(Ordering::Relaxed) || looked >= PER_SCAN {
            break;
        }
        if !e.file_type().is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy();
        if name.starts_with('.') || name.ends_with(".part") || name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        let path = e.path().to_string_lossy().to_string();
        let (size, mtime) = (meta.len() as i64, unix(modified));
        if size == 0 || db.watched_file_seen(&path, size, mtime).unwrap_or(false) {
            continue;
        }
        // Wait until the file has stayed the same between scans.
        match pending.get(&path) {
            Some((s, m, since)) if *s == size && *m == mtime && now.duration_since(*since) >= settle => {
                pending.remove(&path);
            }
            Some((s, m, _)) if *s == size && *m == mtime => continue,
            _ => {
                pending.insert(path, (size, mtime, now));
                continue;
            }
        }
        looked += 1;
        match import_file(db, paths, e.path(), mode) {
            Ok(FileOutcome::Imported {
                study_id,
                new_study,
                image_id,
                ..
            }) => {
                summary.imported += 1;
                summary.image_ids.push(image_id);
                if new_study && !summary.new_study_ids.contains(&study_id) {
                    summary.new_study_ids.push(study_id);
                }
                if !summary.touched_study_ids.contains(&study_id) {
                    summary.touched_study_ids.push(study_id);
                }
            }
            Ok(FileOutcome::Duplicate) => summary.duplicates += 1,
            Ok(FileOutcome::NotDicom) => summary.not_dicom += 1,
            Err(err) => {
                tracing::warn!("watch folder file {} failed: {err}", e.path().display());
                summary.errors.push((e.path().to_path_buf(), err.to_string()));
            }
        }
        // Remembered whatever the outcome: a file that is not DICOM, or
        // that failed, is tried again only when it changes.
        let _ = db.mark_watched_file(&path, size, mtime);
    }
    if summary.imported > 0 {
        tracing::info!(
            "watch folder {}: {} imported, {} already present, {} not DICOM, {} errors",
            folder.path.display(),
            summary.imported,
            summary.duplicates,
            summary.not_dicom,
            summary.errors.len()
        );
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(tag: &str) -> (Arc<Database>, DataPaths, PathBuf) {
        let root = std::env::temp_dir().join(format!("ruthos-watch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = DataPaths::new(root.join("archive"));
        paths.ensure().unwrap();
        let db = Database::open(&paths).unwrap();
        (db, paths, root)
    }

    fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/pydicom")
            .join(name)
    }

    #[test]
    fn new_files_are_imported_once_and_changed_files_again() {
        let (db, paths, root) = archive("once");
        let watched = root.join("modality-export");
        std::fs::create_dir_all(watched.join("day1")).unwrap();
        let ct = watched.join("day1").join("ct.dcm");
        std::fs::copy(sample("CT_small.dcm"), &ct).unwrap();
        std::fs::write(watched.join("notes.txt"), b"not an image").unwrap();
        let folder = WatchFolder {
            path: watched.clone(),
            ..Default::default()
        };
        let stop = AtomicBool::new(false);
        let mut pending = Pending::new();
        let scan = |pending: &mut Pending| scan_once(&db, &paths, &folder, pending, Duration::ZERO, &stop).unwrap();
        // The first scan only notes the files; the next imports them.
        assert_eq!(scan(&mut pending).total_files(), 0);
        let s = scan(&mut pending);
        assert_eq!((s.imported, s.not_dicom), (1, 1), "{s:?}");
        // Seen files are not read again.
        assert_eq!(scan(&mut pending).total_files(), 0);
        // A file that grows between scans waits until it stops.
        let mr = watched.join("mr.dcm");
        let bytes = std::fs::read(sample("MR_small.dcm")).unwrap();
        std::fs::write(&mr, &bytes[..bytes.len() / 2]).unwrap();
        assert_eq!(scan(&mut pending).total_files(), 0);
        std::fs::write(&mr, &bytes).unwrap();
        assert_eq!(scan(&mut pending).total_files(), 0, "changed since the last scan");
        let s = scan(&mut pending);
        assert_eq!(s.imported, 1, "{s:?}");
        // The originals stay where they were.
        assert!(ct.is_file() && mr.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_archive_itself_and_missing_folders_are_refused() {
        let (db, paths, root) = archive("refuse");
        assert!(refuse(&paths.root.join("DATABASE.noindex"), &paths).is_some());
        assert!(refuse(&root, &paths).is_some(), "a folder containing the archive");
        assert!(refuse(&root.join("elsewhere"), &paths).is_none());
        let gone = WatchFolder {
            path: root.join("unplugged"),
            ..Default::default()
        };
        assert!(scan_once(
            &db,
            &paths,
            &gone,
            &mut Pending::new(),
            Duration::ZERO,
            &AtomicBool::new(false)
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
