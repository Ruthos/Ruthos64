//! Multiplanar reconstruction: three linked planes through a volume, in
//! their own window (Horos: 2D Viewer > 3D > 2D Orthogonal MPR and 3D MPR).
//!
//! The three planes start square to the acquisition (axial, coronal and
//! sagittal for an axial CT) and meet at the crosshair. In each pane the
//! other two planes show as coloured lines:
//!
//! - dragging a line moves that plane; dragging where the lines cross moves
//!   both;
//! - dragging one of the round handles on a line tilts the planes about
//!   the pane's own axis, which gives oblique planes; the pane being worked
//!   in keeps still while the other two turn;
//! - the wheel steps the pane's own plane through the volume, Ctrl and the
//!   wheel zoom, the right button sets window and level and the middle
//!   button pans;
//! - a thick slab shows a maximum, minimum or mean intensity projection.
//!
//! Each plane is resampled on the CPU into an ordinary frame with square
//! pixels of a known size, then drawn by the same GPU path as the 2D viewer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportId};
use parking_lot::Mutex;

use ruthos_core::volume::{
    cross, dot, normalize, orientation_label, reslice, rotate, scale, sub, Plane, SlabMode, Volume, V3,
};
use ruthos_core::{SeriesRow, StudyRow};
use ruthos_dicom::{Clut, Frame};
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::pacs::Task;
use crate::shared::Shared;

/// The colour of each plane's line, as Horos draws them.
const PLANE_COLOURS: [Color32; 3] = [
    Color32::from_rgb(235, 80, 70),
    Color32::from_rgb(90, 200, 90),
    Color32::from_rgb(80, 150, 245),
];

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
    panes: [PaneView; 3],
    window: (f32, f32),
    slab_mm: f32,
    slab_mode: SlabMode,
    drag: Option<(usize, Drag)>,
    clut: String,
    status: String,
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

    fn basis(&self, i: usize) -> (V3, V3, V3) {
        pane_basis(&self.axes, self.phi[i], i)
    }

    fn pane_name(&self, i: usize) -> String {
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

    fn pump(&mut self) {
        let Some((task, _, _)) = &self.loading else { return };
        let Some(result) = task.take() else { return };
        self.loading = None;
        match result {
            Ok(v) => {
                let (c, w) = volume_window(&v);
                self.window = (c, w);
                if v.irregular_spacing {
                    self.status =
                        "Some slices are further apart than the rest: distances across the gap are approximate".into();
                }
                self.volume = Some(Arc::new(v));
                self.reset();
            }
            Err(e) if e == "cancelled" => self.close_requested = true,
            Err(e) => self.error = Some(e),
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
        self.pump();
        egui::Panel::top("mpr-toolbar").show(ui, |ui| self.toolbar(ui, shared));
        egui::Panel::bottom("mpr-status").show(ui, |ui| {
            ui.label(egui::RichText::new(&self.status).size(12.0));
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.panes_ui(ui, shared));
    }

    fn toolbar(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.volume.is_some(), |ui| {
                if ui
                    .button("Reset")
                    .on_hover_text("Back to the original planes")
                    .clicked()
                {
                    self.reset();
                }
                ui.separator();
                ui.label("WL/WW");
                let (mut c, mut w) = self.window;
                ui.add(egui::DragValue::new(&mut c).speed(2.0));
                ui.add(egui::DragValue::new(&mut w).speed(2.0).range(1.0..=100000.0));
                self.window = (c, w);
                egui::ComboBox::from_id_salt("mpr-presets")
                    .selected_text("Presets")
                    .width(130.0)
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
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for name in shared.cluts.names() {
                            if ui.selectable_label(self.clut == name, name).clicked() {
                                self.clut = name.to_string();
                            }
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
        let w = (area.width() - 2.0 * gap) / 3.0;
        let rects: Vec<Rect> = (0..3)
            .map(|i| {
                Rect::from_min_size(
                    Pos2::new(area.left() + i as f32 * (w + gap), area.top()),
                    Vec2::new(w, area.height()),
                )
            })
            .collect();
        let clut = Arc::new(shared.cluts.get(&self.clut).cloned().unwrap_or_else(Clut::grayscale));
        for (i, rect) in rects.iter().enumerate() {
            self.pane(ui, shared, &volume, i, *rect, &clut);
        }
    }

    fn pane(&mut self, ui: &mut Ui, shared: &Shared, volume: &Volume, i: usize, rect: Rect, clut: &Arc<Clut>) {
        // The image, remade when this pane's plane changed.
        let plane = self.plane(i, volume);
        let n = plane.normal();
        let sig = format!(
            "{:.4?}|{:.4?}|{:.4?}|{:.3}|{:.2}|{:?}",
            plane.right,
            plane.down,
            plane.centre,
            dot(self.centre, n),
            self.slab_mm,
            self.slab_mode
        );
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
            let d = normalize(cross(n, nj));
            let tip = t.image_to_screen(Vec2::new(
                (cx + dot(d, plane.right)) as f32,
                (cy + dot(d, plane.down)) as f32,
            ));
            let dir = (tip - centre_screen).normalized();
            lines.push((j, dir));
        }
        for (j, dir) in &lines {
            let colour = PLANE_COLOURS[*j];
            let gapr = 12.0;
            for s in [-1.0f32, 1.0] {
                painter.line_segment(
                    [centre_screen + *dir * s * gapr, centre_screen + *dir * s * reach],
                    Stroke::new(1.0, colour),
                );
                painter.circle_stroke(centre_screen + *dir * s * handle_at, 5.0, Stroke::new(1.5, colour));
            }
        }
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, PLANE_COLOURS[i]), egui::StrokeKind::Inside);

        // Words: the plane, its position, the slab and the orientation.
        let font = FontId::proportional(13.0);
        let text = Color32::from_gray(230);
        crate::widgets::shadow_text(
            &painter,
            rect.left_top() + Vec2::new(8.0, 6.0),
            Align2::LEFT_TOP,
            &format!(
                "{}\n{:.1} mm{}",
                self.pane_name(i),
                dot(self.centre, n),
                if self.slab_mm > 0.0 {
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
                }
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

        self.pane_input(ui, &response, i, &t, &plane, centre_screen, &lines, handle_at);
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        i: usize,
        t: &ViewTransform,
        plane: &Plane,
        centre_screen: Pos2,
        lines: &[(usize, Vec2)],
        handle_at: f32,
    ) {
        let n = plane.normal();
        // Wheel: step this pane's plane; with Ctrl, zoom.
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
                let state = &mut self.panes[i].state;
                let before = t.screen_to_image(anchor);
                state.zoom = (state.zoom * factor).clamp(0.05, 40.0);
                let t2 = ViewTransform::new(t.rect, t.image_w, t.image_h, 1.0, *state);
                state.pan += anchor - t2.image_to_screen(before);
            } else if scroll.abs() > 0.0 {
                let step = self.volume.as_ref().map(|v| v.finest()).unwrap_or(1.0);
                let steps = (scroll / 40.0).round().clamp(-10.0, 10.0);
                let steps = if steps == 0.0 { scroll.signum() } else { steps };
                self.centre = sub(self.centre, scale(n, step * steps as f64));
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
                let rel = p - centre_screen;
                let on_handle = lines.iter().any(|(_, dir)| {
                    [-1.0f32, 1.0]
                        .iter()
                        .any(|s| (p - (centre_screen + *dir * *s * handle_at)).length() < 9.0)
                });
                if on_handle {
                    Drag::Rotate((rel.y as f64).atan2(rel.x as f64))
                } else if rel.length() < 12.0 {
                    Drag::Centre
                } else {
                    // Distance to each line, through the centre.
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
                match what {
                    Drag::Centre => self.centre = ruthos_core::volume::add(self.centre, delta),
                    Drag::Line(j) => {
                        let (nj, _, _) = self.basis(j);
                        self.centre = ruthos_core::volume::add(self.centre, scale(nj, dot(delta, nj)));
                    }
                    Drag::Rotate(last) => {
                        if let Some(p) = response.hover_pos().or(ui.input(|inp| inp.pointer.latest_pos())) {
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
                }
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        if response.double_clicked() {
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
        // The planes stay at right angles to each other.
        let ns: Vec<V3> = (0..3).map(|i| pane_basis(&axes, phi[i], i).0).collect();
        assert!(dot(ns[0], ns[1]).abs() < 1e-9 && dot(ns[1], ns[2]).abs() < 1e-9 && dot(ns[0], ns[2]).abs() < 1e-9);
    }
}
