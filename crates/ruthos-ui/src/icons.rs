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

/// Register the icon font with egui so the glyph constants render.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("phosphor".to_string(), std::sync::Arc::new(egui::FontData::from_static(FONT_BYTES)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("phosphor".to_string());
    }
    ctx.set_fonts(fonts);
}
