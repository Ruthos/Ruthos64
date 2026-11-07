//! The 2D viewer window (Horos: ViewerController + DCMView).
//!
//! One instance per open study, shown in its own native viewport. Holds the
//! series list, the current image, decoded-frame and GPU caches, the view
//! state, the active tool and the ROIs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use egui::{Align2, Color32, FontId, Key, PointerButton, Pos2, Rect, Sense, Ui, Vec2, ViewportId};

use ruthos_core::measure::Library;
use ruthos_core::{ImageRow, SeriesRow, StudyRow};

use ruthos_dicom::Frame;
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::loupe::{self, LoupeSettings, LoupeSource, LoupeTarget};

use crate::annotations::{self, AnnotationContext, AnnotationLevel};
use crate::icons;
use crate::icons::Bitmap;
use crate::loader::FrameKey;
use crate::roi::{roi_color, MmodePhase, Roi, RoiKind};
use crate::shared::Shared;
use crate::theme;
#[path = "session_view.rs"]
mod session_view;

use crate::widgets::{
    menu_check, menu_heading, menu_item, menu_item_enabled, menu_item_shortcut, shadow_text, thumbnail_cell,
    tool_menu_row, toolbar_caption, toolbar_separator, ToolbarButton,
};

/// Mouse tools, in the order Horos lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    WindowLevel,
    Move,
    Zoom,
    Rotate,
    Length,
    Angle,
    Rectangle,
    Oval,
    Arrow,
    Point,
    /// Left-ventricular dimensions, in M-mode or 2D.
    LvMmode,
    /// A closed outline clicked point by point.
    Polygon,
    /// The Cobb angle between two lines.
    Cobb,
    /// The angle between two lines that need not meet.
    DynamicAngle,
    /// An open outline clicked point by point: a length along a curve.
    OpenPolygon,
    /// A closed outline drawn freehand.
    Pencil,
    /// Paint a region; Alt erases.
    Brush,
    /// A text note.
    Text,
    /// Push the points of outlines to reshape them.
    Repulsor,
}

impl Tool {
    /// The mouse tools of the Tools menu, besides Measure.
    pub const NAVIGATE: [Tool; 4] = [Tool::WindowLevel, Tool::Move, Tool::Zoom, Tool::Rotate];

    /// What Measure draws, chosen in the Measurements menu, in its order.
    /// What the Measure tool can be, chosen in the Measurements menu.
    pub const MEASURE: [Tool; 6] = [
        Tool::Length,
        Tool::LvMmode,
        Tool::OpenPolygon,
        Tool::Angle,
        Tool::Cobb,
        Tool::DynamicAngle,
    ];

    /// Drawing and annotation tools, in the Tools menu itself.
    pub const DRAW: [Tool; 9] = [
        Tool::Arrow,
        Tool::Text,
        Tool::Point,
        Tool::Rectangle,
        Tool::Oval,
        Tool::Polygon,
        Tool::Pencil,
        Tool::Brush,
        Tool::Repulsor,
    ];

    pub fn is_measure(self) -> bool {
        Tool::MEASURE.contains(&self)
    }

    /// The Measurements menu heading a measuring tool sits under.
    pub fn group(self) -> &'static str {
        match self {
            Tool::Rectangle | Tool::Oval | Tool::Polygon | Tool::Pencil | Tool::Brush => "Area",
            Tool::Angle | Tool::Cobb | Tool::DynamicAngle => "Angle",
            Tool::Point | Tool::Arrow | Tool::Text => "Mark",
            Tool::Repulsor => "Edit",
            _ => "Measure",
        }
    }

    /// The picker's line for a measuring tool.
    pub fn picker_label(self) -> &'static str {
        match self {
            Tool::Length => "Caliper: distance, time, velocity",
            Tool::LvMmode => "LV dimensions: M-mode or 2D",
            Tool::Angle => "Angle",
            Tool::Rectangle => "Rectangle",
            Tool::Oval => "Ellipse",
            Tool::Polygon => "Polygon",
            Tool::Point => "Point: value or velocity",
            Tool::Arrow => "Arrow",
            Tool::OpenPolygon => "Open polygon: length along a curve",
            Tool::Cobb => "Cobb angle",
            Tool::DynamicAngle => "Angle between two lines",
            Tool::Pencil => "Pencil: freehand outline",
            Tool::Brush => "Brush: paint a region (Alt erases)",
            Tool::Text => "Text",
            Tool::Repulsor => "Repulsor: push outline points",
            other => other.label(),
        }
    }

    pub const ALL: [Tool; 19] = [
        Tool::WindowLevel,
        Tool::Move,
        Tool::Zoom,
        Tool::Rotate,
        Tool::Length,
        Tool::Angle,
        Tool::Rectangle,
        Tool::Oval,
        Tool::Arrow,
        Tool::Point,
        Tool::LvMmode,
        Tool::Polygon,
        Tool::Cobb,
        Tool::DynamicAngle,
        Tool::OpenPolygon,
        Tool::Pencil,
        Tool::Brush,
        Tool::Text,
        Tool::Repulsor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::WindowLevel => "WL/WW",
            Tool::Move => "Move",
            Tool::Zoom => "Zoom",
            Tool::Rotate => "Rotate",
            Tool::Length => "Caliper",
            Tool::Angle => "Angle",
            Tool::Rectangle => "Rectangle",
            Tool::Oval => "Ellipse",
            Tool::Polygon => "Polygon",
            Tool::Arrow => "Arrow",
            Tool::Point => "Point",
            Tool::LvMmode => "LV dimensions",
            Tool::Cobb => "Cobb angle",
            Tool::DynamicAngle => "Dynamic angle",
            Tool::OpenPolygon => "Open polygon",
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Text => "Text",
            Tool::Repulsor => "Repulsor",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Tool::WindowLevel => icons::TOOL_WL,
            Tool::Move => icons::TOOL_MOVE,
            Tool::Zoom => icons::TOOL_ZOOM,
            Tool::Rotate => icons::TOOL_ROTATE,
            Tool::Length => icons::TOOL_LENGTH,
            Tool::Angle => icons::TOOL_ANGLE,
            Tool::Rectangle => icons::TOOL_RECT,
            Tool::Oval => icons::TOOL_OVAL,
            Tool::Polygon => icons::TOOL_RECT,
            Tool::Arrow => icons::TOOL_ARROW,
            Tool::Point => icons::TOOL_POINT,
            Tool::LvMmode => icons::TOOL_LENGTH,
            Tool::Cobb | Tool::DynamicAngle => icons::TOOL_ANGLE,
            Tool::OpenPolygon | Tool::Pencil => icons::TOOL_LENGTH,
            Tool::Brush | Tool::Repulsor => icons::TOOL_OVAL,
            Tool::Text => icons::ANNOTATIONS,
        }
    }

    /// The Horos artwork for the tool.
    /// Single-key shortcut, as shown in the Measurements menu.
    pub fn shortcut(self) -> &'static str {
        match self {
            Tool::WindowLevel => "W",
            Tool::Move => "M",
            Tool::Zoom => "Z",
            Tool::Rotate => "T",
            Tool::Length => "L",
            Tool::Angle => "A",
            Tool::Rectangle => "B",
            Tool::Oval => "E",
            Tool::Polygon => "G",
            Tool::Arrow => "N",
            Tool::Point => "P",
            Tool::LvMmode => "K",
            Tool::Cobb => "Y",
            Tool::DynamicAngle => "D",
            Tool::OpenPolygon => "O",
            Tool::Pencil => "S",
            Tool::Brush => "U",
            Tool::Text => "X",
            Tool::Repulsor => "Q",
        }
    }

    pub fn from_key(key: Key) -> Option<Tool> {
        Some(match key {
            Key::W => Tool::WindowLevel,
            Key::M => Tool::Move,
            Key::Z => Tool::Zoom,
            Key::T => Tool::Rotate,
            Key::L => Tool::Length,
            Key::A => Tool::Angle,
            Key::B => Tool::Rectangle,
            Key::E => Tool::Oval,
            Key::G => Tool::Polygon,
            Key::N => Tool::Arrow,
            Key::P => Tool::Point,
            Key::K => Tool::LvMmode,
            Key::Y => Tool::Cobb,
            Key::D => Tool::DynamicAngle,
            Key::O => Tool::OpenPolygon,
            Key::S => Tool::Pencil,
            Key::U => Tool::Brush,
            Key::X => Tool::Text,
            Key::Q => Tool::Repulsor,
            _ => return None,
        })
    }

    pub fn bitmap(self) -> Bitmap {
        match self {
            Tool::WindowLevel => Bitmap::Wlww,
            Tool::Move => Bitmap::Move,
            Tool::Zoom => Bitmap::Zoom,
            Tool::Rotate => Bitmap::Rotate,
            Tool::Length => Bitmap::Length,
            Tool::Angle => Bitmap::Angle,
            Tool::Rectangle => Bitmap::Rectangle,
            Tool::Oval => Bitmap::Oval,
            Tool::Polygon => Bitmap::Roi,
            Tool::Arrow => Bitmap::Arrow,
            Tool::Point => Bitmap::Point,
            Tool::LvMmode => Bitmap::Length,
            Tool::Cobb => Bitmap::Cobb,
            Tool::DynamicAngle => Bitmap::DynamicAngle,
            Tool::OpenPolygon => Bitmap::Rois,
            Tool::Pencil | Tool::Brush => Bitmap::Pencil,
            Tool::Text => Bitmap::Text,
            Tool::Repulsor => Bitmap::Revert,
        }
    }

    fn is_roi(self) -> bool {
        matches!(
            self,
            Tool::Length
                | Tool::Angle
                | Tool::Rectangle
                | Tool::Oval
                | Tool::Arrow
                | Tool::Point
                | Tool::LvMmode
                | Tool::Polygon
                | Tool::Cobb
                | Tool::DynamicAngle
                | Tool::OpenPolygon
                | Tool::Pencil
                | Tool::Brush
                | Tool::Text
                | Tool::Repulsor
        )
    }
}

/// A Cobb or dynamic angle being drawn: two drags.
#[derive(Debug, Clone, Copy)]
struct TwoLineDraft {
    cobb: bool,
    first: (Vec2, Vec2),
    second: Option<(Vec2, Vec2)>,
}

impl TwoLineDraft {
    fn roi(&self) -> Roi {
        let (a, b) = self.first;
        let (c, d) = self.second.unwrap_or((b, b));
        Roi::new(RoiKind::TwoLines {
            cobb: self.cobb,
            p: [a, b, c, d],
        })
    }
}

/// The text box for a text note: where it goes, and the text so far.
/// `edit` is the note being changed, by index on the frame.
#[derive(Debug, Clone)]
struct TextPrompt {
    key: FrameKey,
    at: Vec2,
    screen: Pos2,
    text: String,
    edit: Option<usize>,
    focus: bool,
}

struct AngleDraft {
    a: Vec2,
    b: Vec2,
    /// After the first drag, the vertex is fixed and the mouse previews `c`.
    b_fixed: bool,
    c: Vec2,
}

/// How the image area is divided (Horos: image tiling, and the series
/// layout of a study).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilingMode {
    /// Every cell shows the same series, consecutive images from the first cell.
    Series,
    /// One series of the study per cell.
    Study,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiling {
    pub mode: TilingMode,
    pub rows: usize,
    pub cols: usize,
}

impl Tiling {
    /// Grids offered in the menu, as (rows, columns).
    pub const GRIDS: [(usize, usize); 8] = [(1, 1), (1, 2), (2, 1), (2, 2), (2, 3), (3, 2), (3, 3), (4, 4)];

    pub fn cells(self) -> usize {
        self.rows * self.cols
    }

    pub fn label(self) -> String {
        format!("{}\u{00d7}{}", self.rows, self.cols)
    }
}

/// The first movie: Cisco's encoder is offered, with Cisco's licence, and
/// nothing is fetched unless the user agrees.
struct EncoderPrompt {
    /// Where the movie was asked to go, when it was given in advance.
    to: Option<std::path::PathBuf>,
    fetching: Option<crate::pacs::Task<Result<std::path::PathBuf, String>>>,
    error: Option<String>,
}

/// A movie export running on its own thread.
struct MovieJob {
    task: crate::pacs::Task<Result<std::path::PathBuf, String>>,
    progress: Arc<parking_lot::Mutex<(usize, usize)>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// One cell of the viewer grid: a series, a position in it and a view.
#[derive(Clone)]
pub struct Pane {
    pub series_idx: usize,
    pub images: Vec<ImageRow>,
    /// May run past `images` in a tiled series: that cell is blank.
    pub image_idx: usize,
    pub frame_idx: u32,
    pub view: ViewState,
    pub clut: String,
    /// A locked cell keeps its image, window, colours and measurements:
    /// tools, keys, scrolling and the toolbar leave it alone.
    pub locked: bool,
    fit_pending: bool,
    window_initialised_for: Option<i64>,
    load_error: Option<String>,
    /// The cine clock while playing: where and when playback started and at
    /// what rate, and the position last shown.
    cine: Option<CineClock>,
}

/// Cine playback by the clock: the position is the start plus elapsed time
/// times the rate, so a loop keeps real time whatever the screen does,
/// skipping frames when the screen refreshes more slowly than the loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CineClock {
    start: Instant,
    from: usize,
    rate: f32,
    shown: usize,
}

/// Where a loop is after `elapsed` seconds at `rate` frames a second,
/// starting at `from`, in a loop of `n`.
pub fn cine_position(from: usize, elapsed: f32, rate: f32, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let steps = (elapsed.max(0.0) * rate.max(0.0)).floor() as usize;
    (from + steps) % n
}

/// A measured refresh rate, snapped to the usual display rate it is within
/// 4 per cent of (timing jitter makes 60 Hz measure as 59.3 or 61.0).
pub fn snap_refresh(hz: f32) -> f32 {
    const RATES: [f32; 14] = [
        24.0, 30.0, 48.0, 50.0, 60.0, 72.0, 75.0, 90.0, 100.0, 120.0, 144.0, 165.0, 240.0, 360.0,
    ];
    RATES
        .into_iter()
        .find(|r| (hz - r).abs() <= r * 0.04)
        .unwrap_or_else(|| hz.round())
}

/// "1×", "0.5×", "0.25×", "0.33×".
pub fn speed_label(s: f32) -> String {
    let t = format!("{s:.2}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    format!("{t}\u{d7}")
}

/// The speeds offered when the file gives a frame rate, fastest first.
pub const CINE_SPEEDS: [f32; 3] = [1.0, 0.5, 0.25];

/// The fastest speed, in hundredths, that shows every frame of a loop
/// recorded at `rate` on a screen refreshing at `hz`; `None` below the
/// slider's slowest (0.05x).
pub fn speed_for_every_frame(rate: f32, hz: f32) -> Option<f32> {
    if rate <= hz * 1.02 {
        return Some(1.0);
    }
    let s = ((hz / rate) * 100.0).floor() / 100.0;
    (s >= 0.05).then_some(s)
}

impl Pane {
    fn new(shared: &Shared, series: &SeriesRow, series_idx: usize) -> Self {
        let clut = shared.settings.lock().default_clut.clone();
        Self {
            series_idx,
            images: shared.db.images_for_series(series.id).unwrap_or_default(),
            image_idx: 0,
            frame_idx: 0,
            view: ViewState::default(),
            clut,
            locked: false,
            fit_pending: true,
            window_initialised_for: None,
            load_error: None,
            cine: None,
        }
    }

    fn image(&self) -> Option<&ImageRow> {
        self.images.get(self.image_idx)
    }

    fn key(&self) -> Option<FrameKey> {
        let img = self.image()?;
        if self.frame_idx >= img.number_of_frames.max(1) as u32 {
            return None;
        }
        Some(FrameKey {
            image_id: img.id,
            frame: self.frame_idx,
        })
    }

    /// Scroll positions: the images, or the frames of a single multiframe image.
    fn positions(&self) -> usize {
        if self.images.len() == 1 {
            self.images[0].number_of_frames.max(1) as usize
        } else {
            self.images.len()
        }
    }

    fn position(&self) -> usize {
        if self.images.len() == 1 {
            self.frame_idx as usize
        } else {
            self.image_idx
        }
    }

    fn set_position(&mut self, pos: usize) {
        if self.images.len() == 1 {
            self.image_idx = 0;
            self.frame_idx = pos as u32;
        } else {
            self.image_idx = pos;
            self.frame_idx = 0;
        }
    }

    /// What cine steps through: the frames of the current image when it is
    /// a loop (which then repeats), otherwise the images of the series.
    fn loop_len(&self) -> usize {
        match self.image() {
            Some(img) if img.number_of_frames > 1 => img.number_of_frames as usize,
            _ => self.images.len(),
        }
    }

    fn loop_pos(&self) -> usize {
        match self.image() {
            Some(img) if img.number_of_frames > 1 => self.frame_idx as usize,
            _ => self.image_idx,
        }
    }

    /// The frame shown at loop position `pos`.
    fn key_at(&self, pos: usize) -> Option<FrameKey> {
        match self.image() {
            Some(img) if img.number_of_frames > 1 => Some(FrameKey {
                image_id: img.id,
                frame: pos as u32,
            }),
            _ => self.images.get(pos).map(|img| FrameKey {
                image_id: img.id,
                frame: 0,
            }),
        }
    }

    fn set_loop_pos(&mut self, pos: usize) {
        match self.image() {
            Some(img) if img.number_of_frames > 1 => self.frame_idx = pos as u32,
            _ => {
                self.image_idx = pos;
                self.frame_idx = 0;
            }
        }
    }
}

pub struct ViewerState {
    pub id: u64,
    pub viewport_id: ViewportId,
    pub study: StudyRow,
    pub series: Vec<SeriesRow>,
    /// The cells of the grid; `active` is the one tools, keys and the
    /// toolbar act on.
    pub panes: Vec<Pane>,
    pub active: usize,
    pub tiling: Tiling,

    /// Tools assigned to the left and right mouse buttons. The middle button
    /// always pans, as in Horos.
    pub tool: Tool,
    pub right_tool: Tool,
    pub annotations: AnnotationLevel,
    pub bilinear: bool,

    pub rois: HashMap<FrameKey, Vec<Roi>>,
    /// Frames whose stored ROIs have been read from the archive.
    /// A line on a loop and its anatomical M-mode strip.
    amm: Option<crate::amm_ui::AmmView>,
    /// The slice scrolled to, shown as soon as it has been read.
    scroll_target: Option<i64>,
    scroll_direction: i64,
    rois_loaded: HashSet<FrameKey>,
    /// Frames whose ROIs changed and must be written back.
    dirty_rois: HashSet<FrameKey>,
    /// Label rectangles painted last frame, for click-to-name.
    label_hits: Vec<(FrameKey, usize, Rect)>,
    /// The "assign measurement" popup, when open.
    assign: Option<AssignPopup>,
    drag_draft: Option<(Vec2, Vec2)>,
    angle_draft: Option<AngleDraft>,
    mmode_draft: Option<MmodeDraft>,
    /// A polygon being clicked out, and where the next point would go.
    polygon_draft: Option<(Vec<Vec2>, Option<Vec2>)>,
    /// The polygon being clicked out stays open (Open polygon).
    polygon_open: bool,
    /// Cobb or dynamic angle: the first line, then the second being drawn.
    two_line_draft: Option<TwoLineDraft>,
    /// A freehand outline being drawn.
    pencil_draft: Option<Vec<Vec2>>,
    /// Brush and Repulsor radius, in screen points.
    pub brush_radius: f32,
    /// The brush region being painted (index on this frame) and the last
    /// point painted, so a fast stroke leaves no gaps.
    brush_stroke: Option<(usize, Vec2)>,
    /// The text box for a new or edited text note.
    text_prompt: Option<TextPrompt>,
    /// Where the pointer is over the image, for the brush outline.
    tool_hover: Option<Pos2>,
    /// The kind of study the naming menus offer sets for; `None` follows
    /// the study.
    region_override: Option<ruthos_core::measure::Region>,
    /// The ROI Manager and ROI Info windows.
    pub show_roi_manager: bool,
    pub show_roi_info: bool,
    /// The Histogram and plot window, when open.
    histogram: Option<crate::histogram::HistogramWindow>,
    /// What the Measure tool draws, chosen in the Measurements menu.
    measure_tool: Tool,
    /// Per series: (roi id, edit time, LVID mm) of the latest lines.
    mmode_latest: HashMap<i64, MmodeLatest>,
    /// The stored LV lines of each series, for pairing 2D lines across
    /// the frames of a loop.
    mmode_rows: HashMap<i64, Vec<ruthos_core::mmode::MmodeRow>>,
    mmode_refresh: HashSet<i64>,
    pub selected_roi: Option<usize>,
    /// The handle the keyboard nudges, index into `Roi::handles`.
    active_handle: usize,
    handle_drag: Option<HandleDrag>,
    move_drag: Option<MoveDrag>,
    dev_selected: bool,
    /// Loupe settings, refreshed from Preferences each frame.
    loupe: LoupeSettings,
    /// The measurement library, refreshed each frame.
    library: Arc<Library>,
    /// The set the Measurements menu names from.
    current_set: usize,
    show_rois: bool,
    show_labels: bool,
    basis_panel: bool,
    /// A Measurements menu choice to carry out once `Shared` is at hand.
    pending_menu: Option<MenuAction>,
    /// Where the loupe shows this frame, set by input handling.
    loupe_target: Option<LoupeTarget>,
    /// Previous geometry of edited measurements, newest last.
    undo: Vec<UndoEntry>,

    pub playing: bool,
    /// Frames a second when the file gives no rate.
    pub fps: f32,
    /// 1, 0.5 or 0.25 of the rate the file was recorded at.
    pub speed: f32,
    last_tick: Option<Instant>,
    /// The time between this window's frames, smoothed, while playing: the
    /// screen's refresh as it actually happens.
    frame_interval: f32,
    last_frame_at: Option<Instant>,
    /// Frames shown and skipped since `stats_since`.
    cine_stats: (u32, u32, Option<Instant>),
    /// When cine is skipping frames: the loop's rate, the screen's and the
    /// share skipped, for the banner.
    skipping: Option<(f32, f32, f32)>,
    /// Frames a second each image was recorded at, once known.
    file_rates: HashMap<i64, Option<f32>>,
    /// While playing, how many positions ahead to decode: half a second.
    cine_ahead: usize,
    /// When the window opened, and whether the developer screenshot and
    /// playback options have run.
    opened_at: Instant,
    dev_shot_done: bool,
    dev_play_done: bool,
    /// A movie being written (the Movie button).
    movie: Option<MovieJob>,
    /// Waiting for the user's consent to fetch Cisco's encoder.
    encoder_prompt: Option<EncoderPrompt>,
    /// The dial's mode and the indicator (dial/mod.rs).
    dial: crate::dial::Dial,
    /// Dial events routed to this viewer by the app.
    pub dial_inbox: Vec<crate::dial::DialEvent>,
    /// The cell under the pointer and the cells' rectangles, last frame.
    hover_cell: Option<usize>,
    cell_rects: Vec<Rect>,
    dial_script_done: bool,
    dev_movie_done: bool,

    frames: HashMap<FrameKey, Arc<Frame>>,
    frame_order: VecDeque<FrameKey>,
    gpu: HashMap<FrameKey, Arc<GpuImage>>,
    gpu_order: VecDeque<FrameKey>,

    /// What this viewer shows and sends in a guided session.
    session: session_view::SessionView,

    pub close_requested: bool,
    /// An echo's images in the order they were taken. Empty for other studies.
    timeline: Vec<ruthos_core::model::TimelineEntry>,
    timeline_read: bool,
    /// The image the timeline last scrolled to.
    timeline_shown: Option<i64>,
    /// Show the series list, not the timeline.
    grouped: bool,
    /// Opened on the study, not on a chosen series: start at its first image.
    timeline_from_start: bool,
    /// The `series_revision` this window's series list was read at.
    series_seen: u64,
    /// This frame leaves a gap so the main window can paint.
    give_way: bool,
    gave_way_at: Option<Instant>,
    /// Teaching copy with an answer key: measurements hidden until revealed.
    pub reveal_answers: bool,
    mouse_pixel: Option<(i64, i64)>,
    status: Option<(String, Instant)>,
    wl_drag_anchor: Option<(f32, f32)>,
    calibrate: Option<CalibrateDraft>,
    pending_clear_calibration: Option<i64>,
}

/// An LV M-mode line being placed: the time column and the depths so far.
struct MmodeDraft {
    /// In an M-mode strip the points share one time column; on a 2D image
    /// they lie along the line from the first point.
    mmode: bool,
    points: Vec<Vec2>,
    /// Where the next point would go, for the preview segment.
    hover: Option<Vec2>,
}

impl MmodeDraft {
    /// The next point for a pointer position: in M-mode, on the column.
    fn snap(&self, p: Vec2) -> Vec2 {
        match (self.mmode, self.points.first()) {
            (true, Some(first)) => Vec2::new(first.x, p.y),
            _ => p,
        }
    }

    /// How far along the line a point is: depth in M-mode, distance from
    /// the first point in 2D. Each point must be further than the last.
    fn progress(&self, p: Vec2) -> f32 {
        match (self.mmode, self.points.first()) {
            (true, _) => p.y,
            (false, Some(first)) => (p - *first).length(),
            (false, None) => 0.0,
        }
    }
}

/// The latest diastolic and systolic lines of a series, for choosing the
/// phase of a new line and for fractional shortening beside the systole.
#[derive(Debug, Clone, Default)]
struct MmodeLatest {
    diastole: Option<(i64, i64, f64)>,
    systole: Option<(i64, i64, f64)>,
}

/// A handle being dragged.
struct HandleDrag {
    key: FrameKey,
    index: usize,
    handle: usize,
    original: RoiKind,
    /// Sub-pixel position carried while Shift scales the movement.
    fine: Option<Vec2>,
}

/// A whole measurement being dragged by its outline.
struct MoveDrag {
    key: FrameKey,
    index: usize,
    original: RoiKind,
    last: Vec2,
}

/// What Ctrl+Z puts back.
struct UndoEntry {
    key: FrameKey,
    id: Option<i64>,
    index: usize,
    kind: RoiKind,
}

/// What the Measurements menu (toolbar and right-click) can do. Both menus
/// are drawn from the same list and the choice runs once per frame.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    SetTool(Tool),
    SetPhase(MmodePhase),
    SetSet(usize),
    SetRegion(Option<ruthos_core::measure::Region>),
    /// Label and, when it comes from a definition, its key.
    Name(String, Option<String>),
    NamePopup,
    Undo,
    DeleteSelected,
    DeleteAll,
    Calibrate,
    RemoveCalibration,
    ToggleBasisPanel,
    ToggleRois,
    ToggleLabels,
    CopyValues,
    AddToReport,
    OpenReport,
    Fit,
    ToggleRoiManager,
    ToggleRoiInfo,
    ToggleHistogram,
}

/// The popup that assigns a ROI to a named measurement.
struct AssignPopup {
    key: FrameKey,
    index: usize,
    pos: Pos2,
    custom: String,
}

/// The "Calibrate from this length" dialog.
struct CalibrateDraft {
    image_id: i64,
    pixel_length: f32,
    known_mm: String,
    apply_to_series: bool,
    /// Midpoint of the reference length: scopes the calibration to the
    /// ultrasound region under it, when the image has regions.
    scope: Option<(f64, f64)>,
}

const FRAME_CACHE: usize = 48;

/// Slices read in the direction of scrolling, and behind it.
const READ_AHEAD: usize = 12;
const READ_BEHIND: usize = 3;

/// Size of the lock control in a cell's bottom-right corner.
const LOCK_CHIP: Vec2 = Vec2::new(40.0, 18.0);

fn lock_chip_rect(cell: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(cell.right() - 6.0 - LOCK_CHIP.x, cell.bottom() - 6.0 - LOCK_CHIP.y),
        LOCK_CHIP,
    )
}
const GPU_CACHE: usize = 8;
/// How long a playing viewer pauses for the main window, and how often at most.
const GIVE_WAY: std::time::Duration = std::time::Duration::from_millis(40);
const GIVE_WAY_EVERY: std::time::Duration = std::time::Duration::from_millis(300);

impl ViewerState {
    pub fn open(shared: &Shared, study: StudyRow, series_id: Option<i64>) -> anyhow::Result<Self> {
        // A SEG is shown over its images in MPR, not as images of its own.
        let mut series = shared.db.series_for_study(study.id)?;
        series.retain(|s| s.modality != ruthos_core::seg::MODALITY);
        if series.is_empty() {
            anyhow::bail!("study has no series");
        }
        let series_idx = series_id
            .and_then(|id| series.iter().position(|s| s.id == id))
            .unwrap_or(0);
        let pane = Pane::new(shared, &series[series_idx], series_idx);
        let _ = shared.db.mark_study_opened(study.id);
        let id = shared.next_id();
        let settings = shared.settings.lock();
        let annotations = match settings.annotations_level {
            0 => AnnotationLevel::None,
            1 => AnnotationLevel::Partial,
            _ => AnnotationLevel::Full,
        };
        let grouped = settings.series_grouped;
        drop(settings);
        let mut viewer = Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-viewer", id)),
            study,
            series,
            panes: vec![pane],
            active: 0,
            tiling: Tiling {
                mode: TilingMode::Series,
                rows: 1,
                cols: 1,
            },
            tool: Tool::WindowLevel,
            right_tool: Tool::Zoom,
            annotations,
            bilinear: true,
            rois: HashMap::new(),
            amm: None,
            scroll_target: None,
            scroll_direction: 1,
            rois_loaded: HashSet::new(),
            dirty_rois: HashSet::new(),
            label_hits: Vec::new(),
            assign: None,
            drag_draft: None,
            angle_draft: None,
            mmode_draft: None,
            polygon_draft: None,
            polygon_open: false,
            two_line_draft: None,
            pencil_draft: None,
            brush_radius: 12.0,
            brush_stroke: None,
            text_prompt: None,
            tool_hover: None,
            region_override: None,
            // Developer option: `RUTHOS_ROI_WINDOWS=1` opens ROI Manager and ROI Info.
            show_roi_manager: std::env::var_os("RUTHOS_ROI_WINDOWS").is_some(),
            show_roi_info: std::env::var_os("RUTHOS_ROI_WINDOWS").is_some(),
            // Developer option: `RUTHOS_HISTOGRAM=1` opens Histogram and plot.
            histogram: std::env::var_os("RUTHOS_HISTOGRAM").map(|_| Default::default()),
            measure_tool: Tool::Length,
            mmode_latest: HashMap::new(),
            mmode_rows: HashMap::new(),
            mmode_refresh: HashSet::new(),
            selected_roi: None,
            active_handle: 0,
            handle_drag: None,
            move_drag: None,
            dev_selected: false,
            loupe: LoupeSettings::default(),
            loupe_target: None,
            library: Arc::new(Library::default()),
            current_set: 0,
            show_rois: true,
            show_labels: true,
            // Developer option: `RUTHOS_BASIS_PANEL=1` opens the calibration basis panel.
            basis_panel: std::env::var_os("RUTHOS_BASIS_PANEL").is_some(),
            pending_menu: None,
            undo: Vec::new(),
            playing: false,
            fps: 10.0,
            speed: 1.0,
            last_tick: None,
            frame_interval: 1.0 / 60.0,
            last_frame_at: None,
            cine_stats: (0, 0, None),
            skipping: None,
            file_rates: HashMap::new(),
            cine_ahead: 1,
            opened_at: Instant::now(),
            dev_shot_done: false,
            dev_play_done: false,
            movie: None,
            encoder_prompt: None,
            dial: crate::dial::Dial::new(&shared.settings.lock().dial),
            dial_inbox: Vec::new(),
            hover_cell: None,
            cell_rects: Vec::new(),
            dial_script_done: false,
            dev_movie_done: false,
            frames: HashMap::new(),
            frame_order: VecDeque::new(),
            gpu: HashMap::new(),
            gpu_order: VecDeque::new(),
            session: Default::default(),
            close_requested: false,
            timeline: Vec::new(),
            timeline_read: false,
            timeline_shown: None,
            grouped,
            timeline_from_start: series_id.is_none(),
            series_seen: shared.series_revision.load(std::sync::atomic::Ordering::Relaxed),
            give_way: false,
            gave_way_at: None,
            reveal_answers: false,
            mouse_pixel: None,
            status: None,
            wl_drag_anchor: None,
            calibrate: None,
            pending_clear_calibration: None,
        };
        // Developer option: `RUTHOS_TILING=series:2x2` or `study:1x2`.
        if let Some(t) = std::env::var("RUTHOS_TILING").ok().and_then(|v| parse_tiling(&v)) {
            viewer.set_tiling(shared, t);
        }
        // Developer option: `RUTHOS_OPEN_IMAGE=<image id>` starts on that image.
        if let Some(id) = std::env::var("RUTHOS_OPEN_IMAGE")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
        {
            let a = viewer.active;
            if let Some(idx) = viewer.panes[a].images.iter().position(|i| i.id == id) {
                viewer.panes[a].image_idx = idx;
            }
        }
        Ok(viewer)
    }

    pub fn title(&self) -> String {
        let s = &self.study;
        let mut t = s.patient_name.clone();
        if let Some(d) = &s.study_date {
            t.push_str(&format!("  {d}"));
        }
        if !s.study_description.is_empty() {
            t.push_str(&format!("  {}", s.study_description));
        }
        format!("{t} — Ruthos")
    }

    fn current_image(&self) -> Option<&ImageRow> {
        self.panes[self.active].image()
    }

    fn current_series(&self) -> &SeriesRow {
        &self.series[self.panes[self.active].series_idx]
    }

    fn current_key(&self) -> Option<FrameKey> {
        self.panes[self.active].key()
    }

    /// Series shown in cell `k`.
    fn series_of(&self, k: usize) -> &SeriesRow {
        &self.series[self.panes[k].series_idx]
    }

    /// Show another series of the study in the active cell. In a tiled
    /// series every cell follows.
    pub fn select_series(&mut self, shared: &Shared, idx: usize) {
        let a = self.active;
        if idx >= self.series.len() || idx == self.panes[a].series_idx {
            return;
        }
        if self.panes[a].locked {
            self.refuse_locked();
            return;
        }
        self.persist_pane(shared, a);
        let p = &mut self.panes[a];
        p.series_idx = idx;
        p.images = shared.db.images_for_series(self.series[idx].id).unwrap_or_default();
        p.image_idx = 0;
        p.frame_idx = 0;
        p.fit_pending = true;
        p.window_initialised_for = None;
        p.load_error = None;
        self.selected_roi = None;
        self.drag_draft = None;
        self.angle_draft = None;
        self.sync_series_cells();
        self.keep_still_alone();
    }

    /// Change the grid. A tiled series spreads the active cell's series over
    /// the cells; a tiled study gives each cell its own series.
    pub fn set_tiling(&mut self, shared: &Shared, tiling: Tiling) {
        if tiling == self.tiling {
            return;
        }
        self.restore_stack(shared);
        for k in 0..self.panes.len() {
            self.persist_pane(shared, k);
        }
        let cells = tiling.cells().max(1);
        let current = self.panes[self.active].clone();
        let mut panes = Vec::with_capacity(cells);
        match tiling.mode {
            TilingMode::Series => {
                let base = current.position().min(current.positions().saturating_sub(cells));
                for k in 0..cells {
                    let mut p = current.clone();
                    p.set_position(base + k);
                    p.fit_pending = true;
                    panes.push(p);
                }
                self.active = 0;
            }
            TilingMode::Study => {
                let n = self.series.len();
                let first = current.series_idx.min(n.saturating_sub(cells));
                for k in 0..cells.min(n) {
                    let idx = first + k;
                    if idx == current.series_idx {
                        let mut p = current.clone();
                        p.image_idx = p.image_idx.min(p.images.len().saturating_sub(1));
                        p.fit_pending = true;
                        panes.push(p);
                    } else {
                        panes.push(Pane::new(shared, &self.series[idx], idx));
                    }
                }
                self.active = (current.series_idx - first).min(panes.len().saturating_sub(1));
            }
        }
        self.panes = panes;
        self.tiling = tiling;
        self.keep_still_alone();
        self.selected_roi = None;
        self.drag_draft = None;
        self.angle_draft = None;
        self.set_status(match tiling.mode {
            TilingMode::Series => format!("Tiling {}: images of this series", tiling.label()),
            TilingMode::Study => format!("Tiling {}: series of this study", tiling.label()),
        });
    }

    fn tiled_series(&self) -> bool {
        self.tiling.mode == TilingMode::Series && self.panes.len() > 1
    }

    /// In a tiled series, keep every cell on the active cell's series and
    /// view, showing consecutive positions from the first cell.
    fn sync_series_cells(&mut self) {
        if !self.tiled_series() {
            return;
        }
        let Some(a) = self.sync_source() else { return };
        let src = self.panes[a].clone();
        let n = src.positions();
        for (k, p) in self.panes.iter_mut().enumerate() {
            if p.locked {
                continue;
            }
            if p.series_idx != src.series_idx {
                p.series_idx = src.series_idx;
                p.images = src.images.clone();
                p.load_error = None;
                p.set_position(k.min(n.saturating_sub(1)));
            }
            p.view = src.view;
            p.clut = src.clut.clone();
            p.window_initialised_for = src.window_initialised_for;
            if k != a {
                p.fit_pending = src.fit_pending;
            }
        }
    }

    /// Make cell `k` the one tools and keys act on.
    fn activate(&mut self, k: usize) {
        if k < self.panes.len() && k != self.active {
            self.active = k;
            self.selected_roi = None;
            self.drag_draft = None;
            self.angle_draft = None;
        }
    }

    /// Scroll the active cell (or the whole tiled series) to a position.
    fn set_active_position(&mut self, pos: usize) {
        if self.active_locked() {
            self.refuse_locked();
            return;
        }
        if self.tiled_series() {
            let delta = pos as i64 - self.panes[self.active].position() as i64;
            self.step_block(delta);
        } else {
            self.panes[self.active].set_position(pos);
        }
        self.selected_roi = None;
    }

    /// Move every unlocked cell of a tiled series by `delta`, stopping when
    /// the first or last of them reaches an end of the series. Cells never go
    /// blank, and locked cells stay where they are.
    fn step_block(&mut self, delta: i64) {
        let unlocked: Vec<usize> = (0..self.panes.len()).filter(|&k| !self.panes[k].locked).collect();
        let Some(&first) = unlocked.first() else { return };
        let n = self.panes[first].positions() as i64;
        if n == 0 {
            return;
        }
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for &k in &unlocked {
            let pos = self.panes[k].position().min((n - 1) as usize) as i64;
            lo = lo.max(-pos);
            hi = hi.min(n - 1 - pos);
        }
        let d = delta.clamp(lo, hi);
        if d == 0 {
            return;
        }
        for &k in &unlocked {
            let pos = (self.panes[k].position().min((n - 1) as usize) as i64 + d) as usize;
            self.panes[k].set_position(pos);
        }
        self.selected_roi = None;
    }

    fn active_locked(&self) -> bool {
        self.panes[self.active].locked
    }

    fn refuse_locked(&mut self) {
        self.set_status("This cell is locked");
    }

    /// The unlocked cell a tiled series takes its series, view and position
    /// from: the active cell, or the first unlocked one.
    fn sync_source(&self) -> Option<usize> {
        if !self.panes[self.active].locked {
            return Some(self.active);
        }
        self.panes.iter().position(|p| !p.locked)
    }

    fn toggle_lock(&mut self, k: usize) {
        let Some(p) = self.panes.get_mut(k) else { return };
        p.locked = !p.locked;
        let locked = p.locked;
        self.set_status(if locked {
            "Cell locked: image, window, colours and measurements are kept"
        } else {
            "Cell unlocked"
        });
        self.selected_roi = None;
        self.drag_draft = None;
        self.angle_draft = None;
        // Locking the active cell hands the keys and slider to the next
        // unlocked one, so scrolling carries on around the locked image.
        if locked && k == self.active {
            if let Some(next) = (0..self.panes.len())
                .map(|i| (k + 1 + i) % self.panes.len())
                .find(|&i| !self.panes[i].locked)
            {
                self.active = next;
            }
        }
    }

    /// Store WL, rotation and flips for every shown series, as Horos does.
    pub fn persist_series_display(&self, shared: &Shared) {
        for k in 0..self.panes.len() {
            self.persist_pane(shared, k);
        }
    }

    fn persist_pane(&self, shared: &Shared, k: usize) {
        let p = &self.panes[k];
        if p.window_initialised_for.is_none() {
            return;
        }
        let s = self.series_of(k);
        let _ = shared.db.set_series_display(
            s.id,
            Some(p.view.window_center as f64),
            Some(p.view.window_width as f64),
            p.view.rotation_deg as f64,
            p.view.flip_horizontal,
            p.view.flip_vertical,
        );
    }

    fn step_image(&mut self, delta: i64) {
        if self.active_locked() {
            return;
        }
        if self.tiled_series() {
            self.step_block(delta);
            return;
        }
        let p = &mut self.panes[self.active];
        if p.images.is_empty() {
            return;
        }
        let idx = p.image_idx.min(p.images.len() - 1);
        let frames = p.images[idx].number_of_frames.max(1);
        if frames > 1 {
            // Multiframe: step frames first, wrapping into neighbouring images.
            let f = p.frame_idx as i64 + delta;
            if f >= 0 && f < frames {
                p.frame_idx = f as u32;
                return;
            }
        }
        let n = p.images.len() as i64;
        // Through a stack the view stays on a slice until the next is ready, so
        // the picture, its slice number and its measurements always agree.
        if !self.playing && frames == 1 {
            let from = self
                .scroll_target
                .and_then(|id| p.images.iter().position(|i| i.id == id))
                .unwrap_or(idx);
            let next = &p.images[(from as i64 + delta).rem_euclid(n) as usize];
            if next.number_of_frames <= 1 {
                self.scroll_target = Some(next.id);
                self.scroll_direction = delta.signum();
                self.selected_roi = None;
                return;
            }
        }
        self.scroll_target = None;
        let next = (idx as i64 + delta).rem_euclid(n);
        p.image_idx = next as usize;
        p.frame_idx = if delta < 0 {
            (p.images[p.image_idx].number_of_frames.max(1) - 1) as u32
        } else {
            0
        };
        self.selected_roi = None;
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    /// The text box for a text note, at the point clicked.
    fn text_prompt_window(&mut self, ctx: &egui::Context) {
        let Some(mut tp) = self.text_prompt.take() else { return };
        let mut done: Option<bool> = None;
        egui::Area::new(egui::Id::unique(("ruthos-text-prompt", self.id)))
            .fixed_pos(tp.screen + Vec2::new(0.0, -28.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut tp.text)
                                .desired_width(220.0)
                                .hint_text("Text"),
                        );
                        if tp.focus {
                            edit.request_focus();
                            tp.focus = false;
                        }
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.button(if tp.edit.is_some() { "Change" } else { "Add" }).clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            done = Some(false);
                        }
                    });
                });
            });
        match done {
            None => self.text_prompt = Some(tp),
            Some(false) => {}
            Some(true) => {
                let text = tp.text.trim().to_string();
                let list = self.rois.entry(tp.key).or_default();
                match tp.edit {
                    Some(i) if i < list.len() => {
                        if text.is_empty() {
                            list.remove(i);
                            self.selected_roi = None;
                        } else {
                            list[i].name = Some(text);
                        }
                    }
                    _ if !text.is_empty() => {
                        let mut roi = Roi::new(RoiKind::Text { p: tp.at });
                        roi.name = Some(text);
                        list.push(roi);
                        self.selected_roi = Some(list.len() - 1);
                    }
                    _ => {}
                }
                self.dirty_rois.insert(tp.key);
            }
        }
    }

    /// ROI Manager: every measurement on the current image, to select,
    /// rename (in ROI Info) or delete.
    fn roi_manager_window(&mut self, ctx: &egui::Context) {
        if !self.show_roi_manager {
            return;
        }
        let Some(key) = self.current_key() else { return };
        let frame = self.frames.get(&key).cloned();
        let locked = self.active_locked();
        let mut open = true;
        let mut select = None;
        let mut delete = None;
        let mut delete_all = false;
        egui::Window::new("ROI Manager")
            .id(egui::Id::unique(("ruthos-roi-manager", self.id)))
            .open(&mut open)
            .default_size([420.0, 260.0])
            .show(ctx, |ui| {
                let list = self.rois.get(&key).cloned().unwrap_or_default();
                ui.label(
                    egui::RichText::new(format!(
                        "{} on this image{}",
                        match list.len() {
                            0 => "No measurements".to_string(),
                            1 => "1 measurement".to_string(),
                            n => format!("{n} measurements"),
                        },
                        if self.current_image().is_some_and(|i| i.number_of_frames > 1) {
                            format!(", frame {}", key.frame + 1)
                        } else {
                            String::new()
                        }
                    ))
                    .color(Color32::from_gray(110)),
                );
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    egui::Grid::new("roi-manager-grid")
                        .num_columns(4)
                        .striped(true)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Type").strong());
                            ui.label(egui::RichText::new("Name").strong());
                            ui.label(egui::RichText::new("Value").strong());
                            ui.label("");
                            ui.end_row();
                            for (i, roi) in list.iter().enumerate() {
                                let selected = self.selected_roi == Some(i);
                                if ui.selectable_label(selected, kind_label(&roi.kind)).clicked() {
                                    select = Some(i);
                                }
                                ui.label(roi.name.clone().unwrap_or_default());
                                let value = frame
                                    .as_ref()
                                    .map(|f| roi.value_text(f).replace('\n', "  "))
                                    .unwrap_or_default();
                                ui.label(value);
                                if ui.add_enabled(!locked, egui::Button::new("Delete").small()).clicked() {
                                    delete = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!locked && !list.is_empty(), egui::Button::new("Delete all"))
                        .clicked()
                    {
                        delete_all = true;
                    }
                    if ui.button("ROI Info").clicked() {
                        self.show_roi_info = true;
                    }
                });
            });
        self.show_roi_manager = open;
        if let Some(i) = select {
            self.selected_roi = Some(i);
            self.active_handle = 0;
            self.show_roi_info = true;
        }
        if let Some(i) = delete {
            if let Some(list) = self.rois.get_mut(&key) {
                if i < list.len() {
                    list.remove(i);
                    self.dirty_rois.insert(key);
                }
            }
            self.selected_roi = None;
        }
        if delete_all {
            self.rois.remove(&key);
            self.selected_roi = None;
            self.dirty_rois.insert(key);
        }
    }

    /// ROI Info: the selected measurement's name, value and pixel
    /// statistics (Horos's ROI Info window).
    fn roi_info_window(&mut self, ctx: &egui::Context, shared: &Shared) {
        if !self.show_roi_info {
            return;
        }
        let key = self.current_key();
        let frame = key.and_then(|k| self.frames.get(&k).cloned());
        let locked = self.active_locked();
        let selected = key
            .zip(self.selected_roi)
            .and_then(|(k, i)| self.rois.get(&k).and_then(|l| l.get(i)).cloned());
        let mut open = true;
        let mut new_name: Option<String> = None;
        let mut delete = false;
        let mut histogram = false;
        egui::Window::new("ROI Info")
            .id(egui::Id::unique(("ruthos-roi-info", self.id)))
            .open(&mut open)
            .default_width(300.0)
            .default_pos([620.0, 90.0])
            .resizable(false)
            .show(ctx, |ui| {
                let (Some(roi), Some(frame)) = (&selected, &frame) else {
                    ui.label("Select a measurement to see its details.");
                    return;
                };
                egui::Grid::new("roi-info-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Type");
                        ui.label(kind_label(&roi.kind));
                        ui.end_row();
                        ui.label(if matches!(roi.kind, RoiKind::Text { .. }) {
                            "Text"
                        } else {
                            "Name"
                        });
                        let mut name = roi.name.clone().unwrap_or_default();
                        let edit = ui.add_enabled(!locked, egui::TextEdit::singleline(&mut name).desired_width(180.0));
                        if edit.changed() {
                            new_name = Some(name);
                        }
                        ui.end_row();
                        let value = roi.value_text(frame);
                        if !value.is_empty() {
                            ui.label("Value");
                            ui.label(value);
                            ui.end_row();
                        }
                        if let Some(st) = roi.stats(frame) {
                            ui.label("Pixels");
                            ui.label(st.count.to_string());
                            ui.end_row();
                            ui.label("Mean");
                            ui.label(format!("{:.2}", st.mean));
                            ui.end_row();
                            ui.label("SD");
                            ui.label(format!("{:.2}", st.sd));
                            ui.end_row();
                            ui.label("Min / Max");
                            ui.label(format!("{:.0} / {:.0}", st.min, st.max));
                            ui.end_row();
                            if let Some(a) = st.area_mm2 {
                                ui.label("Area");
                                ui.label(if a >= 100.0 {
                                    format!("{:.2} cm\u{b2}", a / 100.0)
                                } else {
                                    format!("{a:.1} mm\u{b2}")
                                });
                                ui.end_row();
                            }
                        }
                        if let Some(per) = perimeter_mm(roi, frame) {
                            ui.label("Perimeter");
                            ui.label(if per >= 10.0 {
                                format!("{:.2} cm", per / 10.0)
                            } else {
                                format!("{per:.1} mm")
                            });
                            ui.end_row();
                        }
                        let m = roi.measure(frame);
                        if let Some(b) = m.basis {
                            ui.label("Scale");
                            ui.label(b.label());
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!locked, egui::Button::new("Delete")).clicked() {
                        delete = true;
                    }
                    let closed = roi.area_px().is_some()
                        || matches!(roi.kind, RoiKind::Length { .. } | RoiKind::Polyline { .. });
                    if ui
                        .add_enabled(closed && !frame.is_color(), egui::Button::new("Histogram and plot"))
                        .on_hover_text("Pixel values inside an area, or along a line")
                        .clicked()
                    {
                        histogram = true;
                    }
                });
            });
        self.show_roi_info = open;
        let (Some(key), Some(sel)) = (key, self.selected_roi) else {
            return;
        };
        if let Some(name) = new_name {
            if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(sel)) {
                let name = name.trim().to_string();
                if !matches!(roi.kind, RoiKind::Text { .. }) {
                    // A name typed by hand is not a library definition.
                    roi.def_key = None;
                    roi.quantity = None;
                }
                roi.name = (!name.is_empty()).then_some(name);
                self.dirty_rois.insert(key);
            }
        }
        if delete {
            if let Some(list) = self.rois.get_mut(&key) {
                if sel < list.len() {
                    list.remove(sel);
                    self.dirty_rois.insert(key);
                }
            }
            self.selected_roi = None;
        }
        if histogram {
            self.open_histogram(shared, key, sel);
        }
    }

    // ------------------------------------------------------------ the dial

    /// Developer options: `RUTHOS_CINE_PLAY=1` starts cine once an image
    /// is up, `RUTHOS_CINE_FPS=<n>` sets the rate for files without one,
    /// `RUTHOS_CINE_SPEED=0.5` the speed, and
    /// `RUTHOS_VIEWER_SCREENSHOT=<file.png>` saves this window as drawn
    /// four seconds after it opened.
    fn dev_options(&mut self, ui: &Ui) {
        if !self.dev_play_done && self.current_key().is_some_and(|k| self.frames.contains_key(&k)) {
            self.dev_play_done = true;
            if let Some(f) = std::env::var("RUTHOS_CINE_FPS").ok().and_then(|v| v.parse().ok()) {
                self.fps = f;
            }
            if let Some(s) = std::env::var("RUTHOS_CINE_SPEED").ok().and_then(|v| v.parse().ok()) {
                self.speed = s;
            }
            if std::env::var_os("RUTHOS_CINE_PLAY").is_some() {
                self.playing = true;
            }
        }
        // `RUTHOS_MENU_PREVIEW=1` draws the Measurements menu in a window, and
        // with a measurement selected opens the naming popup beside it.
        if std::env::var_os("RUTHOS_MENU_PREVIEW").is_some() && self.current_key().is_some() {
            let ctx = ui.ctx().clone();
            egui::Window::new("Measurements menu (preview)")
                .id(egui::Id::unique(("menu-preview", self.id)))
                .default_pos([260.0, 140.0])
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.set_width(240.0);
                    self.measurements_menu(ui, false);
                });
            if self.assign.is_none() && self.selected_roi.is_some() && !self.dev_shot_done {
                if let (Some(key), Some(index)) = (self.current_key(), self.selected_roi) {
                    self.assign = Some(AssignPopup {
                        key,
                        index,
                        pos: Pos2::new(620.0, 140.0),
                        custom: String::new(),
                    });
                }
            }
        }
        if !self.dev_shot_done && self.opened_at.elapsed() > std::time::Duration::from_secs(4) {
            if let Some(path) = std::env::var_os("RUTHOS_VIEWER_SCREENSHOT") {
                self.dev_shot_done = true;
                ui.ctx().request_screenshot(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(png) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                        let _ = png.save(&path);
                    }
                });
            } else {
                self.dev_shot_done = true;
            }
        }
        if !self.dev_shot_done {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// What the dial can act on in the active cell.
    fn dial_context(&self) -> crate::dial::Context {
        // An image still loading counts as the series' other images do.
        let greyscale = match self.current_key().and_then(|k| self.frames.get(&k)) {
            Some(f) => !f.is_color(),
            None => {
                let ids: std::collections::HashSet<i64> = self.panes[self.active].images.iter().map(|i| i.id).collect();
                self.frames
                    .iter()
                    .find(|(k, _)| ids.contains(&k.image_id))
                    .is_none_or(|(_, f)| !f.is_color())
            }
        };
        crate::dial::Context {
            greyscale,
            measurement_selected: self.selected_roi.is_some()
                && self
                    .current_key()
                    .and_then(|k| self.rois.get(&k))
                    .is_some_and(|l| self.selected_roi.is_some_and(|i| i < l.len())),
            loupe_enabled: self.loupe.enabled,
        }
    }

    /// Dial events and the dial's keyboard shortcuts, applied to the cell
    /// under the pointer (or the active one).
    fn dial_input(&mut self, ui: &mut Ui, shared: &Shared) {
        use crate::dial::{DialEvent, DialMode};
        let settings = shared.settings.lock().dial.clone();
        let focused = ui.input(|i| i.viewport().focused.unwrap_or(false));
        let mut events: Vec<DialEvent> = Vec::new();
        if settings.enabled {
            // This window gets a controller as soon as it is in front; the
            // app routes the events here.
            if focused {
                shared.dial.lock().pump(&ui.ctx().clone(), &settings);
            }
            events.append(&mut self.dial_inbox);
        } else {
            self.dial_inbox.clear();
        }
        // The shortcuts: each names its mode, so it works whatever the
        // dial's mode is. Taken before the plain arrows and tool letters
        // see them.
        let mut keyed: Vec<(DialMode, i64)> = Vec::new();
        let mut next_mode = false;
        if !ui.ctx().memory(|m| m.focused().is_some()) {
            use egui::Modifiers;
            let cs = Modifiers::CTRL | Modifiers::SHIFT;
            ui.input_mut(|i| {
                for (m, k, d) in [
                    (DialMode::Navigate, Key::ArrowLeft, -1),
                    (DialMode::Navigate, Key::ArrowRight, 1),
                    (DialMode::WindowWidth, Key::ArrowDown, -1),
                    (DialMode::WindowWidth, Key::ArrowUp, 1),
                    (DialMode::WindowLevel, Key::PageDown, -1),
                    (DialMode::WindowLevel, Key::PageUp, 1),
                    (DialMode::LoupeMagnification, Key::Minus, -1),
                    (DialMode::LoupeMagnification, Key::Plus, 1),
                    (DialMode::LoupeMagnification, Key::Equals, 1),
                ] {
                    while i.consume_key(cs, k) {
                        keyed.push((m, d));
                    }
                }
                for (k, d) in [(Key::ArrowLeft, -1), (Key::ArrowRight, 1)] {
                    while i.consume_key(Modifiers::ALT, k) {
                        keyed.push((DialMode::MeasurementPoint, d));
                    }
                }
                if i.consume_key(cs, Key::M) {
                    next_mode = true;
                }
            });
        }
        if next_mode {
            events.push(DialEvent::Press);
        }
        // Developer option: `RUTHOS_DIAL_EVENTS=rot:10,rot:10,press,rot:-10`
        // plays dial events once the first image is on screen, and keeps the
        // indicator up for 20 s so it can be captured.
        let mut scripted = false;
        if !self.dial_script_done && self.current_key().is_some_and(|k| self.frames.contains_key(&k)) {
            self.dial_script_done = true;
            if let Ok(script) = std::env::var("RUTHOS_DIAL_EVENTS") {
                for part in script.split(',').map(str::trim) {
                    match part.split_once(':') {
                        Some(("rot", d)) => {
                            if let Ok(d) = d.parse::<f64>() {
                                events.push(DialEvent::Rotate(d));
                            }
                        }
                        _ if part == "press" => events.push(DialEvent::Press),
                        _ => {}
                    }
                }
                scripted = true;
            }
        }
        if events.is_empty() && keyed.is_empty() {
            self.request_indicator_repaint(ui, &settings);
            return;
        }
        // The dial acts on the cell under the pointer, like the wheel.
        if let Some(k) = self.hover_cell.filter(|k| *k < self.panes.len()) {
            if k != self.active {
                self.active = k;
            }
        }
        let now = Instant::now();
        for ev in events {
            match ev {
                DialEvent::Press => {
                    let cx = self.dial_context();
                    let mode = self.dial.next_mode(&settings, cx);
                    // The dial's own menu shows the same mode.
                    shared.dial.lock().select(mode);
                }
                DialEvent::Choose(m) => self.dial.mode = m,
                DialEvent::Rotate(deg) => {
                    let n = self.dial.steps(deg, now, &settings);
                    if n != 0 {
                        let mode = self.dial.mode;
                        self.dial_step(mode, n);
                    }
                }
            }
            self.dial.shown = Some(now);
        }
        for (mode, n) in keyed {
            self.dial.mode = mode;
            self.dial_step(mode, n);
            self.dial.shown = Some(now);
        }
        if scripted {
            self.dial.shown = Some(now + std::time::Duration::from_secs(20));
        }
        ui.ctx().request_repaint();
    }

    fn request_indicator_repaint(&self, ui: &Ui, settings: &crate::dial::DialSettings) {
        if settings.show_indicator && self.dial.shown.is_some_and(|t| t.elapsed() < crate::dial::INDICATOR) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// One or more detents in a mode. A mode that cannot act does nothing.
    fn dial_step(&mut self, mode: crate::dial::DialMode, n: i64) {
        use crate::dial::DialMode;
        if !crate::dial::available(mode, self.dial_context()) {
            return;
        }
        match mode {
            DialMode::Navigate => {
                // Reaching for the dial means stopping on a frame.
                self.playing = false;
                if self.active_locked() {
                    return;
                }
                if self.tiled_series() {
                    self.step_block(n);
                    return;
                }
                let p = &mut self.panes[self.active];
                if p.images.is_empty() {
                    return;
                }
                let idx = p.image_idx.min(p.images.len() - 1);
                let frames = p.images[idx].number_of_frames.max(1) as usize;
                if frames > 1 {
                    p.frame_idx = crate::dial::clamp_step(p.frame_idx as usize, n, frames) as u32;
                } else {
                    let next = crate::dial::clamp_step(idx, n, p.images.len());
                    if next != idx {
                        p.image_idx = next;
                        p.frame_idx = 0;
                        self.selected_roi = None;
                    }
                }
            }
            DialMode::WindowWidth | DialMode::WindowLevel => {
                if self.active_locked() {
                    return;
                }
                // The image's own frame, or one of its series while it loads.
                let ids: std::collections::HashSet<i64> = self.panes[self.active].images.iter().map(|i| i.id).collect();
                let f = match self.current_key().and_then(|k| self.frames.get(&k)) {
                    Some(f) => f,
                    None => match self.frames.iter().find(|(k, _)| ids.contains(&k.image_id)) {
                        Some((_, f)) => f,
                        None => return,
                    },
                };
                let range = crate::dial::stored_range(f.meta.bits_stored, f.meta.rescale_slope, f.min, f.max);
                let view = &mut self.panes[self.active].view;
                if mode == DialMode::WindowWidth {
                    view.window_width = (view.window_width + crate::dial::width_step(range) * n as f32).max(1.0);
                } else {
                    view.window_center += crate::dial::level_step(range) * n as f32;
                }
            }
            DialMode::MeasurementPoint => {
                let (Some(key), Some(sel)) = (self.current_key(), self.selected_roi) else {
                    return;
                };
                let Some(roi) = self.rois.get(&key).and_then(|l| l.get(sel)) else {
                    return;
                };
                let handles = roi.handles();
                if handles.is_empty() {
                    return;
                }
                let h = self.active_handle.min(handles.len() - 1);
                let before = roi.kind.clone();
                let axis = crate::dial::point_axis(&roi.kind, h);
                let target = handles[h] + axis * (crate::dial::POINT_STEP * n as f32);
                // A run of detents is one undo step.
                let fresh = self
                    .dial
                    .last_point_edit
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(1));
                if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(sel)) {
                    roi.set_handle(h, target);
                }
                if fresh {
                    self.push_undo(key, sel, before);
                }
                self.dial.last_point_edit = Some(Instant::now());
                self.dirty_rois.insert(key);
            }
            DialMode::LoupeMagnification => {
                self.loupe.magnification =
                    crate::dial::next_magnification(&crate::loupe::MAGNIFICATIONS, self.loupe.magnification, n);
            }
        }
    }

    /// The indicator's text: the mode and its current value.
    fn dial_text(&self) -> String {
        use crate::dial::DialMode;
        let p = &self.panes[self.active];
        match self.dial.mode {
            DialMode::Navigate => match p.image() {
                Some(img) if img.number_of_frames > 1 => {
                    format!("Frame {} of {}", p.frame_idx + 1, img.number_of_frames)
                }
                _ => format!("Image {} of {}", p.image_idx + 1, p.images.len()),
            },
            DialMode::WindowWidth => format!("Width {:.0}", p.view.window_width),
            DialMode::WindowLevel => format!("Level {:.0}", p.view.window_center),
            DialMode::MeasurementPoint => {
                let key = self.current_key();
                let roi = key
                    .zip(self.selected_roi)
                    .and_then(|(k, i)| self.rois.get(&k).and_then(|l| l.get(i)));
                let frame = key.and_then(|k| self.frames.get(&k));
                match (roi, frame) {
                    (Some(r), Some(f)) => {
                        let name = r.name.clone().unwrap_or_else(|| kind_label(&r.kind).to_string());
                        let value = r.value_text(f).replace('\n', "  ");
                        if value.is_empty() {
                            format!("{name} point")
                        } else {
                            format!("{name} point, {value}")
                        }
                    }
                    _ => "Measurement point: select a measurement".into(),
                }
            }
            DialMode::LoupeMagnification => format!("Loupe {}\u{d7}", self.loupe.magnification as u32),
        }
    }

    /// The mode indicator at the top centre of the active cell, fading 1.5 s
    /// after the last press or turn.
    fn draw_dial_indicator(&self, ui: &Ui, shared: &Shared) {
        if !shared.settings.lock().dial.show_indicator {
            return;
        }
        let Some(shown) = self.dial.shown else { return };
        let age = shown.elapsed();
        if age >= crate::dial::INDICATOR {
            return;
        }
        let Some(rect) = self.cell_rects.get(self.active) else {
            return;
        };
        let fade = 1.0 - (age.as_secs_f32() - 1.2).max(0.0) / 0.3;
        let text = format!("{}   {}", self.dial.mode.label(), self.dial_text());
        // On the foreground layer, so the image drawn later sits beneath it.
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique(("ruthos-dial-indicator", self.id)),
            ))
            .with_clip_rect(*rect);
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(14.0), Color32::WHITE);
        let size = galley.size() + Vec2::new(20.0, 10.0);
        let r = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 14.0 + size.y / 2.0), size);
        painter.rect_filled(r, 6.0, Color32::from_black_alpha((180.0 * fade) as u8));
        painter.galley(
            r.min + Vec2::new(10.0, 5.0),
            galley,
            Color32::from_white_alpha((255.0 * fade) as u8),
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }

    /// The kind of study: chosen in the Measurements menu, or read from the
    /// series and study (cardiac for an echo, abdominal, musculoskeletal).
    fn region(&self) -> ruthos_core::measure::Region {
        if let Some(r) = self.region_override {
            return r;
        }
        let s = self.current_series();
        ruthos_core::measure::infer_region(
            &s.modality,
            &[
                &s.series_description,
                &s.body_part_examined,
                &self.study.study_description,
            ],
        )
    }

    /// The sets offered in the current context, by index into the library.
    fn sets_in_context(&self) -> Vec<usize> {
        let region = self.region();
        (0..self.library.sets.len())
            .filter(|i| self.library.sets[*i].applies_to(region))
            .collect()
    }

    /// Whether there is anything to make a movie of: a loop, or a series
    /// of more than one image.
    fn can_export_movie(&self) -> bool {
        let p = &self.panes[self.active];
        p.images.len() > 1 || self.current_image().is_some_and(|i| i.number_of_frames > 1)
    }

    /// Export the current loop (a multi-frame image) or, for single-frame
    /// images, the series, as an MP4 at the cine speed, with the current
    /// window and colour table. `to` skips the save dialog. Pressed again
    /// while writing, it stops.
    fn export_movie(&mut self, ctx: &egui::Context, shared: &Shared, to: Option<std::path::PathBuf>) {
        if let Some(job) = &self.movie {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if self.encoder_prompt.is_some() {
            return;
        }
        let root = shared.paths.root.clone();
        if !ruthos_core::movie::encoder_installed(&root) {
            self.encoder_prompt = Some(EncoderPrompt {
                to,
                fetching: None,
                error: None,
            });
            return;
        }
        let Some(encoder) = ruthos_core::movie::encoder_path(&root) else {
            return;
        };
        let Some(img) = self.current_image().cloned() else {
            return;
        };
        // The movie plays at the rate cine shows: the file's times the speed.
        let movie_rate = self.cine_rate(self.active);
        let p = &self.panes[self.active];
        let (loop_only, files): (bool, Vec<std::path::PathBuf>) = if img.number_of_frames > 1 {
            (true, vec![shared.resolve(&img.path)])
        } else {
            (false, p.images.iter().map(|i| shared.resolve(&i.path)).collect())
        };
        let default_name = format!(
            "{}_{}{}.mp4",
            self.study.patient_name.replace([',', ' ', '^'], "_"),
            self.current_series().display_name().replace(' ', "_"),
            if loop_only {
                format!("_{}", p.image_idx + 1)
            } else {
                String::new()
            }
        );
        let path = match to {
            Some(p) => p,
            None => {
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Export movie")
                    .add_filter("MP4 movie", &["mp4"])
                    .set_file_name(default_name)
                    .save_file()
                else {
                    return;
                };
                path
            }
        };
        let clut = shared
            .cluts
            .get(&p.clut)
            .cloned()
            .unwrap_or_else(ruthos_dicom::Clut::grayscale);
        let (center, width) = (p.view.window_center, p.view.window_width);
        let fps = movie_rate;
        let progress = Arc::new(parking_lot::Mutex::new((0usize, 0usize)));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (prog, stop, out) = (progress.clone(), cancel.clone(), path.clone());
        let repaint = ctx.clone();
        let task = crate::pacs::Task::spawn(ctx, "ruthos-movie", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ruthos_core::movie::export_dicom_movie(
                    &encoder,
                    &out,
                    &ruthos_core::movie::MovieSource { files },
                    center,
                    width,
                    &clut,
                    fps,
                    &|done, total| {
                        *prog.lock() = (done, total);
                        repaint.request_repaint();
                    },
                    &stop,
                )
                .map(|()| out.clone())
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("an internal error stopped the movie; see crash.log".into()))
        });
        self.movie = Some(MovieJob { task, progress, cancel });
        self.set_status("Writing movie…");
    }

    fn open_histogram(&mut self, _shared: &Shared, _key: FrameKey, index: usize) {
        self.selected_roi = Some(index);
        if self.histogram.is_none() {
            self.histogram = Some(Default::default());
        }
    }

    /// Histogram and plot: the selected measurement's values, or the whole
    /// image's when nothing is selected.
    fn histogram_window(&mut self, ctx: &egui::Context) {
        let Some(mut h) = self.histogram.take() else { return };
        let Some(key) = self.current_key() else {
            self.histogram = Some(h);
            return;
        };
        let Some(frame) = self.frames.get(&key).cloned() else {
            self.histogram = Some(h);
            return;
        };
        let roi = self
            .selected_roi
            .and_then(|i| self.rois.get(&key).and_then(|l| l.get(i)))
            .cloned();
        h.update(key, &frame, roi.as_ref());
        let title = match (&roi, h.data()) {
            (_, crate::histogram::PlotData::Profile(_)) => "Plot along the line".to_string(),
            (Some(r), _) => format!(
                "Histogram: {}",
                r.name.clone().unwrap_or_else(|| kind_label(&r.kind).to_string())
            ),
            (None, _) => "Histogram: whole image".to_string(),
        };
        let mut open = true;
        egui::Window::new("Histogram and plot")
            .id(egui::Id::unique(("ruthos-histogram", self.id)))
            .open(&mut open)
            .resizable(false)
            .default_pos([40.0, 290.0])
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(title).strong());
                h.ui(ui);
                ui.horizontal(|ui| {
                    if ui.button("Copy values").clicked() {
                        ui.ctx().copy_text(h.csv());
                    }
                    ui.label(
                        egui::RichText::new(
                            "Select an area or a line to plot it; nothing selected shows the whole image.",
                        )
                        .size(11.5)
                        .color(Color32::from_gray(110)),
                    );
                });
            });
        if open {
            self.histogram = Some(h);
        }
    }

    /// Progress and the end of a movie export.
    /// Offer Cisco's encoder, with its licence, the first time a movie is made.
    fn encoder_window(&mut self, ctx: &egui::Context, shared: &Shared) {
        let Some(prompt) = self.encoder_prompt.as_mut() else {
            return;
        };
        if let Some(done) = prompt.fetching.as_ref().and_then(|t| t.take()) {
            prompt.fetching = None;
            match done {
                Ok(_) => {
                    let to = prompt.to.take();
                    self.encoder_prompt = None;
                    self.export_movie(ctx, shared, to);
                    return;
                }
                Err(e) => prompt.error = Some(e),
            }
        }
        let (mut cancel, mut fetch) = (false, false);
        egui::Window::new("Movie export needs an encoder")
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(
                    "Movies are written as H.264, which is covered by patents. Cisco offers its encoder free of \
                     those royalties, but only as its own binary, so Ruthos fetches it from Cisco rather than \
                     including it.",
                );
                ui.add_space(4.0);
                ui.label(egui::RichText::new("OpenH264 Video Codec provided by Cisco Systems, Inc.").strong());
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(ruthos_core::movie::CISCO_LICENCE).small());
                });
                if let Some(e) = &prompt.error {
                    ui.colored_label(Color32::from_rgb(240, 150, 120), e);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if prompt.fetching.is_some() {
                        ui.spinner();
                        ui.label("Fetching from Cisco\u{2026}");
                    } else {
                        fetch = ui.button("Agree and download from Cisco").clicked();
                        cancel = ui.button("Cancel").clicked();
                    }
                });
            });
        if fetch {
            let root = shared.paths.root.clone();
            prompt.error = None;
            prompt.fetching = Some(crate::pacs::Task::spawn(ctx, "ruthos-encoder", move || {
                ruthos_core::movie::install_encoder(&root).map_err(|e| e.to_string())
            }));
        }
        if cancel {
            self.encoder_prompt = None;
        }
    }

    fn pump_movie(&mut self) {
        let Some(job) = &self.movie else { return };
        if let Some(result) = job.task.take() {
            self.movie = None;
            match result {
                Ok(path) => self.set_status(format!("Movie written to {}", path.display())),
                Err(e) if e == "cancelled" => self.set_status("Movie cancelled"),
                Err(e) => self.set_status(format!("Movie not written: {e}")),
            }
            return;
        }
        let (done, total) = *job.progress.lock();
        if total > 0 {
            self.set_status(format!("Writing movie: frame {done} of {total}"));
        }
    }

    // ------------------------------------------------------------- rois

    /// Read stored ROIs for a frame the first time it is shown.
    fn ensure_rois_loaded(&mut self, shared: &Shared, key: FrameKey) {
        if !self.rois_loaded.insert(key) {
            return;
        }
        if let Ok(records) = shared.db.rois(key.image_id, key.frame as i64) {
            let list: Vec<Roi> = records
                .iter()
                .filter_map(Roi::from_record)
                .map(|mut r| {
                    r.resolve(&self.library);
                    r
                })
                .collect();
            if !list.is_empty() {
                self.rois.entry(key).or_default().extend(list);
            }
        }
    }

    /// Write back every frame whose ROIs changed this frame.
    fn flush_rois(&mut self, shared: &Shared) {
        for key in std::mem::take(&mut self.dirty_rois) {
            let frame = self.frames.get(&key).cloned();
            let mut records: Vec<ruthos_core::RoiRecord> = self
                .rois
                .get(&key)
                .map(|list| {
                    list.iter()
                        .map(|r| r.to_record(key.image_id, key.frame, frame.as_deref()))
                        .collect()
                })
                .unwrap_or_default();
            match shared.db.sync_rois(key.image_id, key.frame as i64, &mut records) {
                Ok(()) => {
                    // New rows now have ids; keep them so edits update in place.
                    let mut mmode_values = Vec::new();
                    if let Some(list) = self.rois.get_mut(&key) {
                        for (roi, rec) in list.iter_mut().zip(records.iter()) {
                            roi.id = Some(rec.id);
                            if let (RoiKind::Lv { phase, .. }, Some(f)) = (&roi.kind, frame.as_deref()) {
                                let values: Vec<ruthos_core::RoiValue> = match roi.mmode_values(f) {
                                    Ok((v, basis)) => phase
                                        .keys()
                                        .iter()
                                        .zip(v)
                                        .map(|(k, mm)| ruthos_core::RoiValue {
                                            def_key: (*k).into(),
                                            value: Some(mm),
                                            unit: "mm".into(),
                                            basis: Some(basis.key().into()),
                                        })
                                        .collect(),
                                    Err(_) => Vec::new(),
                                };
                                mmode_values.push((rec.id, values));
                            }
                        }
                    }
                    for (id, values) in mmode_values {
                        if let Err(e) = shared.db.set_roi_values(id, &values) {
                            tracing::warn!("could not store M-mode values: {e}");
                        }
                        if let Some(sid) = self.series_id_of_image(key.image_id) {
                            self.mmode_refresh.insert(sid);
                        }
                    }
                }
                Err(e) => tracing::warn!("could not store ROIs: {e}"),
            }
        }
    }

    fn current_series_id(&self) -> Option<i64> {
        self.series.get(self.panes[self.active].series_idx).map(|s| s.id)
    }

    fn series_id_of_image(&self, image_id: i64) -> Option<i64> {
        self.panes
            .iter()
            .flat_map(|p| p.images.iter())
            .find(|i| i.id == image_id)
            .map(|i| i.series_id)
    }

    /// Re-read the latest M-mode lines of a series from the archive.
    fn refresh_mmode_latest(&mut self, shared: &Shared, series_id: i64) {
        let rows = shared.db.mmode_rows_for_series(series_id).unwrap_or_default();
        let (d, s) = ruthos_core::mmode::latest_per_series(&rows, series_id);
        let latest = MmodeLatest {
            diastole: d.and_then(|d| d.value("lv.lvidd").map(|v| (d.roi_id, d.date_modified, v))),
            systole: s.and_then(|s| s.value("lv.lvids").map(|v| (s.roi_id, s.date_modified, v))),
        };
        self.mmode_latest.insert(series_id, latest);
        self.mmode_rows.insert(series_id, rows);
    }

    /// The M-mode lines of a frame as (index in the frame's list, phase,
    /// time column), for pairing into cycles.
    fn mmode_lines(&self, key: &FrameKey) -> Vec<(usize, char, f32)> {
        self.rois
            .get(key)
            .map(|l| {
                l.iter()
                    .enumerate()
                    .filter_map(|(i, r)| match &r.kind {
                        RoiKind::Lv { phase, mmode: true, p } => {
                            Some((i, if *phase == MmodePhase::Systole { 's' } else { 'd' }, p[0].x))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The cycles on a frame: (diastole, systole) as indices into the
    /// frame's measurement list, in time order.
    fn mmode_sets(&self, key: &FrameKey) -> Vec<(Option<usize>, Option<usize>)> {
        let lines = self.mmode_lines(key);
        let keys: Vec<(char, f32)> = lines.iter().map(|(_, p, x)| (*p, *x)).collect();
        ruthos_core::mmode::pair_on_frame(&keys)
            .into_iter()
            .map(|(d, s)| (d.map(|i| lines[i].0), s.map(|i| lines[i].0)))
            .collect()
    }

    /// The phase a new M-mode line at time column `x` takes. On a frame
    /// that already has lines: systole when a diastole to its left is still
    /// unpaired, else diastole, so cycle after cycle alternates. On a frame
    /// without lines: systole when the series' latest line is a diastole
    /// without a newer systole, else diastole.
    fn next_mmode_phase_at(&self, key: &FrameKey, x: f32) -> MmodePhase {
        let lines = self.mmode_lines(key);
        if !lines.is_empty() {
            let open_diastole_left = self.mmode_sets(key).iter().any(|(d, s)| {
                s.is_none()
                    && d.and_then(|d| lines.iter().find(|(i, _, _)| *i == d))
                        .is_some_and(|(_, _, dx)| *dx <= x)
            });
            return if open_diastole_left {
                MmodePhase::Systole
            } else {
                MmodePhase::Diastole
            };
        }
        self.next_mmode_phase()
    }

    /// The phase a new M-mode line takes from the series alone.
    fn next_mmode_phase(&self) -> MmodePhase {
        let Some(sid) = self.current_series_id() else {
            return MmodePhase::Diastole;
        };
        match self.mmode_latest.get(&sid) {
            Some(MmodeLatest {
                diastole: Some((_, dt, _)),
                systole,
            }) => match systole {
                Some((_, st, _)) if st >= dt => MmodePhase::Diastole,
                _ => MmodePhase::Systole,
            },
            _ => MmodePhase::Diastole,
        }
    }

    /// Fractional shortening for a systolic line: its LVIDs against the
    /// diastole of its own cycle on the same frame, or the series' latest
    /// diastole when the frame has none.
    fn fractional_shortening(&self, key: &FrameKey, roi: &Roi, frame: &Frame) -> Option<f64> {
        let (v, _) = roi.mmode_values(frame).ok()?;
        let lvids = v[1];
        // A 2D line: its diastole is on another frame of the loop, so the
        // pairing comes from the stored lines of the series.
        if let (RoiKind::Lv { mmode: false, .. }, Some(id)) = (&roi.kind, roi.id) {
            let sid = self.series_id_of_image(key.image_id)?;
            let rows = self.mmode_rows.get(&sid)?.clone();
            return ruthos_core::mmode::sets_from_rows(rows)
                .into_iter()
                .find(|s| s.systole.as_ref().is_some_and(|r| r.roi_id == id))
                .and_then(|s| s.fs_percent);
        }
        // The diastole paired with this systole in its cycle on this frame.
        let list = self.rois.get(key)?;
        let index = list.iter().position(|r| std::ptr::eq(r, roi))?;
        let same_frame = self
            .mmode_sets(key)
            .into_iter()
            .find(|(_, s)| *s == Some(index))
            .and_then(|(d, _)| d)
            .and_then(|d| list.get(d))
            .and_then(|r| r.mmode_values(frame).ok())
            .map(|(v, _)| v[1]);
        let lvidd = same_frame.or_else(|| {
            self.series_id_of_image(key.image_id)
                .and_then(|sid| self.mmode_latest.get(&sid))
                .and_then(|l| l.diastole.map(|d| d.2))
        })?;
        ruthos_core::mmode::fractional_shortening(lvidd, lvids)
    }

    /// Record a measurement's geometry before an edit so Ctrl+Z can restore it.
    fn push_undo(&mut self, key: FrameKey, index: usize, kind: RoiKind) {
        let id = self.rois.get(&key).and_then(|l| l.get(index)).and_then(|r| r.id);
        self.undo.push(UndoEntry { key, id, index, kind });
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }

    /// Undo the last edit to any measurement in this viewer.
    fn undo_edit(&mut self) {
        let Some(entry) = self.undo.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        if let Some(list) = self.rois.get_mut(&entry.key) {
            let idx = entry
                .id
                .and_then(|id| list.iter().position(|r| r.id == Some(id)))
                .or(Some(entry.index).filter(|i| *i < list.len()));
            if let Some(i) = idx {
                list[i].kind = entry.kind;
                self.dirty_rois.insert(entry.key);
                self.set_status("Measurement edit undone");
                return;
            }
        }
        self.set_status("The edited measurement is no longer there");
    }

    /// Handle and whole-measurement drags on the selected measurement. They
    /// take priority over every tool, so a crowded image does not steal the
    /// drag. Returns true while such a drag is in progress.
    fn edit_input(&mut self, ui: &Ui, response: &egui::Response, t: &ViewTransform, key: &FrameKey) -> bool {
        let (fine_enabled, magnification) = (self.loupe.fine_movement, self.loupe.magnification);
        let pos = response.interact_pointer_pos();
        if response.drag_started_by(PointerButton::Primary) {
            // A drag only starts once the pointer has moved a few pixels, so
            // the hit test uses the point where the press began.
            if let Some(p) = press_origin(ui).or(pos) {
                // A handle of the selected measurement first, then a handle
                // of any other measurement on the frame.
                let order: Vec<usize> = {
                    let n = self.rois.get(key).map(|l| l.len()).unwrap_or(0);
                    self.selected_roi
                        .filter(|s| *s < n)
                        .into_iter()
                        .chain((0..n).filter(|i| Some(*i) != self.selected_roi))
                        .collect()
                };
                for sel in order {
                    if let Some(roi) = self.rois.get(key).and_then(|l| l.get(sel)) {
                        if let Some(h) = roi.handle_hit(t, p) {
                            self.handle_drag = Some(HandleDrag {
                                key: *key,
                                index: sel,
                                handle: h,
                                original: roi.kind.clone(),
                                fine: None,
                            });
                            self.selected_roi = Some(sel);
                            self.active_handle = h;
                            return true;
                        }
                    }
                }
                // Then any measurement's outline, with a measuring tool: move it whole.
                if self.tool.is_roi() {
                    if let Some(list) = self.rois.get(key) {
                        let hit = list
                            .iter()
                            .enumerate()
                            .map(|(i, r)| (i, r.screen_distance(t, p)))
                            .filter(|(_, d)| *d < 8.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| i);
                        if let Some(i) = hit {
                            self.selected_roi = Some(i);
                            self.active_handle = 0;
                            self.move_drag = Some(MoveDrag {
                                key: *key,
                                index: i,
                                original: list[i].kind.clone(),
                                last: t.screen_to_image(p),
                            });
                            return true;
                        }
                    }
                }
            }
        }
        let escape = ui.input(|i| i.key_pressed(Key::Escape));
        if let Some(drag) = &mut self.handle_drag {
            if escape {
                if let Some(roi) = self.rois.get_mut(&drag.key).and_then(|l| l.get_mut(drag.index)) {
                    roi.kind = drag.original.clone();
                }
                self.handle_drag = None;
                return true;
            }
            if response.dragged_by(PointerButton::Primary) {
                if let Some(p) = pos {
                    let shift = ui.input(|i| i.modifiers.shift);
                    let target = if shift && fine_enabled {
                        // Fine positioning: pointer movement scaled by 1/magnification.
                        let delta = response.drag_delta() / magnification.max(1.0);
                        let base = drag.fine.unwrap_or_else(|| {
                            self.rois
                                .get(&drag.key)
                                .and_then(|l| l.get(drag.index))
                                .map(|r| r.handles()[drag.handle])
                                .unwrap_or_else(|| t.screen_to_image(p))
                        });
                        let moved = base + delta / t.state.zoom.max(1e-6);
                        drag.fine = Some(moved);
                        moved
                    } else {
                        drag.fine = None;
                        t.screen_to_image(p)
                    };
                    if let Some(roi) = self.rois.get_mut(&drag.key).and_then(|l| l.get_mut(drag.index)) {
                        roi.set_handle(drag.handle, target);
                    }
                    self.loupe_target = Some(LoupeTarget {
                        image_point: target,
                        pointer: p,
                    });
                }
            }
            if response.drag_stopped_by(PointerButton::Primary) {
                let d = self.handle_drag.take().unwrap();
                self.push_undo(d.key, d.index, d.original);
                self.dirty_rois.insert(d.key);
            }
            return true;
        }
        if let Some(drag) = &mut self.move_drag {
            if escape {
                if let Some(roi) = self.rois.get_mut(&drag.key).and_then(|l| l.get_mut(drag.index)) {
                    roi.kind = drag.original.clone();
                }
                self.move_drag = None;
                return true;
            }
            if response.dragged_by(PointerButton::Primary) {
                if let Some(p) = pos {
                    let now = t.screen_to_image(p);
                    let delta = now - drag.last;
                    drag.last = now;
                    if let Some(roi) = self.rois.get_mut(&drag.key).and_then(|l| l.get_mut(drag.index)) {
                        roi.translate(delta);
                    }
                }
            }
            if response.drag_stopped_by(PointerButton::Primary) {
                let d = self.move_drag.take().unwrap();
                self.push_undo(d.key, d.index, d.original);
                self.dirty_rois.insert(d.key);
            }
            return true;
        }
        false
    }

    /// Nudge the active handle of the selected measurement by image pixels.
    fn nudge_handle(&mut self, dx: f32, dy: f32) {
        let Some(key) = self.current_key() else { return };
        let Some(sel) = self.selected_roi else { return };
        let Some(roi) = self.rois.get(&key).and_then(|l| l.get(sel)) else {
            return;
        };
        let handles = roi.handles();
        if handles.is_empty() {
            return;
        }
        let h = self.active_handle.min(handles.len() - 1);
        let before = roi.kind.clone();
        let target = handles[h] + Vec2::new(dx, dy);
        if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(sel)) {
            roi.set_handle(h, target);
        }
        self.push_undo(key, sel, before);
        self.dirty_rois.insert(key);
    }

    /// The Measurements menu. The toolbar menu and the right-click menu are
    /// both drawn from here; `with_view` adds the view items the right-click
    /// menu carries.
    fn measurements_menu(&mut self, ui: &mut Ui, with_view: bool) {
        let key = self.current_key();
        let selected = key
            .and_then(|k| self.rois.get(&k))
            .and_then(|l| self.selected_roi.and_then(|i| l.get(i)));
        let has_selected = selected.is_some();
        let selected_is_length = matches!(selected.map(|r| &r.kind), Some(RoiKind::Length { .. }));
        let any_rois = key.and_then(|k| self.rois.get(&k)).is_some_and(|l| !l.is_empty());
        let has_hand = key
            .and_then(|k| self.frames.get(&k))
            .is_some_and(|f| f.meta.hand_calibration.is_some() || f.meta.user_calibrated)
            || self
                .current_image()
                .and_then(|i| {
                    ui.ctx()
                        .data(|d| d.get_temp::<bool>(egui::Id::unique(("has-cal", i.id))))
                })
                .unwrap_or(false);
        let locked = self.active_locked();
        let mut action: Option<MenuAction> = None;
        let mut pick = |ui: &mut Ui, a: MenuAction| {
            action = Some(a);
            ui.close();
        };

        // The context: which sets the naming menus offer.
        let region = self.region();
        let followed = self.region_override.is_none();
        ui.menu_button(format!("For: {}", region.label()), |ui| {
            if menu_item(ui, "Follow the study", followed).clicked() {
                pick(ui, MenuAction::SetRegion(None));
            }
            ui.separator();
            for r in ruthos_core::measure::Region::ALL {
                if menu_item(ui, r.label(), !followed && region == r).clicked() {
                    pick(ui, MenuAction::SetRegion(Some(r)));
                }
            }
        });
        ui.separator();

        // What the Measure tool draws.
        menu_heading(ui, "Measure with");
        for t in Tool::MEASURE {
            if menu_item_shortcut(ui, t.picker_label(), t.shortcut(), self.measure_tool == t, true).clicked() {
                pick(ui, MenuAction::SetTool(t));
            }
        }
        ui.separator();

        // Naming the selected measurement: the context's sets, one submenu each.
        let selected_phase = match selected.map(|r| &r.kind) {
            Some(RoiKind::Lv { phase, .. }) => Some(*phase),
            _ => None,
        };
        let in_context = self.sets_in_context();
        ui.add_enabled_ui(has_selected && !locked, |ui| {
            ui.menu_button("Name selected", |ui| {
                let current = selected.and_then(|r| r.name.clone());
                let current_key = selected.and_then(|r| r.def_key.clone());
                if let Some(cur) = selected_phase {
                    menu_heading(ui, "LV phase");
                    for phase in [MmodePhase::Diastole, MmodePhase::Systole] {
                        if menu_item(ui, phase.label(), cur == phase).clicked() {
                            pick(ui, MenuAction::SetPhase(phase));
                        }
                    }
                    ui.separator();
                }
                for &i in &in_context {
                    let set = &self.library.sets[i];
                    ui.menu_button(set.name.as_str(), |ui| {
                        for key in &set.keys {
                            let label = self.library.label(key);
                            let is_current = current_key.as_deref() == Some(key.as_str())
                                || (current_key.is_none() && current.as_deref() == Some(label.as_str()));
                            if menu_item(ui, label.as_str(), is_current).clicked() {
                                pick(ui, MenuAction::Name(label.clone(), Some(key.clone())));
                            }
                        }
                    });
                }
                ui.separator();
                if menu_item(ui, "Other name…", false).clicked() {
                    pick(ui, MenuAction::NamePopup);
                }
            });
        });
        ui.menu_button("Edit", |ui| {
            if menu_item_shortcut(
                ui,
                "Undo edit",
                &crate::platform::shortcut("Ctrl+Z"),
                false,
                !self.undo.is_empty() && !locked,
            )
            .clicked()
            {
                pick(ui, MenuAction::Undo);
            }
            if menu_item_shortcut(ui, "Delete selected", "Delete", false, has_selected && !locked).clicked() {
                pick(ui, MenuAction::DeleteSelected);
            }
            if menu_item_enabled(ui, "Delete all on this image", false, any_rois && !locked).clicked() {
                pick(ui, MenuAction::DeleteAll);
            }
        });
        ui.menu_button("Calibration", |ui| {
            if menu_item_enabled(ui, "Calibrate from selected length…", false, selected_is_length).clicked() {
                pick(ui, MenuAction::Calibrate);
            }
            if menu_item_enabled(ui, "Remove hand calibration", false, has_hand).clicked() {
                pick(ui, MenuAction::RemoveCalibration);
            }
            if menu_item(ui, "Show calibration basis", self.basis_panel).clicked() {
                pick(ui, MenuAction::ToggleBasisPanel);
            }
        });
        ui.menu_button("Windows", |ui| {
            if menu_item(ui, "ROI Manager", self.show_roi_manager).clicked() {
                pick(ui, MenuAction::ToggleRoiManager);
            }
            if menu_item(ui, "ROI Info", self.show_roi_info).clicked() {
                pick(ui, MenuAction::ToggleRoiInfo);
            }
            if menu_item(ui, "Histogram and plot", self.histogram.is_some()).clicked() {
                pick(ui, MenuAction::ToggleHistogram);
            }
        });
        ui.menu_button("Show", |ui| {
            if menu_item(ui, "Measurements", self.show_rois).clicked() {
                pick(ui, MenuAction::ToggleRois);
            }
            if menu_item(ui, "Labels", self.show_labels).clicked() {
                pick(ui, MenuAction::ToggleLabels);
            }
        });
        ui.menu_button("Report", |ui| {
            if menu_item_shortcut(ui, "Copy values", &crate::platform::shortcut("Ctrl+C"), false, any_rois).clicked() {
                pick(ui, MenuAction::CopyValues);
            }
            if menu_item_enabled(ui, "Add selected to report", false, has_selected).clicked() {
                pick(ui, MenuAction::AddToReport);
            }
            if menu_item(ui, "Open report", false).clicked() {
                pick(ui, MenuAction::OpenReport);
            }
        });
        if with_view {
            ui.separator();
            if menu_item_shortcut(ui, "Fit image to window", "F", false, true).clicked() {
                pick(ui, MenuAction::Fit);
            }
        }
        if action.is_some() {
            self.pending_menu = action;
        }
    }

    /// Carry out the Measurements menu choice made this frame.
    fn run_menu_action(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(action) = self.pending_menu.take() else { return };
        let key = self.current_key();
        match action {
            MenuAction::SetTool(t) => {
                self.tool = t;
                if t.is_measure() {
                    self.measure_tool = t;
                    // A right button that was measuring follows the choice.
                    if self.right_tool.is_measure() {
                        self.right_tool = t;
                    }
                }
                self.drag_draft = None;
                self.angle_draft = None;
                self.mmode_draft = None;
                self.polygon_draft = None;
                self.set_status(format!("Tool: {}", t.label()));
            }
            MenuAction::SetPhase(phase) => {
                if let (Some(key), Some(sel)) = (key, self.selected_roi) {
                    if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(sel)) {
                        if let RoiKind::Lv { phase: p, .. } = &mut roi.kind {
                            *p = phase;
                            self.dirty_rois.insert(key);
                            self.set_status(format!("LV line set to {}", phase.label().to_lowercase()));
                        }
                    }
                }
            }
            MenuAction::SetRegion(r) => {
                self.region_override = r;
                self.set_status(format!("Measurements for {}", self.region().label().to_lowercase()));
            }
            MenuAction::SetSet(i) => {
                self.current_set = i;
                if let Some(set) = self.library.sets.get(i) {
                    self.set_status(format!("Measurement set: {}", set.name));
                }
            }
            MenuAction::Name(name, def_key) => {
                if let (Some(key), Some(sel)) = (key, self.selected_roi) {
                    if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(sel)) {
                        roi.name = Some(name.clone());
                        roi.def_key = def_key;
                        roi.resolve(&self.library);
                        self.dirty_rois.insert(key);
                        self.set_status(format!("Named {name}"));
                    }
                }
            }
            MenuAction::NamePopup => self.open_name_popup(ui),
            MenuAction::Undo => self.undo_edit(),
            MenuAction::DeleteSelected => {
                if let (Some(key), Some(sel)) = (key, self.selected_roi) {
                    if let Some(list) = self.rois.get_mut(&key) {
                        if sel < list.len() {
                            list.remove(sel);
                            self.dirty_rois.insert(key);
                        }
                    }
                    self.selected_roi = None;
                }
            }
            MenuAction::DeleteAll => {
                if let Some(key) = key {
                    self.rois.remove(&key);
                    self.selected_roi = None;
                    self.dirty_rois.insert(key);
                }
            }
            MenuAction::Calibrate => {
                if let (Some(px), Some(img)) = (self.selected_length_px(), self.current_image()) {
                    self.calibrate = Some(CalibrateDraft {
                        image_id: img.id,
                        pixel_length: px,
                        known_mm: String::new(),
                        apply_to_series: self.panes[self.active].images.len() > 1,
                        scope: self.calibration_scope(),
                    });
                }
            }
            MenuAction::RemoveCalibration => {
                if let Some(img) = self.current_image() {
                    self.pending_clear_calibration = Some(img.id);
                }
            }
            MenuAction::ToggleBasisPanel => self.basis_panel = !self.basis_panel,
            MenuAction::ToggleRoiManager => self.show_roi_manager = !self.show_roi_manager,
            MenuAction::ToggleRoiInfo => self.show_roi_info = !self.show_roi_info,
            MenuAction::ToggleHistogram => {
                self.histogram = match self.histogram.take() {
                    Some(_) => None,
                    None => Some(Default::default()),
                }
            }
            MenuAction::ToggleRois => self.show_rois = !self.show_rois,
            MenuAction::ToggleLabels => self.show_labels = !self.show_labels,
            MenuAction::CopyValues => {
                let text = self.values_tsv();
                let n = text.lines().count().saturating_sub(1);
                ui.ctx().copy_text(text);
                self.set_status(format!("Copied {n} measurement{}", if n == 1 { "" } else { "s" }));
            }
            MenuAction::AddToReport => {
                let named = key
                    .and_then(|k| self.rois.get(&k))
                    .and_then(|l| self.selected_roi.and_then(|i| l.get(i)))
                    .is_some_and(|r| r.name.is_some() || matches!(r.kind, RoiKind::Lv { .. }));
                if named {
                    self.flush_rois(shared);
                    shared.open_reports.lock().push(self.study.id);
                } else {
                    self.set_status("Name the measurement; named measurements go into the report");
                    self.open_name_popup(ui);
                }
            }
            MenuAction::OpenReport => {
                self.flush_rois(shared);
                shared.open_reports.lock().push(self.study.id);
            }
            MenuAction::Fit => self.panes[self.active].fit_pending = true,
        }
    }

    /// Open the naming popup for the selected measurement, at its label if
    /// it is on screen, else at the pointer.
    fn open_name_popup(&mut self, ui: &Ui) {
        let (Some(key), Some(sel)) = (self.current_key(), self.selected_roi) else {
            return;
        };
        let at_label = self
            .label_hits
            .iter()
            .find(|(k, i, _)| *k == key && *i == sel)
            .map(|(_, _, r)| r.left_bottom());
        let pos = at_label
            .or_else(|| ui.input(|i| i.pointer.latest_pos()))
            .unwrap_or_else(|| ui.ctx().content_rect().center());
        self.assign = Some(AssignPopup {
            key,
            index: sel,
            pos,
            custom: String::new(),
        });
    }

    /// Every measurement on the current frame as tab-separated text:
    /// name, value, unit, basis.
    fn values_tsv(&self) -> String {
        let mut out = String::from("Name\tValue\tUnit\tBasis\n");
        let Some(key) = self.current_key() else { return out };
        let Some(frame) = self.frames.get(&key) else { return out };
        for roi in self.rois.get(&key).into_iter().flatten() {
            let m = roi.measure(frame);
            let name = roi.name.clone().unwrap_or_else(|| kind_label(&roi.kind).to_string());
            let value = m.value.map(|v| format!("{v:.2}")).unwrap_or_default();
            let basis = m.basis.map(|b| b.label()).or(m.issue.map(|i| i.label())).unwrap_or("");
            out.push_str(&format!("{name}\t{value}\t{}\t{basis}\n", m.unit));
        }
        out
    }

    /// The calibration basis panel: what the file says about scale and
    /// which basis the viewer chose for the current image.
    fn basis_panel(&mut self, ui: &mut Ui) {
        if !self.basis_panel {
            return;
        }
        let Some(key) = self.current_key() else { return };
        let Some(frame) = self.frames.get(&key).cloned() else {
            return;
        };
        let meta = &frame.meta;
        let mut open = true;
        egui::Window::new("Calibration basis")
            .id(egui::Id::unique(("basis-panel", self.id)))
            .open(&mut open)
            .resizable(false)
            .default_width(360.0)
            .show(ui.ctx(), |ui| {
                let pair = |v: Option<[f64; 2]>| {
                    v.map(|s| format!("{:.4} \\ {:.4} mm", s[0], s[1]))
                        .unwrap_or_else(|| "absent".into())
                };
                let rows: Vec<(String, String)> = vec![
                    ("Modality".into(), meta.modality.clone()),
                    ("Pixel Spacing (0028,0030)".into(), pair(meta.pixel_spacing_tag)),
                    (
                        "Imager Pixel Spacing (0018,1164)".into(),
                        pair(meta.imager_pixel_spacing),
                    ),
                    (
                        "Magnification Factor (0018,1114)".into(),
                        meta.magnification_factor
                            .map(|m| format!("{m:.3}"))
                            .unwrap_or_else(|| "absent".into()),
                    ),
                    (
                        "Calibration Type (0028,0A02)".into(),
                        if meta.pixel_spacing_calibration_type.is_empty() {
                            "absent".into()
                        } else {
                            meta.pixel_spacing_calibration_type.clone()
                        },
                    ),
                    (
                        "Ultrasound regions (0018,6011)".into(),
                        if meta.us_regions.is_empty() {
                            "none".into()
                        } else {
                            format!("{}", meta.us_regions.len())
                        },
                    ),
                    (
                        "Hand calibration".into(),
                        meta.hand_calibration
                            .as_ref()
                            .map(|h| {
                                let scope = h.region.map(|r| format!(", region {}", r + 1)).unwrap_or_default();
                                format!("{:.4} \\ {:.4} mm{scope}", h.spacing[0], h.spacing[1])
                            })
                            .unwrap_or_else(|| "none".into()),
                    ),
                ];
                egui::Grid::new("basis-grid")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (k, v) in &rows {
                            ui.label(egui::RichText::new(k).color(Color32::from_gray(170)));
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                let pointer_region = self
                    .mouse_pixel
                    .and_then(|p| meta.us_region_index_at(p.0 as f64, p.1 as f64));
                for (i, r) in meta.us_regions.iter().enumerate() {
                    let here = pointer_region == Some(i);
                    let colour = if here { Color32::from_gray(235) } else { Color32::from_gray(170) };
                    ui.label(
                        egui::RichText::new(format!(
                            "Region {} \u{b7} {}{}: x {}\u{2013}{}, y {}\u{2013}{}",
                            i + 1,
                            r.format_name(),
                            if here { " (under the pointer)" } else { "" },
                            r.x0,
                            r.x1,
                            r.y0,
                            r.y1
                        ))
                        .size(12.0)
                        .strong()
                        .color(colour),
                    );
                    let mut detail = format!(
                        "    across: {}\n    down: {}",
                        ruthos_dicom::UsRegion::describe_axis(r.units_x, r.delta_x),
                        ruthos_dicom::UsRegion::describe_axis(r.units_y, r.delta_y)
                    );
                    if let Some(row) = r.baseline_row() {
                        detail.push_str(&format!(
                            "\n    baseline: image row {row} (reference pixel y {}, {:.2} cm/s)",
                            r.ref_y0, r.ref_value_y
                        ));
                    }
                    ui.label(egui::RichText::new(detail).size(11.5).color(colour));
                }
                ui.separator();
                let scale = meta.image_scale();
                ui.label(egui::RichText::new(format!("Chosen basis: {}", scale.basis.label())).strong());
                ui.label(format!(
                    "{:.4} \\ {:.4} mm per pixel at the image centre",
                    scale.spacing[0], scale.spacing[1]
                ));
                ui.label(
                    egui::RichText::new(scale.basis.explanation())
                        .size(11.5)
                        .color(Color32::from_gray(170)),
                );
                if let Some(p) = self.mouse_pixel {
                    match meta.scale_at(p.0 as f64, p.1 as f64) {
                        Some(s) if s.basis != scale.basis => {
                            ui.label(format!(
                                "Under the pointer: {} ({:.4} \\ {:.4} mm)",
                                s.basis.label(),
                                s.spacing[0],
                                s.spacing[1]
                            ));
                        }
                        None => {
                            let (x, y) = (p.0 as f64, p.1 as f64);
                            match meta.velocity_at(x, y) {
                                Some(v) => {
                                    ui.label(format!(
                                        "Under the pointer: spectral Doppler, {:.2} m/s. A point measures velocity; a line measures time.",
                                        v.abs()
                                    ));
                                }
                                None => {
                                    ui.label(
                                        "Under the pointer: M-mode. A line measures depth down and time across.",
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        if !open {
            self.basis_panel = false;
        }
    }

    /// The popup under a clicked measurement label.
    fn assign_popup(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(popup) = &self.assign else { return };
        let (key, index, pos) = (popup.key, popup.index, popup.pos);
        let mut custom = popup.custom.clone();
        let current = self
            .rois
            .get(&key)
            .and_then(|l| l.get(index))
            .and_then(|r| r.name.clone());
        let current_key = self
            .rois
            .get(&key)
            .and_then(|l| l.get(index))
            .and_then(|r| r.def_key.clone());
        let library = self.library.clone();
        let _ = shared;
        let region = self.region();
        let mut new_region: Option<ruthos_core::measure::Region> = None;
        let ctx = ui.ctx().clone();
        // Chosen label and definition key; `Some(None)` removes the name.
        let mut choice: Option<Option<(String, Option<String>)>> = None;
        let mut close = ui.input(|i| i.key_pressed(Key::Escape));
        let area = egui::Area::new(egui::Id::unique(("assign-roi", self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(230.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Name this measurement").strong());
                    });
                    // The context, changeable here too.
                    ui.horizontal_wrapped(|ui| {
                        for r in ruthos_core::measure::Region::ALL {
                            if ui
                                .selectable_label(region == r, egui::RichText::new(r.label()).size(12.0))
                                .clicked()
                            {
                                new_region = Some(r);
                            }
                        }
                    });
                    ui.separator();
                    // The context's sets, one submenu each, so the list stays short.
                    for set in library.sets.iter().filter(|s| s.applies_to(region)) {
                        let holds_current = set.keys.iter().any(|k| current_key.as_deref() == Some(k.as_str()));
                        let title = if holds_current {
                            format!("{}  \u{2713}", set.name)
                        } else {
                            set.name.clone()
                        };
                        ui.menu_button(title, |ui| {
                            for k in &set.keys {
                                let label = library.label(k);
                                let is_current = current_key.as_deref() == Some(k.as_str())
                                    || (current_key.is_none() && current.as_deref() == Some(label.as_str()));
                                if menu_item(ui, label.as_str(), is_current).clicked() {
                                    choice = Some(Some((label.clone(), Some(k.clone()))));
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.add_space(4.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut custom)
                                .hint_text("Custom name")
                                .desired_width(150.0),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if (enter || ui.button("Set").clicked()) && !custom.trim().is_empty() {
                            // A custom name takes a definition's key when it matches its label.
                            let text = custom.trim().to_string();
                            let key = library.definition_by_label(&text).map(|d| d.key.clone());
                            choice = Some(Some((text, key)));
                        }
                    });
                    if menu_item_enabled(ui, "Remove name", false, current.is_some()).clicked() {
                        choice = Some(None);
                    }
                });
            });
        let submenu_open = egui::Popup::is_any_open(&ctx);
        if let Some(p) = ui.input(|i| {
            if i.pointer.any_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) {
            if !area.response.rect.contains(p) && !submenu_open && choice.is_none() {
                close = true;
            }
        }
        if let Some(name) = choice {
            if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(index)) {
                match &name {
                    Some((label, def_key)) => {
                        roi.name = Some(label.clone());
                        roi.def_key = def_key.clone();
                        roi.resolve(&self.library);
                    }
                    None => {
                        roi.name = None;
                        roi.def_key = None;
                    }
                }
                self.dirty_rois.insert(key);
                self.set_status(match name {
                    Some((n, _)) => format!("Measurement assigned to {n}"),
                    None => "Measurement name removed".to_string(),
                });
            }
            close = true;
        }
        if let Some(r) = new_region {
            self.region_override = Some(r);
            close = false;
        }
        if close {
            self.assign = None;
        } else if let Some(popup) = &mut self.assign {
            popup.custom = custom;
        }
    }

    // ------------------------------------------------------------ caches

    /// The anatomical M-mode strip of the loop on screen.
    fn amm_window(&mut self, ctx: &egui::Context, shared: &Shared) {
        let Some(mut amm) = self.amm.take() else { return };
        let Some(image) = self.current_image().cloned().filter(|i| i.number_of_frames > 1) else {
            return;
        };
        if amm.image_id != image.id {
            amm = crate::amm_ui::AmmView::new(image.id);
        }
        let n = image.number_of_frames.max(1) as u32;
        let frames: Vec<Arc<ruthos_dicom::Frame>> = (0..n)
            .filter_map(|frame| {
                self.frames
                    .get(&FrameKey {
                        image_id: image.id,
                        frame,
                    })
                    .cloned()
            })
            .collect();
        let p = &self.panes[self.active];
        if frames.len() == n as usize {
            amm.refresh(ctx, &frames, (p.view.window_center, p.view.window_width));
        }
        let can_save = !ruthos_core::portable::active();
        match amm.window(ctx, p.frame_idx as usize, can_save, self.id) {
            crate::amm_ui::Action::Nothing => {}
            crate::amm_ui::Action::Close => return,
            crate::amm_ui::Action::Jump(frame) => {
                self.playing = false;
                self.panes[self.active].frame_idx = frame.min(n as usize - 1) as u32;
            }
            crate::amm_ui::Action::Save => {
                amm.message = Some(match self.save_amm(shared, &amm, &image) {
                    Ok(()) => {
                        "Saved to the study. It is in this window's series list, where the M-mode tools measure it."
                            .to_string()
                    }
                    Err(e) => format!("Not saved: {e}"),
                });
            }
        }
        self.amm = Some(amm);
    }

    fn save_amm(&self, shared: &Shared, amm: &crate::amm_ui::AmmView, image: &ImageRow) -> anyhow::Result<()> {
        let Some(strip) = &amm.strip else { return Ok(()) };
        let view = &self.panes[self.active].view;
        let window = (view.window_center, view.window_width);
        let folder = shared.paths.temp_dir();
        let file = ruthos_dicom::amm::write_strip(&shared.resolve(&image.path), strip, window, amm.smooth, &folder)?;
        let outcome =
            ruthos_core::import::import_file(&shared.db, &shared.paths, &file, ruthos_core::import::ImportMode::Move)?;
        if let ruthos_core::import::FileOutcome::Imported { image_id, study_id, .. } = outcome {
            let summary = ruthos_core::import::ImportSummary {
                imported: 1,
                image_ids: vec![image_id],
                touched_study_ids: vec![study_id],
                ..Default::default()
            };
            let _ = shared.import_tx.send(ruthos_core::ImportEvent::Finished(summary));
        }
        Ok(())
    }

    /// Move to the slice scrolled to, once it is there.
    fn settle_scroll(&mut self) {
        let Some(id) = self.scroll_target else { return };
        let ready = self.frames.contains_key(&FrameKey { image_id: id, frame: 0 });
        let p = &mut self.panes[self.active];
        match p.images.iter().position(|i| i.id == id) {
            Some(at) if ready => {
                p.image_idx = at;
                p.frame_idx = 0;
                self.scroll_target = None;
            }
            Some(_) => {}
            None => self.scroll_target = None,
        }
    }

    fn pump_loader(&mut self, shared: &Shared) {
        for notice in shared.loader.take_notices() {
            self.set_status(notice);
        }
        let mine: HashSet<i64> = self.panes.iter().flat_map(|p| p.images.iter().map(|i| i.id)).collect();
        for loaded in shared.loader.poll_for(&|id| mine.contains(&id)) {
            match loaded.result {
                Ok(frame) => {
                    // A user calibration overrides whatever the header says.
                    let frame = match shared.db.calibration_scoped(loaded.key.image_id) {
                        Ok(Some((spacing, scope))) => {
                            let mut f = Arc::try_unwrap(frame).unwrap_or_else(|a| (*a).clone());
                            let region = scope.and_then(|(x, y)| f.meta.us_region_index_at(x, y));
                            f.meta.hand_calibration = Some(ruthos_dicom::HandCalibration { spacing, region });
                            f.meta.pixel_spacing_is_calibrated = true;
                            f.meta.user_calibrated = true;
                            Arc::new(f)
                        }
                        _ => frame,
                    };
                    self.frames.insert(loaded.key, frame);
                    self.frame_order.push_back(loaded.key);
                    let (frame_cache, _) = self.cache_limits();
                    let shown: HashSet<FrameKey> = self.panes.iter().filter_map(|p| p.key()).collect();
                    // Oldest first, but never what is on screen.
                    for _ in 0..self.frame_order.len() {
                        if self.frame_order.len() <= frame_cache {
                            break;
                        }
                        let Some(old) = self.frame_order.pop_front() else { break };
                        if shown.contains(&old) {
                            self.frame_order.push_back(old);
                        } else {
                            self.frames.remove(&old);
                        }
                    }
                }
                Err(e) => {
                    // A slice that cannot be read is still shown, as the error.
                    if self.scroll_target == Some(loaded.key.image_id) {
                        self.scroll_target = None;
                        let p = &mut self.panes[self.active];
                        if let Some(at) = p.images.iter().position(|i| i.id == loaded.key.image_id) {
                            p.image_idx = at;
                            p.frame_idx = 0;
                        }
                    }
                    for p in &mut self.panes {
                        if p.key() == Some(loaded.key) {
                            p.load_error = Some(e.clone());
                        }
                    }
                }
            }
        }
    }

    fn ensure_requested(&self, shared: &Shared, images: &[ImageRow], idx: usize, frame: u32) {
        if let Some(img) = images.get(idx) {
            let frames = img.number_of_frames.max(1) as u32;
            if frame >= frames {
                return;
            }
            let key = FrameKey {
                image_id: img.id,
                frame,
            };
            if self.frames.contains_key(&key) {
                return;
            }
            if frames > 1 {
                // A loop is decoded in one pass and kept whole.
                shared
                    .loader
                    .request_all(img.id, shared.resolve(&img.path), frames, frame);
            } else {
                shared.loader.request(key, shared.resolve(&img.path));
            }
        }
    }

    /// Queue a slice of a stack that scrolling may reach.
    fn read_ahead(&self, shared: &Shared, images: &[ImageRow], idx: usize) {
        let Some(img) = images.get(idx).filter(|i| i.number_of_frames <= 1) else {
            return;
        };
        let key = FrameKey {
            image_id: img.id,
            frame: 0,
        };
        if !self.frames.contains_key(&key) {
            shared.loader.request_ahead(key, shared.resolve(&img.path));
        }
    }

    /// Cache sizes grow with the number of cells on screen, and hold every
    /// frame of every loop being shown so cine never re-decodes. Capped so a
    /// 4x4 of long loops cannot exhaust memory; beyond the cap playback
    /// degrades instead of failing.
    fn cache_limits(&self) -> (usize, usize) {
        const MAX_CACHED_FRAMES: usize = 900;
        let cells = self.panes.len();
        let mut seen = std::collections::HashSet::new();
        let loop_frames: usize = self
            .panes
            .iter()
            .filter_map(|p| p.image())
            .filter(|i| seen.insert(i.id))
            .map(|i| i.number_of_frames.max(1) as usize)
            .sum::<usize>()
            .min(MAX_CACHED_FRAMES);
        // A stack being played is kept whole, so each pass is not decoded again.
        let stack = if self.playing {
            self.panes
                .iter()
                .filter(|p| p.images.len() > 1)
                .map(|p| p.images.len())
                .sum::<usize>()
                .min(MAX_CACHED_FRAMES)
        } else {
            0
        };
        let loop_frames = loop_frames.max(stack);
        (
            FRAME_CACHE.max(cells * 3 + 6).max(loop_frames + 8),
            GPU_CACHE.max(cells * 2).max(loop_frames + cells * 2),
        )
    }

    fn gpu_for(&mut self, shared: &Shared, key: FrameKey) -> Option<Arc<GpuImage>> {
        if let Some(g) = self.gpu.get(&key) {
            return Some(g.clone());
        }
        let frame = self.frames.get(&key)?.clone();
        let g = Arc::new(GpuImage::upload(
            &shared.render_state.device,
            &shared.render_state.queue,
            &frame,
        ));
        self.gpu.insert(key, g.clone());
        self.gpu_order.push_back(key);
        let (_, gpu_cache) = self.cache_limits();
        while self.gpu_order.len() > gpu_cache {
            if let Some(old) = self.gpu_order.pop_front() {
                self.gpu.remove(&old);
            }
        }
        Some(g)
    }

    // ---------------------------------------------------------------- UI

    /// The Measurements menu item behind a native menu id
    /// (`platform::MENU_BAR`): `tool:length`, `undo`, `copy`…
    pub fn menu_from_id(id: &str) -> Option<MenuAction> {
        Some(match id {
            "tool:wl" => MenuAction::SetTool(Tool::WindowLevel),
            "tool:move" => MenuAction::SetTool(Tool::Move),
            "tool:zoom" => MenuAction::SetTool(Tool::Zoom),
            "tool:rotate" => MenuAction::SetTool(Tool::Rotate),
            "tool:length" => MenuAction::SetTool(Tool::Length),
            "tool:angle" => MenuAction::SetTool(Tool::Angle),
            "tool:rect" => MenuAction::SetTool(Tool::Rectangle),
            "tool:oval" => MenuAction::SetTool(Tool::Oval),
            "tool:arrow" => MenuAction::SetTool(Tool::Arrow),
            "tool:point" => MenuAction::SetTool(Tool::Point),
            "tool:lvmmode" => MenuAction::SetTool(Tool::LvMmode),
            "tool:polygon" => MenuAction::SetTool(Tool::Polygon),
            "tool:cobb" => MenuAction::SetTool(Tool::Cobb),
            "tool:dynangle" => MenuAction::SetTool(Tool::DynamicAngle),
            "tool:openpolygon" => MenuAction::SetTool(Tool::OpenPolygon),
            "tool:pencil" => MenuAction::SetTool(Tool::Pencil),
            "tool:brush" => MenuAction::SetTool(Tool::Brush),
            "tool:text" => MenuAction::SetTool(Tool::Text),
            "tool:repulsor" => MenuAction::SetTool(Tool::Repulsor),
            "undo" => MenuAction::Undo,
            "delete" => MenuAction::DeleteSelected,
            "delete-all" => MenuAction::DeleteAll,
            "calibrate" => MenuAction::Calibrate,
            "remove-calibration" => MenuAction::RemoveCalibration,
            "basis" => MenuAction::ToggleBasisPanel,
            "roi-manager" => MenuAction::ToggleRoiManager,
            "roi-info" => MenuAction::ToggleRoiInfo,
            "histogram" => MenuAction::ToggleHistogram,
            "show-rois" => MenuAction::ToggleRois,
            "show-labels" => MenuAction::ToggleLabels,
            "copy" => MenuAction::CopyValues,
            "add-to-report" => MenuAction::AddToReport,
            "report" => MenuAction::OpenReport,
            _ => return None,
        })
    }

    /// Carry out a menu action on the next frame (the native menu bar).
    pub fn request_menu(&mut self, action: MenuAction) {
        self.pending_menu = Some(action);
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        crate::crash::debug_panic("viewer");
        self.pump_movie();
        self.dial_input(ui, shared);
        self.dev_options(ui);
        let ctx = ui.ctx().clone();
        self.text_prompt_window(&ctx);
        self.amm_window(&ctx, shared);
        self.roi_manager_window(&ctx);
        self.roi_info_window(&ctx, shared);
        self.histogram_window(&ctx);
        self.encoder_window(&ctx, shared);
        // Developer option: `RUTHOS_EXPORT_MOVIE=<file.mp4>` writes the
        // current loop or series as a movie once the viewer is showing it.
        if !self.dev_movie_done && self.current_key().is_some_and(|k| self.frames.contains_key(&k)) {
            if let Some(out) = std::env::var_os("RUTHOS_EXPORT_MOVIE") {
                self.dev_movie_done = true;
                self.export_movie(&ui.ctx().clone(), shared, Some(out.into()));
            }
        }
        if ui.input(|i| i.viewport().focused.unwrap_or(false)) {
            shared
                .focused_viewer
                .store(self.id, std::sync::atomic::Ordering::Relaxed);
        }
        {
            let st = shared.settings.lock();
            self.loupe = LoupeSettings::from(&*st);
            crate::roi::set_echo_cm(st.echo_length_unit == "cm");
        }
        {
            let library = shared.library.lock().clone();
            if !Arc::ptr_eq(&library, &self.library) {
                // Definitions changed in Preferences: what each named
                // measurement records may have changed with them.
                for list in self.rois.values_mut() {
                    for roi in list.iter_mut() {
                        roi.resolve(&library);
                    }
                }
                self.dirty_rois.extend(self.rois.keys().copied());
                self.library = library;
            }
        }
        if let Some(sid) = self.current_series_id() {
            if !self.mmode_latest.contains_key(&sid) {
                self.mmode_refresh.insert(sid);
            }
        }
        for sid in std::mem::take(&mut self.mmode_refresh) {
            self.refresh_mmode_latest(shared, sid);
        }
        if self.current_set >= self.library.sets.len() {
            self.current_set = 0;
        }
        ui.style_mut().visuals = theme::viewer_visuals();
        if ui.input(|i| i.viewport().close_requested()) {
            self.persist_series_display(shared);
            self.close_requested = true;
            // The main window removes this one, so leave it room to.
            self.playing = false;
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        if shared.main_window_waiting() && self.gave_way_at.is_none_or(|t| t.elapsed() > GIVE_WAY_EVERY) {
            self.gave_way_at = Some(Instant::now());
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        // Held for more than one frame: egui follows any immediate repaint
        // with one more, which would fill a single frame's gap.
        self.give_way = self.gave_way_at.is_some_and(|t| t.elapsed() < GIVE_WAY);
        self.reload_series(shared);
        if !self.timeline_read {
            self.load_timeline(shared);
        }
        self.pump_loader(shared);
        self.settle_scroll();
        self.session_sync(ui, shared);
        self.tick_cine(ui);
        if !self.session.following {
            self.keyboard(ui, shared);
        }
        self.draw_dial_indicator(ui, shared);
        self.draw_cine_banner(ui);
        self.draw_status(ui);

        let dark = true;
        egui::Panel::top(("viewer-toolbar", self.id))
            .exact_size(theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(theme::toolbar_stroke(dark))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                theme::paint_toolbar_background(ui, true, egui::vec2(6.0, 3.0));
                self.toolbar(ui, shared)
            });

        egui::Panel::left(("viewer-drawer", self.id))
            .exact_size(theme::THUMB_SIZE + 36.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(30, 30, 32))
                    .inner_margin(egui::Margin::same(6)),
            )
            .show(ui, |ui| self.drawer(ui, shared));

        if self.panes[self.active].positions() > 1 {
            egui::Panel::bottom(("viewer-slider", self.id))
                .exact_size(26.0)
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(30, 30, 32))
                        .inner_margin(egui::Margin::symmetric(10, 4)),
                )
                .show(ui, |ui| self.slider(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.image_area(ui, shared));

        self.run_menu_action(ui, shared);
        self.basis_panel(ui);
        self.assign_popup(ui, shared);
        self.calibrate_modal(ui, shared);
        self.flush_rois(shared);
        if let Some(id) = self.pending_clear_calibration.take() {
            match shared.db.clear_calibration(id) {
                Ok(()) => {
                    self.frames.clear();
                    self.frame_order.clear();
                    self.set_status("User calibration removed");
                }
                Err(e) => self.set_status(format!("Could not remove calibration: {e}")),
            }
        }

        if !self.close_requested && (shared.loader.has_pending() || self.playing) {
            self.repaint_after(ui, std::time::Duration::from_millis(16));
        }
    }

    /// Ask for the next frame, leaving a gap if the main window needs one.
    fn repaint_after(&self, ui: &Ui, wanted: std::time::Duration) {
        ui.ctx()
            .request_repaint_after(if self.give_way { wanted.max(GIVE_WAY) } else { wanted });
    }

    /// Show series that reached this study after the window opened.
    fn reload_series(&mut self, shared: &Shared) {
        let revision = shared.series_revision.load(std::sync::atomic::Ordering::Relaxed);
        if revision == self.series_seen {
            return;
        }
        self.series_seen = revision;
        let Ok(mut series) = shared.db.series_for_study(self.study.id) else {
            return;
        };
        series.retain(|s| s.modality != ruthos_core::seg::MODALITY);
        let moved: Option<Vec<usize>> = self
            .panes
            .iter()
            .map(|p| {
                let id = self.series.get(p.series_idx)?.id;
                series.iter().position(|s| s.id == id)
            })
            .collect();
        // A series shown here has gone: leave the list until the window closes.
        let Some(moved) = moved else { return };
        for (pane, index) in self.panes.iter_mut().zip(moved) {
            pane.series_idx = index;
        }
        self.series = series;
        self.timeline_read = false;
    }

    /// Put an echo's images in order, reading the time of any image imported
    /// before times were kept.
    fn load_timeline(&mut self, shared: &Shared) {
        self.timeline_read = true;
        self.timeline.clear();
        if !self.series.iter().any(|s| s.modality == "US") {
            return;
        }
        for image in shared.db.images_without_time(self.study.id).unwrap_or_default() {
            // A file that cannot be reached now may be back later.
            if let Ok(meta) = ruthos_dicom::read_meta(&shared.resolve(&image.path)) {
                let time = meta.acquired().unwrap_or_default();
                if let Err(e) = shared.db.set_image_time(image.id, &time) {
                    tracing::warn!("an image time could not be saved: {e}");
                }
            }
        }
        let mut entries = shared.db.timeline(self.study.id).unwrap_or_default();
        entries.retain(|e| self.series.iter().any(|s| s.id == e.image.series_id));
        if entries.len() > 1 {
            self.timeline = entries;
        }
        let from_start = std::mem::take(&mut self.timeline_from_start);
        if self.on_timeline() {
            let current = self.current_image().map(|i| i.id);
            let here = self.timeline.iter().position(|e| Some(e.image.id) == current);
            self.show_timeline_entry(shared, if from_start { 0 } else { here.unwrap_or(0) });
        }
    }

    /// On the timeline a still is one image, not a place in a stack, so the
    /// wheel and Play have nothing to run through. A tiled series is a stack.
    fn keep_still_alone(&mut self) {
        if !self.on_timeline() || self.panes.len() != 1 {
            return;
        }
        let p = &mut self.panes[self.active];
        if let Some(image) = p.image().filter(|i| i.number_of_frames <= 1).cloned() {
            p.images = vec![image];
            p.image_idx = 0;
            p.frame_idx = 0;
        }
    }

    /// Give the active cell its whole series again, on the same image.
    fn restore_stack(&mut self, shared: &Shared) {
        let id = self.series_of(self.active).id;
        let p = &mut self.panes[self.active];
        let current = p.image().map(|i| i.id);
        let images = shared.db.images_for_series(id).unwrap_or_default();
        if images.len() != p.images.len() {
            p.image_idx = images.iter().position(|i| Some(i.id) == current).unwrap_or(0);
            p.images = images;
        }
    }

    fn on_timeline(&self) -> bool {
        !self.timeline.is_empty() && !self.grouped
    }

    /// Show an image of the timeline in the active cell.
    fn show_timeline_entry(&mut self, shared: &Shared, index: usize) {
        let Some(entry) = self.timeline.get(index) else { return };
        let (series_id, image_id) = (entry.image.series_id, entry.image.id);
        let still = entry.image.number_of_frames <= 1;
        let Some(series) = self.series.iter().position(|s| s.id == series_id) else {
            return;
        };
        self.scroll_target = None;
        self.select_series(shared, series);
        if self.panes[self.active].series_idx != series {
            return; // A locked cell.
        }
        if still {
            self.playing = false;
        }
        self.restore_stack(shared);
        let pane = &self.panes[self.active];
        if pane.images.len() > 1 {
            if let Some(position) = pane.images.iter().position(|i| i.id == image_id) {
                self.set_active_position(position);
            }
        }
        self.keep_still_alone();
    }

    /// Move along the timeline, across stills and loops alike.
    fn step_timeline(&mut self, shared: &Shared, delta: i64) {
        let current = self.current_image().map(|i| i.id);
        let Some(here) = self.timeline.iter().position(|e| Some(e.image.id) == current) else {
            return;
        };
        let last = self.timeline.len() as i64 - 1;
        self.show_timeline_entry(shared, (here as i64 + delta).clamp(0, last) as usize);
    }

    /// Pixel length of the selected Length ROI on the current frame, if any.
    fn selected_length_px(&self) -> Option<f32> {
        self.selected_length().map(|(px, _)| px)
    }

    /// Pixel length and midpoint of the selected Length ROI.
    fn selected_length(&self) -> Option<(f32, (f64, f64))> {
        let key = self.current_key()?;
        let roi = self.rois.get(&key)?.get(self.selected_roi?)?;
        match &roi.kind {
            RoiKind::Length { a, b } => Some((
                (*b - *a).length(),
                (((a.x + b.x) / 2.0) as f64, ((a.y + b.y) / 2.0) as f64),
            )),
            _ => None,
        }
    }

    /// Scope for a calibration taken from the selected length: the region
    /// under it, when the image has ultrasound regions.
    fn calibration_scope(&self) -> Option<(f64, f64)> {
        let (_, mid) = self.selected_length()?;
        let key = self.current_key()?;
        let frame = self.frames.get(&key)?;
        if frame.meta.us_regions.is_empty() {
            None
        } else {
            Some(mid)
        }
    }

    fn calibrate_modal(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(draft) = self.calibrate.as_mut() else { return };
        let mut close = false;
        let mut apply: Option<(f64, bool)> = None;
        let pixel_length = draft.pixel_length;
        let ctx = ui.ctx().clone();
        egui::Modal::new(egui::Id::unique(("calibrate", self.id))).show(&ctx, |ui| {
            ui.set_min_width(380.0);
            ui.heading("Calibrate measurements");
            ui.label(format!(
                "The selected length is {pixel_length:.1} pixels. Enter its real length and every \
                 measurement on this image will be reported in millimetres."
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Real length");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut draft.known_mm)
                        .desired_width(90.0)
                        .hint_text("e.g. 100"),
                );
                r.request_focus();
                ui.label("mm");
            });
            ui.checkbox(&mut draft.apply_to_series, "Apply to every image in this series");
            ui.add_space(8.0);
            let mm: Option<f64> = draft
                .known_mm
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|v| *v > 0.0);
            if let Some(mm) = mm {
                let spacing = mm / pixel_length as f64;
                ui.label(
                    egui::RichText::new(format!("Pixel size: {spacing:.4} mm/px"))
                        .size(12.0)
                        .color(Color32::from_gray(170)),
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(mm.is_some(), egui::Button::new("Calibrate")).clicked() {
                    apply = Some((mm.unwrap_or(1.0), draft.apply_to_series));
                    close = true;
                }
            });
        });
        if let Some((mm, to_series)) = apply {
            let spacing = mm / pixel_length as f64;
            let image_id = draft.image_id;
            let scope = draft.scope;
            let series_id = self.current_series().id;
            let result = if to_series {
                shared
                    .db
                    .set_series_calibration_scoped(series_id, spacing, spacing, "user length", scope)
                    .map(|_| ())
            } else {
                shared
                    .db
                    .set_calibration_scoped(image_id, spacing, spacing, "user length", scope)
            };
            match result {
                Ok(()) => {
                    // Reload cached frames so the new spacing takes effect.
                    self.frames.clear();
                    self.frame_order.clear();
                    self.set_status(format!("Calibrated: {spacing:.4} mm per pixel"));
                }
                Err(e) => self.set_status(format!("Calibration failed: {e}")),
            }
        }
        if close {
            self.calibrate = None;
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, shared: &Shared) {
        // A locked cell ignores the toolbar: whatever a menu changes is put back.
        let guard = if self.active_locked() {
            let p = &self.panes[self.active];
            Some((p.view, p.clut.clone(), p.fit_pending, p.image_idx, p.frame_idx))
        } else {
            None
        };
        self.toolbar_inner(ui, shared);
        if let Some((view, clut, fit, image_idx, frame_idx)) = guard {
            let p = &mut self.panes[self.active];
            let changed = p.view != view || p.clut != clut || p.image_idx != image_idx || p.frame_idx != frame_idx;
            p.view = view;
            p.clut = clut;
            p.fit_pending = fit;
            p.image_idx = image_idx;
            p.frame_idx = frame_idx;
            if changed {
                self.refuse_locked();
            }
        }
    }

    fn toolbar_inner(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            let viewing = ruthos_core::portable::active();
            if !viewing {
                if ToolbarButton::new(Bitmap::DatabaseWindow, "Database")
                    .tooltip("Show the database window")
                    .show(ui)
                    .clicked()
                {
                    shared.focus_browser.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                toolbar_separator(ui);
            }

            // Series popup
            ui.vertical(|ui| {
                ui.add_space(4.0);
                toolbar_caption(ui, "Series");
                let current = self
                    .series
                    .get(self.panes[self.active].series_idx)
                    .map(|s| s.display_name())
                    .unwrap_or_default();
                let mut chosen = self.panes[self.active].series_idx;
                // A short button; the menu is as wide as the names need.
                ui.menu_button(
                    (
                        egui::RichText::new(crate::widgets::truncate(&current, 11)).size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        for (i, s) in self.series.iter().enumerate() {
                            let label = format!("{}  ({} im)", s.display_name(), s.number_of_images);
                            if menu_item(ui, label.as_str(), chosen == i).clicked() {
                                chosen = i;
                                ui.close();
                            }
                        }
                    },
                )
                .response
                .on_hover_text(current.clone());
                if chosen != self.panes[self.active].series_idx {
                    self.select_series(shared, chosen);
                }
            });
            toolbar_separator(ui);

            // Tiling menu (Horos: image tiling and series layout)
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let label = format!(" {} ", self.tiling.label());
                let caption = match self.tiling.mode {
                    TilingMode::Series => "Tiling",
                    TilingMode::Study => "Tiling: series",
                };
                let mut chosen: Option<Tiling> = None;
                ui.menu_button(
                    (
                        icons::rich(icons::TILING, 16.0),
                        egui::RichText::new(label).size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        for (mode, title) in [
                            (TilingMode::Series, "Images of this series"),
                            (TilingMode::Study, "Series of this study"),
                        ] {
                            menu_heading(ui, title);
                            ui.horizontal_wrapped(|ui| {
                                ui.set_max_width(236.0);
                                for (rows, cols) in Tiling::GRIDS {
                                    let t = Tiling { mode, rows, cols };
                                    let enabled = mode == TilingMode::Series
                                        || rows * cols <= self.series.len().max(1)
                                        || rows * cols == 1;
                                    let r = crate::widgets::tiling_button(ui, rows, cols, self.tiling == t, enabled);
                                    if r.clicked() {
                                        chosen = Some(t);
                                        ui.close();
                                    }
                                }
                            });
                            if mode == TilingMode::Series {
                                ui.add_space(4.0);
                                ui.separator();
                            }
                        }
                    },
                );
                toolbar_caption(ui, caption);
                if let Some(t) = chosen {
                    self.set_tiling(shared, t);
                }
            });
            toolbar_separator(ui);
            self.cine_controls(ui);
            toolbar_separator(ui);

            if ToolbarButton::new(icons::ANNOTATIONS, "Annotations")
                .tooltip("Cycle annotations: Full, Partial, None")
                .show(ui)
                .clicked()
            {
                self.annotations = self.annotations.next();
                self.set_status(format!("Annotations: {}", self.annotations.label()));
            }

            // Tools menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let title_icon = icons::image(ui.ctx(), self.tool.bitmap(), 18.0);
                ui.menu_button(
                    (
                        title_icon,
                        egui::RichText::new(" ").size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        // One row per tool. Left-click puts it on the left mouse
                        // button, right-click on the right button; a small mouse
                        // glyph beside the name shows which.
                        let mut changed = false;
                        menu_heading(ui, "Navigate");
                        for t in Tool::NAVIGATE {
                            let r = tool_menu_row(ui, t.bitmap(), t.label(), self.tool == t, self.right_tool == t);
                            if r.clicked() {
                                self.tool = t;
                                changed = true;
                                ui.close();
                            } else if r.secondary_clicked() {
                                self.right_tool = t;
                                changed = true;
                                ui.close();
                            }
                        }
                        // Drawing and annotation.
                        menu_heading(ui, "Draw");
                        for t in Tool::DRAW {
                            let r = tool_menu_row(ui, t.bitmap(), t.label(), self.tool == t, self.right_tool == t);
                            if r.clicked() {
                                self.tool = t;
                                changed = true;
                                ui.close();
                            } else if r.secondary_clicked() {
                                self.right_tool = t;
                                changed = true;
                                ui.close();
                            }
                        }
                        // One row for measuring; what it measures is chosen in
                        // the Measurements menu beside this one.
                        menu_heading(ui, "Measure");
                        let m = self.measure_tool;
                        let label = format!("Measure: {}", m.label());
                        let r = tool_menu_row(
                            ui,
                            m.bitmap(),
                            &label,
                            self.tool.is_measure(),
                            self.right_tool.is_measure(),
                        );
                        if r.clicked() {
                            self.tool = m;
                            changed = true;
                            ui.close();
                        } else if r.secondary_clicked() {
                            self.right_tool = m;
                            changed = true;
                            ui.close();
                        }
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new("Click: left button  ·  Right-click: right button")
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        );
                        if changed {
                            self.drag_draft = None;
                            self.angle_draft = None;
                            self.mmode_draft = None;
                            self.polygon_draft = None;
                        }
                    },
                );
                toolbar_caption(ui, &format!("Tool: {}", self.tool.label()));
            });

            // Measurements menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let icon = icons::image(ui.ctx(), Bitmap::Length, 18.0);
                ui.menu_button(
                    (
                        icon,
                        egui::RichText::new(" ").size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        self.measurements_menu(ui, false);
                    },
                );
                toolbar_caption(ui, &format!("Measure: {}", self.measure_tool.label()));
            });
            // WL/WW menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let wl_icon = icons::image(ui.ctx(), Bitmap::Wlww, 18.0);
                ui.menu_button(
                    (
                        wl_icon,
                        egui::RichText::new(" ").size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        if menu_item(ui, "Default WL & WW", false).clicked() {
                            self.apply_default_window(shared, false);
                            ui.close();
                        }
                        if menu_item(ui, "Full dynamic", false).clicked() {
                            self.apply_default_window(shared, true);
                            ui.close();
                        }
                        ui.separator();
                        for p in &shared.presets.presets {
                            if menu_item(ui, format!("{}  ({} / {})", p.name, p.center, p.width), false).clicked() {
                                self.panes[self.active].view.window_center = p.center;
                                self.panes[self.active].view.window_width = p.width;
                                ui.close();
                            }
                        }
                        ui.separator();
                        menu_heading(ui, "Custom");
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            ui.label("WL");
                            ui.add(egui::DragValue::new(&mut self.panes[self.active].view.window_center).speed(1.0));
                            ui.label("WW");
                            ui.add(
                                egui::DragValue::new(&mut self.panes[self.active].view.window_width)
                                    .speed(1.0)
                                    .range(1.0..=f32::MAX),
                            );
                        });
                    },
                );
                toolbar_caption(ui, "WL/WW");
            });

            // CLUT menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let clut_caption = crate::widgets::truncate(&self.panes[self.active].clut, 12);
                let mut picked = None;
                ui.menu_button(
                    (
                        icons::rich(icons::CLUT, 16.0),
                        egui::RichText::new(" ").size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        let modality = self.current_series().modality.clone();
                        picked = crate::clut_menu::show(ui, &shared.cluts, &self.panes[self.active].clut, &modality);
                    },
                );
                toolbar_caption(ui, &clut_caption);
                match picked {
                    Some(crate::clut_menu::Pick::ImageDefault) => {
                        self.panes[self.active].clut = ruthos_dicom::Clut::grayscale().name;
                        self.apply_default_window(shared, false);
                    }
                    Some(crate::clut_menu::Pick::Table { name, window }) => {
                        self.panes[self.active].clut = name;
                        if let Some((c, w)) = window {
                            self.panes[self.active].view.window_center = c;
                            self.panes[self.active].view.window_width = w;
                        }
                    }
                    None => {}
                }
                let pane = &mut self.panes[self.active];
                let clut = shared.cluts.get(&pane.clut);
                let window = (pane.view.window_center, pane.view.window_width);
                if crate::clut_menu::moved_warning(ui, clut, window) {
                    if let Some((c, w)) = clut.and_then(|c| c.window()) {
                        pane.view.window_center = c;
                        pane.view.window_width = w;
                    }
                }
            });

            let looping = self.current_image().is_some_and(|i| i.number_of_frames > 1);
            if ToolbarButton::new(icons::TOOL_LENGTH, "Anat. M-mode")
                .width(76.0)
                .enabled(looping)
                .selected(self.amm.is_some())
                .tooltip(if looping {
                    "Draw a line or curve on this loop to read a time and depth strip along it"
                } else {
                    "Anatomical M-mode needs a loop"
                })
                .show(ui)
                .clicked()
            {
                self.amm = match (&self.amm, self.current_image()) {
                    (None, Some(image)) => Some(crate::amm_ui::AmmView::new(image.id)),
                    _ => None,
                };
            }
            let mpr_ok = crate::mpr::series_suits_mpr(&self.panes[self.active].images);
            if ToolbarButton::new(Bitmap::Mpr, "MPR")
                .enabled(mpr_ok)
                .tooltip(if mpr_ok {
                    "Multiplanar reconstruction: axial, coronal, sagittal and oblique planes through this series"
                } else {
                    "MPR needs a series of at least three slices, such as a CT or MR"
                })
                .show(ui)
                .clicked()
            {
                let series = self.current_series().id;
                shared.open_mpr.lock().push((self.study.id, series));
            }

            // Orientation menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.menu_button(
                    (
                        icons::rich(icons::ORIENTATION, 16.0),
                        egui::RichText::new(" ").size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        if menu_item(ui, "Rotate 90° clockwise", false).clicked() {
                            self.panes[self.active].view.rotation_deg =
                                (self.panes[self.active].view.rotation_deg + 90.0).rem_euclid(360.0);
                            self.panes[self.active].fit_pending = true;
                            ui.close();
                        }
                        if menu_item(ui, "Rotate 90° counter-clockwise", false).clicked() {
                            self.panes[self.active].view.rotation_deg =
                                (self.panes[self.active].view.rotation_deg - 90.0).rem_euclid(360.0);
                            self.panes[self.active].fit_pending = true;
                            ui.close();
                        }
                        if menu_item(ui, "Flip horizontal", false).clicked() {
                            self.panes[self.active].view.flip_horizontal =
                                !self.panes[self.active].view.flip_horizontal;
                            ui.close();
                        }
                        if menu_item(ui, "Flip vertical", false).clicked() {
                            self.panes[self.active].view.flip_vertical = !self.panes[self.active].view.flip_vertical;
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(ui, "Reset orientation", false).clicked() {
                            self.panes[self.active].view.rotation_deg = 0.0;
                            self.panes[self.active].view.flip_horizontal = false;
                            self.panes[self.active].view.flip_vertical = false;
                            self.panes[self.active].fit_pending = true;
                            ui.close();
                        }
                        ui.separator();
                        if menu_check(ui, &mut self.bilinear, "Smooth interpolation").clicked() {
                            ui.close();
                        }
                    },
                );
                toolbar_caption(ui, "Orientation");
            });
            toolbar_separator(ui);

            if ToolbarButton::new(Bitmap::Reset, "Reset")
                .tooltip("Fit image and restore default window")
                .show(ui)
                .clicked()
            {
                self.panes[self.active].fit_pending = true;
                self.apply_default_window(shared, false);
            }
            let is_key = self.current_image().map(|i| i.is_key_image).unwrap_or(false);
            if ToolbarButton::new(Bitmap::Key, "Key Image")
                .selected(is_key)
                .tooltip("Mark this image as a key image")
                .show(ui)
                .clicked()
            {
                if let Some(img) = self.current_image() {
                    let (id, flag) = (img.id, !img.is_key_image);
                    let _ = shared.db.set_key_image(id, flag);
                    for p in &mut self.panes {
                        for im in p.images.iter_mut().filter(|im| im.id == id) {
                            im.is_key_image = flag;
                        }
                    }
                }
            }
            if ToolbarButton::new(Bitmap::Export, "Export")
                .tooltip("Export the current image as PNG or JPEG")
                .show(ui)
                .clicked()
            {
                self.export_current(shared);
            }
            let writing = self.movie.is_some();
            if !ruthos_core::portable::active()
                && ToolbarButton::new(Bitmap::Movie, if writing { "Stop" } else { "Movie" })
                    .enabled(writing || self.can_export_movie())
                    .tooltip(if writing {
                        "Stop writing the movie"
                    } else {
                        "Export the loop, or the series, as an MP4 movie at the cine speed"
                    })
                    .show(ui)
                    .clicked()
            {
                self.export_movie(&ui.ctx().clone(), shared, None);
            }
            if !ruthos_core::portable::active()
                && ToolbarButton::new(Bitmap::ReportCustom, "Report")
                    .tooltip("The report for this study: measurements, findings, key images")
                    .show(ui)
                    .clicked()
            {
                shared.open_reports.lock().push(self.study.id);
            }
            if self.study.teaching_answer_key {
                let label = if self.reveal_answers { "Hide answers" } else { "Answers" };
                if ToolbarButton::new(icons::ANONYMIZE, label)
                    .selected(self.reveal_answers)
                    .tooltip("This teaching copy carries its measurements as an answer key")
                    .show(ui)
                    .clicked()
                {
                    self.reveal_answers = !self.reveal_answers;
                }
            }
            toolbar_separator(ui);
        });
    }

    /// Play and the speed: early in the toolbar, so a narrow window never
    /// hides them. The file's own rate plays at 1x, 0.5x or 0.25x; without
    /// one, a frames-a-second slider.
    fn cine_controls(&mut self, ui: &mut Ui) {
        let can_play = self.panes[self.active].images.len() > 1
            || self.current_image().map(|i| i.number_of_frames > 1).unwrap_or(false);
        let (bitmap, label) = if self.playing {
            (Bitmap::Pause, "Pause")
        } else {
            (Bitmap::Play, "Play")
        };
        if ToolbarButton::new(bitmap, label).enabled(can_play).show(ui).clicked() {
            self.playing = !self.playing;
            self.last_tick = None;
        }
        // One compact button; its menu is a vertical slider. A file
        // with a frame rate is played at a fraction of it, otherwise at
        // so many frames a second.
        let file_rate = self.active_file_rate();
        let label = match file_rate {
            Some(rate) => format!("{} \u{b7} {:.0} fps", speed_label(self.speed), rate * self.speed),
            None => format!("{:.0} fps", self.fps),
        };
        ui.vertical(|ui| {
            ui.add_space(2.0);
            ui.add_enabled_ui(can_play, |ui| {
                ui.menu_button(
                    (
                        egui::RichText::new(label).size(13.0),
                        icons::rich(icons::CARET_DOWN, 11.0),
                    ),
                    |ui| {
                        ui.set_min_width(150.0);
                        ui.scope(|ui| {
                            let v = ui.visuals_mut();
                            v.widgets.inactive.bg_fill = Color32::from_gray(120);
                            v.widgets.hovered.bg_fill = Color32::from_gray(150);
                            v.widgets.active.bg_fill = Color32::from_gray(170);
                            v.selection.bg_fill = Color32::from_rgb(70, 140, 230);
                            v.slider_trailing_fill = true;
                            ui.spacing_mut().slider_width = 180.0;
                            ui.horizontal(|ui| match file_rate {
                                Some(rate) => {
                                    ui.add(
                                        egui::Slider::new(&mut self.speed, 0.05..=1.0)
                                            .vertical()
                                            .logarithmic(true)
                                            .show_value(false)
                                            .trailing_fill(true),
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(speed_label(self.speed)).strong().size(16.0));
                                        ui.label(format!("{:.0} fps", rate * self.speed));
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new(format!("Recorded at\n{rate:.0} fps"))
                                                .size(11.5)
                                                .color(Color32::from_gray(150)),
                                        );
                                        if self.speed < 0.999 && ui.small_button("Real time").clicked() {
                                            self.speed = 1.0;
                                        }
                                    });
                                }
                                None => {
                                    ui.add(
                                        egui::Slider::new(&mut self.fps, 1.0..=240.0)
                                            .vertical()
                                            .logarithmic(true)
                                            .show_value(false)
                                            .trailing_fill(true),
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{:.0} fps", self.fps)).strong().size(16.0),
                                        );
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new("This series gives\nno frame rate")
                                                .size(11.5)
                                                .color(Color32::from_gray(150)),
                                        );
                                    });
                                }
                            });
                        });
                    },
                );
            });
            toolbar_caption(ui, "Speed");
        });
    }

    /// An echo's images one by one, in the order they were taken.
    fn timeline_drawer(&mut self, ui: &mut Ui, shared: &Shared) {
        let current = self.current_image().map(|i| i.id);
        let follow = current != self.timeline_shown;
        self.timeline_shown = current;
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut earlier = None;
            for (i, entry) in self.timeline.iter().enumerate() {
                let gap = earlier.and_then(|e| entry.gap_since(e));
                if let Some(gap) = gap {
                    ui.label(
                        egui::RichText::new(gap)
                            .size(11.0)
                            .color(Color32::from_rgb(240, 190, 90)),
                    );
                }
                earlier = Some(entry);
                let image = &entry.image;
                let looping = image.number_of_frames > 1;
                let tex = if looping {
                    shared.thumbnail(image.series_id)
                } else {
                    shared.image_thumbnail(image)
                };
                let caption = entry
                    .clock()
                    .unwrap_or_else(|| format!("Image {}", image.instance_number.unwrap_or(i as i64 + 1)));
                let sub = if looping {
                    format!("Loop, {} frames", image.number_of_frames)
                } else {
                    "Still".to_string()
                };
                let selected = current == Some(image.id);
                let r = thumbnail_cell(ui, tex.as_ref(), &caption, &sub, selected, theme::THUMB_SIZE);
                if selected && follow {
                    r.scroll_to_me(None);
                }
                if r.clicked() {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = clicked {
            self.show_timeline_entry(shared, i);
        }
    }

    fn drawer(&mut self, ui: &mut Ui, shared: &Shared) {
        if !self.timeline.is_empty() {
            ui.horizontal(|ui| {
                let mut grouped = self.grouped;
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding.x = 4.0;
                let word = |text: &str| egui::RichText::new(text).size(11.5);
                ui.selectable_value(&mut grouped, false, word("Timeline"))
                    .on_hover_text("Stills and loops in the order they were taken. Page Up and Down move along it.");
                ui.selectable_value(&mut grouped, true, word("Grouped"))
                    .on_hover_text("By series, with the stills together");
                if grouped != self.grouped {
                    self.grouped = grouped;
                    if grouped {
                        self.restore_stack(shared);
                    } else {
                        self.keep_still_alone();
                    }
                    let mut settings = shared.settings.lock();
                    settings.series_grouped = grouped;
                    if let Err(e) = settings.save() {
                        tracing::warn!("the series view could not be saved: {e}");
                    }
                }
            });
            ui.add_space(4.0);
            if !self.grouped {
                self.timeline_drawer(ui, shared);
                return;
            }
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut clicked = None;
            for (i, s) in self.series.iter().enumerate() {
                let tex = shared.thumbnail(s.id);
                let sub = format!("{} {}", s.modality, s.count_label());
                let r = thumbnail_cell(
                    ui,
                    tex.as_ref(),
                    &s.display_name(),
                    &sub,
                    i == self.panes[self.active].series_idx,
                    theme::THUMB_SIZE,
                );
                if r.clicked() {
                    clicked = Some(i);
                }
            }
            if let Some(i) = clicked {
                self.select_series(shared, i);
            }
        });
    }

    fn slider(&mut self, ui: &mut Ui) {
        let n = self.panes[self.active].positions();
        if n <= 1 {
            return;
        }
        let current = self.panes[self.active].position().min(n - 1);
        let mut v = current;
        ui.spacing_mut().slider_width = ui.available_width() - 70.0;
        ui.add_enabled(
            !self.active_locked(),
            egui::Slider::new(&mut v, 0..=n - 1).show_value(false),
        );
        ui.label(
            egui::RichText::new(format!("{}/{}", v + 1, n))
                .size(11.5)
                .color(Color32::from_gray(200)),
        );
        if v != current {
            self.set_active_position(v);
        }
    }

    /// Frames a second image `image_id` was recorded at, from any of its
    /// decoded frames' headers.
    fn file_rate(&mut self, image_id: i64) -> Option<f32> {
        if let Some(r) = self.file_rates.get(&image_id) {
            return *r;
        }
        let rate = self.frames.iter().find(|(k, _)| k.image_id == image_id).map(|(_, f)| {
            f.meta
                .frame_time_ms
                .filter(|ms| *ms > 0.0)
                .map(|ms| (1000.0 / ms) as f32)
        })?;
        self.file_rates.insert(image_id, rate);
        rate
    }

    /// The rate pane `k` plays at: the file's times the speed, or the
    /// slider's when the file gives none.
    fn cine_rate(&mut self, k: usize) -> f32 {
        let image = self.panes[k].image().map(|i| i.id);
        match image.and_then(|id| self.file_rate(id)) {
            Some(r) => r * self.speed,
            None => self.fps.max(0.1),
        }
    }

    /// The active image's recorded rate, if its file gives one.
    fn active_file_rate(&mut self) -> Option<f32> {
        let id = self.current_image().map(|i| i.id)?;
        self.file_rate(id)
    }

    fn tick_cine(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        // One image has nothing to play, and Play is off for it.
        if self.panes.len() == 1 && self.panes[self.active].loop_len() <= 1 {
            self.playing = false;
        }
        if !self.playing {
            for p in &mut self.panes {
                p.cine = None;
            }
            self.skipping = None;
            self.cine_stats = (0, 0, None);
            self.last_frame_at = None;
            return;
        }
        // How often this window is actually drawn.
        if let Some(t) = self.last_frame_at {
            let dt = now.duration_since(t).as_secs_f32();
            if dt > 0.0 && dt < 0.25 {
                self.frame_interval = self.frame_interval * 0.9 + dt * 0.1;
            }
        }
        self.last_frame_at = Some(now);

        // A tiled series steps its block of cells, as before.
        if self.tiled_series() {
            let period = 1.0 / self.fps.max(1.0);
            match self.last_tick {
                Some(t) if now.duration_since(t).as_secs_f32() < period => {}
                _ => {
                    if !self.active_locked() && self.last_tick.is_some() {
                        self.step_block(1);
                    }
                    self.last_tick = Some(now);
                }
            }
            self.repaint_after(ui, std::time::Duration::from_secs_f32(period / 2.0));
            return;
        }

        let targets: Vec<usize> = if self.tiling.mode == TilingMode::Study && self.panes.len() > 1 {
            (0..self.panes.len()).collect()
        } else {
            vec![self.active]
        };
        let mut fastest = 0.0f32;
        for k in targets {
            if self.panes[k].locked || self.panes[k].loop_len() < 2 {
                self.panes[k].cine = None;
                continue;
            }
            let rate = self.cine_rate(k);
            fastest = fastest.max(rate);
            let p = &mut self.panes[k];
            let n = p.loop_len();
            let pos = p.loop_pos();
            // Start (or restart) the clock: at the first tick, on a new rate,
            // or when the position was moved by hand.
            let clock = match p.cine {
                Some(c) if c.rate == rate && c.shown == pos => c,
                _ => CineClock {
                    start: now,
                    from: pos,
                    rate,
                    shown: pos,
                },
            };
            let next = cine_position(clock.from, now.duration_since(clock.start).as_secs_f32(), rate, n);
            let advance = (next + n - pos) % n;
            // A frame still decoding is not shown blank: the cell keeps its
            // frame and the clock runs on, so real time is kept.
            let ready = p.key_at(next).is_some_and(|k| self.frames.contains_key(&k));
            let shown = if advance > 0 && ready {
                p.set_loop_pos(next);
                self.cine_stats.0 += 1;
                self.cine_stats.1 += advance as u32 - 1;
                next
            } else {
                pos
            };
            p.cine = Some(CineClock { shown, ..clock });
        }

        self.cine_ahead = ((fastest * 0.5).ceil() as usize).clamp(1, 64);

        // Once a second: is the screen keeping up?
        let since = *self.cine_stats.2.get_or_insert(now);
        if now.duration_since(since).as_secs_f32() >= 1.0 {
            let (shown, skipped, _) = self.cine_stats;
            let hz = snap_refresh(1.0 / self.frame_interval.max(1e-3));
            let share = skipped as f32 / (shown + skipped).max(1) as f32;
            self.skipping = (share > 0.05).then_some((fastest, hz, share));
            self.cine_stats = (0, 0, Some(now));
        }

        // Fast loops ask for every screen frame; slow ones just in time.
        if fastest > 20.0 && !self.give_way {
            ui.ctx().request_repaint();
        } else {
            let period = 1.0 / fastest.max(0.5);
            self.repaint_after(ui, std::time::Duration::from_secs_f32(period / 2.0));
        }
    }

    /// The latest status message, briefly, on the image above its bottom
    /// edge, rather than squeezed into the toolbar.
    fn draw_status(&self, ui: &Ui) {
        let Some((text, when)) = &self.status else { return };
        let age = when.elapsed().as_secs_f32();
        if age >= 3.0 {
            return;
        }
        let Some(rect) = self.cell_rects.get(self.active) else {
            return;
        };
        let fade = (1.0 - (age - 2.5).max(0.0) / 0.5).clamp(0.0, 1.0);
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique(("ruthos-status", self.id)),
            ))
            .with_clip_rect(*rect);
        let galley = painter.layout(
            text.clone(),
            egui::FontId::proportional(13.0),
            Color32::WHITE,
            rect.width() - 40.0,
        );
        let lift = if self.skipping.is_some() && self.playing {
            44.0
        } else {
            0.0
        };
        let size = galley.size() + Vec2::new(16.0, 8.0);
        let r = Rect::from_min_size(
            Pos2::new(rect.center().x - size.x / 2.0, rect.bottom() - size.y - 54.0 - lift),
            size,
        );
        painter.rect_filled(r, 5.0, Color32::from_black_alpha((190.0 * fade) as u8));
        painter.galley(
            r.min + Vec2::new(8.0, 4.0),
            galley,
            Color32::from_white_alpha((255.0 * fade) as u8),
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// The banner on the image while cine skips frames: why, and what shows
    /// every frame. Part of the image area, not a pop-up.
    fn draw_cine_banner(&self, ui: &Ui) {
        let Some((rate, hz, share)) = self.skipping.filter(|_| self.playing) else {
            return;
        };
        let Some(rect) = self.cell_rects.get(self.active) else {
            return;
        };
        let has_file_rate = self
            .current_image()
            .and_then(|i| self.file_rates.get(&i.id).copied().flatten())
            .is_some();
        let advice = if has_file_rate {
            let recorded = rate / self.speed.max(0.01);
            match speed_for_every_frame(recorded, hz) {
                Some(s) if s < self.speed => format!(" Set the speed to {} or slower to see every frame.", speed_label(s)),
                Some(_) => String::new(),
                None => {
                    " Even the slowest speed skips frames on this screen; step with the arrow keys or the dial to see each one.".into()
                }
            }
        } else {
            format!(" Set the speed to {hz:.0} fps or less to see every frame.")
        };
        let text = format!(
            "Playing at {:.0} frames a second on a screen that shows {:.0}: about {:.0}% of frames are skipped to keep real time.{advice}",
            rate,
            hz,
            share * 100.0
        );
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique(("ruthos-cine-banner", self.id)),
            ))
            .with_clip_rect(*rect);
        let font = egui::FontId::proportional(13.0);
        let galley = painter.layout(text, font, Color32::from_rgb(40, 30, 0), rect.width() - 24.0);
        let band = Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - galley.size().y - 12.0),
            Vec2::new(rect.width(), galley.size().y + 12.0),
        );
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(250, 205, 90, 235));
        painter.galley(band.min + Vec2::new(12.0, 6.0), galley, Color32::from_rgb(40, 30, 0));
    }

    fn keyboard(&mut self, ui: &mut Ui, shared: &Shared) {
        let (down, up, pgdown, pgup, home, end, space, del, esc, r, f, h, v, i_key) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::R),
                i.key_pressed(Key::F),
                i.key_pressed(Key::H),
                i.key_pressed(Key::V),
                i.key_pressed(Key::I),
            )
        });
        // Don't steal keys from text fields.
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        // While an M-mode line is being placed, Backspace removes the last
        // point and Escape abandons it.
        if let Some((pts, _)) = &mut self.polygon_draft {
            let (backspace, enter) = ui.input(|i| (i.key_pressed(Key::Backspace), i.key_pressed(Key::Enter)));
            if esc {
                self.polygon_draft = None;
                self.set_status("Polygon abandoned");
                return;
            }
            if backspace {
                pts.pop();
                if pts.is_empty() {
                    self.polygon_draft = None;
                }
                return;
            }
            let open = self.polygon_open;
            if enter && pts.len() >= if open { 2 } else { 3 } {
                let roi = Roi::new(if open {
                    RoiKind::Polyline { points: pts.clone() }
                } else {
                    RoiKind::Polygon { points: pts.clone() }
                });
                self.polygon_draft = None;
                if let (false, Some(key)) = (roi.is_degenerate(), self.current_key()) {
                    let list = self.rois.entry(key).or_default();
                    list.push(roi);
                    self.selected_roi = Some(list.len() - 1);
                    self.dirty_rois.insert(key);
                }
                return;
            }
        }
        if self.mmode_draft.is_some() {
            let backspace = ui.input(|i| i.key_pressed(Key::Backspace));
            if esc {
                self.mmode_draft = None;
                self.set_status("LV M-mode abandoned");
                return;
            }
            if backspace {
                let left = self.mmode_draft.as_mut().map(|d| {
                    d.points.pop();
                    d.points.len()
                });
                match left {
                    Some(0) | None => {
                        self.mmode_draft = None;
                        self.set_status("LV measurement abandoned");
                    }
                    Some(n) => self.set_status(format!("LV: {n} point{} placed", if n == 1 { "" } else { "s" })),
                }
                return;
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Z)) {
            if !self.active_locked() {
                self.undo_edit();
            }
            return;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::C)) {
            self.pending_menu = Some(MenuAction::CopyValues);
            return;
        }
        // Single-letter tool shortcuts, as listed in the Measurements menu.
        let plain = ui.input(|i| i.modifiers.is_none());
        if plain {
            let picked = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.is_none() => Tool::from_key(*key),
                    _ => None,
                })
            });
            if let Some(t) = picked {
                self.pending_menu = Some(MenuAction::SetTool(t));
                return;
            }
        }
        // With a measurement selected, Tab and the arrows edit its points.
        if self.selected_roi.is_some() && !self.active_locked() {
            let (tab, left, right, up_k, down_k, shift) = ui.input(|i| {
                (
                    i.key_pressed(Key::Tab),
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.modifiers.shift,
                )
            });
            let step = if shift { 0.1 } else { 1.0 };
            if tab {
                let n = self
                    .current_key()
                    .and_then(|k| self.rois.get(&k))
                    .and_then(|l| self.selected_roi.and_then(|s| l.get(s)))
                    .map(|r| r.handles().len())
                    .unwrap_or(1)
                    .max(1);
                self.active_handle = (self.active_handle + 1) % n;
            }
            if left {
                self.nudge_handle(-step, 0.0);
            }
            if right {
                self.nudge_handle(step, 0.0);
            }
            if up_k {
                self.nudge_handle(0.0, -step);
            }
            if down_k {
                self.nudge_handle(0.0, step);
            }
            if tab || left || right || up_k || down_k {
                return;
            }
        }
        if self.active_locked() {
            if esc {
                self.drag_draft = None;
                self.angle_draft = None;
                self.selected_roi = None;
            }
            if down || up || pgdown || pgup || home || end || space || del || r || f || h || v || i_key {
                self.refuse_locked();
            }
            return;
        }
        if down {
            self.step_image(1);
        }
        if up {
            self.step_image(-1);
        }
        if pgdown {
            if self.on_timeline() {
                self.step_timeline(shared, 1);
            } else {
                self.step_image(10);
            }
        }
        if pgup {
            if self.on_timeline() {
                self.step_timeline(shared, -1);
            } else {
                self.step_image(-10);
            }
        }
        if home {
            self.set_active_position(0);
        }
        if end {
            let n = self.panes[self.active].positions();
            if n > 0 {
                self.set_active_position(n - 1);
            }
        }
        if space {
            self.playing = !self.playing;
            self.last_tick = None;
        }
        if del {
            if let (Some(key), Some(sel)) = (self.current_key(), self.selected_roi) {
                if let Some(list) = self.rois.get_mut(&key) {
                    if sel < list.len() {
                        list.remove(sel);
                        self.dirty_rois.insert(key);
                    }
                }
                self.selected_roi = None;
            }
        }
        if esc {
            self.drag_draft = None;
            self.angle_draft = None;
            self.mmode_draft = None;
            self.polygon_draft = None;
            self.selected_roi = None;
        }
        if r {
            self.panes[self.active].fit_pending = true;
            self.apply_default_window(shared, false);
        }
        if f {
            self.panes[self.active].fit_pending = true;
        }
        if h {
            self.panes[self.active].view.flip_horizontal = !self.panes[self.active].view.flip_horizontal;
        }
        if v {
            self.panes[self.active].view.flip_vertical = !self.panes[self.active].view.flip_vertical;
        }
        if i_key {
            self.panes[self.active].view.rotation_deg =
                (self.panes[self.active].view.rotation_deg + 90.0).rem_euclid(360.0);
            self.panes[self.active].fit_pending = true;
        }
    }

    fn apply_default_window(&mut self, _shared: &Shared, full_dynamic: bool) {
        if let Some(key) = self.current_key() {
            if let Some(frame) = self.frames.get(&key) {
                let (c, w) = if full_dynamic {
                    frame.full_dynamic_window()
                } else {
                    frame.default_window()
                };
                self.panes[self.active].view.window_center = c;
                self.panes[self.active].view.window_width = w;
            }
        }
    }

    fn export_current(&mut self, shared: &Shared) {
        let Some(key) = self.current_key() else { return };
        let Some(frame) = self.frames.get(&key).cloned() else {
            return;
        };
        let default_name = format!(
            "{}_{}_{}.png",
            self.study.patient_name.replace([',', ' '], "_"),
            self.current_series().display_name().replace(' ', "_"),
            self.panes[self.active].image_idx + 1
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export image")
            .add_filter("PNG image", &["png"])
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        let clut = shared
            .cluts
            .get(&self.panes[self.active].clut)
            .cloned()
            .unwrap_or_else(ruthos_dicom::Clut::grayscale);
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(
            &frame,
            self.panes[self.active].view.window_center,
            self.panes[self.active].view.window_width,
            &clut,
        );
        match image::RgbImage::from_raw(frame.width, frame.height, rgb) {
            Some(img) => match img.save(&path) {
                Ok(()) => self.set_status(format!("Exported {}", path.display())),
                Err(e) => self.set_status(format!("Export failed: {e}")),
            },
            None => self.set_status("Export failed: buffer size mismatch"),
        }
    }

    // --------------------------------------------------------- image area

    fn image_area(&mut self, ui: &mut Ui, shared: &Shared) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, Color32::BLACK);
        self.sync_series_cells();

        let (rows, cols) = (self.tiling.rows.max(1), self.tiling.cols.max(1));
        let cells = rows * cols;
        let gap = if cells > 1 { 1.0 } else { 0.0 };
        let cw = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let ch = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
        let rects: Vec<Rect> = (0..cells)
            .map(|k| {
                let (r, c) = ((k / cols) as f32, (k % cols) as f32);
                Rect::from_min_size(
                    Pos2::new(area.left() + c * (cw + gap), area.top() + r * (ch + gap)),
                    Vec2::new(cw, ch),
                )
            })
            .collect();

        self.hover_cell = ui
            .input(|i| i.pointer.hover_pos())
            .and_then(|p| rects.iter().position(|r| r.contains(p)));
        self.cell_rects = rects.clone();

        // A press or a wheel turn in another cell makes it the active one
        // before any tool runs, so scrolling follows the pointer.
        if cells > 1 {
            let target = ui.input(|i| {
                if i.pointer.any_pressed() {
                    i.pointer.interact_pos()
                } else if i.smooth_scroll_delta.y.abs() > 0.0 || (i.zoom_delta() - 1.0).abs() > 1e-3 {
                    i.pointer.hover_pos()
                } else {
                    None
                }
            });
            if let Some(p) = target {
                if let Some(k) = rects.iter().position(|r| r.contains(p)) {
                    if k < self.panes.len() {
                        self.activate(k);
                    }
                }
            }
        }

        for (k, rect) in rects.iter().enumerate() {
            if k < self.panes.len() {
                self.paint_cell(ui, shared, *rect, k, cells > 1);
            }
        }

        if cells > 1 {
            let painter = ui.painter().with_clip_rect(area);
            let line = Color32::from_gray(70);
            for c in 1..cols {
                let x = area.left() + c as f32 * (cw + gap) - gap / 2.0;
                painter.line_segment(
                    [Pos2::new(x, area.top()), Pos2::new(x, area.bottom())],
                    egui::Stroke::new(1.0, line),
                );
            }
            for r in 1..rows {
                let y = area.top() + r as f32 * (ch + gap) - gap / 2.0;
                painter.line_segment(
                    [Pos2::new(area.left(), y), Pos2::new(area.right(), y)],
                    egui::Stroke::new(1.0, line),
                );
            }
            if let Some(r) = rects.get(self.active) {
                painter.rect_stroke(
                    r.shrink(1.0),
                    0.0,
                    egui::Stroke::new(1.5, theme::SELECTION_BLUE),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    /// Paint one cell; only the active cell takes mouse input.
    fn paint_cell(&mut self, ui: &mut Ui, shared: &Shared, rect: Rect, k: usize, tiled: bool) {
        let active = k == self.active;
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);

        if self.panes[k].images.is_empty() {
            shadow_text(
                &painter,
                rect.center(),
                Align2::CENTER_CENTER,
                "No images in this series",
                FontId::proportional(16.0),
                Color32::from_gray(160),
            );
            return;
        }
        // A cell past the end of a tiled series stays blank.
        let Some(key) = self.panes[k].key() else { return };

        // Request this frame and, for the active cell, its neighbours.
        {
            let p = &self.panes[k];
            self.ensure_requested(shared, &p.images, p.image_idx, p.frame_idx);
            if active {
                let n = p.positions();
                if n > 1 {
                    let ahead = if self.tiled_series() { self.panes.len() } else { 1 };
                    let pos = self.panes[0].position();
                    let (next, prev) = ((pos + ahead) % n, (pos + n - 1) % n);
                    if p.images.len() == 1 {
                        self.ensure_requested(shared, &p.images, 0, next as u32);
                        self.ensure_requested(shared, &p.images, 0, prev as u32);
                    } else if self.tiled_series() {
                        self.ensure_requested(shared, &p.images, next, 0);
                        self.ensure_requested(shared, &p.images, prev, 0);
                    } else {
                        // The slice scrolled to first, then outwards from it,
                        // mostly the way the wheel is going.
                        let at = self
                            .scroll_target
                            .and_then(|id| p.images.iter().position(|i| i.id == id))
                            .unwrap_or(p.image_idx);
                        self.ensure_requested(shared, &p.images, at, 0);
                        let forward = self.scroll_direction >= 0;
                        let along = |by: usize, ahead: bool| {
                            if ahead == forward {
                                (at + by) % n
                            } else {
                                (at + n - by % n) % n
                            }
                        };
                        for by in 1..=READ_AHEAD {
                            self.read_ahead(shared, &p.images, along(by, true));
                            if by <= READ_BEHIND {
                                self.read_ahead(shared, &p.images, along(by, false));
                            }
                        }
                    }
                    // Playing a stack: half a second of images ahead.
                    if self.playing && p.images.len() > 1 && p.image().is_some_and(|i| i.number_of_frames <= 1) {
                        let at = p.image_idx;
                        for k in 1..=self.cine_ahead {
                            self.ensure_requested(shared, &p.images, (at + k) % p.images.len(), 0);
                        }
                    }
                }
            }
        }

        let Some(frame) = self.frames.get(&key).cloned() else {
            let msg = match &self.panes[k].load_error {
                Some(e) => format!("Could not decode image:\n{e}"),
                None => "Loading\u{2026}".to_string(),
            };
            shadow_text(
                &painter,
                rect.center(),
                Align2::CENTER_CENTER,
                &msg,
                FontId::proportional(15.0),
                Color32::from_gray(170),
            );
            return;
        };
        self.panes[k].load_error = None;

        // First display of a series: window from the stored series state or
        // the header, and fit to the cell.
        let series = self.series_of(k).clone();
        if self.panes[k].window_initialised_for != Some(series.id) {
            let p = &mut self.panes[k];
            match (series.window_center, series.window_width) {
                (Some(c), Some(w)) if w > 0.0 => {
                    p.view.window_center = c as f32;
                    p.view.window_width = w as f32;
                }
                _ => {
                    let (c, w) = frame.default_window();
                    p.view.window_center = c;
                    p.view.window_width = w;
                }
            }
            p.view.rotation_deg = series.rotation_angle as f32;
            p.view.flip_horizontal = series.x_flipped;
            p.view.flip_vertical = series.y_flipped;
            p.window_initialised_for = Some(series.id);
            // Cine plays at the rate the file was recorded at.
            if let Some(ms) = frame.meta.frame_time_ms.filter(|ms| *ms > 0.0) {
                if active {
                    self.fps = (1000.0 / ms as f32).clamp(1.0, 240.0);
                }
            }
        }
        let aspect = frame
            .meta
            .pixel_spacing
            .map(|[row, col]| if col > 0.0 { (row / col) as f32 } else { 1.0 })
            .unwrap_or(1.0);
        if self.panes[k].fit_pending {
            self.panes[k].view.fit(rect, frame.width, frame.height, aspect);
            self.panes[k].fit_pending = false;
        }

        let mut transform = ViewTransform::new(rect, frame.width, frame.height, aspect, self.panes[k].view);
        if let (true, Some(amm)) = (active, self.amm.as_mut()) {
            amm.input(ui, &response, &transform);
        } else if active && !self.session.following {
            self.handle_input(ui, &response, &mut transform, &frame, &key);
            self.panes[k].view = transform.state;
            // Input may have scrolled this cell to another image (or past the
            // end of a tiled series). This frame still paints the image it
            // began with: stopping here would leave the cell black for a frame,
            // which flickers on every step of the wheel.
            if self.panes[k].key() != Some(key) || self.scroll_target.is_some() {
                ui.ctx().request_repaint();
            }
        }
        let transform = ViewTransform::new(rect, frame.width, frame.height, aspect, self.panes[k].view);

        // The image itself.
        let mut loupe_source = None;
        if let Some(gpu) = self.gpu_for(shared, key) {
            let clut = Arc::new(
                shared
                    .cluts
                    .get(&self.panes[k].clut)
                    .cloned()
                    .unwrap_or_else(ruthos_dicom::Clut::grayscale),
            );
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ImageCallback {
                    key: self.id * 64 + k as u64,
                    image: gpu.clone(),
                    clut: clut.clone(),
                    transform,
                    inverted: frame.inverted,
                    interpolation: if self.bilinear {
                        Interpolation::Bilinear
                    } else {
                        Interpolation::Nearest
                    },
                },
            ));
            // The loupe draws the same image with its own key (cells use 0..32).
            loupe_source = Some(LoupeSource {
                key: self.id * 64 + 32 + k as u64,
                image: gpu,
                clut,
                inverted: frame.inverted,
            });
        }

        if let (true, Some(amm)) = (active, self.amm.as_ref()) {
            amm.draw(&painter, &transform);
        }

        // ROIs and drafts.
        self.ensure_rois_loaded(shared, key);
        if active {
            self.label_hits.retain(|(k, _, _)| *k != key);
            // Developer option: `RUTHOS_SELECT_ROI=<index>` selects a stored
            // measurement on the first frame shown, for screenshots.
            if self.selected_roi.is_none() && !self.dev_selected {
                if let Some(i) = std::env::var("RUTHOS_SELECT_ROI")
                    .ok()
                    .and_then(|v| v.trim().parse::<usize>().ok())
                {
                    if self.rois.get(&key).is_some_and(|l| i < l.len()) {
                        self.selected_roi = Some(i);
                        self.active_handle = 0;
                        self.dev_selected = true;
                    }
                }
            }
        }
        let hidden = (self.study.teaching_answer_key && !self.reveal_answers) || !self.show_rois;
        if let Some(list) = self.rois.get(&key).filter(|_| !hidden) {
            for (i, roi) in list.iter().enumerate() {
                let sel = active && self.selected_roi == Some(i);
                let label = roi.draw_opts(&painter, &transform, &frame, roi_color(sel), sel, self.show_labels);
                if sel {
                    roi.draw_handles(&painter, &transform, Some(self.active_handle), roi_color(true));
                }
                if self.show_labels && roi.lv_phase() == Some(MmodePhase::Systole) {
                    if let Some(fs) = self.fractional_shortening(&key, roi, &frame) {
                        let anchor = label
                            .map(|r| r.left_bottom())
                            .unwrap_or_else(|| transform.image_to_screen(roi.points()[3]) + Vec2::new(10.0, 24.0));
                        shadow_text(
                            &painter,
                            anchor + Vec2::new(0.0, 2.0),
                            Align2::LEFT_TOP,
                            &format!("FS {fs:.0}%"),
                            FontId::proportional(12.5),
                            roi_color(sel),
                        );
                    }
                }
                if active {
                    if let Some(r) = label {
                        self.label_hits.push((key, i, r));
                    }
                }
            }
        }
        if let Some(list) = self.session.overlay.get(&key).filter(|_| self.show_rois) {
            for (roi, colour) in list {
                roi.draw_opts(&painter, &transform, &frame, *colour, false, self.show_labels);
            }
        }
        self.draw_session(&painter, &transform, k);
        if active {
            if let Some((a, b)) = self.drag_draft {
                if let Some(kind) = draft_kind(self.tool, a, b) {
                    Roi::new(kind).draw(&painter, &transform, &frame, theme::ROI_DRAFT, false);
                }
            }
            if let Some(d) = &self.angle_draft {
                let roi = Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c });
                roi.draw(&painter, &transform, &frame, theme::ROI_DRAFT, false);
            }
            if let Some(d) = &self.mmode_draft {
                draw_mmode_draft(&painter, &transform, d, theme::ROI_DRAFT);
            }
            if let Some((pts, hover)) = &self.polygon_draft {
                draw_polygon_draft(&painter, &transform, pts, *hover, theme::ROI_DRAFT);
            }
            if let Some(d) = &self.two_line_draft {
                d.roi().draw(&painter, &transform, &frame, theme::ROI_DRAFT, false);
            }
            if let Some(pts) = &self.pencil_draft {
                let screen: Vec<Pos2> = pts.iter().map(|p| transform.image_to_screen(*p)).collect();
                painter.line(screen, egui::Stroke::new(1.5, theme::ROI_DRAFT));
            }
            // The brush and repulsor outline.
            if matches!(self.tool, Tool::Brush | Tool::Repulsor) {
                if let Some(h) = self.tool_hover.filter(|h| transform.rect.contains(*h)) {
                    painter.circle_stroke(h, self.brush_radius, egui::Stroke::new(1.0, theme::ROI_DRAFT));
                }
            }

            // The loupe, with the geometry being placed drawn through it.
            if let (true, Some(target), Some(source)) = (self.loupe.enabled, self.loupe_target, loupe_source.as_ref()) {
                let draft = self
                    .drag_draft
                    .and_then(|(a, b)| draft_kind(self.tool, a, b))
                    .map(Roi::new);
                let angle = self
                    .angle_draft
                    .as_ref()
                    .map(|d| Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c }));
                let selected = self
                    .selected_roi
                    .and_then(|i| self.rois.get(&key).and_then(|l| l.get(i)))
                    .cloned();
                let mmode = self.mmode_draft.as_ref();
                let polygon = self.polygon_draft.as_ref();
                let overlay = |p: &egui::Painter, lt: &ViewTransform| {
                    if let Some((pts, hover)) = polygon {
                        draw_polygon_draft(p, lt, pts, *hover, theme::ROI_DRAFT);
                    }
                    for roi in draft.iter().chain(angle.iter()) {
                        roi.draw(p, lt, &frame, theme::ROI_DRAFT, false);
                    }
                    if let Some(d) = mmode {
                        draw_mmode_draft(p, lt, d, theme::ROI_DRAFT);
                    }
                    if let Some(roi) = &selected {
                        roi.draw(p, lt, &frame, roi_color(true), false);
                    }
                };
                loupe::draw(ui, &painter, rect, &transform, target, &self.loupe, source, &overlay);
            }
        }

        // Annotations; small cells drop to the partial set.
        let p = &self.panes[k];
        let Some(image) = p.images.get(p.image_idx).cloned() else {
            return;
        };
        let level = if tiled && rect.width() < 420.0 && self.annotations == AnnotationLevel::Full {
            AnnotationLevel::Partial
        } else {
            self.annotations
        };
        let ctx = AnnotationContext {
            study: &self.study,
            series: &series,
            image: &image,
            frame: &frame,
            image_index: p.position(),
            image_count: p.positions(),
            transform: &transform,
            mouse_pixel: if active { self.mouse_pixel } else { None },
            level,
            bottom_right_inset: LOCK_CHIP.y + 4.0,
        };
        annotations::draw(&painter, rect, &ctx);

        // Lock control: a check box with a padlock.
        let chip = lock_chip_rect(rect);
        let locked = self.panes[k].locked;
        let hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| chip.contains(p));
        let color = if locked {
            theme::ROI_SELECTED
        } else if hovered {
            Color32::from_gray(235)
        } else {
            Color32::from_gray(175)
        };
        painter.rect_filled(chip.expand(2.0), 4.0, Color32::from_black_alpha(140));
        let box_rect = Rect::from_center_size(Pos2::new(chip.left() + 8.0, chip.center().y), Vec2::splat(12.0));
        painter.rect_stroke(box_rect, 2.0, egui::Stroke::new(1.2, color), egui::StrokeKind::Inside);
        if locked {
            painter.line_segment(
                [
                    box_rect.left_center() + Vec2::new(2.5, 0.5),
                    box_rect.center() + Vec2::new(-0.5, 3.0),
                ],
                egui::Stroke::new(1.6, color),
            );
            painter.line_segment(
                [
                    box_rect.center() + Vec2::new(-0.5, 3.0),
                    box_rect.right_top() + Vec2::new(-2.0, 2.5),
                ],
                egui::Stroke::new(1.6, color),
            );
        }
        painter.text(
            Pos2::new(chip.right() - 2.0, chip.center().y),
            Align2::RIGHT_CENTER,
            if locked { icons::LOCKED } else { icons::UNLOCKED },
            icons::font(14.0),
            color,
        );

        if self.playing && active {
            shadow_text(
                &painter,
                Pos2::new(rect.center().x, rect.top() + 6.0),
                Align2::CENTER_TOP,
                icons::PLAY,
                icons::font(14.0),
                Color32::from_gray(200),
            );
        }
    }

    fn handle_input(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        t: &mut ViewTransform,
        frame: &Frame,
        key: &FrameKey,
    ) {
        let hover = response.hover_pos();
        self.mouse_pixel = hover.and_then(|p| t.pixel_at(p));
        self.loupe_target = None;
        // Developer option: `RUTHOS_LOUPE_AT=x,y` shows the loupe at an image point.
        if let Some((x, y)) = std::env::var("RUTHOS_LOUPE_AT").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))
        }) {
            let image_point = Vec2::new(x, y);
            self.loupe_target = Some(LoupeTarget {
                image_point,
                pointer: t.image_to_screen(image_point),
            });
        }

        // The lock control in the bottom-right corner.
        let chip = lock_chip_rect(t.rect);
        if response.clicked_by(PointerButton::Primary)
            && response.interact_pointer_pos().is_some_and(|p| chip.contains(p))
        {
            let k = self.active;
            self.toggle_lock(k);
            return;
        }
        if self.active_locked() {
            return;
        }

        // Wheel: scroll images; ctrl+wheel / pinch: zoom.
        if response.hovered() {
            let (scroll, zoom_delta, ctrl) = ui.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    i.zoom_delta(),
                    i.modifiers.ctrl || i.modifiers.command,
                )
            });
            if (zoom_delta - 1.0).abs() > 1e-3 {
                self.zoom_about(t, hover, zoom_delta);
            } else if !ctrl && scroll.abs() > 0.0 {
                // Accumulate small deltas into discrete steps.
                let id = egui::Id::unique(("wheel-acc", self.id));
                let acc = ui.ctx().data_mut(|d| {
                    let v: &mut f32 = d.get_temp_mut_or_default(id);
                    *v += scroll;
                    let steps = (*v / 40.0).trunc();
                    *v -= steps * 40.0;
                    steps
                });
                if steps_nonzero(acc) {
                    self.step_image(-(acc as i64));
                }
            }
        }

        let pos = response.interact_pointer_pos();

        // A click on a measurement's label opens the naming popup instead of
        // running a tool.
        if self.assign.is_some() {
            return;
        }
        if response.clicked_by(PointerButton::Primary) {
            if let Some(p) = pos {
                if let Some((k, i, _)) = self
                    .label_hits
                    .iter()
                    .find(|(k, _, r)| k == key && r.expand(3.0).contains(p))
                    .copied()
                {
                    self.assign = Some(AssignPopup {
                        key: k,
                        index: i,
                        pos: p,
                        custom: String::new(),
                    });
                    self.selected_roi = Some(i);
                    return;
                }
            }
        }

        // Editing a selected measurement comes before any tool.
        if self.edit_input(ui, response, t, key) {
            return;
        }

        // Alt while hovering with a measuring tool previews the loupe.
        if self.tool.is_roi() && self.drag_draft.is_none() && self.angle_draft.is_none() {
            if let Some(hp) = hover.filter(|_| ui.input(|i| i.modifiers.alt)) {
                self.loupe_target = Some(LoupeTarget {
                    image_point: t.screen_to_image(hp),
                    pointer: hp,
                });
            }
        }

        self.tool_hover = hover;
        // Drafts of another tool are dropped when the tool changes.
        if !matches!(self.tool, Tool::Cobb | Tool::DynamicAngle) {
            self.two_line_draft = None;
        }
        if self.tool != Tool::Pencil {
            self.pencil_draft = None;
        }
        // [ and ] size the brush and the repulsor.
        if matches!(self.tool, Tool::Brush | Tool::Repulsor) && !ui.ctx().memory(|m| m.focused().is_some()) {
            let (smaller, larger) = ui.input(|i| (i.key_pressed(Key::OpenBracket), i.key_pressed(Key::CloseBracket)));
            if smaller {
                self.brush_radius = (self.brush_radius / 1.25).max(2.0);
            }
            if larger {
                self.brush_radius = (self.brush_radius * 1.25).min(200.0);
            }
        }

        // Each mouse button drives the tool assigned to it (Tools menu).
        for (button, tool) in [
            (PointerButton::Primary, self.tool),
            (PointerButton::Secondary, self.right_tool),
            (PointerButton::Middle, Tool::Move),
        ] {
            self.apply_tool(ui, response, t, frame, key, tool, button, hover, pos);
        }

        // Double-click: fit (Horos resets on double click with some tools).
        if response.double_clicked_by(PointerButton::Primary) && !self.tool.is_roi() {
            self.panes[self.active].fit_pending = true;
        }

        // Right-click menu: the Measurements menu, plus the view.
        response.context_menu(|ui| {
            self.measurements_menu(ui, true);
        });
        if false {
            self.rois.remove(key);
            self.selected_roi = None;
            self.dirty_rois.insert(*key);
        }
    }

    /// Run one tool for one mouse button.
    #[allow(clippy::too_many_arguments)]
    fn apply_tool(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        t: &mut ViewTransform,
        frame: &Frame,
        key: &FrameKey,
        tool: Tool,
        button: PointerButton,
        hover: Option<Pos2>,
        pos: Option<Pos2>,
    ) {
        match tool {
            Tool::WindowLevel => {
                if response.drag_started_by(button) {
                    self.wl_drag_anchor = Some((t.state.window_center, t.state.window_width));
                }
                if response.dragged_by(button) {
                    let d = response.drag_delta();
                    let scale = (t.state.window_width.abs() / 256.0).max(0.5);
                    t.state.window_width = (t.state.window_width + d.x * scale).max(1.0);
                    t.state.window_center += d.y * scale;
                }
                if response.drag_stopped_by(button) {
                    self.wl_drag_anchor = None;
                }
            }
            Tool::Move => {
                if response.dragged_by(button) {
                    t.state.pan += response.drag_delta();
                }
            }
            Tool::Zoom => {
                if response.dragged_by(button) {
                    let dy = response.drag_delta().y;
                    self.zoom_about(t, Some(t.rect.center() + t.state.pan), (-dy * 0.01).exp());
                }
            }
            Tool::Rotate => {
                if response.dragged_by(button) {
                    t.state.rotation_deg = (t.state.rotation_deg + response.drag_delta().x * 0.5).rem_euclid(360.0);
                }
            }
            Tool::Angle => {
                let img_pos = pos.map(|p| t.screen_to_image(p));
                let fine = ui.input(|i| i.modifiers.shift) && self.loupe.fine_movement;
                let fine_step =
                    ui.input(|i| i.pointer.delta()) / (self.loupe.magnification.max(1.0) * t.state.zoom.max(1e-6));
                if let Some(d) = &mut self.angle_draft {
                    if d.b_fixed {
                        if let Some(hp) = hover {
                            d.c = if fine { d.c + fine_step } else { t.screen_to_image(hp) };
                            self.loupe_target = Some(LoupeTarget {
                                image_point: d.c,
                                pointer: hp,
                            });
                        }
                        if response.clicked_by(button) {
                            let roi = Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c });
                            if !roi.is_degenerate() {
                                self.rois.entry(*key).or_default().push(roi);
                                self.dirty_rois.insert(*key);
                            }
                            self.angle_draft = None;
                        }
                    } else {
                        if let Some(p) = img_pos {
                            d.b = if fine { d.b + fine_step } else { p };
                            d.c = d.b;
                            if let Some(sp) = pos {
                                self.loupe_target = Some(LoupeTarget {
                                    image_point: d.b,
                                    pointer: sp,
                                });
                            }
                        }
                        if response.drag_stopped_by(button) {
                            if (d.b - d.a).length() < 1.0 {
                                self.angle_draft = None;
                            } else {
                                d.b_fixed = true;
                            }
                        }
                    }
                } else if response.drag_started_by(button) {
                    let origin = press_origin(ui).or(pos);
                    if let Some(p) = origin.map(|p| t.screen_to_image(p)) {
                        if !self.try_select_roi(t, key, origin) {
                            self.angle_draft = Some(AngleDraft {
                                a: p,
                                b: p,
                                b_fixed: false,
                                c: p,
                            });
                        }
                    }
                } else if response.clicked_by(button) {
                    self.try_select_roi(t, key, pos);
                }
            }
            Tool::LvMmode => {
                // Four clicks along one line: septum top, septum bottom,
                // free wall top, free wall bottom. In an M-mode strip the
                // first click fixes the time column; on a 2D image the line
                // runs from the first point in any direction, and the inner
                // points are placed on the line from the first to the last.
                if let Some(hp) = hover {
                    let ip = t.screen_to_image(hp);
                    if let Some(d) = &mut self.mmode_draft {
                        let snapped = d.snap(ip);
                        d.hover = Some(snapped);
                        self.loupe_target = Some(LoupeTarget {
                            image_point: snapped,
                            pointer: hp,
                        });
                    }
                }
                if response.clicked_by(button) {
                    if let Some(p) = pos.map(|p| t.screen_to_image(p)) {
                        let mut message: Option<String> = None;
                        let mut finished: Option<(bool, [Vec2; 4])> = None;
                        let mut start = false;
                        match &mut self.mmode_draft {
                            None => start = true,
                            Some(d) => {
                                let q = d.snap(p);
                                let last = d.points.last().map(|l| d.progress(*l)).unwrap_or(f32::NEG_INFINITY);
                                if d.progress(q) <= last + 0.5 {
                                    message = Some(if d.mmode {
                                        "Each point must be deeper than the last: septum top, septum bottom, free wall top, free wall bottom".into()
                                    } else {
                                        "Each point must be further along the line than the last: septum top, septum bottom, free wall top, free wall bottom".into()
                                    });
                                } else {
                                    d.points.push(q);
                                    match d.points.len() {
                                        2 => message = Some("LV: now the top of the free wall".into()),
                                        3 => message = Some("LV: now the bottom of the free wall".into()),
                                        4 => {
                                            let mut pts = [d.points[0], d.points[1], d.points[2], d.points[3]];
                                            if !d.mmode {
                                                // Straighten: the inner points onto the line.
                                                let (a, b) = (pts[0], pts[3]);
                                                let dir = b - a;
                                                let len2 = dir.length_sq().max(1e-6);
                                                for q in pts.iter_mut().take(3).skip(1) {
                                                    let t = ((*q - a).dot(dir) / len2).clamp(0.0, 1.0);
                                                    *q = a + dir * t;
                                                }
                                            }
                                            finished = Some((d.mmode, pts));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        if start {
                            if self.try_select_roi(t, key, pos) {
                                // An existing measurement was picked instead.
                            } else {
                                let mmode = frame
                                    .meta
                                    .us_region_at(p.x as f64, p.y as f64)
                                    .is_some_and(|r| r.is_time_across());
                                self.mmode_draft = Some(MmodeDraft {
                                    mmode,
                                    points: vec![p],
                                    hover: None,
                                });
                                message = Some(format!(
                                    "LV {}: septum top placed; next the septum's ventricular side. Backspace removes a point, Escape abandons",
                                    if mmode { "M-mode" } else { "2D" }
                                ));
                            }
                        }
                        if let Some((mmode, pts)) = finished {
                            self.mmode_draft = None;
                            let phase = if mmode {
                                self.next_mmode_phase_at(key, pts[0].x)
                            } else {
                                self.next_mmode_phase()
                            };
                            let roi = Roi::new(RoiKind::Lv { phase, mmode, p: pts });
                            let text = roi.value_text(frame);
                            let list = self.rois.entry(*key).or_default();
                            list.push(roi);
                            self.selected_roi = Some(list.len() - 1);
                            self.active_handle = 0;
                            self.dirty_rois.insert(*key);
                            message = Some(format!("{}: {text}", phase.label()));
                        }
                        if let Some(m) = message {
                            self.set_status(m);
                        }
                    }
                }
            }
            Tool::Polygon | Tool::OpenPolygon => {
                // Click point by point; close with a double-click, a click
                // on the first point, or Enter. An open polygon ends with a
                // double-click or Enter and is measured along its length.
                let open = tool == Tool::OpenPolygon;
                if let (Some(hp), Some((_, hover_pt))) = (hover, &mut self.polygon_draft) {
                    let ip = t.screen_to_image(hp);
                    *hover_pt = Some(ip);
                    self.loupe_target = Some(LoupeTarget {
                        image_point: ip,
                        pointer: hp,
                    });
                }
                let double = response.double_clicked_by(button);
                if response.clicked_by(button) || double {
                    if let Some(p) = pos.map(|p| t.screen_to_image(p)) {
                        let close_to_first = !open
                            && self.polygon_draft.as_ref().is_some_and(|(pts, _)| {
                                pts.len() >= 3 && pos.is_some_and(|sp| (t.image_to_screen(pts[0]) - sp).length() < 8.0)
                            });
                        match &mut self.polygon_draft {
                            None if !double => {
                                if !self.try_select_roi(t, key, pos) {
                                    self.polygon_draft = Some((vec![p], None));
                                    self.polygon_open = open;
                                    self.set_status(if open {
                                        "Open polygon: click each point; double-click or press Enter to finish"
                                    } else {
                                        "Polygon: click each point; double-click, click the first point or press Enter to close"
                                    });
                                }
                            }
                            None => {}
                            Some((pts, _)) => {
                                if double || close_to_first {
                                    if pts.len() >= if open { 2 } else { 3 } {
                                        let roi = Roi::new(if open {
                                            RoiKind::Polyline { points: pts.clone() }
                                        } else {
                                            RoiKind::Polygon { points: pts.clone() }
                                        });
                                        self.polygon_draft = None;
                                        if !roi.is_degenerate() {
                                            let list = self.rois.entry(*key).or_default();
                                            list.push(roi);
                                            self.selected_roi = Some(list.len() - 1);
                                            self.dirty_rois.insert(*key);
                                        }
                                    }
                                } else if pts.last().is_none_or(|l| (*l - p).length() > 0.5) {
                                    pts.push(p);
                                }
                            }
                        }
                    }
                }
            }
            Tool::Cobb | Tool::DynamicAngle => {
                // Drag the first line, then drag the second.
                let cobb = tool == Tool::Cobb;
                if response.drag_started_by(button) {
                    let origin = press_origin(ui).or(pos);
                    if let Some(p) = origin.map(|p| t.screen_to_image(p)) {
                        match &mut self.two_line_draft {
                            Some(d) if d.cobb == cobb => d.second = Some((p, p)),
                            _ => {
                                if !self.try_select_roi(t, key, origin) {
                                    self.two_line_draft = Some(TwoLineDraft {
                                        cobb,
                                        first: (p, p),
                                        second: None,
                                    });
                                }
                            }
                        }
                    }
                }
                if response.dragged_by(button) {
                    if let (Some(d), Some(sp)) = (&mut self.two_line_draft, pos) {
                        let p = t.screen_to_image(sp);
                        match &mut d.second {
                            Some((_, b)) => *b = p,
                            None => d.first.1 = p,
                        }
                        self.loupe_target = Some(LoupeTarget {
                            image_point: p,
                            pointer: sp,
                        });
                    }
                }
                if response.drag_stopped_by(button) {
                    if let Some(d) = self.two_line_draft {
                        match d.second {
                            None if (d.first.1 - d.first.0).length() < 1.0 => self.two_line_draft = None,
                            None => self.set_status("Now drag the second line"),
                            Some(_) => {
                                self.two_line_draft = None;
                                let roi = d.roi();
                                if !roi.is_degenerate() {
                                    let list = self.rois.entry(*key).or_default();
                                    list.push(roi);
                                    self.selected_roi = Some(list.len() - 1);
                                    self.dirty_rois.insert(*key);
                                }
                            }
                        }
                    }
                } else if response.clicked_by(button) && self.two_line_draft.is_none() {
                    self.try_select_roi(t, key, pos);
                }
            }
            Tool::Pencil => {
                // Drag around the outline; it closes when the button is let go.
                if response.drag_started_by(button) {
                    let origin = press_origin(ui).or(pos);
                    if let Some(p) = origin.map(|p| t.screen_to_image(p)) {
                        self.pencil_draft = Some(vec![p]);
                    }
                }
                if response.dragged_by(button) {
                    if let (Some(pts), Some(sp)) = (&mut self.pencil_draft, pos) {
                        let last = pts.last().map(|l| t.image_to_screen(*l));
                        if last.is_none_or(|l| (l - sp).length() >= 2.0) {
                            pts.push(t.screen_to_image(sp));
                        }
                    }
                }
                if response.drag_stopped_by(button) {
                    if let Some(pts) = self.pencil_draft.take() {
                        let roi = Roi::new(RoiKind::Pencil { points: pts });
                        if !roi.is_degenerate() {
                            let list = self.rois.entry(*key).or_default();
                            list.push(roi);
                            self.selected_roi = Some(list.len() - 1);
                            self.dirty_rois.insert(*key);
                        }
                    }
                } else if response.clicked_by(button) {
                    self.try_select_roi(t, key, pos);
                }
            }
            Tool::Brush => {
                // Paint into the selected brush region, or a new one. Alt
                // erases. The radius follows the zoom, so the brush is the
                // same size on screen.
                let erase = ui.input(|i| i.modifiers.alt);
                let radius = self.brush_radius / t.state.zoom.max(1e-6);
                if response.drag_started_by(button) || response.clicked_by(button) {
                    let origin = press_origin(ui).or(pos);
                    if let Some(p) = origin.map(|p| t.screen_to_image(p)) {
                        let list = self.rois.entry(*key).or_default();
                        let target = self
                            .selected_roi
                            .filter(|i| matches!(list.get(*i).map(|r| &r.kind), Some(RoiKind::Brush { .. })));
                        let index = match target {
                            Some(i) => i,
                            None if erase => {
                                // Erase from whichever region is under the pointer.
                                match list
                                    .iter()
                                    .position(|r| matches!(&r.kind, RoiKind::Brush { mask } if mask.contains(p)))
                                {
                                    Some(i) => i,
                                    None => return,
                                }
                            }
                            None => {
                                list.push(Roi::new(RoiKind::Brush {
                                    mask: ruthos_brush_mask(),
                                }));
                                list.len() - 1
                            }
                        };
                        if let Some(RoiKind::Brush { mask }) = list.get_mut(index).map(|r| &mut r.kind) {
                            mask.paint(p, radius, erase);
                        }
                        self.selected_roi = Some(index);
                        self.brush_stroke = Some((index, p));
                        self.dirty_rois.insert(*key);
                    }
                }
                if response.dragged_by(button) {
                    if let (Some((index, last)), Some(sp)) = (self.brush_stroke, pos) {
                        let p = t.screen_to_image(sp);
                        if let Some(RoiKind::Brush { mask }) = self
                            .rois
                            .get_mut(key)
                            .and_then(|l| l.get_mut(index))
                            .map(|r| &mut r.kind)
                        {
                            // Dabs at most half a radius apart along the stroke.
                            let steps = ((p - last).length() / (radius * 0.5).max(0.5)).ceil().max(1.0) as usize;
                            for k in 1..=steps {
                                mask.paint(last + (p - last) * (k as f32 / steps as f32), radius, erase);
                            }
                        }
                        self.brush_stroke = Some((index, p));
                        self.dirty_rois.insert(*key);
                    }
                }
                if response.drag_stopped_by(button) || response.clicked_by(button) {
                    if let Some((index, _)) = self.brush_stroke.take() {
                        // A region erased to nothing goes.
                        let empty = self
                            .rois
                            .get(key)
                            .and_then(|l| l.get(index))
                            .is_some_and(|r| r.is_degenerate());
                        if empty {
                            if let Some(list) = self.rois.get_mut(key) {
                                list.remove(index);
                            }
                            self.selected_roi = None;
                        }
                    }
                }
            }
            Tool::Repulsor => {
                // Points of outlines inside the circle are pushed to its
                // edge; where points spread apart, new ones are added so
                // the outline stays smooth.
                if response.dragged_by(button) {
                    if let Some(sp) = pos {
                        let c = t.screen_to_image(sp);
                        let radius = self.brush_radius / t.state.zoom.max(1e-6);
                        let mut changed = false;
                        if let Some(list) = self.rois.get_mut(key) {
                            for roi in list.iter_mut() {
                                let closed = matches!(roi.kind, RoiKind::Polygon { .. } | RoiKind::Pencil { .. });
                                let points = match &mut roi.kind {
                                    RoiKind::Polygon { points }
                                    | RoiKind::Pencil { points }
                                    | RoiKind::Polyline { points } => points,
                                    _ => continue,
                                };
                                changed |= repulse(points, c, radius, closed);
                            }
                        }
                        if changed {
                            self.dirty_rois.insert(*key);
                        }
                    }
                }
            }
            Tool::Text => {
                if response.clicked_by(button) || response.double_clicked_by(button) {
                    if self.try_select_roi(t, key, pos) {
                        // A second click on a selected note edits it.
                        let index = self.selected_roi.unwrap_or(0);
                        if let Some(roi) = self.rois.get(key).and_then(|l| l.get(index)) {
                            if let (RoiKind::Text { p }, true) = (&roi.kind, response.double_clicked_by(button)) {
                                self.text_prompt = Some(TextPrompt {
                                    key: *key,
                                    at: *p,
                                    screen: t.image_to_screen(*p),
                                    text: roi.name.clone().unwrap_or_default(),
                                    edit: Some(index),
                                    focus: true,
                                });
                            }
                        }
                    } else if let Some(sp) = pos {
                        self.text_prompt = Some(TextPrompt {
                            key: *key,
                            at: t.screen_to_image(sp),
                            screen: sp,
                            text: String::new(),
                            edit: None,
                            focus: true,
                        });
                    }
                }
            }
            Tool::Point => {
                if response.clicked_by(button) && !self.try_select_roi(t, key, pos) {
                    if let Some(p) = pos.map(|p| t.screen_to_image(p)) {
                        // Anywhere on the image: a colour image (ultrasound)
                        // has no single value, but a point still marks it.
                        let inside = p.x >= 0.0 && p.y >= 0.0 && p.x < frame.width as f32 && p.y < frame.height as f32;
                        if inside {
                            self.rois.entry(*key).or_default().push(Roi::new(RoiKind::Point { p }));
                            self.dirty_rois.insert(*key);
                        }
                    }
                }
            }
            Tool::Length | Tool::Rectangle | Tool::Oval | Tool::Arrow => {
                if response.drag_started_by(button) {
                    let origin = press_origin(ui).or(pos);
                    if let Some(p) = origin.map(|p| t.screen_to_image(p)) {
                        if !self.try_select_roi(t, key, origin) {
                            self.drag_draft = Some((p, p));
                        }
                    }
                }
                if response.dragged_by(button) {
                    if let (Some((a, prev_b)), Some(p)) = (self.drag_draft, pos) {
                        // Shift: fine placement, pointer movement scaled by
                        // the reciprocal of the loupe magnification.
                        let b = if ui.input(|i| i.modifiers.shift) && self.loupe.fine_movement {
                            prev_b
                                + response.drag_delta() / (self.loupe.magnification.max(1.0) * t.state.zoom.max(1e-6))
                        } else {
                            t.screen_to_image(p)
                        };
                        self.drag_draft = Some((a, b));
                        self.loupe_target = Some(LoupeTarget {
                            image_point: b,
                            pointer: p,
                        });
                    }
                }
                if response.drag_stopped_by(button) {
                    if let Some((a, b)) = self.drag_draft.take() {
                        if let Some(kind) = draft_kind(tool, a, b) {
                            let roi = Roi::new(kind);
                            if !roi.is_degenerate() {
                                let list = self.rois.entry(*key).or_default();
                                list.push(roi);
                                self.selected_roi = Some(list.len() - 1);
                                self.dirty_rois.insert(*key);
                            }
                        }
                    }
                } else if response.clicked_by(button) {
                    self.try_select_roi(t, key, pos);
                }
            }
        }
    }

    fn try_select_roi(&mut self, t: &ViewTransform, key: &FrameKey, pos: Option<Pos2>) -> bool {
        let Some(p) = pos else { return false };
        let Some(list) = self.rois.get(key) else {
            self.selected_roi = None;
            return false;
        };
        let mut best: Option<(usize, f32)> = None;
        for (i, roi) in list.iter().enumerate() {
            let d = roi.screen_distance(t, p);
            if d < 8.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        match best {
            Some((i, _)) => {
                if self.selected_roi != Some(i) {
                    self.active_handle = 0;
                }
                self.selected_roi = Some(i);
                true
            }
            None => {
                self.selected_roi = None;
                false
            }
        }
    }

    /// Zoom keeping the image point under `anchor` fixed on screen.
    fn zoom_about(&mut self, t: &mut ViewTransform, anchor: Option<Pos2>, factor: f32) {
        let anchor = anchor.unwrap_or(t.rect.center());
        let before = t.screen_to_image(anchor);
        t.state.zoom = (t.state.zoom * factor).clamp(0.02, 64.0);
        let after = t.image_to_screen(before);
        t.state.pan += anchor - after;
    }
}

/// Parse `series:2x2` / `study:1x2` (developer option).
fn parse_tiling(v: &str) -> Option<Tiling> {
    let (mode, grid) = v.trim().split_once(':')?;
    let mode = match mode {
        "series" => TilingMode::Series,
        "study" => TilingMode::Study,
        _ => return None,
    };
    let (r, c) = grid.split_once('x')?;
    Some(Tiling {
        mode,
        rows: r.parse().ok()?,
        cols: c.parse().ok()?,
    })
}

/// The plain name of a measurement kind, for unnamed rows in copied values.
/// The M-mode line while it is being placed: the column from the first
/// depth down to the pointer, with a tick at each placed depth.
fn draw_mmode_draft(painter: &egui::Painter, t: &ViewTransform, d: &MmodeDraft, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let Some(first) = d.points.first() else { return };
    let last = *d.points.last().unwrap_or(first);
    let tip = d.hover.unwrap_or(last);
    let start = t.image_to_screen(*first);
    let end = t.image_to_screen(tip);
    painter.line_segment([start, end], stroke);
    let along = end - start;
    let unit = if along.length() > 1e-3 {
        along / along.length()
    } else {
        Vec2::Y
    };
    let normal = Vec2::new(-unit.y, unit.x);
    for q in &d.points {
        let p = t.image_to_screen(*q);
        painter.line_segment([p - normal * 7.0, p + normal * 7.0], stroke);
    }
    let names = ["septum top", "septum bottom", "free wall top", "free wall bottom"];
    if let Some(name) = names.get(d.points.len()) {
        shadow_text(
            painter,
            end + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(12.0),
            color,
        );
    }
}

/// A polygon being clicked out: its points, and the edge to the pointer.
fn draw_polygon_draft(painter: &egui::Painter, t: &ViewTransform, pts: &[Vec2], hover: Option<Vec2>, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let mut screen: Vec<Pos2> = pts.iter().map(|p| t.image_to_screen(*p)).collect();
    if let Some(h) = hover {
        screen.push(t.image_to_screen(h));
    }
    if screen.len() >= 2 {
        painter.line(screen.clone(), stroke);
    }
    for p in &screen[..pts.len()] {
        painter.circle_filled(*p, 2.5, color);
    }
    // The first point is where a click closes the outline.
    if let Some(first) = screen.first() {
        painter.circle_stroke(*first, 6.0, egui::Stroke::new(1.0, color));
    }
}

/// Where the current press began, in screen points.
fn press_origin(ui: &Ui) -> Option<Pos2> {
    ui.input(|i| i.pointer.press_origin())
}

fn kind_label(kind: &RoiKind) -> &'static str {
    match kind {
        RoiKind::Length { .. } => "Caliper",
        RoiKind::Polygon { .. } => "Polygon",
        RoiKind::Lv { phase, mmode, .. } => match (phase, mmode) {
            (MmodePhase::Diastole, true) => "LV M-mode (diastole)",
            (MmodePhase::Systole, true) => "LV M-mode (systole)",
            (MmodePhase::Diastole, false) => "LV 2D (diastole)",
            (MmodePhase::Systole, false) => "LV 2D (systole)",
        },
        RoiKind::Angle { .. } => "Angle",
        RoiKind::Rect { .. } => "Rectangle",
        RoiKind::Oval { .. } => "Ellipse",
        RoiKind::Arrow { .. } => "Arrow",
        RoiKind::Point { .. } => "Point",
        RoiKind::TwoLines { cobb: true, .. } => "Cobb angle",
        RoiKind::TwoLines { cobb: false, .. } => "Dynamic angle",
        RoiKind::Polyline { .. } => "Open polygon",
        RoiKind::Pencil { .. } => "Pencil",
        RoiKind::Brush { .. } => "Brush",
        RoiKind::Text { .. } => "Text",
    }
}

/// The outline length of a closed measurement, in millimetres, when the
/// image is calibrated.
fn perimeter_mm(roi: &Roi, frame: &Frame) -> Option<f64> {
    let outline: Vec<Vec2> = match &roi.kind {
        RoiKind::Polygon { points } | RoiKind::Pencil { points } => points.clone(),
        RoiKind::Rect { a, b } => vec![*a, Vec2::new(b.x, a.y), *b, Vec2::new(a.x, b.y)],
        RoiKind::Oval { a, b } => {
            let c = (*a + *b) / 2.0;
            let (rx, ry) = (((b.x - a.x) / 2.0).abs(), ((b.y - a.y) / 2.0).abs());
            (0..96)
                .map(|i| {
                    let t = i as f32 / 96.0 * std::f32::consts::TAU;
                    c + Vec2::new(rx * t.cos(), ry * t.sin())
                })
                .collect()
        }
        _ => return None,
    };
    if outline.len() < 3 {
        return None;
    }
    let mut total = 0.0;
    for i in 0..outline.len() {
        let (p, q) = (outline[i], outline[(i + 1) % outline.len()]);
        total += frame
            .meta
            .length([p.x as f64, p.y as f64], [q.x as f64, q.y as f64])
            .ok()?
            .mm;
    }
    Some(total)
}

fn ruthos_brush_mask() -> crate::roi::BrushMask {
    crate::roi::BrushMask::default()
}

/// Push the points of an outline that lie within `radius` of `c` out to
/// the circle, and add points where neighbours have spread more than half
/// a radius apart. Returns whether anything moved.
fn repulse(points: &mut Vec<Vec2>, c: Vec2, radius: f32, closed: bool) -> bool {
    let push = |points: &mut Vec<Vec2>| {
        let mut moved = false;
        for q in points.iter_mut() {
            let d = *q - c;
            let len = d.length();
            if len < radius - 1e-4 {
                let dir = if len > 1e-4 { d / len } else { Vec2::X };
                *q = c + dir * radius;
                moved = true;
            }
        }
        moved
    };
    if !push(points) {
        return false;
    }
    // Fill the gaps the push opened, then push the new points out too;
    // a few rounds settle the outline around the circle.
    let gap = (radius * 0.5).max(1.0);
    for _ in 0..12 {
        let n = points.len();
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let a = points[i];
            out.push(a);
            let next = if i + 1 < n {
                Some(points[i + 1])
            } else if closed {
                points.first().copied()
            } else {
                None
            };
            if let Some(b) = next {
                let len = (b - a).length();
                if len > gap {
                    let k = (len / gap).ceil() as usize;
                    for j in 1..k {
                        out.push(a + (b - a) * (j as f32 / k as f32));
                    }
                }
            }
        }
        *points = out;
        if !push(points) {
            break;
        }
    }
    true
}

fn steps_nonzero(steps: f32) -> bool {
    steps.abs() >= 1.0
}

fn draft_kind(tool: Tool, a: Vec2, b: Vec2) -> Option<RoiKind> {
    Some(match tool {
        Tool::Length => RoiKind::Length { a, b },
        Tool::Rectangle => RoiKind::Rect { a, b },
        Tool::Oval => RoiKind::Oval { a, b },
        Tool::Arrow => RoiKind::Arrow { a, b },
        _ => return None,
    })
}

#[allow(dead_code)]
fn rect_from_points(a: Pos2, b: Pos2) -> Rect {
    Rect::from_two_pos(a, b)
}

#[cfg(test)]
mod cine_tests {
    use super::*;

    #[test]
    fn the_clock_keeps_real_time_by_skipping() {
        // 120 fps for 1/60 s moves two frames: one is skipped on a 60 Hz screen.
        assert_eq!(cine_position(0, 1.0 / 60.0 + 1e-4, 120.0, 64), 2);
        // A second at 30 fps is 30 frames, wrapping round a 25-frame loop.
        assert_eq!(cine_position(0, 1.0, 30.0, 25), 5);
        assert_eq!(cine_position(10, 0.0, 30.0, 25), 10);
        assert_eq!(cine_position(3, 1.0, 30.0, 0), 0);
    }

    #[test]
    fn the_speed_that_shows_every_frame() {
        assert_eq!(speed_for_every_frame(60.0, 60.0), Some(1.0));
        assert_eq!(speed_for_every_frame(110.0, 60.0), Some(0.54));
        assert_eq!(speed_for_every_frame(200.0, 60.0), Some(0.3));
        assert_eq!(speed_for_every_frame(2000.0, 60.0), None);
        assert_eq!(speed_for_every_frame(200.0, 144.0), Some(0.72));
        assert_eq!(speed_label(0.333), "0.33\u{d7}");
        assert_eq!(speed_label(0.25), "0.25\u{d7}");
        assert_eq!(speed_label(1.0), "1\u{d7}");
        assert_eq!(snap_refresh(118.7), 120.0);
        assert_eq!(snap_refresh(59.2), 60.0);
        assert_eq!(snap_refresh(143.0), 144.0);
        assert_eq!(snap_refresh(87.3), 90.0);
        assert_eq!(snap_refresh(80.4), 80.0);
    }
}

#[cfg(test)]
mod repulsor_tests {
    use super::*;

    #[test]
    fn the_repulsor_pushes_points_out_and_keeps_the_outline_smooth() {
        let mut pts = vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)];
        assert!(!repulse(&mut pts, Vec2::new(50.0, 50.0), 5.0, true), "nothing near");
        // A circle pressed into the bottom edge, a little off its line.
        let c = Vec2::new(8.0, -1.0);
        assert!(repulse(&mut pts, c, 4.0, true));
        assert!(pts.iter().all(|p| (*p - c).length() >= 4.0 - 1e-3));
        // Points were added where the gaps opened up.
        assert!(pts.len() > 3);
        assert!(pts.windows(2).all(|w| (w[1] - w[0]).length() <= 4.0 + 1e-3));
    }
}
