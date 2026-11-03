//! The 2D viewer window (Horos: ViewerController + DCMView).
//!
//! One instance per open study, shown in its own native viewport. Holds the
//! series list, the current image, decoded-frame and GPU caches, the view
//! state, the active tool and the ROIs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use egui::{Align2, Color32, FontId, Key, PointerButton, Pos2, Rect, Sense, Ui, Vec2, ViewportId};

use ruthos_core::{ImageRow, SeriesRow, StudyRow};

use ruthos_dicom::Frame;
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::annotations::{self, AnnotationContext, AnnotationLevel};
use crate::loader::FrameKey;
use crate::roi::{roi_color, Roi, RoiKind};
use crate::shared::Shared;
use crate::icons;
use crate::theme;
use crate::icons::Bitmap;
use crate::widgets::{
    menu_check, menu_heading, menu_item, menu_item_enabled, shadow_text, thumbnail_cell, tool_menu_row, toolbar_caption,
    toolbar_separator, ToolbarButton,
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
}

impl Tool {
    pub const ALL: [Tool; 10] = [
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
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::WindowLevel => "WL/WW",
            Tool::Move => "Move",
            Tool::Zoom => "Zoom",
            Tool::Rotate => "Rotate",
            Tool::Length => "Length",
            Tool::Angle => "Angle",
            Tool::Rectangle => "Rectangle",
            Tool::Oval => "Oval",
            Tool::Arrow => "Arrow",
            Tool::Point => "Point",
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
            Tool::Arrow => icons::TOOL_ARROW,
            Tool::Point => icons::TOOL_POINT,
        }
    }

    /// The Horos artwork for the tool.
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
            Tool::Arrow => Bitmap::Arrow,
            Tool::Point => Bitmap::Point,
        }
    }

    fn is_roi(self) -> bool {
        matches!(
            self,
            Tool::Length | Tool::Angle | Tool::Rectangle | Tool::Oval | Tool::Arrow | Tool::Point
        )
    }
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
        Some(FrameKey { image_id: img.id, frame: self.frame_idx })
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
    rois_loaded: HashSet<FrameKey>,
    /// Frames whose ROIs changed and must be written back.
    dirty_rois: HashSet<FrameKey>,
    /// Label rectangles painted last frame, for click-to-name.
    label_hits: Vec<(FrameKey, usize, Rect)>,
    /// The "assign measurement" popup, when open.
    assign: Option<AssignPopup>,
    drag_draft: Option<(Vec2, Vec2)>,
    angle_draft: Option<AngleDraft>,
    pub selected_roi: Option<usize>,

    pub playing: bool,
    pub fps: f32,
    last_tick: Option<Instant>,

    frames: HashMap<FrameKey, Arc<Frame>>,
    frame_order: VecDeque<FrameKey>,
    gpu: HashMap<FrameKey, Arc<GpuImage>>,
    gpu_order: VecDeque<FrameKey>,

    pub close_requested: bool,
    /// Teaching copy with an answer key: measurements hidden until revealed.
    pub reveal_answers: bool,
    mouse_pixel: Option<(i64, i64)>,
    status: Option<(String, Instant)>,
    wl_drag_anchor: Option<(f32, f32)>,
    calibrate: Option<CalibrateDraft>,
    pending_clear_calibration: Option<i64>,
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
}

const FRAME_CACHE: usize = 24;

/// Size of the lock control in a cell's bottom-right corner.
const LOCK_CHIP: Vec2 = Vec2::new(40.0, 18.0);

fn lock_chip_rect(cell: Rect) -> Rect {
    Rect::from_min_size(Pos2::new(cell.right() - 6.0 - LOCK_CHIP.x, cell.bottom() - 6.0 - LOCK_CHIP.y), LOCK_CHIP)
}
const GPU_CACHE: usize = 8;

impl ViewerState {
    pub fn open(shared: &Shared, study: StudyRow, series_id: Option<i64>) -> anyhow::Result<Self> {
        let series = shared.db.series_for_study(study.id)?;
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
        drop(settings);
        let mut viewer = Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-viewer", id)),
            study,
            series,
            panes: vec![pane],
            active: 0,
            tiling: Tiling { mode: TilingMode::Series, rows: 1, cols: 1 },
            tool: Tool::WindowLevel,
            right_tool: Tool::Zoom,
            annotations,
            bilinear: true,
            rois: HashMap::new(),
            rois_loaded: HashSet::new(),
            dirty_rois: HashSet::new(),
            label_hits: Vec::new(),
            assign: None,
            drag_draft: None,
            angle_draft: None,
            selected_roi: None,
            playing: false,
            fps: 10.0,
            last_tick: None,
            frames: HashMap::new(),
            frame_order: VecDeque::new(),
            gpu: HashMap::new(),
            gpu_order: VecDeque::new(),
            close_requested: false,
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
    }

    /// Change the grid. A tiled series spreads the active cell's series over
    /// the cells; a tiled study gives each cell its own series.
    pub fn set_tiling(&mut self, shared: &Shared, tiling: Tiling) {
        if tiling == self.tiling {
            return;
        }
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
        self.set_status(if locked { "Cell locked: image, window, colours and measurements are kept" } else { "Cell unlocked" });
        self.selected_roi = None;
        self.drag_draft = None;
        self.angle_draft = None;
        // Locking the active cell hands the keys and slider to the next
        // unlocked one, so scrolling carries on around the locked image.
        if locked && k == self.active {
            if let Some(next) = (0..self.panes.len()).map(|i| (k + 1 + i) % self.panes.len()).find(|&i| !self.panes[i].locked) {
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
        let frames = p.images[idx].number_of_frames.max(1) as i64;
        if frames > 1 {
            // Multiframe: step frames first, wrapping into neighbouring images.
            let f = p.frame_idx as i64 + delta;
            if f >= 0 && f < frames {
                p.frame_idx = f as u32;
                return;
            }
        }
        let n = p.images.len() as i64;
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

    // ------------------------------------------------------------- rois

    /// Read stored ROIs for a frame the first time it is shown.
    fn ensure_rois_loaded(&mut self, shared: &Shared, key: FrameKey) {
        if !self.rois_loaded.insert(key) {
            return;
        }
        if let Ok(records) = shared.db.rois(key.image_id, key.frame as i64) {
            let list: Vec<Roi> = records.iter().filter_map(Roi::from_record).collect();
            if !list.is_empty() {
                self.rois.entry(key).or_default().extend(list);
            }
        }
    }

    /// Write back every frame whose ROIs changed this frame.
    fn flush_rois(&mut self, shared: &Shared) {
        for key in std::mem::take(&mut self.dirty_rois) {
            let frame = self.frames.get(&key).cloned();
            let records: Vec<ruthos_core::RoiRecord> = self
                .rois
                .get(&key)
                .map(|list| list.iter().map(|r| r.to_record(key.image_id, key.frame, frame.as_deref())).collect())
                .unwrap_or_default();
            if let Err(e) = shared.db.replace_rois(key.image_id, key.frame as i64, &records) {
                tracing::warn!("could not store ROIs: {e}");
            }
        }
    }

    /// The popup under a clicked measurement label.
    fn assign_popup(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(popup) = &self.assign else { return };
        let (key, index, pos) = (popup.key, popup.index, popup.pos);
        let mut custom = popup.custom.clone();
        let current = self.rois.get(&key).and_then(|l| l.get(index)).and_then(|r| r.name.clone());
        let sets = shared.settings.lock().measurement_sets.clone();
        let ctx = ui.ctx().clone();
        let mut choice: Option<Option<String>> = None;
        let mut close = ui.input(|i| i.key_pressed(Key::Escape));
        let area = egui::Area::new(egui::Id::unique(("assign-roi", self.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(210.0);
                    ui.label(egui::RichText::new("Assign measurement").strong());
                    for set in &sets {
                        menu_heading(ui, &set.name);
                        for item in &set.items {
                            if menu_item(ui, item.as_str(), current.as_deref() == Some(item.as_str())).clicked() {
                                choice = Some(Some(item.clone()));
                            }
                        }
                    }
                    ui.add_space(4.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut custom).hint_text("Custom name").desired_width(150.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if (enter || ui.button("Set").clicked()) && !custom.trim().is_empty() {
                            choice = Some(Some(custom.trim().to_string()));
                        }
                    });
                    if menu_item_enabled(ui, "Remove name", false, current.is_some()).clicked() {
                        choice = Some(None);
                    }
                });
            });
        if let Some(p) = ui.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }) {
            if !area.response.rect.contains(p) {
                close = true;
            }
        }
        if let Some(name) = choice {
            if let Some(roi) = self.rois.get_mut(&key).and_then(|l| l.get_mut(index)) {
                roi.name = name.clone();
                self.dirty_rois.insert(key);
                self.set_status(match name {
                    Some(n) => format!("Measurement assigned to {n}"),
                    None => "Measurement name removed".to_string(),
                });
            }
            close = true;
        }
        if close {
            self.assign = None;
        } else if let Some(popup) = &mut self.assign {
            popup.custom = custom;
        }
    }

    // ------------------------------------------------------------ caches

    fn pump_loader(&mut self, shared: &Shared) {
        for loaded in shared.loader.poll() {
            match loaded.result {
                Ok(frame) => {
                    // A user calibration overrides whatever the header says.
                    let frame = match shared.db.calibration(loaded.key.image_id) {
                        Ok(Some(spacing)) => {
                            let mut f = Arc::try_unwrap(frame).unwrap_or_else(|a| (*a).clone());
                            f.meta.pixel_spacing = Some(spacing);
                            f.meta.pixel_spacing_is_calibrated = true;
                            f.meta.user_calibrated = true;
                            Arc::new(f)
                        }
                        _ => frame,
                    };
                    self.frames.insert(loaded.key, frame);
                    self.frame_order.push_back(loaded.key);
                    let (frame_cache, _) = self.cache_limits();
                    while self.frame_order.len() > frame_cache {
                        if let Some(old) = self.frame_order.pop_front() {
                            self.frames.remove(&old);
                        }
                    }
                }
                Err(e) => {
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
            let key = FrameKey { image_id: img.id, frame };
            if self.frames.contains_key(&key) {
                return;
            }
            if frames > 1 {
                // A loop is decoded in one pass and kept whole.
                shared.loader.request_all(img.id, shared.resolve(&img.path), frames);
            } else {
                shared.loader.request(key, shared.resolve(&img.path));
            }
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
        let g = Arc::new(GpuImage::upload(&shared.render_state.device, &shared.render_state.queue, &frame));
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

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        crate::crash::debug_panic("viewer");
        ui.style_mut().visuals = theme::viewer_visuals();
        if ui.input(|i| i.viewport().close_requested()) {
            self.persist_series_display(shared);
            self.close_requested = true;
        }
        self.pump_loader(shared);
        self.tick_cine(ui);
        self.keyboard(ui, shared);

        let dark = true;
        egui::Panel::top(("viewer-toolbar", self.id))
            .exact_size(theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(theme::toolbar_stroke(dark))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| { theme::paint_toolbar_background(ui, true, egui::vec2(6.0, 3.0)); self.toolbar(ui, shared) });

        egui::Panel::left(("viewer-drawer", self.id))
            .exact_size(theme::THUMB_SIZE + 36.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(30, 30, 32)).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| self.drawer(ui, shared));

        if self.panes[self.active].positions() > 1 {
            egui::Panel::bottom(("viewer-slider", self.id))
                .exact_size(26.0)
                .frame(egui::Frame::NONE.fill(Color32::from_rgb(30, 30, 32)).inner_margin(egui::Margin::symmetric(10, 4)))
                .show(ui, |ui| self.slider(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.image_area(ui, shared));

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

        if shared.loader.has_pending() || self.playing {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// Pixel length of the selected Length ROI on the current frame, if any.
    fn selected_length_px(&self) -> Option<f32> {
        let key = self.current_key()?;
        let roi = self.rois.get(&key)?.get(self.selected_roi?)?;
        match &roi.kind {
            RoiKind::Length { a, b } => Some((*b - *a).length()),
            _ => None,
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
                let r = ui.add(egui::TextEdit::singleline(&mut draft.known_mm).desired_width(90.0).hint_text("e.g. 100"));
                r.request_focus();
                ui.label("mm");
            });
            ui.checkbox(&mut draft.apply_to_series, "Apply to every image in this series");
            ui.add_space(8.0);
            let mm: Option<f64> = draft.known_mm.trim().replace(',', ".").parse::<f64>().ok().filter(|v| *v > 0.0);
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
            let series_id = self.current_series().id;
            let result = if to_series {
                shared.db.set_series_calibration(series_id, spacing, spacing, "user length").map(|_| ())
            } else {
                shared.db.set_calibration(image_id, spacing, spacing, "user length")
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
            if ToolbarButton::new(Bitmap::DatabaseWindow, "Database").tooltip("Show the database window").show(ui).clicked() {
                shared.focus_browser.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            toolbar_separator(ui);

            // Series popup
            ui.vertical(|ui| {
                ui.add_space(4.0);
                toolbar_caption(ui, "Series");
                let current = self.series.get(self.panes[self.active].series_idx).map(|s| s.display_name()).unwrap_or_default();
                let mut chosen = self.panes[self.active].series_idx;
                egui::ComboBox::from_id_salt(("series-popup", self.id))
                    .selected_text(crate::widgets::truncate(&current, 15))
                    .width(138.0)
                    .show_ui(ui, |ui| {
                        for (i, s) in self.series.iter().enumerate() {
                            let label = format!("{}  ({} im)", s.display_name(), s.number_of_images);
                            ui.selectable_value(&mut chosen, i, label);
                        }
                    });
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
                ui.menu_button((icons::rich(icons::TILING, 16.0), egui::RichText::new(label).size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    for (mode, title) in [
                        (TilingMode::Series, "Images of this series"),
                        (TilingMode::Study, "Series of this study"),
                    ] {
                        menu_heading(ui, title);
                        ui.horizontal_wrapped(|ui| {
                            ui.set_max_width(236.0);
                            for (rows, cols) in Tiling::GRIDS {
                                let t = Tiling { mode, rows, cols };
                                let enabled = mode == TilingMode::Series || rows * cols <= self.series.len().max(1) || rows * cols == 1;
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
                });
                toolbar_caption(ui, caption);
                if let Some(t) = chosen {
                    self.set_tiling(shared, t);
                }
            });
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
                ui.menu_button((title_icon, egui::RichText::new(" ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    // One row per tool. Left-click puts it on the left mouse
                    // button, right-click on the right button; a small mouse
                    // glyph beside the name shows which.
                    let mut changed = false;
                    for t in Tool::ALL {
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
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new("Click: left button  ·  Right-click: right button")
                            .size(11.0)
                            .color(Color32::from_gray(150)),
                    );
                    if changed {
                        self.drag_draft = None;
                        self.angle_draft = None;
                    }
                });
                toolbar_caption(ui, &format!("Tool: {}", self.tool.label()));
            });

            // WL/WW menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let wl_icon = icons::image(ui.ctx(), Bitmap::Wlww, 18.0);
                ui.menu_button((wl_icon, egui::RichText::new(" ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
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
                        ui.add(egui::DragValue::new(&mut self.panes[self.active].view.window_width).speed(1.0).range(1.0..=f32::MAX));
                    });
                });
                toolbar_caption(ui, "WL/WW");
            });

            // CLUT menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                let clut_caption = crate::widgets::truncate(&self.panes[self.active].clut, 12);
                ui.menu_button((icons::rich(icons::CLUT, 16.0), egui::RichText::new(" ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        for name in shared.cluts.names() {
                            if menu_item(ui, name, self.panes[self.active].clut == name).clicked() {
                                self.panes[self.active].clut = name.to_string();
                                ui.close();
                            }
                        }
                    });
                });
                toolbar_caption(ui, &clut_caption);
            });

            ToolbarButton::new(Bitmap::ThreeD, "3D")
                .enabled(false)
                .tooltip("MPR and volume rendering arrive in Tier 2")
                .show(ui);

            // Orientation menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.menu_button((icons::rich(icons::ORIENTATION, 16.0), egui::RichText::new(" ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    if menu_item(ui, "Rotate 90° clockwise", false).clicked() {
                        self.panes[self.active].view.rotation_deg = (self.panes[self.active].view.rotation_deg + 90.0).rem_euclid(360.0);
                        self.panes[self.active].fit_pending = true;
                        ui.close();
                    }
                    if menu_item(ui, "Rotate 90° counter-clockwise", false).clicked() {
                        self.panes[self.active].view.rotation_deg = (self.panes[self.active].view.rotation_deg - 90.0).rem_euclid(360.0);
                        self.panes[self.active].fit_pending = true;
                        ui.close();
                    }
                    if menu_item(ui, "Flip horizontal", false).clicked() {
                        self.panes[self.active].view.flip_horizontal = !self.panes[self.active].view.flip_horizontal;
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
                });
                toolbar_caption(ui, "Orientation");
            });
            toolbar_separator(ui);

            if ToolbarButton::new(Bitmap::Reset, "Reset").tooltip("Fit image and restore default window").show(ui).clicked() {
                self.panes[self.active].fit_pending = true;
                self.apply_default_window(shared, false);
            }
            let is_key = self.current_image().map(|i| i.is_key_image).unwrap_or(false);
            if ToolbarButton::new(Bitmap::Key, "Key Image").selected(is_key).tooltip("Mark this image as a key image").show(ui).clicked() {
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
            let selected_length = self.selected_length_px();
            if ToolbarButton::new(Bitmap::Length, "Calibrate")
                .enabled(selected_length.is_some())
                .tooltip("Draw a Length over something of known size, select it, then set its real length to calibrate this image")
                .show(ui)
                .clicked()
            {
                if let (Some(px), Some(img)) = (selected_length, self.current_image()) {
                    self.calibrate = Some(CalibrateDraft {
                        image_id: img.id,
                        pixel_length: px,
                        known_mm: String::new(),
                        apply_to_series: self.panes[self.active].images.len() > 1,
                    });
                }
            }
            if ToolbarButton::new(Bitmap::Export, "Export").tooltip("Export the current image as PNG or JPEG").show(ui).clicked() {
                self.export_current(shared);
            }
            if ToolbarButton::new(Bitmap::ReportCustom, "Report").tooltip("The report for this study: measurements, findings, key images").show(ui).clicked() {
                shared.open_reports.lock().push(self.study.id);
            }
            if self.study.teaching_answer_key {
                let label = if self.reveal_answers { "Hide answers" } else { "Answers" };
                if ToolbarButton::new(icons::ANONYMIZE, label).selected(self.reveal_answers).tooltip("This teaching copy carries its measurements as an answer key").show(ui).clicked() {
                    self.reveal_answers = !self.reveal_answers;
                }
            }
            toolbar_separator(ui);

            let can_play = self.panes[self.active].images.len() > 1 || self.current_image().map(|i| i.number_of_frames > 1).unwrap_or(false);
            let (bitmap, label) = if self.playing { (Bitmap::Pause, "Pause") } else { (Bitmap::Play, "Play") };
            if ToolbarButton::new(bitmap, label).enabled(can_play).show(ui).clicked() {
                self.playing = !self.playing;
                self.last_tick = None;
            }
            ui.vertical(|ui| {
                ui.add_space(6.0);
                ui.add_enabled(can_play, egui::Slider::new(&mut self.fps, 1.0..=120.0).show_value(false));
                toolbar_caption(ui, &format!("Speed {} fps", self.fps.round() as i32));
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((text, when)) = &self.status {
                    if when.elapsed().as_secs_f32() < 3.0 {
                        ui.label(egui::RichText::new(text).size(12.0).color(Color32::from_gray(210)));
                    }
                }
            });
        });
    }

    fn drawer(&mut self, ui: &mut Ui, shared: &Shared) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut clicked = None;
            for (i, s) in self.series.iter().enumerate() {
                let tex = shared.thumbnail(s.id);
                let sub = format!("{} {}", s.modality, s.count_label());
                let r = thumbnail_cell(ui, tex.as_ref(), &s.display_name(), &sub, i == self.panes[self.active].series_idx, theme::THUMB_SIZE);
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
        ui.add_enabled(!self.active_locked(), egui::Slider::new(&mut v, 0..=n - 1).show_value(false));
        ui.label(egui::RichText::new(format!("{}/{}", v + 1, n)).size(11.5).color(Color32::from_gray(200)));
        if v != current {
            self.set_active_position(v);
        }
    }

    fn tick_cine(&mut self, ui: &mut Ui) {
        if !self.playing {
            return;
        }
        let now = Instant::now();
        let period = 1.0 / self.fps.max(1.0);
        match self.last_tick {
            None => self.last_tick = Some(now),
            Some(t) if now.duration_since(t).as_secs_f32() >= period => {
                if self.tiling.mode == TilingMode::Study && self.panes.len() > 1 {
                    // Every unlocked loop on screen plays.
                    for p in &mut self.panes {
                        let n = p.positions();
                        if !p.locked && n > 1 {
                            p.set_position((p.position() + 1) % n);
                        }
                    }
                } else if !self.active_locked() {
                    self.step_image(1);
                }
                self.last_tick = Some(now);
            }
            _ => {}
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(period / 2.0));
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
            self.step_image(10);
        }
        if pgup {
            self.step_image(-10);
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
            self.panes[self.active].view.rotation_deg = (self.panes[self.active].view.rotation_deg + 90.0).rem_euclid(360.0);
            self.panes[self.active].fit_pending = true;
        }
    }

    fn apply_default_window(&mut self, _shared: &Shared, full_dynamic: bool) {
        if let Some(key) = self.current_key() {
            if let Some(frame) = self.frames.get(&key) {
                let (c, w) = if full_dynamic { frame.full_dynamic_window() } else { frame.default_window() };
                self.panes[self.active].view.window_center = c;
                self.panes[self.active].view.window_width = w;
            }
        }
    }

    fn export_current(&mut self, shared: &Shared) {
        let Some(key) = self.current_key() else { return };
        let Some(frame) = self.frames.get(&key).cloned() else { return };
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
        let clut = shared.cluts.get(&self.panes[self.active].clut).cloned().unwrap_or_else(ruthos_dicom::Clut::grayscale);
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, self.panes[self.active].view.window_center, self.panes[self.active].view.window_width, &clut);
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
                painter.line_segment([Pos2::new(x, area.top()), Pos2::new(x, area.bottom())], egui::Stroke::new(1.0, line));
            }
            for r in 1..rows {
                let y = area.top() + r as f32 * (ch + gap) - gap / 2.0;
                painter.line_segment([Pos2::new(area.left(), y), Pos2::new(area.right(), y)], egui::Stroke::new(1.0, line));
            }
            if let Some(r) = rects.get(self.active) {
                painter.rect_stroke(r.shrink(1.0), 0.0, egui::Stroke::new(1.5, theme::SELECTION_BLUE), egui::StrokeKind::Inside);
            }
        }
    }

    /// Paint one cell; only the active cell takes mouse input.
    fn paint_cell(&mut self, ui: &mut Ui, shared: &Shared, rect: Rect, k: usize, tiled: bool) {
        let active = k == self.active;
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);

        if self.panes[k].images.is_empty() {
            shadow_text(&painter, rect.center(), Align2::CENTER_CENTER, "No images in this series", FontId::proportional(16.0), Color32::from_gray(160));
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
                    } else {
                        self.ensure_requested(shared, &p.images, next, 0);
                        self.ensure_requested(shared, &p.images, prev, 0);
                    }
                }
            }
        }

        let Some(frame) = self.frames.get(&key).cloned() else {
            let msg = match &self.panes[k].load_error {
                Some(e) => format!("Could not decode image:\n{e}"),
                None => "Loading\u{2026}".to_string(),
            };
            shadow_text(&painter, rect.center(), Align2::CENTER_CENTER, &msg, FontId::proportional(15.0), Color32::from_gray(170));
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
                    self.fps = (1000.0 / ms as f32).clamp(1.0, 120.0);
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
        if active {
            self.handle_input(ui, &response, &mut transform, &frame, &key);
            self.panes[k].view = transform.state;
            // Input may have scrolled this cell to another image (or past the
            // end of a tiled series); finish painting on the next frame.
            if self.panes[k].key() != Some(key) {
                ui.ctx().request_repaint();
                return;
            }
        }
        let transform = ViewTransform::new(rect, frame.width, frame.height, aspect, self.panes[k].view);

        // The image itself.
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
                    image: gpu,
                    clut,
                    transform,
                    inverted: frame.inverted,
                    interpolation: if self.bilinear { Interpolation::Bilinear } else { Interpolation::Nearest },
                },
            ));
        }

        // ROIs and drafts.
        self.ensure_rois_loaded(shared, key);
        if active {
            self.label_hits.retain(|(k, _, _)| *k != key);
        }
        let hidden = self.study.teaching_answer_key && !self.reveal_answers;
        if let Some(list) = self.rois.get(&key).filter(|_| !hidden) {
            for (i, roi) in list.iter().enumerate() {
                let sel = active && self.selected_roi == Some(i);
                let label = roi.draw(&painter, &transform, &frame, roi_color(sel), sel);
                if active {
                    if let Some(r) = label {
                        self.label_hits.push((key, i, r));
                    }
                }
            }
        }
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
        }

        // Annotations; small cells drop to the partial set.
        let p = &self.panes[k];
        let Some(image) = p.images.get(p.image_idx).cloned() else { return };
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
            painter.line_segment([box_rect.left_center() + Vec2::new(2.5, 0.5), box_rect.center() + Vec2::new(-0.5, 3.0)], egui::Stroke::new(1.6, color));
            painter.line_segment([box_rect.center() + Vec2::new(-0.5, 3.0), box_rect.right_top() + Vec2::new(-2.0, 2.5)], egui::Stroke::new(1.6, color));
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

    fn handle_input(&mut self, ui: &mut Ui, response: &egui::Response, t: &mut ViewTransform, frame: &Frame, key: &FrameKey) {
        let hover = response.hover_pos();
        self.mouse_pixel = hover.and_then(|p| t.pixel_at(p));

        // The lock control in the bottom-right corner.
        let chip = lock_chip_rect(t.rect);
        if response.clicked_by(PointerButton::Primary) && response.interact_pointer_pos().is_some_and(|p| chip.contains(p)) {
            let k = self.active;
            self.toggle_lock(k);
            return;
        }
        if self.active_locked() {
            return;
        }

        // Wheel: scroll images; ctrl+wheel / pinch: zoom.
        if response.hovered() {
            let (scroll, zoom_delta, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.modifiers.ctrl || i.modifiers.command));
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
                if let Some((k, i, _)) = self.label_hits.iter().find(|(k, _, r)| k == key && r.expand(3.0).contains(p)).copied() {
                    self.assign = Some(AssignPopup { key: k, index: i, pos: p, custom: String::new() });
                    self.selected_roi = Some(i);
                    return;
                }
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

        // Right-click menu: calibration and ROI housekeeping.
        let selected_length = self.selected_length_px();
        let current_image_id = self.current_image().map(|i| i.id);
        let has_calibration = current_image_id
            .and_then(|id| ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::unique(("has-cal", id)))))
            .unwrap_or(false);
        let mut open_calibrate = false;
        let mut clear_calibration = false;
        let mut delete_all = false;
        response.context_menu(|ui| {
            if menu_item_enabled(ui, "Calibrate from selected length…", false, selected_length.is_some()).clicked() {
                open_calibrate = true;
                ui.close();
            }
            if menu_item_enabled(ui, "Remove user calibration", false, has_calibration || frame.meta.pixel_spacing_is_calibrated).clicked() {
                clear_calibration = true;
                ui.close();
            }
            ui.separator();
            if menu_item(ui, "Delete all measurements on this image", false).clicked() {
                delete_all = true;
                ui.close();
            }
            if menu_item(ui, "Fit image to window", false).clicked() {
                self.panes[self.active].fit_pending = true;
                ui.close();
            }
        });
        if open_calibrate {
            if let (Some(px), Some(id)) = (selected_length, current_image_id) {
                self.calibrate = Some(CalibrateDraft {
                    image_id: id,
                    pixel_length: px,
                    known_mm: String::new(),
                    apply_to_series: self.panes[self.active].images.len() > 1,
                });
            }
        }
        if clear_calibration {
            if let Some(id) = current_image_id {
                self.pending_clear_calibration = Some(id);
            }
        }
        if delete_all {
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
                if let Some(d) = &mut self.angle_draft {
                    if d.b_fixed {
                        if let Some(hp) = hover {
                            d.c = t.screen_to_image(hp);
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
                            d.b = p;
                            d.c = p;
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
                    if let Some(p) = img_pos {
                        if !self.try_select_roi(t, key, pos) {
                            self.angle_draft = Some(AngleDraft { a: p, b: p, b_fixed: false, c: p });
                        }
                    }
                } else if response.clicked_by(button) {
                    self.try_select_roi(t, key, pos);
                }
            }
            Tool::Point => {
                if response.clicked_by(button) && !self.try_select_roi(t, key, pos) {
                    if let Some(p) = pos.map(|p| t.screen_to_image(p)) {
                        if frame.value_at(p.x.floor() as i64, p.y.floor() as i64).is_some() {
                            self.rois.entry(*key).or_default().push(Roi::new(RoiKind::Point { p }));
                            self.dirty_rois.insert(*key);
                        }
                    }
                }
            }
            Tool::Length | Tool::Rectangle | Tool::Oval | Tool::Arrow => {
                if response.drag_started_by(button) {
                    if let Some(p) = pos.map(|p| t.screen_to_image(p)) {
                        if !self.try_select_roi(t, key, pos) {
                            self.drag_draft = Some((p, p));
                        }
                    }
                }
                if response.dragged_by(button) {
                    if let (Some((a, _)), Some(p)) = (self.drag_draft, pos) {
                        let mut b = t.screen_to_image(p);
                        if ui.input(|i| i.modifiers.shift) && matches!(tool, Tool::Rectangle | Tool::Oval) {
                            // Shift constrains to a square / circle.
                            let d = b - a;
                            let m = d.x.abs().max(d.y.abs());
                            b = a + Vec2::new(m * d.x.signum(), m * d.y.signum());
                        }
                        self.drag_draft = Some((a, b));
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
    Some(Tiling { mode, rows: r.parse().ok()?, cols: c.parse().ok()? })
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
