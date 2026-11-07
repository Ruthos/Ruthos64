//! The incoming folder scanner.
//!
//! The DICOM listener writes received files into `INCOMING.noindex`. This
//! scanner polls that folder and moves settled files into the archive. Polling
//! rather than file-system notifications keeps the behaviour identical for
//! files dropped there by hand, by a network share, or by the listener.
//!
//! A file that cannot be imported is moved to `NOT READABLE` beside the
//! incoming folder (Horos's folder for files it keeps rather than deletes),
//! so it is reported once and not read again on every scan.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;

use crate::db::Database;
use crate::import::{import_file, FileOutcome, ImportEvent, ImportMode, ImportSummary};
use crate::paths::DataPaths;

/// A file must have been left alone this long before it is imported.
const SETTLE: Duration = Duration::from_secs(1);

pub struct IncomingScanner;

impl IncomingScanner {
    pub fn spawn(
        db: Arc<Database>,
        paths: DataPaths,
        interval: Duration,
        tx: Sender<ImportEvent>,
        stop: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("ruthos-incoming".into())
            .spawn(move || {
                let _ = std::fs::create_dir_all(paths.incoming_dir());
                while !stop.load(Ordering::Relaxed) {
                    if let Some(summary) = scan_once(&db, &paths, SETTLE) {
                        let _ = tx.send(ImportEvent::Finished(summary));
                    }
                    // Sleep in short steps so shutdown is prompt.
                    let mut slept = Duration::ZERO;
                    while slept < interval && !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        slept += Duration::from_millis(200);
                    }
                }
            })
            .expect("spawn incoming scanner")
    }
}

/// Import what has settled in the incoming folder. Files that cannot be
/// imported are moved to `NOT READABLE`. `None` when there was nothing to
/// look at.
pub fn scan_once(db: &Database, paths: &DataPaths, settle: Duration) -> Option<ImportSummary> {
    let files = settled_files(&paths.incoming_dir(), settle);
    if files.is_empty() {
        return None;
    }
    let mut summary = ImportSummary {
        from_network: true,
        ..Default::default()
    };
    for file in files {
        match import_file(db, paths, &file, ImportMode::Move) {
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
            Ok(FileOutcome::Duplicate) => {
                summary.duplicates += 1;
                let _ = std::fs::remove_file(&file);
            }
            Ok(FileOutcome::NotDicom) => {
                summary.not_dicom += 1;
                quarantine(paths, &file);
            }
            Err(e) => {
                tracing::warn!("incoming file {} failed: {e}", file.display());
                summary.errors.push((file.clone(), e.to_string()));
                quarantine(paths, &file);
            }
        }
    }
    remove_empty_dirs(&paths.incoming_dir());
    Some(summary)
}

/// Files in `dir` (recursively) not modified within `settle`.
fn settled_files(dir: &Path, settle: Duration) -> Vec<PathBuf> {
    let now = SystemTime::now();
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && !name.ends_with(".part") && !name.ends_with(".tmp")
        })
        .filter(|e| match e.metadata().ok().and_then(|m| m.modified().ok()) {
            Some(modified) => now.duration_since(modified).map(|d| d >= settle).unwrap_or(true),
            None => false,
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Move a file that could not be imported out of the incoming folder, to
/// `NOT READABLE` (Horos's name and place for them), so the next scan does
/// not pick it up again. Files an earlier version left in
/// `INCOMING.noindex/_rejected` are moved there by the first scan.
fn quarantine(paths: &DataPaths, file: &Path) {
    let Some(name) = file.file_name() else { return };
    let dir = paths.not_readable_dir();
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::rename(file, dir.join(name))) {
        tracing::warn!("could not move {} to {}: {e}", file.display(), dir.display());
    }
}

fn remove_empty_dirs(dir: &Path) {
    let entries: Vec<_> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .collect();
    for e in entries {
        let _ = std::fs::remove_dir(e.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(tag: &str) -> (Arc<Database>, DataPaths, PathBuf) {
        let root = std::env::temp_dir().join(format!("ruthos-incoming-{tag}-{}", std::process::id()));
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
    fn a_file_that_cannot_be_imported_is_set_aside_and_reported_once() {
        let (db, paths, root) = archive("reject");
        let incoming = paths.incoming_dir();
        std::fs::write(incoming.join("notes.bin"), b"not a DICOM file").unwrap();
        std::fs::copy(sample("CT_small.dcm"), incoming.join("ct.dcm")).unwrap();

        let first = scan_once(&db, &paths, Duration::ZERO).expect("two files to look at");
        assert_eq!((first.imported, first.not_dicom), (1, 1), "{first:?}");
        assert!(paths.not_readable_dir().join("notes.bin").is_file());
        assert!(!incoming.join("notes.bin").exists());

        // Later scans find nothing: the rejected file is not read again.
        assert!(scan_once(&db, &paths, Duration::ZERO).is_none());
        assert!(scan_once(&db, &paths, Duration::ZERO).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_an_earlier_version_rejected_move_out_on_the_first_scan() {
        let (db, paths, root) = archive("legacy");
        let legacy = paths.incoming_dir().join("_rejected");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("old.bin"), b"rejected by 0.1.5-alpha").unwrap();

        let first = scan_once(&db, &paths, Duration::ZERO).expect("one file to look at");
        assert_eq!(first.not_dicom, 1, "{first:?}");
        assert!(paths.not_readable_dir().join("old.bin").is_file());
        assert!(!legacy.exists(), "the emptied folder is removed");
        assert!(scan_once(&db, &paths, Duration::ZERO).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
