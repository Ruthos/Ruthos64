//! What differs between Windows, macOS and Linux, in one place.
//!
//! Shortcut text (`Ctrl+S` or `⌘S`), opening a file or URL with the
//! system's program, files handed to the running application (Finder's
//! "Open with" and Dock drops on macOS), and the menu bar as data: one
//! definition that macOS renders as native menus and that names every
//! action the other platforms reach through the toolbars.

use std::path::PathBuf;

use parking_lot::Mutex;

#[cfg(target_os = "macos")]
pub mod macos;

// -------------------------------------------------------------- shortcuts

/// A shortcut written the Windows way (`Ctrl+S`, `Ctrl+Shift+Z`) rendered
/// for this platform: unchanged on Windows and Linux, `⌘S` and `⇧⌘Z` on
/// macOS.
pub fn shortcut(canonical: &str) -> String {
    shortcut_for(canonical, cfg!(target_os = "macos"))
}

pub fn shortcut_for(canonical: &str, mac: bool) -> String {
    if !mac {
        return canonical.to_string();
    }
    let mut parts: Vec<&str> = canonical.split('+').collect();
    // "Ctrl++" style keys: the last part may itself be a plus.
    if canonical.ends_with('+') {
        parts.retain(|p| !p.is_empty());
        parts.push("+");
    }
    let (key, modifiers) = match parts.split_last() {
        Some((k, m)) => (*k, m),
        None => return canonical.to_string(),
    };
    let mut out = String::new();
    // Apple's order: Control, Option, Shift, Command; Ctrl stands for Command.
    for want in ["Alt", "Shift", "Ctrl"] {
        if modifiers.iter().any(|m| m.eq_ignore_ascii_case(want)) {
            out.push(match want {
                "Alt" => '\u{2325}',   // ⌥
                "Shift" => '\u{21e7}', // ⇧
                _ => '\u{2318}',       // ⌘ (Ctrl stands for the command key)
            });
        }
    }
    out.push_str(&key.to_uppercase());
    out
}

// -------------------------------------------------------------- opening

/// Open a file or folder with the program the system uses for it: the
/// shell's `start` on Windows, `open` on macOS, `xdg-open` on Linux.
pub fn open_path(path: &std::path::Path) {
    if let Err(e) = open::that_detached(path) {
        tracing::warn!("could not open {}: {e}", path.display());
    }
}

pub fn open_url(url: &str) {
    if let Err(e) = open::that_detached(url) {
        tracing::warn!("could not open {url}: {e}");
    }
}

// ---------------------------------------------------- files to open later

static OPEN_QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Files or folders the system asked the running application to open.
/// The application drains them every frame and imports them.
pub fn queue_open_files(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    tracing::info!("{} path(s) handed to the running application", paths.len());
    OPEN_QUEUE.lock().extend(paths);
}

pub fn take_open_files() -> Vec<PathBuf> {
    std::mem::take(&mut *OPEN_QUEUE.lock())
}

// -------------------------------------------------------------- menu bar

/// What a menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    Preferences,
    ImportFolder,
    ImportFiles,
    DatabaseWindow,
    /// Open a web page.
    Url(&'static str),
    /// An item of the viewer's Measurements menu, by its id
    /// (`ViewerState::menu_from_id`), sent to the focused viewer.
    Viewer(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItemDef {
    /// Stable id, also the native menu item's id: `file:import-folder`.
    pub id: &'static str,
    pub label: &'static str,
    /// Written the Windows way; `shortcut` renders it per platform.
    pub shortcut: Option<&'static str>,
    pub action: AppAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Item(MenuItemDef),
    Separator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuDef {
    pub title: &'static str,
    pub entries: &'static [Entry],
}

const fn item(id: &'static str, label: &'static str, shortcut: Option<&'static str>, action: AppAction) -> Entry {
    Entry::Item(MenuItemDef {
        id,
        label,
        shortcut,
        action,
    })
}

pub const DOCS_URL: &str = "https://github.com/Ruthos/Ruthos64#readme";
pub const MEASUREMENTS_URL: &str = "https://github.com/Ruthos/Ruthos64/blob/main/docs/measurements.md";
pub const ISSUES_URL: &str = "https://github.com/Ruthos/Ruthos64/issues/new/choose";

/// The menus after the application menu: File, Measurements, Window, Help.
/// The application menu itself (About, Preferences, Hide, Quit) is a
/// macOS convention and is built natively; Preferences is reachable here
/// too so the other platforms share the definition.
pub const MENU_BAR: &[MenuDef] = &[
    MenuDef {
        title: "File",
        entries: &[
            item(
                "file:import-folder",
                "Import Folder…",
                Some("Ctrl+O"),
                AppAction::ImportFolder,
            ),
            item(
                "file:import-files",
                "Import Files…",
                Some("Ctrl+Shift+O"),
                AppAction::ImportFiles,
            ),
            Entry::Separator,
            item(
                "app:preferences",
                "Preferences…",
                Some("Ctrl+,"),
                AppAction::Preferences,
            ),
        ],
    },
    MenuDef {
        title: "Measurements",
        entries: &[
            item(
                "viewer:tool:wl",
                "Window/Level",
                Some("W"),
                AppAction::Viewer("tool:wl"),
            ),
            item("viewer:tool:move", "Move", Some("M"), AppAction::Viewer("tool:move")),
            item("viewer:tool:zoom", "Zoom", Some("Z"), AppAction::Viewer("tool:zoom")),
            item(
                "viewer:tool:rotate",
                "Rotate",
                Some("T"),
                AppAction::Viewer("tool:rotate"),
            ),
            item(
                "viewer:tool:length",
                "Caliper",
                Some("L"),
                AppAction::Viewer("tool:length"),
            ),
            item("viewer:tool:angle", "Angle", Some("A"), AppAction::Viewer("tool:angle")),
            item(
                "viewer:tool:rect",
                "Rectangle Area",
                Some("B"),
                AppAction::Viewer("tool:rect"),
            ),
            item(
                "viewer:tool:oval",
                "Ellipse Area",
                Some("E"),
                AppAction::Viewer("tool:oval"),
            ),
            item(
                "viewer:tool:polygon",
                "Polygon Area",
                Some("G"),
                AppAction::Viewer("tool:polygon"),
            ),
            item("viewer:tool:arrow", "Arrow", Some("N"), AppAction::Viewer("tool:arrow")),
            item("viewer:tool:point", "Point", Some("P"), AppAction::Viewer("tool:point")),
            item(
                "viewer:tool:lvmmode",
                "LV Dimensions",
                Some("K"),
                AppAction::Viewer("tool:lvmmode"),
            ),
            Entry::Separator,
            item("viewer:undo", "Undo Edit", Some("Ctrl+Z"), AppAction::Viewer("undo")),
            item("viewer:delete", "Delete Selected", None, AppAction::Viewer("delete")),
            item(
                "viewer:delete-all",
                "Delete All on This Image",
                None,
                AppAction::Viewer("delete-all"),
            ),
            Entry::Separator,
            item(
                "viewer:calibrate",
                "Calibrate from Selected Length…",
                None,
                AppAction::Viewer("calibrate"),
            ),
            item(
                "viewer:remove-calibration",
                "Remove Hand Calibration",
                None,
                AppAction::Viewer("remove-calibration"),
            ),
            item(
                "viewer:basis",
                "Show Calibration Basis",
                None,
                AppAction::Viewer("basis"),
            ),
            Entry::Separator,
            item(
                "viewer:show-rois",
                "Show Measurements",
                None,
                AppAction::Viewer("show-rois"),
            ),
            item(
                "viewer:show-labels",
                "Show Labels",
                None,
                AppAction::Viewer("show-labels"),
            ),
            Entry::Separator,
            item("viewer:copy", "Copy Values", Some("Ctrl+C"), AppAction::Viewer("copy")),
            item(
                "viewer:add-to-report",
                "Add to Report",
                None,
                AppAction::Viewer("add-to-report"),
            ),
            item("viewer:report", "Open Report", None, AppAction::Viewer("report")),
        ],
    },
    MenuDef {
        title: "Window",
        entries: &[item(
            "window:database",
            "Database",
            Some("Ctrl+D"),
            AppAction::DatabaseWindow,
        )],
    },
    MenuDef {
        title: "Help",
        entries: &[
            item("help:guide", "Ruthos Guide", None, AppAction::Url(DOCS_URL)),
            item(
                "help:measurements",
                "Measurements Guide",
                None,
                AppAction::Url(MEASUREMENTS_URL),
            ),
            Entry::Separator,
            item("help:issue", "Report a Problem…", None, AppAction::Url(ISSUES_URL)),
        ],
    },
];

/// The action behind a menu item id.
pub fn action_for(id: &str) -> Option<AppAction> {
    MENU_BAR.iter().flat_map(|m| m.entries.iter()).find_map(|e| match e {
        Entry::Item(it) if it.id == id => Some(it.action),
        _ => None,
    })
}

/// Install the platform's native pieces: on macOS the Finder open hook and
/// the menu bar; nothing elsewhere.
pub fn install_native() {
    #[cfg(target_os = "macos")]
    macos::install();
}

/// Menu item ids chosen in the native menu bar since the last frame.
pub fn poll_native_menu() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        macos::poll_menu_events()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Whether `RUTHOS_RENDERER=software` can be honoured: Windows has WARP
/// and Linux has llvmpipe; every supported Mac has a Metal GPU.
pub fn software_renderer_available() -> bool {
    !cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_render_per_platform() {
        assert_eq!(shortcut_for("Ctrl+S", false), "Ctrl+S");
        assert_eq!(shortcut_for("Ctrl+S", true), "\u{2318}S");
        assert_eq!(shortcut_for("Ctrl+Shift+O", true), "\u{21e7}\u{2318}O");
        assert_eq!(shortcut_for("Ctrl+,", true), "\u{2318},");
        assert_eq!(shortcut_for("Alt+Z", true), "\u{2325}Z");
        assert_eq!(shortcut_for("W", true), "W");
    }

    #[test]
    fn menu_ids_are_unique_and_resolve() {
        let mut ids: Vec<&str> = MENU_BAR
            .iter()
            .flat_map(|m| m.entries.iter())
            .filter_map(|e| match e {
                Entry::Item(it) => Some(it.id),
                _ => None,
            })
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "menu item ids must be unique");
        assert_eq!(action_for("app:preferences"), Some(AppAction::Preferences));
        assert_eq!(action_for("viewer:undo"), Some(AppAction::Viewer("undo")));
        assert_eq!(action_for("nope"), None);
    }

    #[test]
    fn open_queue_round_trips() {
        queue_open_files(vec![PathBuf::from("a.dcm"), PathBuf::from("b")]);
        let got = take_open_files();
        assert_eq!(got.len(), 2);
        assert!(take_open_files().is_empty());
    }
}
