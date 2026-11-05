//! Import pipeline.
//!
//! Walks folders, reads each file's header, copies (or links, or moves) the
//! file into the database folder and records it. Runs on a background thread
//! and reports progress over a channel so the database window can show it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use rayon::prelude::*;

use ruthos_dicom::{read_meta, DicomError};

use crate::db::{Database, InsertOutcome};
use crate::paths::DataPaths;
use crate::{CoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Copy into the database folder (Horos default).
    Copy,
    /// Record the file where it is.
    Link,
    /// Move into the database folder (used for the incoming folder).
    Move,
}

#[derive(Debug, Clone)]
pub enum ImportEvent {
    Started { total: usize },
    Progress { done: usize, total: usize, current: String },
    Finished(ImportSummary),
}

#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub imported: usize,
    pub duplicates: usize,
    pub not_dicom: usize,
    pub errors: Vec<(PathBuf, String)>,
    pub new_study_ids: Vec<i64>,
    pub touched_study_ids: Vec<i64>,
    /// Images added by this import, for routing.
    pub image_ids: Vec<i64>,
    /// Received over the network (the listener or a PACS retrieval)
    /// rather than imported from a folder or files.
    pub from_network: bool,
    /// A drive was catalogued: its images stay where they are.
    pub catalogued: bool,
}

impl ImportSummary {
    pub fn total_files(&self) -> usize {
        self.imported + self.duplicates + self.not_dicom + self.errors.len()
    }
}

/// Result of importing one file.
#[derive(Debug, Clone)]
pub enum FileOutcome {
    Imported {
        study_id: i64,
        series_id: i64,
        image_id: i64,
        new_study: bool,
    },
    Duplicate,
    NotDicom,
}

pub struct Importer {
    pub db: Arc<Database>,
    pub paths: DataPaths,
}

impl Importer {
    pub fn new(db: Arc<Database>, paths: DataPaths) -> Self {
        Self { db, paths }
    }

    /// Import a set of files and folders on a background thread.
    pub fn spawn(&self, sources: Vec<PathBuf>, mode: ImportMode, tx: Sender<ImportEvent>) -> JoinHandle<()> {
        let db = self.db.clone();
        let paths = self.paths.clone();
        std::thread::Builder::new()
            .name("ruthos-import".into())
            .spawn(move || {
                let files = collect_files(&sources);
                let total = files.len();
                let _ = tx.send(ImportEvent::Started { total });
                let done = AtomicUsize::new(0);

                let results: Vec<(PathBuf, std::result::Result<FileOutcome, CoreError>)> = files
                    .into_par_iter()
                    .map(|file| {
                        let r = import_file(&db, &paths, &file, mode);
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        let _ = tx.send(ImportEvent::Progress {
                            done: n,
                            total,
                            current: file
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        });
                        (file, r)
                    })
                    .collect();

                let summary = summarise(results);
                tracing::info!(
                    "import finished: {} imported, {} duplicates, {} non-DICOM, {} errors",
                    summary.imported,
                    summary.duplicates,
                    summary.not_dicom,
                    summary.errors.len()
                );
                // A folder written by the anonymiser carries its measurements in a sidecar.
                for dir in sidecar_dirs(&sources) {
                    match crate::teaching::apply_sidecar(&db, &dir) {
                        Ok(Some(reference)) => {
                            tracing::info!("applied teaching sidecar {reference} from {}", dir.display())
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("sidecar in {} not applied: {e}", dir.display()),
                    }
                }
                let _ = tx.send(ImportEvent::Finished(summary));
            })
            .expect("spawn import thread")
    }
}

/// Add up what happened to each file.
pub(crate) fn summarise(results: Vec<(PathBuf, std::result::Result<FileOutcome, CoreError>)>) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for (file, r) in results {
        match r {
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
            Err(e) => summary.errors.push((file, e.to_string())),
        }
    }
    summary
}

/// Expand files and folders into a flat list of candidate files.
pub fn collect_files(sources: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for src in sources {
        if src.is_dir() {
            for entry in walkdir::WalkDir::new(src)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file() && !is_ignorable(entry.path()) {
                    out.push(entry.path().to_path_buf());
                }
            }
        } else if src.is_file() && !is_ignorable(src) {
            out.push(src.clone());
        }
    }
    out
}

pub(crate) fn is_ignorable(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.starts_with('.') || name == "thumbs.db" || name == "desktop.ini" {
        return true;
    }
    // Files that are never DICOM; everything else is tried.
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "txt"
            | "pdf"
            | "html"
            | "htm"
            | "xml"
            | "json"
            | "exe"
            | "dll"
            | "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "bmp"
            | "zip"
            | "ini"
            | "log"
            | "db"
            | "sqlite"
            | "plist"
            | "css"
            | "js"
            | "mp4"
            | "mov"
            | "avi"
    )
}

/// Import one file. Safe to call from several threads at once.
pub fn import_file(db: &Database, paths: &DataPaths, file: &Path, mode: ImportMode) -> Result<FileOutcome> {
    let meta = match read_meta(file) {
        Ok(m) => m,
        Err(DicomError::NotDicom(_)) => return Ok(FileOutcome::NotDicom),
        Err(DicomError::Read(msg)) if looks_like_not_dicom(&msg) => return Ok(FileOutcome::NotDicom),
        Err(e) => return Err(e.into()),
    };

    // Images only. Structured reports, presentation states and the like are
    // skipped in this version; they are not "not DICOM", so they count as
    // errors the user can see.
    if meta.rows == 0 || meta.columns == 0 {
        return Err(CoreError::Other(format!(
            "{} has no image pixel data (SOP class {})",
            file.display(),
            meta.sop_class_uid
        )));
    }

    let file_size = std::fs::metadata(file).map(|m| m.len() as i64).unwrap_or(0);

    let (stored_path, in_db_folder) = match mode {
        ImportMode::Link => (file.to_string_lossy().to_string(), false),
        ImportMode::Copy | ImportMode::Move => {
            let dest = paths.stored_path_for(
                &meta.study_instance_uid,
                &meta.series_instance_uid,
                &meta.sop_instance_uid,
            );
            let rel = paths
                .relative_str(&dest)
                .unwrap_or_else(|| dest.to_string_lossy().to_string());
            // If the database already has this instance, do not touch the disk.
            if db.image_path_exists(&rel)? {
                if mode == ImportMode::Move {
                    let _ = std::fs::remove_file(file);
                }
                return Ok(FileOutcome::Duplicate);
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if mode == ImportMode::Move {
                if std::fs::rename(file, &dest).is_err() {
                    std::fs::copy(file, &dest)?;
                    let _ = std::fs::remove_file(file);
                }
            } else {
                std::fs::copy(file, &dest)?;
            }
            (rel, true)
        }
    };

    match db.insert_instance(&meta, &stored_path, in_db_folder, file_size)? {
        InsertOutcome::Inserted {
            study_id,
            series_id,
            image_id,
            new_study,
        } => Ok(FileOutcome::Imported {
            study_id,
            series_id,
            image_id,
            new_study,
        }),
        InsertOutcome::Duplicate => {
            // Raced with another thread or imported earlier with a different path.
            if in_db_folder && mode != ImportMode::Link {
                let dest = paths.resolve(&stored_path);
                let _ = std::fs::remove_file(dest);
            }
            Ok(FileOutcome::Duplicate)
        }
    }
}

fn looks_like_not_dicom(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("preamble")
        || m.contains("magic")
        || m.contains("dicm")
        || m.contains("unexpected end of file")
        || m.contains("invalid tag")
        || m.contains("undefined length")
        || m.contains("could not read")
}

/// Folders among the sources (or containing the source files) that hold a
/// `ruthos-case.json` sidecar.
fn sidecar_dirs(sources: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for src in sources {
        let dir = if src.is_dir() {
            src.clone()
        } else {
            src.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        if dir.join(crate::teaching::SIDECAR_NAME).is_file() && !out.contains(&dir) {
            out.push(dir.clone());
        }
        if src.is_dir() {
            if let Ok(entries) = std::fs::read_dir(src) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() && p.join(crate::teaching::SIDECAR_NAME).is_file() && !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignorable_names() {
        assert!(is_ignorable(Path::new("x/.DS_Store")));
        assert!(is_ignorable(Path::new("x/readme.txt")));
        assert!(!is_ignorable(Path::new("x/IM000001")));
        assert!(!is_ignorable(Path::new("x/scan.dcm")));
    }
}
