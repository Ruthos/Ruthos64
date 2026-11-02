//! The 2D viewer window (Horos: ViewerController + DCMView).
//!
//! One instance per open study, shown in its own native viewport. Holds the
//! series list, the current image, decoded-frame and GPU caches, the view
//! state, the active tool and the ROIs.

use std::collections::{HashMap, VecDeque};
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
use crate::widgets::{shadow_text, thumbnail_cell, tool_menu_row, toolbar_caption, toolbar_separator, ToolbarButton};

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

pub struct ViewerState {
    pub id: u64,
    pub viewport_id: ViewportId,
    pub study: StudyRow,
    pub series: Vec<SeriesRow>,
    pub series_idx: usize,
    pub images: Vec<ImageRow>,
    pub image_idx: usize,
    pub frame_idx: u32,

    pub view: ViewState,
    /// Tools assigned to the left and right mouse buttons. The middle button
    /// always pans, as in Horos.
    pub tool: Tool,
    pub right_tool: Tool,
    pub clut: String,
    pub annotations: AnnotationLevel,
    pub bilinear: bool,

    pub rois: HashMap<FrameKey, Vec<Roi>>,
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
    fit_pending: bool,
    window_initialised_for: Option<i64>,
    mouse_pixel: Option<(i64, i64)>,
    load_error: Option<String>,
    status: Option<(String, Instant)>,
    wl_drag_anchor: Option<(f32, f32)>,
    calibrate: Option<CalibrateDraft>,
    pending_clear_calibration: Option<i64>,
}

/// The "Calibrate from this length" dialog.
struct CalibrateDraft {
    image_id: i64,
    pixel_length: f32,
    known_mm: String,
    apply_to_series: bool,
}

const FRAME_CACHE: usize = 24;
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
        let images = shared.db.images_for_series(series[series_idx].id)?;
        let _ = shared.db.mark_study_opened(study.id);
        let id = shared.next_id();
        let settings = shared.settings.lock();
        let clut = settings.default_clut.clone();
        let annotations = match settings.annotations_level {
            0 => AnnotationLevel::None,
            1 => AnnotationLevel::Partial,
            _ => AnnotationLevel::Full,
        };
        drop(settings);
        Ok(Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-viewer", id)),
            study,
            series,
            series_idx,
            images,
            image_idx: 0,
            frame_idx: 0,
            view: ViewState::default(),
            tool: Tool::WindowLevel,
            right_tool: Tool::Zoom,
            clut,
            annotations,
            bilinear: true,
            rois: HashMap::new(),
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
            fit_pending: true,
            window_initialised_for: None,
            mouse_pixel: None,
            load_error: None,
            status: None,
            wl_drag_anchor: None,
            calibrate: None,
            pending_clear_calibration: None,
        })
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
        self.images.get(self.image_idx)
    }

    fn current_series(&self) -> &SeriesRow {
        &self.series[self.series_idx]
    }

    fn current_key(&self) -> Option<FrameKey> {
        self.current_image().map(|img| FrameKey {
            image_id: img.id,
            frame: self.frame_idx,
        })
    }

    /// Switch to another series of the same study.
    pub fn select_series(&mut self, shared: &Shared, idx: usize) {
        if idx >= self.series.len() || idx == self.series_idx {
            return;
        }
        self.persist_series_display(shared);
        self.series_idx = idx;
        self.images = shared.db.images_for_series(self.series[idx].id).unwrap_or_default();
        self.image_idx = 0;
        self.frame_idx = 0;
        self.fit_pending = true;
        self.window_initialised_for = None;
        self.selected_roi = None;
        self.drag_draft = None;
        self.angle_draft = None;
        self.load_error = None;
    }

    /// Store WL, rotation and flips for the series, as Horos does.
    pub fn persist_series_display(&self, shared: &Shared) {
        let s = self.current_series();
        let _ = shared.db.set_series_display(
            s.id,
            Some(self.view.window_center as f64),
            Some(self.view.window_width as f64),
            self.view.rotation_deg as f64,
            self.view.flip_horizontal,
            self.view.flip_vertical,
        );
    }

    fn step_image(&mut self, delta: i64) {
        if self.images.is_empty() {
            return;
        }
        let img = &self.images[self.image_idx];
        let frames = img.number_of_frames.max(1) as i64;
        if frames > 1 {
            // Multiframe: step frames first, wrapping into neighbouring images.
            let f = self.frame_idx as i64 + delta;
            if f >= 0 && f < frames {
                self.frame_idx = f as u32;
                return;
            }
        }
        let n = self.images.len() as i64;
        let next = (self.image_idx as i64 + delta).rem_euclid(n);
        self.image_idx = next as usize;
        self.frame_idx = if delta < 0 {
            (self.images[self.image_idx].number_of_frames.max(1) - 1) as u32
        } else {
            0
        };
        self.selected_roi = None;
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
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
                            Arc::new(f)
                        }
                        _ => frame,
                    };
                    self.frames.insert(loaded.key, frame);
                    self.frame_order.push_back(loaded.key);
                    while self.frame_order.len() > FRAME_CACHE {
                        if let Some(old) = self.frame_order.pop_front() {
                            self.frames.remove(&old);
                        }
                    }
                }
                Err(e) => {
                    if Some(loaded.key) == self.current_key() {
                        self.load_error = Some(e);
                    }
                }
            }
        }
    }

    fn ensure_requested(&self, shared: &Shared, idx: usize, frame: u32) {
        if let Some(img) = self.images.get(idx) {
            let key = FrameKey { image_id: img.id, frame };
            if !self.frames.contains_key(&key) {
                shared.loader.request(key, shared.resolve(&img.path));
            }
        }
    }

    fn gpu_for(&mut self, shared: &Shared, key: FrameKey) -> Option<Arc<GpuImage>> {
        if let Some(g) = self.gpu.get(&key) {
            return Some(g.clone());
        }
        let frame = self.frames.get(&key)?.clone();
        let g = Arc::new(GpuImage::upload(&shared.render_state.device, &shared.render_state.queue, &frame));
        self.gpu.insert(key, g.clone());
        self.gpu_order.push_back(key);
        while self.gpu_order.len() > GPU_CACHE {
            if let Some(old) = self.gpu_order.pop_front() {
                self.gpu.remove(&old);
            }
        }
        Some(g)
    }

    // ---------------------------------------------------------------- UI

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
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

        if self.images.len() > 1 || self.current_image().map(|i| i.number_of_frames > 1).unwrap_or(false) {
            egui::Panel::bottom(("viewer-slider", self.id))
                .exact_size(26.0)
                .frame(egui::Frame::NONE.fill(Color32::from_rgb(30, 30, 32)).inner_margin(egui::Margin::symmetric(10, 4)))
                .show(ui, |ui| self.slider(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.image_area(ui, shared));

        self.calibrate_modal(ui, shared);
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
        ui.horizontal(|ui| {
            if ToolbarButton::new(icons::DATABASE, "Database").tooltip("Show the database window").show(ui).clicked() {
                shared.focus_browser.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            toolbar_separator(ui);

            // Series popup
            ui.vertical(|ui| {
                ui.add_space(4.0);
                toolbar_caption(ui, "Series");
                let current = self.series.get(self.series_idx).map(|s| s.display_name()).unwrap_or_default();
                let mut chosen = self.series_idx;
                egui::ComboBox::from_id_salt(("series-popup", self.id))
                    .selected_text(crate::widgets::truncate(&current, 24))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for (i, s) in self.series.iter().enumerate() {
                            let label = format!("{}  ({} im)", s.display_name(), s.number_of_images);
                            ui.selectable_value(&mut chosen, i, label);
                        }
                    });
                if chosen != self.series_idx {
                    self.select_series(shared, chosen);
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
                let glyph = self.tool.glyph();
                ui.menu_button((icons::rich(glyph, 16.0), egui::RichText::new(format!("  {}  ", self.tool.label())).size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    // One row per tool. Left-click puts it on the left mouse
                    // button, right-click on the right button; a small mouse
                    // glyph beside the name shows which.
                    let mut changed = false;
                    for t in Tool::ALL {
                        let r = tool_menu_row(ui, t.glyph(), t.label(), self.tool == t, self.right_tool == t);
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
                toolbar_caption(ui, "Tools");
            });

            // WL/WW menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.menu_button((icons::rich(icons::WLWW, 16.0), egui::RichText::new("  WL/WW  ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    if ui.button("Default WL & WW").clicked() {
                        self.apply_default_window(shared, false);
                        ui.close();
                    }
                    if ui.button("Full dynamic").clicked() {
                        self.apply_default_window(shared, true);
                        ui.close();
                    }
                    ui.separator();
                    for p in &shared.presets.presets {
                        if ui.button(format!("{}  ({} / {})", p.name, p.center, p.width)).clicked() {
                            self.view.window_center = p.center;
                            self.view.window_width = p.width;
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.label("Custom");
                    ui.horizontal(|ui| {
                        ui.label("WL");
                        ui.add(egui::DragValue::new(&mut self.view.window_center).speed(1.0));
                        ui.label("WW");
                        ui.add(egui::DragValue::new(&mut self.view.window_width).speed(1.0).range(1.0..=f32::MAX));
                    });
                });
                toolbar_caption(ui, "WL/WW");
            });

            // CLUT menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.menu_button((icons::rich(icons::CLUT, 16.0), egui::RichText::new(format!("  {}  ", crate::widgets::truncate(&self.clut, 12))).size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        for name in shared.cluts.names() {
                            if ui.selectable_label(self.clut == name, name).clicked() {
                                self.clut = name.to_string();
                                ui.close();
                            }
                        }
                    });
                });
                toolbar_caption(ui, "CLUT");
            });

            ToolbarButton::new(icons::THREE_D, "3D")
                .enabled(false)
                .tooltip("MPR and volume rendering arrive in Tier 2")
                .show(ui);

            // Orientation menu
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.menu_button((icons::rich(icons::ORIENTATION, 16.0), egui::RichText::new("  Orientation  ").size(13.0), icons::rich(icons::CARET_DOWN, 11.0)), |ui| {
                    if ui.button("Rotate 90° clockwise").clicked() {
                        self.view.rotation_deg = (self.view.rotation_deg + 90.0).rem_euclid(360.0);
                        self.fit_pending = true;
                        ui.close();
                    }
                    if ui.button("Rotate 90° counter-clockwise").clicked() {
                        self.view.rotation_deg = (self.view.rotation_deg - 90.0).rem_euclid(360.0);
                        self.fit_pending = true;
                        ui.close();
                    }
                    if ui.button("Flip horizontal").clicked() {
                        self.view.flip_horizontal = !self.view.flip_horizontal;
                        ui.close();
                    }
                    if ui.button("Flip vertical").clicked() {
                        self.view.flip_vertical = !self.view.flip_vertical;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Reset orientation").clicked() {
                        self.view.rotation_deg = 0.0;
                        self.view.flip_horizontal = false;
                        self.view.flip_vertical = false;
                        self.fit_pending = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.checkbox(&mut self.bilinear, "Smooth interpolation").clicked() {
                        ui.close();
                    }
                });
                toolbar_caption(ui, "Orientation");
            });
            toolbar_separator(ui);

            if ToolbarButton::new(icons::RESET, "Reset").tooltip("Fit image and restore default window").show(ui).clicked() {
                self.fit_pending = true;
                self.apply_default_window(shared, false);
            }
            let is_key = self.current_image().map(|i| i.is_key_image).unwrap_or(false);
            if ToolbarButton::new(icons::KEY_IMAGE, "Key Image").selected(is_key).tooltip("Mark this image as a key image").show(ui).clicked() {
                if let Some(img) = self.images.get_mut(self.image_idx) {
                    img.is_key_image = !img.is_key_image;
                    let _ = shared.db.set_key_image(img.id, img.is_key_image);
                }
            }
            let selected_length = self.selected_length_px();
            if ToolbarButton::new(icons::TOOL_LENGTH, "Calibrate")
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
                        apply_to_series: self.images.len() > 1,
                    });
                }
            }
            if ToolbarButton::new(icons::EXPORT, "Export").tooltip("Export the current image as PNG or JPEG").show(ui).clicked() {
                self.export_current(shared);
            }
            toolbar_separator(ui);

            let can_play = self.images.len() > 1 || self.current_image().map(|i| i.number_of_frames > 1).unwrap_or(false);
            let (glyph, label) = if self.playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
            if ToolbarButton::new(glyph, label).enabled(can_play).show(ui).clicked() {
                self.playing = !self.playing;
                self.last_tick = None;
            }
            ui.vertical(|ui| {
                ui.add_space(6.0);
                ui.add_enabled(can_play, egui::Slider::new(&mut self.fps, 1.0..=60.0).show_value(false));
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
                let sub = format!("{} {} im", s.modality, s.number_of_images);
                let r = thumbnail_cell(ui, tex.as_ref(), &s.display_name(), &sub, i == self.series_idx, theme::THUMB_SIZE);
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
        let img_frames = self.current_image().map(|i| i.number_of_frames.max(1) as usize).unwrap_or(1);
        if self.images.len() > 1 {
            let n = self.images.len();
            let mut idx = self.image_idx;
            ui.spacing_mut().slider_width = ui.available_width() - 70.0;
            ui.add(egui::Slider::new(&mut idx, 0..=n - 1).show_value(false));
            ui.label(egui::RichText::new(format!("{}/{}", idx + 1, n)).size(11.5).color(Color32::from_gray(200)));
            if idx != self.image_idx {
                self.image_idx = idx;
                self.frame_idx = 0;
                self.selected_roi = None;
            }
        } else if img_frames > 1 {
            let mut f = self.frame_idx as usize;
            ui.spacing_mut().slider_width = ui.available_width() - 70.0;
            ui.add(egui::Slider::new(&mut f, 0..=img_frames - 1).show_value(false));
            ui.label(egui::RichText::new(format!("{}/{}", f + 1, img_frames)).size(11.5).color(Color32::from_gray(200)));
            self.frame_idx = f as u32;
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
                self.step_image(1);
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
            self.image_idx = 0;
            self.frame_idx = 0;
        }
        if end && !self.images.is_empty() {
            self.image_idx = self.images.len() - 1;
            self.frame_idx = 0;
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
            self.fit_pending = true;
            self.apply_default_window(shared, false);
        }
        if f {
            self.fit_pending = true;
        }
        if h {
            self.view.flip_horizontal = !self.view.flip_horizontal;
        }
        if v {
            self.view.flip_vertical = !self.view.flip_vertical;
        }
        if i_key {
            self.view.rotation_deg = (self.view.rotation_deg + 90.0).rem_euclid(360.0);
            self.fit_pending = true;
        }
    }

    fn apply_default_window(&mut self, _shared: &Shared, full_dynamic: bool) {
        if let Some(key) = self.current_key() {
            if let Some(frame) = self.frames.get(&key) {
                let (c, w) = if full_dynamic { frame.full_dynamic_window() } else { frame.default_window() };
                self.view.window_center = c;
                self.view.window_width = w;
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
            self.image_idx + 1
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
        let clut = shared.cluts.get(&self.clut).cloned().unwrap_or_else(ruthos_dicom::Clut::grayscale);
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, self.view.window_center, self.view.window_width, &clut);
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
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);

        let Some(key) = self.current_key() else {
            shadow_text(&painter, rect.center(), Align2::CENTER_CENTER, "No images in this series", FontId::proportional(16.0), Color32::from_gray(160));
            return;
        };

        // Request the current frame and its neighbours.
        self.ensure_requested(shared, self.image_idx, self.frame_idx);
        if self.images.len() > 1 {
            let n = self.images.len();
            self.ensure_requested(shared, (self.image_idx + 1) % n, 0);
            self.ensure_requested(shared, (self.image_idx + n - 1) % n, 0);
        } else if let Some(img) = self.current_image() {
            if img.number_of_frames > 1 {
                let f = img.number_of_frames as u32;
                self.ensure_requested(shared, self.image_idx, (self.frame_idx + 1) % f);
            }
        }

        let Some(frame) = self.frames.get(&key).cloned() else {
            let msg = match &self.load_error {
                Some(e) => format!("Could not decode image:\n{e}"),
                None => "Loading…".to_string(),
            };
            shadow_text(&painter, rect.center(), Align2::CENTER_CENTER, &msg, FontId::proportional(15.0), Color32::from_gray(170));
            return;
        };
        self.load_error = None;

        // First display of a series: window from the stored series state or
        // the header, and fit to the viewport.
        let series_id = self.current_series().id;
        if self.window_initialised_for != Some(series_id) {
            let s = self.current_series().clone();
            match (s.window_center, s.window_width) {
                (Some(c), Some(w)) if w > 0.0 => {
                    self.view.window_center = c as f32;
                    self.view.window_width = w as f32;
                }
                _ => {
                    let (c, w) = frame.default_window();
                    self.view.window_center = c;
                    self.view.window_width = w;
                }
            }
            self.view.rotation_deg = s.rotation_angle as f32;
            self.view.flip_horizontal = s.x_flipped;
            self.view.flip_vertical = s.y_flipped;
            self.window_initialised_for = Some(series_id);
        }
        let aspect = frame
            .meta
            .pixel_spacing
            .map(|[row, col]| if col > 0.0 { (row / col) as f32 } else { 1.0 })
            .unwrap_or(1.0);
        if self.fit_pending {
            self.view.fit(rect, frame.width, frame.height, aspect);
            self.fit_pending = false;
        }

        let mut transform = ViewTransform::new(rect, frame.width, frame.height, aspect, self.view);
        self.handle_input(ui, &response, &mut transform, &frame, &key);
        self.view = transform.state;
        let transform = ViewTransform::new(rect, frame.width, frame.height, aspect, self.view);

        // The image itself.
        if let Some(gpu) = self.gpu_for(shared, key) {
            let clut = Arc::new(
                shared
                    .cluts
                    .get(&self.clut)
                    .cloned()
                    .unwrap_or_else(ruthos_dicom::Clut::grayscale),
            );
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ImageCallback {
                    key: self.id,
                    image: gpu,
                    clut,
                    transform,
                    inverted: frame.inverted,
                    interpolation: if self.bilinear { Interpolation::Bilinear } else { Interpolation::Nearest },
                },
            ));
        }

        // ROIs and drafts.
        if let Some(list) = self.rois.get(&key) {
            for (i, roi) in list.iter().enumerate() {
                roi.draw(&painter, &transform, &frame, roi_color(self.selected_roi == Some(i)));
            }
        }
        if let Some((a, b)) = self.drag_draft {
            if let Some(kind) = draft_kind(self.tool, a, b) {
                Roi::new(kind).draw(&painter, &transform, &frame, theme::ROI_DRAFT);
            }
        }
        if let Some(d) = &self.angle_draft {
            let roi = Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c });
            roi.draw(&painter, &transform, &frame, theme::ROI_DRAFT);
        }

        // Annotations.
        let image = self.images[self.image_idx].clone();
        let ctx = AnnotationContext {
            study: &self.study,
            series: self.current_series(),
            image: &image,
            frame: &frame,
            image_index: self.image_idx,
            image_count: self.images.len(),
            transform: &transform,
            mouse_pixel: self.mouse_pixel,
            level: self.annotations,
        };
        annotations::draw(&painter, rect, &ctx);

        if self.playing {
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
            self.fit_pending = true;
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
            if ui.add_enabled(selected_length.is_some(), egui::Button::new("Calibrate from selected length…")).clicked() {
                open_calibrate = true;
                ui.close();
            }
            if ui.add_enabled(has_calibration || frame.meta.pixel_spacing_is_calibrated, egui::Button::new("Remove user calibration")).clicked() {
                clear_calibration = true;
                ui.close();
            }
            ui.separator();
            if ui.button("Delete all measurements on this image").clicked() {
                delete_all = true;
                ui.close();
            }
            if ui.button("Fit image to window").clicked() {
                self.fit_pending = true;
                ui.close();
            }
        });
        if open_calibrate {
            if let (Some(px), Some(id)) = (selected_length, current_image_id) {
                self.calibrate = Some(CalibrateDraft {
                    image_id: id,
                    pixel_length: px,
                    known_mm: String::new(),
                    apply_to_series: self.images.len() > 1,
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
