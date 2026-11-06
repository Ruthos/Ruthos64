//! A run of Ruthos that only shows one set of images and leaves no trace.
//!
//! A package on a stick or in a download starts the program beside a
//! `ruthos.ini` that names its DICOMDIR. The person opening it may have their
//! own Ruthos on the same computer, so this run reads and writes none of that
//! program's settings or data. Everything it needs lives in memory or in a
//! temporary folder that is deleted when it closes, and nothing is written
//! beside the images, which may be on a read-only stick.
//!
//! The switch is thrown once, before anything else starts, because logging and
//! settings are the first things to touch the disk.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const TEMP_PREFIX: &str = "Ruthos-view-";
/// A folder this old belongs to a run that crashed.
const STALE: Duration = Duration::from_secs(48 * 3600);

/// What a viewer run was asked to show.
#[derive(Debug, Clone)]
pub struct ViewerRun {
    /// A DICOMDIR, or a folder when there is none.
    pub source: PathBuf,
    /// Deleted when the run ends.
    pub temp: PathBuf,
}

static RUN: OnceLock<ViewerRun> = OnceLock::new();

/// Make this a viewer run. Call at most once, first thing.
pub fn enter(source: PathBuf) -> &'static ViewerRun {
    RUN.get_or_init(|| {
        clear_stale(&std::env::temp_dir());
        let name = format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4());
        ViewerRun {
            source,
            temp: std::env::temp_dir().join(name),
        }
    })
}

/// The viewer run, if this is one.
pub fn run() -> Option<&'static ViewerRun> {
    RUN.get()
}

pub fn active() -> bool {
    RUN.get().is_some()
}

/// Delete this run's temporary folder.
pub fn leave() {
    if let Some(run) = RUN.get() {
        let _ = std::fs::remove_dir_all(&run.temp);
    }
}

fn clear_stale(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// True when this program was run out of a zip that was never extracted:
/// Explorer copies just the one file to a temporary folder, without the images
/// beside it. `exe` is this program, `temp` the system's temporary folder.
pub fn run_from_unextracted_zip(exe: &Path, temp: &Path) -> bool {
    let Ok(inside) = exe.strip_prefix(temp) else {
        return false;
    };
    inside
        .components()
        .any(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase().ends_with(".zip"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_inside_an_unextracted_zip_is_recognised() {
        let temp = Path::new("C:/Users/a/AppData/Local/Temp");
        let inside = temp.join("Temp1_Bella CT.zip/Bella CT/Ruthos Viewer.exe");
        assert!(run_from_unextracted_zip(&inside, temp));
        assert!(!run_from_unextracted_zip(
            Path::new("E:/Bella CT/Ruthos Viewer.exe"),
            temp
        ));
        assert!(!run_from_unextracted_zip(&temp.join("build/ruthos.exe"), temp));
        assert!(!run_from_unextracted_zip(
            Path::new("C:/Users/a/Downloads/Bella.zip/Ruthos Viewer.exe"),
            temp
        ));
    }
}
