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
    /// Default location: `%LOCALAPPDATA%\Ruthos\Ruthos Data`.
    pub fn default_root() -> PathBuf {
        if let Some(dirs) = directories::ProjectDirs::from("org", "Ruthos", "Ruthos") {
            dirs.data_local_dir().join("Ruthos Data")
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("ruthos-data")
        }
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

    /// Resolve a path stored in the database back to an absolute path.
    pub fn resolve(&self, stored: &str) -> PathBuf {
        let p = Path::new(stored);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }
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
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
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
    fn stored_path_shape() {
        let p = DataPaths::new("C:/x");
        let path = p.stored_path_for("1.2", "3.4", "5.6/7");
        assert!(path.ends_with("5.6_7.dcm"));
        assert!(path.starts_with("C:/x/DATABASE.noindex"));
    }
}
