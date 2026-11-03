//! On-disk layout.
//!
//! Mirrors the Horos data folder: a `DATABASE.noindex` tree of imported files,
//! an `INCOMING.noindex` drop folder that the DICOM listener and the import
//! scanner share, a thumbnail cache, and the SQLite database itself.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DataPaths {
    pub root: PathBuf,
}

impl DataPaths {
    /// Default location: `%LOCALAPPDATA%\Ruthos\Ruthos Data` on Windows,
    /// `~/Library/Application Support/Ruthos/Ruthos Data` on macOS (not the
    /// Documents folder, which iCloud Drive may sync), and the XDG data
    /// directory on Linux.
    pub fn default_root() -> PathBuf {
        app_support_dir().join("Ruthos Data")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn database_file(&self) -> PathBuf {
        self.root.join("Database.sqlite")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root.join("DATABASE.noindex")
    }

    pub fn incoming_dir(&self) -> PathBuf {
        self.root.join("INCOMING.noindex")
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.root.join("THUMBS")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.root.join("TEMP.noindex")
    }

    /// Create every folder that does not exist yet.
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [
            self.root.clone(),
            self.files_dir(),
            self.incoming_dir(),
            self.thumbs_dir(),
            self.temp_dir(),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }

    /// Where an imported instance is stored. Horos buckets files by study
    /// and series so a single folder never holds tens of thousands of files.
    pub fn stored_path_for(&self, study_uid: &str, series_uid: &str, sop_uid: &str) -> PathBuf {
        self.files_dir()
            .join(short_hash(study_uid))
            .join(short_hash(series_uid))
            .join(format!("{}.dcm", safe_filename(sop_uid)))
    }

    /// Path of a file relative to the data root, when it lives inside it.
    pub fn relative(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(|p| p.to_path_buf())
    }

    /// The relative path as stored in the archive: forward slashes on every
    /// platform, so an archive copied between a PC and a Mac opens on both.
    pub fn relative_str(&self, path: &Path) -> Option<String> {
        self.relative(path).map(|p| stored_form(&p.to_string_lossy()))
    }

    /// Resolve a path stored in the database back to an absolute path.
    /// Relative paths accept either separator, since archives written
    /// before 0.1.0-alpha hold backslashes.
    pub fn resolve(&self, stored: &str) -> PathBuf {
        let p = Path::new(stored);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(stored_form(stored))
        }
    }
}

/// A stored relative path: forward slashes only.
pub fn stored_form(relative: &str) -> String {
    relative.replace('\\', "/")
}

/// Where Ruthos keeps its own files on this platform.
pub fn app_support_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(local).join("Ruthos");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(base) = directories::BaseDirs::new() {
            return base
                .home_dir()
                .join("Library")
                .join("Application Support")
                .join("Ruthos");
        }
    }
    if let Some(dirs) = directories::ProjectDirs::from("org", "ruthos", "Ruthos") {
        return dirs.data_local_dir().to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("ruthos-data")
}

/// Where settings live: `%APPDATA%\Ruthos` on Windows, the application
/// support folder on macOS, the XDG config directory on Linux.
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(roaming) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(roaming).join("Ruthos");
        }
    }
    #[cfg(target_os = "macos")]
    {
        return app_support_dir();
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(dirs) = directories::ProjectDirs::from("org", "ruthos", "Ruthos") {
            return dirs.config_dir().to_path_buf();
        }
        app_support_dir()
    }
}

/// The file-syncing service a folder lives in, if any. A SQLite database
/// in write-ahead-log mode inside a synced folder can be corrupted when
/// the database and its log are uploaded at different moments.
pub fn sync_service(path: &Path) -> Option<&'static str> {
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        let lower = name.to_ascii_lowercase();
        if lower == "mobile documents" || lower.contains("com~apple~clouddocs") || lower == "icloud drive" {
            return Some("iCloud Drive");
        }
        if lower == "dropbox" {
            return Some("Dropbox");
        }
        if lower.starts_with("onedrive") {
            return Some("OneDrive");
        }
        if lower == "google drive" || lower == "googledrive" || lower == "my drive" {
            return Some("Google Drive");
        }
    }
    None
}

/// A 12-hex-character FNV-1a hash of a UID, used for folder names.
pub fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// UIDs are dots and digits, but be defensive about anything else.
pub fn safe_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_short() {
        assert_eq!(short_hash("1.2.3").len(), 12);
        assert_eq!(short_hash("1.2.3"), short_hash("1.2.3"));
        assert_ne!(short_hash("1.2.3"), short_hash("1.2.4"));
    }

    #[test]
    fn stored_paths_use_forward_slashes_and_resolve_either_way() {
        let p = DataPaths::new("C:/x");
        let rel = p
            .relative_str(Path::new("C:/x/DATABASE.noindex/ab/cd/1.2.dcm"))
            .unwrap();
        assert_eq!(rel, "DATABASE.noindex/ab/cd/1.2.dcm");
        assert_eq!(
            stored_form("DATABASE.noindex\\ab\\cd\\1.2.dcm"),
            "DATABASE.noindex/ab/cd/1.2.dcm"
        );
        let a = p.resolve("DATABASE.noindex\\ab\\cd\\1.2.dcm");
        let b = p.resolve("DATABASE.noindex/ab/cd/1.2.dcm");
        assert_eq!(a, b);
        assert_eq!(a.components().count(), Path::new("C:/x").components().count() + 4);
    }

    #[test]
    fn sync_services_are_recognised() {
        assert_eq!(
            sync_service(Path::new(
                "/Users/m/Library/Mobile Documents/com~apple~CloudDocs/Ruthos"
            )),
            Some("iCloud Drive")
        );
        assert_eq!(
            sync_service(Path::new("C:/Users/m/OneDrive - Clinic/Ruthos Data")),
            Some("OneDrive")
        );
        assert_eq!(sync_service(Path::new("/home/m/Dropbox/x")), Some("Dropbox"));
        assert_eq!(
            sync_service(Path::new("/Users/m/Library/Application Support/Ruthos")),
            None
        );
    }

    #[test]
    fn stored_path_shape() {
        let p = DataPaths::new("C:/x");
        let path = p.stored_path_for("1.2", "3.4", "5.6/7");
        assert!(path.ends_with("5.6_7.dcm"));
        assert!(path.starts_with("C:/x/DATABASE.noindex"));
    }
}
