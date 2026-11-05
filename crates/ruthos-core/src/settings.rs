//! Application settings, stored as JSON in the user's config folder.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::measure::{self, Library};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default = "Settings::file_defaults")]
pub struct Settings {
    /// Smart folders hidden from the database sidebar.
    pub hidden_smart_albums: Vec<String>,
    /// How far from a model's edge refinement may move it, in voxels.
    pub refine_band: u8,
    /// Developer option: write each AI segment's unrefined mask into its SEG too,
    /// so a case can be scored both ways without rerunning the model.
    pub save_unrefined_masks: bool,
    /// Load the newest saved segments on opening MPR, without asking.
    pub load_newest_segments: bool,
    /// Written as Content Creator's Name in what Ruthos saves to a study.
    pub user_name: String,
    /// Root of the data folder. `None` means the platform default.
    pub data_root: Option<PathBuf>,
    /// Copy imported files into the database folder (true) or link to them
    /// where they are (false). Horos calls these "Copy" and "Link".
    pub copy_files_on_import: bool,
    /// DICOM listener.
    pub listener_enabled: bool,
    pub listener_port: u16,
    pub listener_ae_title: String,
    /// Devices allowed to send to the listener (Preferences > Listener).
    /// The PACS in Preferences > PACS are allowed too, so retrieval works.
    pub listener_allow: Vec<ListenerPeer>,
    /// Accept SOP classes Ruthos does not recognise; off by default.
    pub listener_promiscuous: bool,
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
    /// PACS that Ruthos can search, retrieve from and send to.
    pub pacs_nodes: Vec<PacsNode>,
    /// Send arriving images on to a PACS automatically.
    pub routing_rules: Vec<crate::routing::RouteRule>,
    /// Folders whose new DICOM files are imported (Preferences > Watch folders).
    pub watch_folders: Vec<crate::watch::WatchFolder>,
    /// The rotary dial (Surface Dial, ASUS Dial and DialPad); Windows only.
    pub dial: DialSettings,
    /// The decode guard's limits (Preferences > Advanced); `None` keeps the
    /// defaults. Held to four times the defaults whatever the file says.
    pub decode_limits: Option<DecodeLimits>,
    /// The guided review server. Empty until an invitation, a profile, a
    /// site's `ruthos.ini` or the user sets one.
    pub session_server: Option<String>,
    /// The signing key first seen for each session server.
    pub session_pins: std::collections::BTreeMap<String, SessionPin>,
    /// Profiles applied, oldest first, with what Revert restores.
    pub applied_profiles: Vec<AppliedProfile>,
}

/// A session server's signing key.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPin {
    pub kid: String,
    pub key: String,
}

/// A profile the user applied.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppliedProfile {
    pub issuer: String,
    pub by: String,
    pub verified: bool,
    pub applied: String,
    pub previous: serde_json::Value,
}

/// A device allowed to send to the listener.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenerPeer {
    /// "Ultrasound, room 2".
    pub name: String,
    pub ae_title: String,
    /// An address or subnet it must connect from; empty for any.
    pub address: String,
}

/// The decode guard's limits as the user sets them, in friendly units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecodeLimits {
    pub side: u32,
    pub megapixels: u32,
    pub frames: u32,
    pub fragment_mb: u32,
    pub series_mb: u32,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        let d = ruthos_dicom::guard::Limits::default();
        Self::from_limits(d)
    }
}

impl DecodeLimits {
    pub fn from_limits(l: ruthos_dicom::guard::Limits) -> Self {
        Self {
            side: l.side,
            megapixels: (l.pixels / (1024 * 1024)) as u32,
            frames: l.frames,
            fragment_mb: (l.fragment_bytes / (1024 * 1024)) as u32,
            series_mb: (l.series_bytes / (1024 * 1024)) as u32,
        }
    }

    pub fn to_limits(self) -> ruthos_dicom::guard::Limits {
        ruthos_dicom::guard::Limits {
            side: self.side,
            pixels: self.megapixels as u64 * 1024 * 1024,
            frames: self.frames,
            fragment_bytes: self.fragment_mb as u64 * 1024 * 1024,
            series_bytes: self.series_mb as u64 * 1024 * 1024,
        }
        .clamped()
    }
}

/// What a turn of the dial adjusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DialMode {
    /// A frame in a multi-frame image, otherwise an image in the series.
    Navigate,
    WindowWidth,
    WindowLevel,
    /// The selected measurement's active handle.
    MeasurementPoint,
    LoupeMagnification,
}

impl DialMode {
    /// Every mode, in the order a press cycles through them.
    pub const ALL: [DialMode; 5] = [
        DialMode::Navigate,
        DialMode::WindowWidth,
        DialMode::WindowLevel,
        DialMode::MeasurementPoint,
        DialMode::LoupeMagnification,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DialMode::Navigate => "Navigate",
            DialMode::WindowWidth => "Window width",
            DialMode::WindowLevel => "Window level",
            DialMode::MeasurementPoint => "Measurement point",
            DialMode::LoupeMagnification => "Loupe magnification",
        }
    }
}

/// Preferences > Viewer > Dial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DialSettings {
    pub enabled: bool,
    /// The modes a press cycles through, in `DialMode::ALL` order.
    pub modes: Vec<DialMode>,
    pub start_mode: DialMode,
    /// 5, 10, 15 or 20.
    pub degrees_per_step: u32,
    /// Fast turns in Navigate move five frames or images a step.
    pub accelerate: bool,
    pub reverse: bool,
    pub show_indicator: bool,
}

impl Default for DialSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            modes: DialMode::ALL.to_vec(),
            start_mode: DialMode::Navigate,
            degrees_per_step: 10,
            accelerate: true,
            reverse: false,
            show_indicator: true,
        }
    }
}

/// A PACS (or another viewer's archive) as Ruthos knows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PacsNode {
    /// Anything the user likes: "Clinic PACS".
    pub name: String,
    /// Computer name or IP address.
    pub host: String,
    pub port: u16,
    /// The PACS's own AE title, matched exactly.
    pub ae_title: String,
    /// `auto` (C-GET, then C-MOVE), `get` or `move`.
    pub retrieve: String,
}

impl Default for PacsNode {
    fn default() -> Self {
        Self {
            name: "New PACS".into(),
            host: String::new(),
            port: 104,
            ae_title: String::new(),
            retrieve: "auto".into(),
        }
    }
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
            &[
                "VHS long axis",
                "VHS short axis",
                "T4 vertebral length",
                "Kidney length (L)",
                "Kidney length (R)",
            ],
        ),
        set(
            "Orthopaedic",
            &[
                "Norberg angle",
                "Tibial plateau angle",
                "Patellar ligament length",
                "Patella length",
            ],
        ),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hidden_smart_albums: Vec::new(),
            refine_band: 3,
            save_unrefined_masks: false,
            load_newest_segments: false,
            user_name: String::new(),
            data_root: None,
            copy_files_on_import: true,
            // Off until the user turns it on: Ruthos makes no network
            // connection the user has not configured.
            listener_enabled: false,
            listener_port: 11112,
            listener_ae_title: "RUTHOS".to_string(),
            incoming_scan_seconds: 2,
            listener_allow: Vec::new(),
            listener_promiscuous: false,
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
            pacs_nodes: Vec::new(),
            routing_rules: Vec::new(),
            watch_folders: Vec::new(),
            dial: DialSettings::default(),
            decode_limits: None,
            session_server: None,
            session_pins: Default::default(),
            applied_profiles: Vec::new(),
        }
    }
}

impl Settings {
    /// Defaults for fields missing from a settings file: as `Default`, but
    /// a file without the migration flag is an old file that needs it.
    fn file_defaults() -> Self {
        Self {
            measurements_migrated: false,
            ..Self::default()
        }
    }

    /// `%APPDATA%\Ruthos\settings.json` on Windows, `~/Library/Application
    /// Support/Ruthos/settings.json` on macOS, `~/.config/ruthos` on Linux.
    pub fn file_path() -> PathBuf {
        // Developer option: `RUTHOS_SETTINGS=<file>` uses another settings
        // file, so tests and screenshots never touch the real one.
        if let Some(p) = std::env::var_os("RUTHOS_SETTINGS").filter(|v| !v.is_empty()) {
            return PathBuf::from(p);
        }
        crate::paths::config_dir().join("settings.json")
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
        let old: Vec<(String, Vec<String>)> = self
            .measurement_sets
            .iter()
            .map(|s| (s.name.clone(), s.items.clone()))
            .collect();
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
        let old =
            r#"{"listener_port": 4242, "measurement_sets": [{"name": "Mine", "items": ["LVIDd", "Spleen width"]}]}"#;
        let mut s: Settings = serde_json::from_str(old).unwrap();
        assert!(!s.measurements_migrated, "a file without the flag is pre-library");
        assert!(s.migrate_measurement_sets());
        assert!(s.measurement_sets.is_empty());
        let lib = s.library();
        assert_eq!(lib.set("Mine").unwrap().keys, vec!["lv.lvidd", "custom.spleen_width"]);
        assert_eq!(s.listener_port, 4242);
        assert!(!s.migrate_measurement_sets(), "only once");
        // A fresh file carries the flag and nothing to migrate.
        let fresh: Settings = serde_json::from_str(&serde_json::to_string(&Settings::default()).unwrap()).unwrap();
        assert!(fresh.measurements_migrated);
    }
}
