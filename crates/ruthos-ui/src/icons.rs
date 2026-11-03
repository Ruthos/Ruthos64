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
pub const TILING: &str = "\u{E464}"; // squares four
pub const WLWW: &str = "\u{E18C}"; // circle half
pub const CLUT: &str = "\u{E6C8}"; // palette
pub const THREE_D: &str = "\u{E1DA}"; // cube
pub const ORIENTATION: &str = "\u{E094}"; // arrows clockwise
pub const RESET: &str = "\u{E038}"; // arrow counter clockwise
pub const KEY_IMAGE: &str = "\u{E46A}"; // star
pub const PLAY: &str = "\u{E3D0}";
pub const PAUSE: &str = "\u{E39E}";
pub const LOCKED: &str = "\u{E308}"; // lock simple
pub const UNLOCKED: &str = "\u{E30A}"; // lock simple open

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
        fonts.font_data.insert(
            "segoe".to_string(),
            std::sync::Arc::new(egui::FontData::from_owned(segoe)),
        );
        text_faces.push("segoe".to_string());
    }
    fonts.font_data.insert(
        "inter".to_string(),
        std::sync::Arc::new(egui::FontData::from_static(INTER_BYTES)),
    );
    text_faces.push("inter".to_string());
    fonts.font_data.insert(
        "phosphor".to_string(),
        std::sync::Arc::new(egui::FontData::from_static(FONT_BYTES)),
    );

    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, face) in text_faces.iter().enumerate() {
        proportional.insert(i, face.clone());
    }
    fonts.families.insert(family(), vec!["phosphor".to_string()]);
    ctx.set_fonts(fonts);
}

fn load_system_font(candidates: &[&str]) -> Option<Vec<u8>> {
    let windir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into());
    for name in candidates {
        let path = windir.join("Fonts").join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(bytes);
        }
    }
    None
}

/// Horos toolbar artwork (LGPL-3.0, see `assets/horos/icons/README.md`),
/// rendered to 128 px PNG with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bitmap {
    /// Ruthos' own artwork: import image files.
    ImportImages,
    /// Ruthos' own artwork: add pictures.
    AddImages,
    /// Ruthos' own artwork: the report.
    ReportCustom,
    Add,
    Album,
    Angle,
    Anonymize,
    Arrow,
    Cobb,
    Database,
    DatabaseWindow,
    DynamicAngle,
    Export,
    FlipH,
    FlipV,
    Import,
    Key,
    Keys,
    Length,
    Minus,
    Move,
    Movie,
    Mpr,
    Network,
    Next,
    Oval,
    Pause,
    Pencil,
    Play,
    Point,
    Presets,
    Prev,
    Print,
    Propagate,
    Query,
    Rectangle,
    Report,
    Reset,
    Revert,
    Roi,
    RoiManager,
    Rois,
    RoisAndKeys,
    Rotate,
    Save,
    Send,
    SmartAlbum,
    Stack,
    Studies,
    Sync,
    Text,
    ThreeD,
    Trash,
    Viewer,
    Volume,
    Wlww,
    Zoom,
    ZoomToFit,
}

impl Bitmap {
    fn bytes(self) -> &'static [u8] {
        match self {
            Bitmap::ImportImages => include_bytes!("../../../assets/horos/icons/import_images.png"),
            Bitmap::AddImages => include_bytes!("../../../assets/horos/icons/add_images.png"),
            Bitmap::ReportCustom => include_bytes!("../../../assets/horos/icons/report_custom.png"),
            Bitmap::Add => include_bytes!("../../../assets/horos/icons/add.png"),
            Bitmap::Album => include_bytes!("../../../assets/horos/icons/album.png"),
            Bitmap::Angle => include_bytes!("../../../assets/horos/icons/angle.png"),
            Bitmap::Anonymize => include_bytes!("../../../assets/horos/icons/anonymize.png"),
            Bitmap::Arrow => include_bytes!("../../../assets/horos/icons/arrow.png"),
            Bitmap::Cobb => include_bytes!("../../../assets/horos/icons/cobb.png"),
            Bitmap::Database => include_bytes!("../../../assets/horos/icons/database.png"),
            Bitmap::DatabaseWindow => include_bytes!("../../../assets/horos/icons/database_window.png"),
            Bitmap::DynamicAngle => include_bytes!("../../../assets/horos/icons/dynamic_angle.png"),
            Bitmap::Export => include_bytes!("../../../assets/horos/icons/export.png"),
            Bitmap::FlipH => include_bytes!("../../../assets/horos/icons/flip_h.png"),
            Bitmap::FlipV => include_bytes!("../../../assets/horos/icons/flip_v.png"),
            Bitmap::Import => include_bytes!("../../../assets/horos/icons/import.png"),
            Bitmap::Key => include_bytes!("../../../assets/horos/icons/key.png"),
            Bitmap::Keys => include_bytes!("../../../assets/horos/icons/keys.png"),
            Bitmap::Length => include_bytes!("../../../assets/horos/icons/length.png"),
            Bitmap::Minus => include_bytes!("../../../assets/horos/icons/minus.png"),
            Bitmap::Move => include_bytes!("../../../assets/horos/icons/move.png"),
            Bitmap::Movie => include_bytes!("../../../assets/horos/icons/movie.png"),
            Bitmap::Mpr => include_bytes!("../../../assets/horos/icons/mpr.png"),
            Bitmap::Network => include_bytes!("../../../assets/horos/icons/network.png"),
            Bitmap::Next => include_bytes!("../../../assets/horos/icons/next.png"),
            Bitmap::Oval => include_bytes!("../../../assets/horos/icons/oval.png"),
            Bitmap::Pause => include_bytes!("../../../assets/horos/icons/pause.png"),
            Bitmap::Pencil => include_bytes!("../../../assets/horos/icons/pencil.png"),
            Bitmap::Play => include_bytes!("../../../assets/horos/icons/play.png"),
            Bitmap::Point => include_bytes!("../../../assets/horos/icons/point.png"),
            Bitmap::Presets => include_bytes!("../../../assets/horos/icons/presets.png"),
            Bitmap::Prev => include_bytes!("../../../assets/horos/icons/prev.png"),
            Bitmap::Print => include_bytes!("../../../assets/horos/icons/print.png"),
            Bitmap::Propagate => include_bytes!("../../../assets/horos/icons/propagate.png"),
            Bitmap::Query => include_bytes!("../../../assets/horos/icons/query.png"),
            Bitmap::Rectangle => include_bytes!("../../../assets/horos/icons/rectangle.png"),
            Bitmap::Report => include_bytes!("../../../assets/horos/icons/report.png"),
            Bitmap::Reset => include_bytes!("../../../assets/horos/icons/reset.png"),
            Bitmap::Revert => include_bytes!("../../../assets/horos/icons/revert.png"),
            Bitmap::Roi => include_bytes!("../../../assets/horos/icons/roi.png"),
            Bitmap::RoiManager => include_bytes!("../../../assets/horos/icons/roi_manager.png"),
            Bitmap::Rois => include_bytes!("../../../assets/horos/icons/rois.png"),
            Bitmap::RoisAndKeys => include_bytes!("../../../assets/horos/icons/rois_and_keys.png"),
            Bitmap::Rotate => include_bytes!("../../../assets/horos/icons/rotate.png"),
            Bitmap::Save => include_bytes!("../../../assets/horos/icons/save.png"),
            Bitmap::Send => include_bytes!("../../../assets/horos/icons/send.png"),
            Bitmap::SmartAlbum => include_bytes!("../../../assets/horos/icons/smart_album.png"),
            Bitmap::Stack => include_bytes!("../../../assets/horos/icons/stack.png"),
            Bitmap::Studies => include_bytes!("../../../assets/horos/icons/studies.png"),
            Bitmap::Sync => include_bytes!("../../../assets/horos/icons/sync.png"),
            Bitmap::Text => include_bytes!("../../../assets/horos/icons/text.png"),
            Bitmap::ThreeD => include_bytes!("../../../assets/horos/icons/three_d.png"),
            Bitmap::Trash => include_bytes!("../../../assets/horos/icons/trash.png"),
            Bitmap::Viewer => include_bytes!("../../../assets/horos/icons/viewer.png"),
            Bitmap::Volume => include_bytes!("../../../assets/horos/icons/volume.png"),
            Bitmap::Wlww => include_bytes!("../../../assets/horos/icons/wlww.png"),
            Bitmap::Zoom => include_bytes!("../../../assets/horos/icons/zoom.png"),
            Bitmap::ZoomToFit => include_bytes!("../../../assets/horos/icons/zoom_to_fit.png"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Bitmap::ImportImages => "import_images",
            Bitmap::AddImages => "add_images",
            Bitmap::ReportCustom => "report_custom",
            Bitmap::Add => "add",
            Bitmap::Album => "album",
            Bitmap::Angle => "angle",
            Bitmap::Anonymize => "anonymize",
            Bitmap::Arrow => "arrow",
            Bitmap::Cobb => "cobb",
            Bitmap::Database => "database",
            Bitmap::DatabaseWindow => "database_window",
            Bitmap::DynamicAngle => "dynamic_angle",
            Bitmap::Export => "export",
            Bitmap::FlipH => "flip_h",
            Bitmap::FlipV => "flip_v",
            Bitmap::Import => "import",
            Bitmap::Key => "key",
            Bitmap::Keys => "keys",
            Bitmap::Length => "length",
            Bitmap::Minus => "minus",
            Bitmap::Move => "move",
            Bitmap::Movie => "movie",
            Bitmap::Mpr => "mpr",
            Bitmap::Network => "network",
            Bitmap::Next => "next",
            Bitmap::Oval => "oval",
            Bitmap::Pause => "pause",
            Bitmap::Pencil => "pencil",
            Bitmap::Play => "play",
            Bitmap::Point => "point",
            Bitmap::Presets => "presets",
            Bitmap::Prev => "prev",
            Bitmap::Print => "print",
            Bitmap::Propagate => "propagate",
            Bitmap::Query => "query",
            Bitmap::Rectangle => "rectangle",
            Bitmap::Report => "report",
            Bitmap::Reset => "reset",
            Bitmap::Revert => "revert",
            Bitmap::Roi => "roi",
            Bitmap::RoiManager => "roi_manager",
            Bitmap::Rois => "rois",
            Bitmap::RoisAndKeys => "rois_and_keys",
            Bitmap::Rotate => "rotate",
            Bitmap::Save => "save",
            Bitmap::Send => "send",
            Bitmap::SmartAlbum => "smart_album",
            Bitmap::Stack => "stack",
            Bitmap::Studies => "studies",
            Bitmap::Sync => "sync",
            Bitmap::Text => "text",
            Bitmap::ThreeD => "three_d",
            Bitmap::Trash => "trash",
            Bitmap::Viewer => "viewer",
            Bitmap::Volume => "volume",
            Bitmap::Wlww => "wlww",
            Bitmap::Zoom => "zoom",
            Bitmap::ZoomToFit => "zoom_to_fit",
        }
    }
}

/// The texture for a bitmap icon, decoded and uploaded on first use and kept
/// in the context for the life of the application.
pub fn texture(ctx: &egui::Context, which: Bitmap) -> egui::TextureId {
    let id = egui::Id::unique(("horos-icon", which.name()));
    if let Some(handle) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return handle.id();
    }
    let image = match image::load_from_memory(which.bytes()) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw())
        }
        Err(e) => {
            tracing::warn!("icon {} failed to decode: {e}", which.name());
            egui::ColorImage::filled([2, 2], egui::Color32::TRANSPARENT)
        }
    };
    let handle = ctx.load_texture(
        format!("horos-icon-{}", which.name()),
        image,
        egui::TextureOptions::LINEAR,
    );
    let tex = handle.id();
    ctx.data_mut(|d| d.insert_temp(id, handle));
    tex
}

/// A bitmap icon as an image atom for buttons and menu titles.
pub fn image(ctx: &egui::Context, which: Bitmap, size: f32) -> egui::Image<'static> {
    egui::Image::from_texture(egui::load::SizedTexture::new(
        texture(ctx, which),
        egui::Vec2::splat(size),
    ))
}
