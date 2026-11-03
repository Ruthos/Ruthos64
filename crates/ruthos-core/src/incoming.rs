//! The incoming folder scanner.
//!
//! The DICOM listener writes received files into `INCOMING.noindex`. This
//! scanner polls that folder and moves settled files into the archive. Polling
//! rather than file-system notifications keeps the behaviour identical for
//! files dropped there by hand, by a network share, or by the listener.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;

use crate::db::Database;
use crate::import::{import_file, FileOutcome, ImportEvent, ImportMode, ImportSummary};
use crate::paths::DataPaths;

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
                    let files = settled_files(&paths.incoming_dir(), Duration::from_secs(1));
                    if !files.is_empty() {
                        let mut summary = ImportSummary {
                            from_network: true,
                            ..Default::default()
                        };
                        for file in files {
                            match import_file(&db, &paths, &file, ImportMode::Move) {
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
                                    quarantine(&paths, &file);
                                }
                                Err(e) => {
                                    tracing::warn!("incoming file {} failed: {e}", file.display());
                                    summary.errors.push((file.clone(), e.to_string()));
                                    quarantine(&paths, &file);
                                }
                            }
                        }
                        remove_empty_dirs(&paths.incoming_dir());
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

/// Files in `dir` (recursively) not modified within `settle`.
fn settled_files(dir: &std::path::Path, settle: Duration) -> Vec<PathBuf> {
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

fn quarantine(paths: &DataPaths, file: &std::path::Path) {
    let dir = paths.root.join("INCOMING.noindex").join("_rejected");
    let _ = std::fs::create_dir_all(&dir);
    if let Some(name) = file.file_name() {
        let _ = std::fs::rename(file, dir.join(name));
    }
}

fn remove_empty_dirs(dir: &std::path::Path) {
    let entries: Vec<_> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .collect();
    for e in entries {
        if e.file_name().to_string_lossy() == "_rejected" {
            continue;
        }
        let _ = std::fs::remove_dir(e.path());
    }
}
