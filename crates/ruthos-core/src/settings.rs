//! Application settings, stored as JSON in the user's config folder.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::measure::{self, Library};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default = "Settings::file_defaults")]
pub struct Settings {
    /// Root of the data folder. `None` means the platform default.
    pub data_root: Option<PathBuf>,
    /// Copy imported files into the database folder (true) or link to them
    /// where they are (false). Horos calls these "Copy" and "Link".
    pub copy_files_on_import: bool,
    /// DICOM listener.
    pub listener_enabled: bool,
    pub listener_port: u16,
    pub listener_ae_title: String,
    /// Poll interval for the incoming folder, in seconds.
    pub incoming_scan_seconds: u64,
    /// Viewer defaults.
    pub annotations_level: u8,
    pub default_clut: String,
    pub thumbnail_size: u32,
    /// Window geometry remembered between runs.
    pub browser_window_size: Option<[f32; 2]>,
    /// Legacy (before the measurement library): named measurements grouped
    /// by study type. Migrated into `measurements` on load and then empty.
    pub measurement_sets: Vec<MeasurementSet>,
    /// The user's measurement definitions, sets and calculations, laid over
    /// the built-in library (`Settings::library`).
    pub measurements: Library,
    /// Set once the legacy sets have been migrated.
    pub measurements_migrated: bool,
    /// Prefix of suggested teaching references, e.g. `EQ-CARD-2026`.
    pub teaching_reference_prefix: String,
    /// Loupe while measuring.
    pub loupe_enabled: bool,
    /// 2, 3, 4, 6 or 8 times.
    pub loupe_magnification: u32,
    /// 120, 160 or 200 screen pixels.
    pub loupe_size: u32,
    /// Shift scales pointer movement by the reciprocal of the magnification.
    pub loupe_fine_movement: bool,
    /// Length unit for echocardiography values: "mm" or "cm".
    pub echo_length_unit: String,
}

/// A named group of measurements, e.g. the M-mode left-ventricle set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementSet {
    pub name: String,
    pub items: Vec<String>,
}

/// The sets shipped by default; the user can edit them in settings.json.
pub fn default_measurement_sets() -> Vec<MeasurementSet> {
    let set = |name: &str, items: &[&str]| MeasurementSet {
        name: name.to_string(),
        items: items.iter().map(|s| s.to_string()).collect(),
    };
    vec![
        set("Echo: LV M-mode", &["IVSd", "LVIDd", "LVFWd", "IVSs", "LVIDs", "LVFWs"]),
        set("Echo: 2D", &["Ao", "LA", "RVIDd", "EPSS", "PA"]),
        set(
            "Radiography",
            &["VHS long axis", "VHS short axis", "T4 vertebral length", "Kidney length (L)", "Kidney length (R)"],
        ),
        set("Orthopaedic", &["Norberg angle", "Tibial plateau angle", "Patellar ligament length", "Patella length"]),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data_root: None,
            copy_files_on_import: true,
            listener_enabled: true,
            listener_port: 11112,
            listener_ae_title: "RUTHOS".to_string(),
            incoming_scan_seconds: 2,
            annotations_level: 2,
            default_clut: "No CLUT".to_string(),
            thumbnail_size: 100,
            browser_window_size: None,
            measurement_sets: Vec::new(),
            measurements: Library::default(),
            measurements_migrated: true,
            teaching_reference_prefix: "RUTHOS-2026".to_string(),
            loupe_enabled: true,
            loupe_magnification: 4,
            loupe_size: 160,
            loupe_fine_movement: true,
            echo_length_unit: "mm".to_string(),
        }
    }
}

impl Settings {
    /// Defaults for fields missing from a settings file: as `Default`, but
    /// a file without the migration flag is an old file that needs it.
    fn file_defaults() -> Self {
        Self { measurements_migrated: false, ..Self::default() }
    }

    /// `%APPDATA%\Ruthos\settings.json` on Windows.
    pub fn file_path() -> PathBuf {
        if let Some(roaming) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            PathBuf::from(roaming).join("Ruthos").join("settings.json")
        } else if let Some(dirs) = directories::ProjectDirs::from("org", "Ruthos", "Ruthos") {
            dirs.config_dir().join("settings.json")
        } else {
            PathBuf::from("ruthos-settings.json")
        }
    }

    pub fn load() -> Self {
        let path = Self::file_path();
        let mut settings: Self = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Self>(&text).unwrap_or_else(|e| {
                tracing::warn!("settings at {} unreadable ({e}), using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        if settings.migrate_measurement_sets() {
            // Keep the file as it was before the library existed.
            let backup = path.with_extension("json.bak");
            if path.exists() && !backup.exists() {
                if let Err(e) = std::fs::copy(&path, &backup) {
                    tracing::warn!("could not back up settings before migration: {e}");
                }
            }
            if let Err(e) = settings.save() {
                tracing::warn!("could not save migrated settings: {e}");
            }
        }
        // The posterior wall is the free wall in veterinary echo; older
        // settings carry the human names.
        for set in &mut settings.measurement_sets {
            for item in &mut set.items {
                match item.as_str() {
                    "LVPWd" => *item = "LVFWd".into(),
                    "LVPWs" => *item = "LVFWs".into(),
                    _ => {}
                }
            }
        }
        settings
    }

    /// The effective measurement library: built-in entries with the user's
    /// laid over them.
    pub fn library(&self) -> Library {
        measure::effective(&self.measurements)
    }

    /// Turn legacy label sets into library entries. Returns true when
    /// something was migrated and the file should be rewritten. A file
    /// from before the flag existed carries `measurements_migrated: false`
    /// only through serde's default, so the flag is set explicitly here.
    fn migrate_measurement_sets(&mut self) -> bool {
        if self.measurements_migrated {
            return false;
        }
        self.measurements_migrated = true;
        if self.measurement_sets.is_empty() {
            return true;
        }
        let old: Vec<(String, Vec<String>)> = self.measurement_sets.iter().map(|s| (s.name.clone(), s.items.clone())).collect();
        let migrated = measure::migrate_label_sets(&old);
        self.measurements.merge(migrated);
        self.measurement_sets.clear();
        true
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// The data root: `RUTHOS_DATA_ROOT` if set (handy for testing against a
    /// scratch archive), else the configured folder, else the platform default.
    pub fn data_root(&self) -> PathBuf {
        if let Some(env) = std::env::var_os("RUTHOS_DATA_ROOT").filter(|s| !s.is_empty()) {
            return PathBuf::from(env);
        }
        self.data_root
            .clone()
            .unwrap_or_else(crate::paths::DataPaths::default_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_files_migrate_their_label_sets() {
        let old = r#"{"listener_port": 4242, "measurement_sets": [{"name": "Mine", "items": ["LVIDd", "Spleen thickness"]}]}"#;
        let mut s: Settings = serde_json::from_str(old).unwrap();
        assert!(!s.measurements_migrated, "a file without the flag is pre-library");
        assert!(s.migrate_measurement_sets());
        assert!(s.measurement_sets.is_empty());
        let lib = s.library();
        assert_eq!(lib.set("Mine").unwrap().keys, vec!["lv.lvidd", "custom.spleen_thickness"]);
        assert_eq!(s.listener_port, 4242);
        assert!(!s.migrate_measurement_sets(), "only once");
        // A fresh file carries the flag and nothing to migrate.
        let fresh: Settings = serde_json::from_str(&serde_json::to_string(&Settings::default()).unwrap()).unwrap();
        assert!(fresh.measurements_migrated);
    }
}
