//! Multiplanar reconstruction: linked planes through a volume, in their own
//! window (Horos: 2D Viewer > 3D > 2D Orthogonal MPR and 3D MPR).
//!
//! The three planes start square to the acquisition (axial, coronal and
//! sagittal for an axial CT) and meet at the crosshair. In each pane the
//! other planes show as coloured lines:
//!
//! - with the Crosshair tool, dragging a line moves that plane, dragging
//!   where the lines cross moves both, and dragging a round handle tilts
//!   the planes about the pane's own axis for oblique planes; the pane being
//!   worked in keeps still while the other two turn. Lock planes stops all
//!   of that, so measuring cannot move them by accident;
//! - the wheel steps the pane's own plane, Ctrl and the wheel zoom, the
//!   right button sets window and level and the middle button pans;
//! - the measuring tools (caliper, angle, rectangle, ellipse, arrow, point,
//!   text) draw on a plane in millimetres. A measurement belongs to the
//!   plane it was drawn on and shows whenever that plane is on screen;
//! - Save to study writes the plane, or a stack of planes, as a new series
//!   with its measurements; Save image writes the pane as a picture;
//! - the layouts are three panes in a row, a 2x2 grid whose fourth pane is
//!   the original slice through the crosshair, or one pane.
//!
//! Each plane is resampled on the CPU into an ordinary frame with square
//! pixels of a known size, then drawn by the same GPU path as the 2D viewer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportId};
use parking_lot::Mutex;

use ruthos_core::mpr_export::{save_planes, stack, MprSaveResult, SavedPlane};
use ruthos_core::volume::{
    cross, dot, norm, normalize, orientation_label, reslice, rotate, scale, sub, Plane, SlabMode, Volume, V3,
};
use ruthos_core::{SeriesRow, StudyRow};
use ruthos_dicom::{Clut, Frame};
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::pacs::Task;
use crate::roi::{roi_color, Roi, RoiKind};
use crate::shared::Shared;

/// The colour of each plane's line, as Horos draws them; the fourth is the
/// original slice.
const PLANE_COLOURS: [Color32; 4] = [
    Color32::from_rgb(235, 80, 70),
    Color32::from_rgb(90, 200, 90),
    Color32::from_rgb(80, 150, 245),
    Color32::from_rgb(200, 200, 200),
];

/// The pane that shows the original slice in the 2x2 layout.
const ORIGINAL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Row,
    Grid,
    Single(usize),
}

/// What the left button does in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MprTool {
    Crosshair,
    Window,
    Pan,
    Zoom,
    Length,
    Angle,
    Rectangle,
    Oval,
    Arrow,
    Point,
    Text,
}

impl MprTool {
    const ALL: [MprTool; 11] = [
        MprTool::Crosshair,
        MprTool::Window,
        MprTool::Pan,
        MprTool::Zoom,
        MprTool::Length,
        MprTool::Angle,
        MprTool::Rectangle,
        MprTool::Oval,
        MprTool::Arrow,
        MprTool::Point,
        MprTool::Text,
    ];

    fn label(self) -> &'static str {
        match self {
            MprTool::Crosshair => "Crosshair",
            MprTool::Window => "WL/WW",
            MprTool::Pan => "Move",
            MprTool::Zoom => "Zoom",
            MprTool::Length => "Caliper",
            MprTool::Angle => "Angle",
            MprTool::Rectangle => "Rectangle",
            MprTool::Oval => "Ellipse",
            MprTool::Arrow => "Arrow",
            MprTool::Point => "Point",
            MprTool::Text => "Text",
        }
    }

    fn key(self) -> Key {
        match self {
            MprTool::Crosshair => Key::C,
            MprTool::Window => Key::W,
            MprTool::Pan => Key::M,
            MprTool::Zoom => Key::Z,
            MprTool::Length => Key::L,
            MprTool::Angle => Key::A,
            MprTool::Rectangle => Key::B,
            MprTool::Oval => Key::E,
            MprTool::Arrow => Key::N,
            MprTool::Point => Key::P,
            MprTool::Text => Key::X,
        }
    }

    fn measures(self) -> bool {
        matches!(
            self,
            MprTool::Length
                | MprTool::Angle
                | MprTool::Rectangle
                | MprTool::Oval
                | MprTool::Arrow
                | MprTool::Point
                | MprTool::Text
        )
    }
}

/// One pane: how it is viewed and the last image made for it.
#[derive(Default)]
struct PaneView {
    state: ViewState,
    fitted: bool,
    /// What the image was made from, so it is remade only on a change.
    signature: String,
    frame: Option<Arc<Frame>>,
    plane: Option<Plane>,
    gpu: Option<Arc<GpuImage>>,
}

/// What a drag in a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    /// Moving another plane (by pane index) along its normal.
    Line(usize),
    /// Moving the crosshair within the pane.
    Centre,
    /// Turning the planes about this pane's normal; the last pointer angle.
    Rotate(f64),
    Window,
    Pan,
    Zoom,
    /// Drawing a measurement from this image point.
    Draw(Vec2),
    /// Moving a measurement's handle.
    Handle(usize, usize),
    /// Moving a whole measurement.
    Move(usize),
}

/// A measurement on a plane: the plane it belongs to, as a key, and the
/// measurement in that plane's pixels.
#[derive(Debug, Clone)]
struct PlaneRoi {
    key: String,
    roi: Roi,
}

/// An angle being drawn: the first arm is dragged, then the end clicked.
#[derive(Debug, Clone, Copy)]
struct AngleDraft {
    pane: usize,
    a: Vec2,
    b: Vec2,
    c: Vec2,
}

/// The text box for a text note.
#[derive(Debug, Clone)]
struct TextPrompt {
    pane: usize,
    at: Vec2,
    screen: Pos2,
    text: String,
    focus: bool,
}

/// Save to study.
struct SaveDialog {
    pane: usize,
    stack: bool,
    spacing_mm: f64,
    description: String,
    running: Option<Task<Result<MprSaveResult, String>>>,
    progress: Arc<Mutex<(usize, usize)>>,
    message: Option<String>,
}

type Loading = (
    Task<Result<Volume, String>>,
    Arc<Mutex<(usize, usize)>>,
    Arc<AtomicBool>,
);

pub struct MprState {
    pub id: u64,
    pub viewport_id: ViewportId,
    pub close_requested: bool,
    pub series_id: i64,
    study: StudyRow,
    series: SeriesRow,
    loading: Option<Loading>,
    volume: Option<Arc<Volume>>,
    error: Option<String>,
    /// The three plane normals' frame: rotated copies of the volume axes.
    axes: [V3; 3],
    /// Where the planes meet, in patient coordinates.
    centre: V3,
    /// Per pane, how far its own image has been turned back so it keeps
    /// still while the planes are tilted in it.
    phi: [f64; 3],
    panes: [PaneView; 4],
    window: (f32, f32),
    slab_mm: f32,
    slab_mode: SlabMode,
    drag: Option<(usize, Drag)>,
    clut: String,
    status: String,
    layout: Layout,
    tool: MprTool,
    /// The planes cannot be moved or tilted.
    locked: bool,
    rois: Vec<PlaneRoi>,
    selected: Option<usize>,
    /// The measurement being dragged out, in the pane's image pixels.
    draft: Option<(usize, Vec2, Vec2)>,
    angle_draft: Option<AngleDraft>,
    text_prompt: Option<TextPrompt>,
    save: Option<SaveDialog>,
    /// The pane last worked in: what Save acts on.
    active: usize,
    /// Frames drawn since the volume arrived, for the screenshot option.
    frames_shown: u32,
}

impl MprState {
    /// Open an MPR window on a series and start building its volume.
    pub fn open(shared: &Shared, study: StudyRow, series: SeriesRow, ctx: &egui::Context) -> Self {
        let id = shared.next_id();
        let files: Vec<std::path::PathBuf> = shared
            .db
            .images_for_series(series.id)
            .unwrap_or_default()
            .iter()
            .map(|i| shared.resolve(&i.path))
            .collect();
        let progress = Arc::new(Mutex::new((0usize, files.len())));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (progress.clone(), cancel.clone());
        let repaint = ctx.clone();
        let task = Task::spawn(ctx, "ruthos-mpr-load", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Volume::load(
                    &files,
                    &|done, total| {
                        *p.lock() = (done, total);
                        repaint.request_repaint();
                    },
                    &c,
                )
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("an internal error stopped the volume from loading; see crash.log".into()))
        });
        let layout = match std::env::var("RUTHOS_MPR_LAYOUT").ok().as_deref() {
            // Developer option: `RUTHOS_MPR_LAYOUT=grid|row|single`.
            Some("grid") => Layout::Grid,
            Some("single") => Layout::Single(0),
            _ => Layout::Row,
        };
        Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-mpr", id)),
            close_requested: false,
            series_id: series.id,
            study,
            series,
            loading: Some((task, progress, cancel)),
            volume: None,
            error: None,
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            centre: [0.0; 3],
            phi: [0.0; 3],
            panes: Default::default(),
            window: (40.0, 400.0),
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
            drag: None,
            clut: "No CLUT".into(),
            status: String::new(),
            layout,
            tool: MprTool::Crosshair,
            locked: false,
            rois: Vec::new(),
            selected: None,
            draft: None,
            angle_draft: None,
            text_prompt: None,
            save: None,
            active: 0,
            frames_shown: 0,
        }
    }

    pub fn title(&self) -> String {
        format!(
            "MPR: {}  {} - Ruthos",
            self.study.patient_name,
            self.series.display_name()
        )
    }

    fn reset(&mut self) {
        let Some(v) = &self.volume else { return };
        self.axes = v.axes;
        self.centre = v.centre();
        self.phi = [0.0; 3];
        for p in &mut self.panes {
            p.fitted = false;
            p.signature.clear();
        }
    }

    /// Normal, right and down of pane `i`; the fourth pane is the original
    /// slice, square to the acquisition whatever the tilt.
    fn basis(&self, i: usize) -> (V3, V3, V3) {
        if i == ORIGINAL {
            if let Some(v) = &self.volume {
                return (v.axes[2], v.axes[0], v.axes[1]);
            }
        }
        pane_basis(&self.axes, self.phi[i.min(2)], i.min(2))
    }

    fn pane_name(&self, i: usize) -> String {
        if i == ORIGINAL {
            return "Original slice".into();
        }
        plane_name(self.basis(i).0)
    }

    /// The plane pane `i` shows.
    fn plane(&self, i: usize, v: &Volume) -> Plane {
        let (n, right, down) = self.basis(i);
        // The image is centred on the volume's centre, moved onto the plane,
        // so it stays put while the crosshair moves.
        let vc = v.centre();
        let centre = sub(vc, scale(n, dot(sub(vc, self.centre), n)));
        let diag = v.diagonal();
        let pixel_mm = v.finest().max(diag / 1024.0);
        let size = (diag / pixel_mm).ceil().clamp(16.0, 1024.0) as u32;
        Plane {
            centre,
            right,
            down,
            width: size,
            height: size,
            pixel_mm,
            slab_mm: self.slab_mm as f64,
            slab_mode: self.slab_mode,
        }
    }

    fn pump(&mut self, shared: &Shared) {
        if let Some((task, _, _)) = &self.loading {
            if let Some(result) = task.take() {
                self.loading = None;
                match result {
                    Ok(v) => {
                        let (c, w) = volume_window(&v);
                        self.window = (c, w);
                        if v.irregular_spacing {
                            self.status =
                                "Some slices are further apart than the rest: distances across the gap are approximate"
                                    .into();
                        }
                        self.volume = Some(Arc::new(v));
                        self.reset();
                        // Developer option: `RUTHOS_MPR_TILT=<degrees>` tilts the
                        // planes in the first pane, as a handle drag would.
                        if let Some(deg) = std::env::var("RUTHOS_MPR_TILT")
                            .ok()
                            .and_then(|v| v.parse::<f64>().ok())
                        {
                            let (n, _, _) = self.basis(0);
                            let da = deg.to_radians();
                            for a in &mut self.axes {
                                *a = normalize(rotate(*a, n, da));
                            }
                            self.phi[0] += da;
                        }
                        // Developer option: `RUTHOS_MPR_MEASURE=1` puts a caliper
                        // and a text note on the coronal plane.
                        if std::env::var_os("RUTHOS_MPR_MEASURE").is_some() {
                            if let Some(v) = self.volume.clone() {
                                let plane = self.plane(1, &v);
                                let key = plane_key(&plane);
                                let (w, h) = (plane.width as f32, plane.height as f32);
                                let a = Vec2::new(w * 0.3, h * 0.5);
                                let b = Vec2::new(w * 0.7, h * 0.5);
                                self.push_roi(&key, &plane, Roi::new(RoiKind::Length { a, b }));
                                let mut note = Roi::new(RoiKind::Text {
                                    p: Vec2::new(w * 0.3, h * 0.3),
                                });
                                note.name = Some("Lung fields".into());
                                self.push_roi(&key, &plane, note);
                                self.selected = Some(0);
                            }
                        }
                    }
                    Err(e) if e == "cancelled" => self.close_requested = true,
                    Err(e) => self.error = Some(e),
                }
            }
        }
        // A save that finished.
        if let Some(d) = &mut self.save {
            if let Some(r) = d.running.as_ref().and_then(|t| t.take()) {
                d.running = None;
                match r {
                    Ok(res) => {
                        let n = res.image_ids.len();
                        d.message = None;
                        self.status = format!(
                            "Saved {} to the study as \"{}\"",
                            if n == 1 {
                                "1 image".to_string()
                            } else {
                                format!("{n} images")
                            },
                            d.description
                        );
                        let _ = shared.import_tx.send(ruthos_core::ImportEvent::Finished(res.summary));
                        self.save = None;
                    }
                    Err(e) => d.message = Some(format!("Not saved: {e}")),
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        crate::crash::debug_panic("mpr");
        if ui.input(|i| i.viewport().close_requested()) {
            if let Some((_, _, cancel)) = &self.loading {
                cancel.store(true, Ordering::Relaxed);
            }
            self.close_requested = true;
        }
        self.pump(shared);
        self.keys(ui);
        egui::Panel::top("mpr-toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::top("mpr-tools").show(ui, |ui| self.tool_bar(ui, shared));
        egui::Panel::bottom("mpr-status").show(ui, |ui| {
            ui.label(egui::RichText::new(&self.status).size(12.0));
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.panes_ui(ui, shared));
        let ctx = ui.ctx().clone();
        self.text_prompt_ui(&ctx);
        self.save_dialog(&ctx, shared);

        // Developer option: `RUTHOS_MPR_SCREENSHOT=<file.png>` saves the
        // window as drawn, once the volume has been on screen a moment.
        if self.volume.is_some() {
            self.frames_shown += 1;
            if self.frames_shown < 30 {
                ui.ctx().request_repaint();
            }
            if self.frames_shown == 20 {
                if let Some(path) = std::env::var_os("RUTHOS_MPR_SCREENSHOT") {
                    ui.ctx().request_screenshot(move |img| {
                        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                        if let Some(png) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                            let _ = png.save(&path);
                        }
                    });
                }
            }
        }
    }

    /// Tool letters, Delete and Escape, as in the 2D viewer.
    fn keys(&mut self, ui: &Ui) {
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let (pressed, delete, escape) = ui.input(|i| {
            let plain = !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.command;
            let tool = MprTool::ALL.into_iter().find(|t| plain && i.key_pressed(t.key()));
            (
                tool,
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::Escape),
            )
        });
        if let Some(t) = pressed {
            self.tool = t;
            self.angle_draft = None;
        }
        if escape {
            self.draft = None;
            self.angle_draft = None;
            self.selected = None;
        }
        if delete {
            if let Some(i) = self.selected.take() {
                if i < self.rois.len() {
                    self.rois.remove(i);
                }
            }
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.volume.is_some(), |ui| {
                if ui
                    .button("Reset")
                    .on_hover_text("Back to the original planes")
                    .clicked()
                {
                    self.reset();
                }
                ui.toggle_value(&mut self.locked, "Lock planes")
                    .on_hover_text("Stop the planes moving or tilting while you measure");
                ui.separator();
                ui.label("Layout");
                let label = match self.layout {
                    Layout::Row => "1×3".to_string(),
                    Layout::Grid => "2×2".to_string(),
                    Layout::Single(i) => format!("1×1: {}", self.pane_name(i)),
                };
                egui::ComboBox::from_id_salt("mpr-layout")
                    .selected_text(label)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.layout, Layout::Row, "1×3: three planes");
                        ui.selectable_value(
                            &mut self.layout,
                            Layout::Grid,
                            "2×2: three planes and the original slice",
                        );
                        for i in 0..4 {
                            let name = format!("1×1: {}", self.pane_name(i));
                            ui.selectable_value(&mut self.layout, Layout::Single(i), name);
                        }
                    });
                ui.separator();
                ui.label("Slab");
                ui.add(
                    egui::Slider::new(&mut self.slab_mm, 0.0..=60.0)
                        .suffix(" mm")
                        .step_by(0.5),
                )
                .on_hover_text("Thickness of a slab of planes; 0 shows a single plane");
                egui::ComboBox::from_id_salt("mpr-slab")
                    .selected_text(match self.slab_mode {
                        SlabMode::Mip => "MIP",
                        SlabMode::MinIp => "MinIP",
                        SlabMode::Mean => "Mean",
                    })
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.slab_mode, SlabMode::Mip, "MIP")
                            .on_hover_text("Maximum intensity: vessels, bone, contrast");
                        ui.selectable_value(&mut self.slab_mode, SlabMode::MinIp, "MinIP")
                            .on_hover_text("Minimum intensity: airways, gas");
                        ui.selectable_value(&mut self.slab_mode, SlabMode::Mean, "Mean")
                            .on_hover_text("Average: a thicker, smoother slice");
                    });
                ui.separator();
                if ui
                    .button("Save to study…")
                    .on_hover_text(
                        "Write this plane, or a stack of planes, as a new series in the study, with its measurements",
                    )
                    .clicked()
                {
                    let pane = self.active;
                    let spacing = self.volume.as_ref().map(|v| v.finest().max(0.5)).unwrap_or(1.0);
                    self.save = Some(SaveDialog {
                        pane,
                        stack: false,
                        spacing_mm: (spacing * 10.0).round() / 10.0,
                        description: format!("MPR {}", self.pane_name(pane).to_lowercase()),
                        running: None,
                        progress: Arc::new(Mutex::new((0, 0))),
                        message: None,
                    });
                }
                if ui
                    .button("Save image…")
                    .on_hover_text("Save the pane last worked in as a PNG or JPEG picture")
                    .clicked()
                {
                    self.save_image();
                }
            });
        });
    }

    /// The tools, window and colours.
    fn tool_bar(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.volume.is_some(), |ui| {
                for t in MprTool::ALL {
                    if t == MprTool::Length {
                        ui.separator();
                    }
                    let key = format!("{:?}", t.key());
                    if ui
                        .selectable_label(self.tool == t, t.label())
                        .on_hover_text(format!("{} ({key})", t.label()))
                        .clicked()
                    {
                        self.tool = t;
                        self.angle_draft = None;
                    }
                }
                ui.separator();
                ui.label("WL/WW");
                let (mut c, mut w) = self.window;
                ui.add(egui::DragValue::new(&mut c).speed(2.0));
                ui.add(egui::DragValue::new(&mut w).speed(2.0).range(1.0..=100000.0));
                self.window = (c, w);
                egui::ComboBox::from_id_salt("mpr-presets")
                    .selected_text("Presets")
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, "Default").clicked() {
                            if let Some(v) = &self.volume {
                                self.window = volume_window(v);
                            }
                        }
                        for p in &shared.presets.presets {
                            if ui.selectable_label(false, &p.name).clicked() {
                                self.window = (p.center, p.width);
                            }
                        }
                    });
                egui::ComboBox::from_id_salt("mpr-clut")
                    .selected_text(self.clut.clone())
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for name in shared.cluts.names() {
                            if ui.selectable_label(self.clut == name, name).clicked() {
                                self.clut = name.to_string();
                            }
                        }
                    });
            });
        });
    }

    fn panes_ui(&mut self, ui: &mut Ui, shared: &Shared) {
        let area = ui.available_rect_before_wrap();
        if let Some(e) = &self.error {
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                format!("This series cannot be reconstructed.\n{e}"),
                FontId::proportional(15.0),
                Color32::from_rgb(240, 150, 120),
            );
            return;
        }
        let Some(volume) = self.volume.clone() else {
            let (done, total) = self.loading.as_ref().map(|l| *l.1.lock()).unwrap_or((0, 0));
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                format!("Building the volume: slice {done} of {total}"),
                FontId::proportional(15.0),
                Color32::from_gray(200),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let gap = 2.0;
        let cells: Vec<(usize, Rect)> = match self.layout {
            Layout::Row => {
                let w = (area.width() - 2.0 * gap) / 3.0;
                (0..3)
                    .map(|i| {
                        (
                            i,
                            Rect::from_min_size(
                                Pos2::new(area.left() + i as f32 * (w + gap), area.top()),
                                Vec2::new(w, area.height()),
                            ),
                        )
                    })
                    .collect()
            }
            Layout::Grid => {
                let (w, h) = ((area.width() - gap) / 2.0, (area.height() - gap) / 2.0);
                (0..4)
                    .map(|i| {
                        let (c, r) = ((i % 2) as f32, (i / 2) as f32);
                        (
                            i,
                            Rect::from_min_size(
                                Pos2::new(area.left() + c * (w + gap), area.top() + r * (h + gap)),
                                Vec2::new(w, h),
                            ),
                        )
                    })
                    .collect()
            }
            Layout::Single(i) => vec![(i, area)],
        };
        let clut = Arc::new(shared.cluts.get(&self.clut).cloned().unwrap_or_else(Clut::grayscale));
        for (i, rect) in cells {
            self.pane(ui, shared, &volume, i, rect, &clut);
        }
    }

    fn pane(&mut self, ui: &mut Ui, shared: &Shared, volume: &Volume, i: usize, rect: Rect, clut: &Arc<Clut>) {
        // The image, remade when this pane's plane changed.
        let plane = self.plane(i, volume);
        let n = plane.normal();
        let key = plane_key(&plane);
        let sig = format!("{key}|{:.2}|{:?}", self.slab_mm, self.slab_mode);
        if self.panes[i].signature != sig {
            let frame = Arc::new(reslice(volume, &plane));
            self.panes[i].gpu = Some(Arc::new(GpuImage::upload(
                &shared.render_state.device,
                &shared.render_state.queue,
                &frame,
            )));
            self.panes[i].frame = Some(frame);
            self.panes[i].plane = Some(plane);
            self.panes[i].signature = sig;
        }
        let response = ui.interact(
            rect,
            egui::Id::unique(("mpr-pane", self.id, i)),
            Sense::click_and_drag(),
        );
        let window = self.window;
        let pane = &mut self.panes[i];
        if !pane.fitted {
            pane.state.fit(rect, plane.width, plane.height, 1.0);
            pane.fitted = true;
        }
        pane.state.window_center = window.0;
        pane.state.window_width = window.1;
        let t = ViewTransform::new(rect, plane.width, plane.height, 1.0, pane.state);
        let (gpu, frame) = (pane.gpu.clone(), pane.frame.clone());
        let painter = ui.painter_at(rect);
        if let Some(gpu) = gpu {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ImageCallback {
                    key: self.id * 64 + 40 + i as u64,
                    image: gpu,
                    clut: clut.clone(),
                    transform: t,
                    inverted: false,
                    interpolation: Interpolation::Bilinear,
                },
            ));
        }

        // The crosshair: where the other planes cut this one.
        let (cx, cy) = plane.pixel_of(self.centre);
        let centre_screen = t.image_to_screen(Vec2::new(cx as f32, cy as f32));
        let reach = rect.width().max(rect.height()) * 2.0;
        let handle_at = rect.width().min(rect.height()) * 0.32;
        let mut lines: Vec<(usize, Vec2)> = Vec::new();
        for j in (0..3).filter(|j| *j != i) {
            let (nj, _, _) = self.basis(j);
            let c = cross(n, nj);
            if norm(c) < 1e-3 {
                continue; // parallel: no line
            }
            let d = normalize(c);
            let tip = t.image_to_screen(Vec2::new(
                (cx + dot(d, plane.right)) as f32,
                (cy + dot(d, plane.down)) as f32,
            ));
            lines.push((j, (tip - centre_screen).normalized()));
        }
        let rotatable = i != ORIGINAL;
        for (j, dir) in &lines {
            let colour = PLANE_COLOURS[*j];
            for s in [-1.0f32, 1.0] {
                painter.line_segment(
                    [centre_screen + *dir * s * 12.0, centre_screen + *dir * s * reach],
                    Stroke::new(1.0, colour),
                );
                if rotatable && !self.locked {
                    painter.circle_stroke(centre_screen + *dir * s * handle_at, 5.0, Stroke::new(1.5, colour));
                }
            }
        }
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, PLANE_COLOURS[i]), egui::StrokeKind::Inside);

        // Measurements on this plane, and the one being drawn.
        if let Some(frame) = &frame {
            for (k, pr) in self.rois.iter().enumerate().filter(|(_, r)| r.key == key) {
                let sel = self.selected == Some(k);
                pr.roi.draw(&painter, &t, frame, roi_color(sel), sel);
                if sel {
                    pr.roi.draw_handles(&painter, &t, None, roi_color(true));
                }
            }
            if let Some((p, a, b)) = self.draft {
                if p == i {
                    if let Some(kind) = draft_kind(self.tool, a, b) {
                        Roi::new(kind).draw(&painter, &t, frame, crate::theme::ROI_DRAFT, false);
                    }
                }
            }
            if let Some(d) = self.angle_draft.filter(|d| d.pane == i) {
                Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c }).draw(
                    &painter,
                    &t,
                    frame,
                    crate::theme::ROI_DRAFT,
                    false,
                );
            }
        }

        // Words: the plane, its position, the slab and the orientation.
        let font = FontId::proportional(13.0);
        let text = Color32::from_gray(230);
        crate::widgets::shadow_text(
            &painter,
            rect.left_top() + Vec2::new(8.0, 6.0),
            Align2::LEFT_TOP,
            &format!(
                "{}\n{:.1} mm{}{}",
                self.pane_name(i),
                dot(self.centre, n),
                if self.slab_mm > 0.0 && i != ORIGINAL {
                    format!(
                        "\nSlab {:.1} mm {}",
                        self.slab_mm,
                        match self.slab_mode {
                            SlabMode::Mip => "MIP",
                            SlabMode::MinIp => "MinIP",
                            SlabMode::Mean => "mean",
                        }
                    )
                } else {
                    String::new()
                },
                if self.locked { "\nPlanes locked" } else { "" }
            ),
            font.clone(),
            text,
        );
        crate::widgets::shadow_text(
            &painter,
            rect.left_bottom() + Vec2::new(8.0, -6.0),
            Align2::LEFT_BOTTOM,
            &format!("WL {:.0}  WW {:.0}", self.window.0, self.window.1),
            font.clone(),
            text,
        );
        let neg = |v: V3| scale(v, -1.0);
        for (pos, anchor, d) in [
            (
                rect.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                neg(plane.right),
            ),
            (
                rect.right_center() - Vec2::new(6.0, 0.0),
                Align2::RIGHT_CENTER,
                plane.right,
            ),
            (
                rect.center_top() + Vec2::new(0.0, 6.0),
                Align2::CENTER_TOP,
                neg(plane.down),
            ),
            (
                rect.center_bottom() - Vec2::new(0.0, 6.0),
                Align2::CENTER_BOTTOM,
                plane.down,
            ),
        ] {
            crate::widgets::shadow_text(&painter, pos, anchor, &orientation_label(d), font.clone(), text);
        }

        // The value and position under the pointer.
        if let Some(h) = response.hover_pos() {
            let ip = t.screen_to_image(h);
            if let Some(frame) = &frame {
                if let Some(v) = frame.value_at(ip.x.floor() as i64, ip.y.floor() as i64) {
                    let p = plane.point(ip.x as f64 - 0.5, ip.y as f64 - 0.5);
                    self.status = format!(
                        "{}: value {v:.0}   x {:.1}  y {:.1}  z {:.1} mm",
                        self.pane_name(i),
                        p[0],
                        p[1],
                        p[2]
                    );
                }
            }
        }

        self.pane_input(ui, &response, i, &t, &plane, &key, centre_screen, &lines, handle_at);
    }

    /// The index of the measurement on this plane under a screen point.
    fn roi_at(&self, key: &str, t: &ViewTransform, p: Pos2) -> Option<usize> {
        self.rois
            .iter()
            .enumerate()
            .filter(|(_, r)| r.key == key)
            .map(|(k, r)| (k, r.roi.screen_distance(t, p)))
            .filter(|(_, d)| *d < 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        i: usize,
        t: &ViewTransform,
        plane: &Plane,
        key: &str,
        centre_screen: Pos2,
        lines: &[(usize, Vec2)],
        handle_at: f32,
    ) {
        let n = plane.normal();
        if response.hovered() && (response.clicked() || response.drag_started()) {
            self.active = i;
        }
        // Wheel: step this pane's plane (unless locked); with Ctrl, zoom.
        if response.hovered() {
            let (scroll, ctrl, zoom) =
                ui.input(|inp| (inp.smooth_scroll_delta.y, inp.modifiers.ctrl, inp.zoom_delta()));
            if (zoom - 1.0).abs() > 1e-3 || (ctrl && scroll.abs() > 0.0) {
                let factor = if (zoom - 1.0).abs() > 1e-3 {
                    zoom
                } else {
                    (scroll * 0.002).exp()
                };
                let anchor = response.hover_pos().unwrap_or(t.rect.center());
                self.zoom_about(i, t, anchor, factor);
            } else if scroll.abs() > 0.0 && !self.locked {
                let step = self.volume.as_ref().map(|v| v.finest()).unwrap_or(1.0);
                let steps = (scroll / 40.0).round().clamp(-10.0, 10.0);
                let steps = if steps == 0.0 { scroll.signum() } else { steps };
                self.centre = sub(self.centre, scale(n, step * steps as f64));
            }
        }

        // Clicks: select a measurement, place a point, a text note, or the
        // end of an angle.
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let ip = t.screen_to_image(p);
                if let Some(d) = self.angle_draft.filter(|d| d.pane == i) {
                    let roi = Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: ip });
                    self.angle_draft = None;
                    if !roi.is_degenerate() {
                        self.push_roi(key, plane, roi);
                    }
                } else if let Some(k) = self.roi_at(key, t, p) {
                    self.selected = Some(k);
                } else {
                    self.selected = None;
                    match self.tool {
                        MprTool::Point => self.push_roi(key, plane, Roi::new(RoiKind::Point { p: ip })),
                        MprTool::Text => {
                            self.text_prompt = Some(TextPrompt {
                                pane: i,
                                at: ip,
                                screen: p,
                                text: String::new(),
                                focus: true,
                            })
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(d) = &mut self.angle_draft {
            if d.pane == i {
                if let Some(h) = response.hover_pos() {
                    d.c = t.screen_to_image(h);
                }
            }
        }

        // A press decides what the drag does.
        if response.drag_started() {
            let origin = ui.input(|inp| inp.pointer.press_origin()).or(response.hover_pos());
            let (primary, secondary, middle) = ui.input(|inp| {
                (
                    inp.pointer.button_down(egui::PointerButton::Primary),
                    inp.pointer.button_down(egui::PointerButton::Secondary),
                    inp.pointer.button_down(egui::PointerButton::Middle),
                )
            });
            let what = if middle {
                Drag::Pan
            } else if secondary {
                Drag::Window
            } else if let (true, Some(p)) = (primary, origin) {
                self.primary_drag(i, t, key, p, centre_screen, lines, handle_at)
            } else {
                Drag::Window
            };
            self.drag = Some((i, what));
        }
        if response.dragged() {
            if let Some((pane, what)) = self.drag.filter(|(p, _)| *p == i) {
                let d = response.drag_delta();
                // Screen points to millimetres in this plane.
                let mm = plane.pixel_mm / self.panes[pane].state.zoom.max(1e-6) as f64;
                let delta =
                    ruthos_core::volume::add(scale(plane.right, d.x as f64 * mm), scale(plane.down, d.y as f64 * mm));
                let pointer = response.hover_pos().or(ui.input(|inp| inp.pointer.latest_pos()));
                match what {
                    Drag::Centre => self.centre = ruthos_core::volume::add(self.centre, delta),
                    Drag::Line(j) => {
                        let (nj, _, _) = self.basis(j);
                        self.centre = ruthos_core::volume::add(self.centre, scale(nj, dot(delta, nj)));
                    }
                    Drag::Rotate(last) => {
                        if let Some(p) = pointer {
                            let rel = p - centre_screen;
                            let angle = (rel.y as f64).atan2(rel.x as f64);
                            let mut da = angle - last;
                            if da > std::f64::consts::PI {
                                da -= std::f64::consts::TAU;
                            } else if da < -std::f64::consts::PI {
                                da += std::f64::consts::TAU;
                            }
                            for a in &mut self.axes {
                                *a = normalize(rotate(*a, n, da));
                            }
                            self.phi[i] += da;
                            self.drag = Some((i, Drag::Rotate(angle)));
                        }
                    }
                    Drag::Window => {
                        let (c, w) = self.window;
                        let s = (w.abs() / 256.0).max(0.5);
                        self.window = (c + d.y * s, (w + d.x * s).max(1.0));
                    }
                    Drag::Pan => self.panes[i].state.pan += d,
                    Drag::Zoom => {
                        let anchor = t.rect.center() + self.panes[i].state.pan;
                        self.zoom_about(i, t, anchor, (-d.y * 0.01).exp());
                    }
                    Drag::Draw(a) => {
                        if let Some(p) = pointer {
                            self.draft = Some((i, a, t.screen_to_image(p)));
                        }
                    }
                    Drag::Handle(k, h) => {
                        if let Some(p) = pointer {
                            if let Some(r) = self.rois.get_mut(k) {
                                r.roi.set_handle(h, t.screen_to_image(p));
                            }
                        }
                    }
                    Drag::Move(k) => {
                        if let Some(r) = self.rois.get_mut(k) {
                            r.roi.translate(d / t.state.zoom.max(1e-6));
                        }
                    }
                }
            }
        }
        if response.drag_stopped() {
            if let Some((p, Drag::Draw(_))) = self.drag {
                if let Some((dp, a, b)) = self.draft.take() {
                    if dp == p && self.tool == MprTool::Angle {
                        if (b - a).length() >= 1.0 {
                            self.angle_draft = Some(AngleDraft { pane: p, a, b, c: b });
                            self.status = "Angle: now click the end of the second arm".into();
                        }
                    } else if let Some(kind) = draft_kind(self.tool, a, b) {
                        let roi = Roi::new(kind);
                        if !roi.is_degenerate() {
                            self.push_roi(key, plane, roi);
                        }
                    }
                }
            }
            self.drag = None;
        }
        if response.double_clicked() && !self.tool.measures() {
            self.panes[i].fitted = false;
        }
        // Keep the crosshair inside the volume.
        if let Some(v) = &self.volume {
            let idx = v.to_index(self.centre);
            let clamped = [
                idx[0].clamp(0.0, (v.dims[0] - 1) as f64),
                idx[1].clamp(0.0, (v.dims[1] - 1) as f64),
                idx[2].clamp(0.0, (v.dims[2] - 1) as f64),
            ];
            if clamped != idx {
                self.centre = v.to_patient(clamped);
            }
        }
    }

    /// What a left-button drag starting at `p` does, by tool.
    #[allow(clippy::too_many_arguments)]
    fn primary_drag(
        &mut self,
        i: usize,
        t: &ViewTransform,
        key: &str,
        p: Pos2,
        centre_screen: Pos2,
        lines: &[(usize, Vec2)],
        handle_at: f32,
    ) -> Drag {
        // A handle or the body of the selected measurement, with any tool.
        if let Some(k) = self
            .selected
            .filter(|k| self.rois.get(*k).is_some_and(|r| r.key == key))
        {
            if let Some(h) = self.rois[k].roi.handle_hit(t, p) {
                return Drag::Handle(k, h);
            }
            if self.rois[k].roi.screen_distance(t, p) < 6.0 {
                return Drag::Move(k);
            }
        }
        match self.tool {
            MprTool::Crosshair if !self.locked => {
                let rel = p - centre_screen;
                let on_handle = i != ORIGINAL
                    && lines.iter().any(|(_, dir)| {
                        [-1.0f32, 1.0]
                            .iter()
                            .any(|s| (p - (centre_screen + *dir * *s * handle_at)).length() < 9.0)
                    });
                if on_handle {
                    Drag::Rotate((rel.y as f64).atan2(rel.x as f64))
                } else if rel.length() < 12.0 {
                    Drag::Centre
                } else {
                    let near = lines
                        .iter()
                        .map(|(j, dir)| (*j, (rel - *dir * rel.dot(*dir)).length()))
                        .filter(|(_, d)| *d < 7.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1));
                    match near {
                        Some((j, _)) => Drag::Line(j),
                        None => Drag::Window,
                    }
                }
            }
            MprTool::Crosshair | MprTool::Window => Drag::Window,
            MprTool::Pan => Drag::Pan,
            MprTool::Zoom => Drag::Zoom,
            MprTool::Length | MprTool::Angle | MprTool::Rectangle | MprTool::Oval | MprTool::Arrow => {
                self.angle_draft = None;
                let a = t.screen_to_image(p);
                self.draft = Some((i, a, a));
                Drag::Draw(a)
            }
            MprTool::Point | MprTool::Text => Drag::Window,
        }
    }

    fn zoom_about(&mut self, i: usize, t: &ViewTransform, anchor: Pos2, factor: f32) {
        let state = &mut self.panes[i].state;
        let before = t.screen_to_image(anchor);
        state.zoom = (state.zoom * factor).clamp(0.05, 40.0);
        let t2 = ViewTransform::new(t.rect, t.image_w, t.image_h, 1.0, *state);
        state.pan += anchor - t2.image_to_screen(before);
    }

    fn push_roi(&mut self, key: &str, _plane: &Plane, roi: Roi) {
        self.rois.push(PlaneRoi {
            key: key.to_string(),
            roi,
        });
        self.selected = Some(self.rois.len() - 1);
    }

    fn text_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(mut tp) = self.text_prompt.take() else { return };
        let mut done: Option<bool> = None;
        egui::Area::new(egui::Id::unique(("ruthos-mpr-text", self.id)))
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
                        if ui.button("Add").clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
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
                if let (false, Some(plane)) = (text.is_empty(), self.panes[tp.pane].plane) {
                    let mut roi = Roi::new(RoiKind::Text { p: tp.at });
                    roi.name = Some(text);
                    let key = plane_key(&plane);
                    self.push_roi(&key, &plane, roi);
                }
            }
        }
    }

    /// Save image: the pane last worked in, windowed as shown.
    fn save_image(&mut self) {
        let i = self.active;
        let Some(frame) = self.panes[i].frame.clone() else {
            return;
        };
        let name = format!(
            "{}_{}.png",
            self.study.patient_name.replace([',', ' ', '^'], "_"),
            self.pane_name(i).replace(' ', "_")
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save image")
            .add_filter("PNG image", &["png"])
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, self.window.0, self.window.1, &Clut::grayscale());
        match image::RgbImage::from_raw(frame.width, frame.height, rgb).map(|img| img.save(&path)) {
            Some(Ok(())) => self.status = format!("Saved {}", path.display()),
            Some(Err(e)) => self.status = format!("Not saved: {e}"),
            None => self.status = "Not saved: the image size did not match".into(),
        }
    }

    /// Save to study: one plane or a stack, with the measurements drawn on
    /// those planes, as a new series.
    fn save_dialog(&mut self, ctx: &egui::Context, shared: &Shared) {
        let Some(mut d) = self.save.take() else { return };
        let mut close = false;
        let mut start = false;
        let pane_name = self.pane_name(d.pane);
        let on_plane = self.panes[d.pane]
            .plane
            .map(|p| {
                let k = plane_key(&p);
                self.rois.iter().filter(|r| r.key == k).count()
            })
            .unwrap_or(0);
        let total_rois = self.rois.len();
        egui::Window::new("Save to study")
            .id(egui::Id::unique(("ruthos-mpr-save", self.id)))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("From the {} pane.", pane_name.to_lowercase()));
                ui.radio_value(
                    &mut d.stack,
                    false,
                    format!(
                        "This plane, with its {}",
                        if on_plane == 1 {
                            "measurement".to_string()
                        } else {
                            format!("{on_plane} measurements")
                        }
                    ),
                );
                ui.radio_value(&mut d.stack, true, "A stack of parallel planes across the volume");
                if d.stack {
                    ui.horizontal(|ui| {
                        ui.label("Every");
                        ui.add(
                            egui::DragValue::new(&mut d.spacing_mm)
                                .range(0.1..=50.0)
                                .speed(0.1)
                                .suffix(" mm"),
                        );
                        if self.slab_mm > 0.0 {
                            ui.label(format!("(each a {:.1} mm slab)", self.slab_mm));
                        }
                    });
                }
                if total_rois > on_plane {
                    ui.label(
                        egui::RichText::new("Measurements on other planes are saved when their plane is.")
                            .size(12.0)
                            .color(Color32::from_gray(110)),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label("Series description");
                    ui.add(egui::TextEdit::singleline(&mut d.description).desired_width(200.0));
                });
                if let Some(m) = &d.message {
                    ui.label(egui::RichText::new(m).color(Color32::from_rgb(190, 40, 30)));
                }
                ui.horizontal(|ui| {
                    if d.running.is_some() {
                        let (done, total) = *d.progress.lock();
                        ui.spinner();
                        ui.label(format!("Saving image {done} of {total}"));
                    } else {
                        if ui.button("Save").clicked() {
                            start = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                });
            });
        if start {
            self.start_save(&mut d, ctx, shared);
        }
        if d.running.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if !close {
            self.save = Some(d);
        }
    }

    fn start_save(&self, d: &mut SaveDialog, ctx: &egui::Context, shared: &Shared) {
        let (Some(volume), Some(base)) = (self.volume.clone(), self.panes[d.pane].plane) else {
            return;
        };
        let planes = if d.stack {
            stack(&volume, &base, d.spacing_mm, None)
        } else {
            vec![base]
        };
        let rois: Vec<PlaneRoi> = self.rois.clone();
        let Some(template) = shared
            .db
            .images_for_series(self.series.id)
            .ok()
            .and_then(|v| v.first().cloned())
            .map(|img| shared.resolve(&img.path))
        else {
            d.message = Some("Not saved: the series has no images".into());
            return;
        };
        let (db, paths) = (shared.db.clone(), shared.paths.clone());
        let description = d.description.trim().to_string();
        let window = self.window;
        let progress = d.progress.clone();
        let repaint = ctx.clone();
        d.message = None;
        d.running = Some(Task::spawn(ctx, "ruthos-mpr-save", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Each plane takes the measurements drawn on it.
                let saved: Vec<SavedPlane> = planes
                    .iter()
                    .map(|p| {
                        let key = plane_key(p);
                        let on: Vec<&PlaneRoi> = rois.iter().filter(|r| r.key == key).collect();
                        let records = if on.is_empty() {
                            Vec::new()
                        } else {
                            let frame = reslice(&volume, p);
                            on.iter().map(|r| r.roi.to_record(0, 0, Some(&frame))).collect()
                        };
                        SavedPlane {
                            plane: *p,
                            rois: records,
                        }
                    })
                    .collect();
                save_planes(
                    &db,
                    &paths,
                    &template,
                    &volume,
                    &saved,
                    if description.is_empty() { "MPR" } else { &description },
                    window,
                    &|done, total| {
                        *progress.lock() = (done, total);
                        repaint.request_repaint();
                    },
                )
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("an internal error stopped the save; see crash.log".into()))
        }));
    }
}

/// A plane's identity for its measurements: where it is and how it lies.
fn plane_key(p: &Plane) -> String {
    let r = |v: f64| (v * 1000.0).round() as i64;
    format!(
        "{},{},{}|{},{},{}|{},{},{}|{}|{}",
        r(p.right[0]),
        r(p.right[1]),
        r(p.right[2]),
        r(p.down[0]),
        r(p.down[1]),
        r(p.down[2]),
        (p.centre[0] * 20.0).round() as i64,
        (p.centre[1] * 20.0).round() as i64,
        (p.centre[2] * 20.0).round() as i64,
        p.width,
        r(p.pixel_mm)
    )
}

fn draft_kind(tool: MprTool, a: Vec2, b: Vec2) -> Option<RoiKind> {
    Some(match tool {
        MprTool::Length | MprTool::Angle => RoiKind::Length { a, b },
        MprTool::Rectangle => RoiKind::Rect { a, b },
        MprTool::Oval => RoiKind::Oval { a, b },
        MprTool::Arrow => RoiKind::Arrow { a, b },
        _ => return None,
    })
}

/// The window the series asks for, else its full range.
fn volume_window(v: &Volume) -> (f32, f32) {
    match (v.meta.window_center, v.meta.window_width) {
        (Some(c), Some(w)) if w > 0.0 => (c as f32, w as f32),
        _ => ((v.min + v.max) / 2.0, (v.max - v.min).max(1.0)),
    }
}

/// Normal, right and down of pane `i` as it is shown, from the planes'
/// frame and how far the pane's own image is turned back.
fn pane_basis(axes: &[V3; 3], phi: f64, i: usize) -> (V3, V3, V3) {
    let [a0, a1, a2] = *axes;
    let neg = |v: V3| scale(v, -1.0);
    let (right, down) = match i {
        0 => (a0, a1),
        1 => (a0, neg(a2)),
        _ => (a1, neg(a2)),
    };
    let n = normalize(cross(right, down));
    (n, rotate(right, n, -phi), rotate(down, n, -phi))
}

/// The anatomical plane a normal is closest to, or "Oblique (near ...)".
fn plane_name(n: V3) -> String {
    let (k, strength) = (0..3)
        .map(|k| (k, n[k].abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((2, 1.0));
    let name = ["Sagittal", "Coronal", "Axial"][k];
    if strength > 0.999 {
        name.to_string()
    } else {
        format!("Oblique (near {})", name.to_lowercase())
    }
}

/// Whether a series can be opened in MPR before its volume is built: a
/// stack of at least three single-frame images.
pub fn series_suits_mpr(images: &[ruthos_core::ImageRow]) -> bool {
    images.len() >= 3 && images.iter().all(|i| i.number_of_frames <= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXES: [V3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    #[test]
    fn the_three_panes_start_square_and_named() {
        let names: Vec<String> = (0..3).map(|i| plane_name(pane_basis(&AXES, 0.0, i).0)).collect();
        assert_eq!(names, ["Axial", "Coronal", "Sagittal"]);
        for i in 0..3 {
            let (n, r, d) = pane_basis(&AXES, 0.0, i);
            assert!(dot(n, r).abs() < 1e-9 && dot(n, d).abs() < 1e-9 && dot(r, d).abs() < 1e-9);
        }
        // Coronal shows the head at the top: down points to the feet.
        assert_eq!(pane_basis(&AXES, 0.0, 1).2, [0.0, 0.0, -1.0]);
    }

    #[test]
    fn tilting_in_a_pane_turns_the_others_but_not_it() {
        let mut axes = AXES;
        let mut phi = [0.0; 3];
        let before = pane_basis(&axes, phi[0], 0);
        // What a rotate drag in the axial pane does.
        let n = before.0;
        let da = 20f64.to_radians();
        for a in &mut axes {
            *a = normalize(rotate(*a, n, da));
        }
        phi[0] += da;
        let after = pane_basis(&axes, phi[0], 0);
        for k in 0..3 {
            assert!((after.1[k] - before.1[k]).abs() < 1e-9, "the axial pane keeps still");
            assert!((after.2[k] - before.2[k]).abs() < 1e-9);
        }
        assert!(plane_name(pane_basis(&axes, phi[1], 1).0).starts_with("Oblique"));
        assert!(plane_name(pane_basis(&axes, phi[2], 2).0).starts_with("Oblique"));
        let ns: Vec<V3> = (0..3).map(|i| pane_basis(&axes, phi[i], i).0).collect();
        assert!(dot(ns[0], ns[1]).abs() < 1e-9 && dot(ns[1], ns[2]).abs() < 1e-9 && dot(ns[0], ns[2]).abs() < 1e-9);
    }

    #[test]
    fn a_measurement_belongs_to_its_plane() {
        let p = Plane {
            centre: [1.0, 2.0, 3.0],
            right: [1.0, 0.0, 0.0],
            down: [0.0, 1.0, 0.0],
            width: 200,
            height: 200,
            pixel_mm: 0.8,
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
        };
        // A slab or a hair of float noise is the same plane; a step is not.
        let same = Plane {
            centre: [1.0 + 1e-6, 2.0, 3.0],
            slab_mm: 10.0,
            ..p
        };
        let moved = Plane {
            centre: [1.0, 2.0, 3.8],
            ..p
        };
        assert_eq!(plane_key(&p), plane_key(&same));
        assert_ne!(plane_key(&p), plane_key(&moved));
        assert!(draft_kind(MprTool::Length, Vec2::ZERO, Vec2::X).is_some());
        assert!(draft_kind(MprTool::Point, Vec2::ZERO, Vec2::X).is_none());
    }
}
