//! Icon glyphs from the Phosphor icon font (MIT), embedded from
//! `assets/fonts/Phosphor.ttf` and registered under egui's proportional font
//! family so they can appear inline in any label.
//!
//! Horos uses hand-drawn Aqua icons; these are placeholders in the same
//! spirit until the Horos icon set is converted.

pub const FONT_BYTES: &[u8] = include_bytes!("../../../assets/fonts/Phosphor.ttf");

// Database window
pub const SIDEBAR: &str = "\u{EC24}";
pub const IMPORT: &str = "\u{E256}"; // folder open
pub const IMPORT_FILES: &str = "\u{E236}"; // file plus
pub const PICTURES: &str = "\u{E836}"; // images
pub const EXPORT: &str = "\u{EAF0}";
pub const QUERY: &str = "\u{E30C}"; // magnifying glass
pub const SEND: &str = "\u{E398}"; // paper plane
pub const ANONYMIZE: &str = "\u{E9F4}"; // mask
pub const TRASH: &str = "\u{E4A6}";
pub const VIEWER: &str = "\u{E2CA}"; // image
pub const REPORT: &str = "\u{E23A}"; // file text
pub const DATABASE: &str = "\u{E1DE}";
pub const ALBUM: &str = "\u{E25A}"; // folder simple
pub const SMART_ALBUM: &str = "\u{E270}"; // gear
pub const LISTENER_ON: &str = "\u{E0F2}"; // broadcast
pub const LISTENER_OFF: &str = "\u{E602}"; // circle dashed
pub const PLUS: &str = "\u{E3D4}";
pub const CARET_DOWN: &str = "\u{E136}";
pub const CARET_RIGHT: &str = "\u{E13A}";

// Viewer window
pub const ANNOTATIONS: &str = "\u{E6EE}"; // text Aa
pub const SERIES: &str = "\u{E466}"; // stack
pub const WLWW: &str = "\u{E18C}"; // circle half
pub const CLUT: &str = "\u{E6C8}"; // palette
pub const THREE_D: &str = "\u{E1DA}"; // cube
pub const ORIENTATION: &str = "\u{E094}"; // arrows clockwise
pub const RESET: &str = "\u{E038}"; // arrow counter clockwise
pub const KEY_IMAGE: &str = "\u{E46A}"; // star
pub const PLAY: &str = "\u{E3D0}";
pub const PAUSE: &str = "\u{E39E}";

// Tools
pub const TOOL_WL: &str = "\u{E18C}"; // circle half
pub const TOOL_MOVE: &str = "\u{E0A4}"; // arrows out cardinal
pub const TOOL_ZOOM: &str = "\u{E310}"; // magnifying glass plus
pub const TOOL_ROTATE: &str = "\u{E036}"; // arrow clockwise
pub const TOOL_LENGTH: &str = "\u{E6B8}"; // ruler
pub const TOOL_ANGLE: &str = "\u{E7BC}"; // angle
pub const TOOL_RECT: &str = "\u{E3F0}"; // rectangle
pub const TOOL_OVAL: &str = "\u{E18A}"; // circle
pub const TOOL_ARROW: &str = "\u{E092}"; // arrow up right
pub const TOOL_POINT: &str = "\u{E1D6}"; // crosshair

/// Inter (SIL Open Font Licence), the UI typeface.
pub const INTER_BYTES: &[u8] = include_bytes!("../../../assets/fonts/Inter.ttf");

/// The font family icons are drawn with. It contains only Phosphor, so icon
/// code points never collide with a text face, and text never reaches
/// Phosphor's blank ligature glyphs for ordinary letters.
pub fn family() -> egui::FontFamily {
    egui::FontFamily::Name("icons".into())
}

pub fn font(size: f32) -> egui::FontId {
    egui::FontId::new(size, family())
}

/// A glyph as rich text in the icon family, for use inside labels and menus.
pub fn rich(glyph: &str, size: f32) -> egui::RichText {
    egui::RichText::new(glyph).family(family()).size(size)
}

/// Register the text face and the icon family.
///
/// Text uses the Windows system face, Segoe UI, when it is installed, so the
/// app reads like any other Windows program; Inter (bundled) is the fallback.
/// egui's own fonts stay behind them for anything either lacks.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut text_faces: Vec<String> = Vec::new();

    if let Some(segoe) = load_system_font(&["segoeui.ttf"]) {
        fonts.font_data.insert("segoe".to_string(), std::sync::Arc::new(egui::FontData::from_owned(segoe)));
        text_faces.push("segoe".to_string());
    }
    fonts
        .font_data
        .insert("inter".to_string(), std::sync::Arc::new(egui::FontData::from_static(INTER_BYTES)));
    text_faces.push("inter".to_string());
    fonts
        .font_data
        .insert("phosphor".to_string(), std::sync::Arc::new(egui::FontData::from_static(FONT_BYTES)));

    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, face) in text_faces.iter().enumerate() {
        proportional.insert(i, face.clone());
    }
    fonts.families.insert(family(), vec!["phosphor".to_string()]);
    ctx.set_fonts(fonts);
}

fn load_system_font(candidates: &[&str]) -> Option<Vec<u8>> {
    let windir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    for name in candidates {
        let path = windir.join("Fonts").join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(bytes);
        }
    }
    None
}
