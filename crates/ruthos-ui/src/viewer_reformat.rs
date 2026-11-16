//! A temporary orthogonal view in the 2D viewer. Derived pixels have no archive image id.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use egui::{Align2, Color32, FontId, Key, PointerButton, Pos2, Sense, Ui, Vec2};
use ruthos_core::volume::{
    add, dot, normalize, orientation_label, reslice, scale, sub, OrthogonalPlane, OrthogonalStack, Plane, Volume, V3,
};
use ruthos_dicom::{Clut, Frame};
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use super::{drag_scroll_position, Tool};
use crate::{pacs::Task, shared::Shared, widgets::shadow_text};

#[derive(Clone, Copy, PartialEq, Default)]
pub(super) struct ReformatOptions {
    pub(super) plane: OrthogonalPlane,
    pub(super) slab_mm: f64,
}

#[derive(Clone, Copy, PartialEq)]
struct SliceRequest {
    options: ReformatOptions,
    index: usize,
}

struct DisplaySlice {
    request: SliceRequest,
    plane: Plane,
    count: usize,
    frame: Frame,
    gpu: Arc<GpuImage>,
}

struct PendingSlice {
    request: SliceRequest,
    plane: Plane,
    count: usize,
    task: Task<Result<Frame, String>>,
}

struct ScrollDrag {
    button: PointerButton,
    y: f32,
    index: usize,
}

pub(super) struct Reformat {
    options: ReformatOptions,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicUsize>,
    total: usize,
    loading: Option<Task<Result<Volume, String>>>,
    volume: Option<Arc<Volume>>,
    stack: Option<OrthogonalStack>,
    position: Option<V3>,
    index: usize,
    pending: Option<PendingSlice>,
    displayed: Option<DisplaySlice>,
    view: ViewState,
    fit: bool,
    clut: String,
    inverted: bool,
    id: u64,
    drag: Option<ScrollDrag>,
    wheel: f32,
}

impl Reformat {
    pub(super) fn new(
        ctx: &egui::Context,
        files: Vec<PathBuf>,
        options: ReformatOptions,
        position: Option<V3>,
        display: (ViewState, String, bool),
        id: u64,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let total = files.len();
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        let loading = Task::spawn(ctx, "ruthos-reformat-load", move || {
            std::panic::catch_unwind(|| {
                Volume::load(
                    &files,
                    &|done, _| {
                        worker_progress.fetch_max(done, Ordering::Relaxed);
                    },
                    &worker_cancel,
                )
                .map_err(|e| e.to_string())
            })
            .unwrap_or_else(|_| Err("An internal error stopped the series from loading".into()))
        });
        let (view, clut, inverted) = display;
        let view = if options.plane == OrthogonalPlane::Acquisition {
            view
        } else {
            ViewState {
                window_center: view.window_center,
                window_width: view.window_width,
                ..ViewState::default()
            }
        };
        Self {
            options,
            cancel,
            progress,
            total,
            loading: Some(loading),
            volume: None,
            stack: None,
            position,
            index: 0,
            pending: None,
            displayed: None,
            view,
            fit: true,
            clut,
            inverted,
            id,
            drag: None,
            wheel: 0.0,
        }
    }

    pub(super) fn options(&self) -> ReformatOptions {
        self.options
    }

    pub(super) fn set_options(&mut self, options: ReformatOptions) {
        if self.options.plane != options.plane {
            self.stack = None;
            self.drag = None;
            self.wheel = 0.0;
            self.fit = true;
            self.view.rotation_deg = 0.0;
            self.view.flip_horizontal = false;
            self.view.flip_vertical = false;
        }
        self.options = options;
    }

    pub(super) fn reset(&mut self) {
        let (center, width) = self
            .displayed
            .as_ref()
            .map(|slice| slice.frame.default_window())
            .unwrap_or((self.view.window_center, self.view.window_width));
        self.view = ViewState {
            window_center: center,
            window_width: width,
            ..ViewState::default()
        };
        self.fit = true;
    }

    pub(super) fn view_mut(&mut self) -> &mut ViewState {
        &mut self.view
    }

    pub(super) fn show(&mut self, ui: &mut Ui, shared: &Shared, tool: Tool, right_tool: Tool) -> Result<(), String> {
        let rect = ui.available_rect_before_wrap();
        if !rect.is_positive() {
            return Ok(());
        }
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);

        if let Some(result) = self.loading.as_ref().and_then(Task::take) {
            self.loading = None;
            self.volume = Some(Arc::new(result?));
        }
        let Some(volume) = self.volume.clone() else {
            shadow_text(
                &painter,
                rect.center(),
                Align2::CENTER_CENTER,
                &format!(
                    "Loading slices… {} / {}",
                    self.progress.load(Ordering::Relaxed),
                    self.total
                ),
                FontId::proportional(14.0),
                Color32::LIGHT_GRAY,
            );
            ui.ctx().request_repaint_after(Duration::from_millis(80));
            return Ok(());
        };
        if self.stack.is_none() {
            let stack = self.options.plane.stack(&volume).map_err(|e| e.to_string())?;
            let point = self.position.unwrap_or_else(|| volume.centre());
            self.index = stack.nearest_index(point);
            self.position = Some(point);
            self.stack = Some(stack);
        }
        self.input(ui, &response, tool, right_tool);

        let current = SliceRequest {
            options: self.options,
            index: self.index,
        };
        let irregular_spacing = volume.irregular_spacing;
        if let Some((request, plane, count, result)) = self.pending.as_ref().and_then(|pending| {
            pending
                .task
                .take()
                .map(|result| (pending.request, pending.plane, pending.count, result))
        }) {
            self.pending = None;
            if request.options == self.options {
                let frame = result?;
                let gpu = Arc::new(GpuImage::upload(
                    &shared.render_state.device,
                    &shared.render_state.queue,
                    &frame,
                ));
                self.displayed = Some(DisplaySlice {
                    request,
                    plane,
                    count,
                    frame,
                    gpu,
                });
            }
        }
        if self.pending.is_none() && self.displayed.as_ref().is_none_or(|slice| slice.request != current) {
            let stack = self.stack.as_ref().expect("the reformat stack is ready");
            let plane = stack.plane(self.index, self.options.slab_mm);
            let cancel = self.cancel.clone();
            let count = stack.slices();
            let task = Task::spawn(ui.ctx(), "ruthos-reformat-slice", move || {
                if cancel.load(Ordering::Relaxed) {
                    return Err("The reformat was closed".into());
                }
                std::panic::catch_unwind(|| reslice(&volume, &plane))
                    .map_err(|_| "An internal error stopped the reformat".into())
            });
            self.pending = Some(PendingSlice {
                request: current,
                plane,
                count,
                task,
            });
        }
        if self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(35));
        }

        if let Some(slice) = &self.displayed {
            if self.fit && slice.request.options.plane == self.options.plane {
                self.view.fit(rect, slice.frame.width, slice.frame.height, 1.0);
                self.fit = false;
            }
            let t = ViewTransform::new(rect, slice.frame.width, slice.frame.height, 1.0, self.view);
            let clut = shared.cluts.get(&self.clut).cloned().unwrap_or_else(Clut::grayscale);
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ImageCallback {
                    key: self.id * 64 + 63,
                    image: slice.gpu.clone(),
                    clut: Arc::new(clut),
                    transform: t,
                    inverted: self.inverted,
                    interpolation: Interpolation::Bilinear,
                },
            ));
            draw_labels(&painter, &t, slice, response.hover_pos());
        }
        if self.pending.is_some() || irregular_spacing {
            shadow_text(
                &painter,
                rect.right_top() + Vec2::new(-8.0, 8.0),
                Align2::RIGHT_TOP,
                if self.pending.is_some() {
                    "Reformatting…"
                } else {
                    "Irregular slice spacing"
                },
                FontId::proportional(13.0),
                Color32::LIGHT_GRAY,
            );
        }
        Ok(())
    }

    fn input(&mut self, ui: &Ui, response: &egui::Response, tool: Tool, right_tool: Tool) {
        let count = self.stack.as_ref().expect("the reformat stack is ready").slices();
        let mut next = self.index;
        let mut zoom = (None, 1.0);
        if !ui.ctx().egui_wants_keyboard_input() {
            ui.input(|i| {
                if i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowRight) {
                    next = next.saturating_add(1);
                }
                if i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowLeft) {
                    next = next.saturating_sub(1);
                }
                if i.key_pressed(Key::PageDown) {
                    next = next.saturating_add(10);
                }
                if i.key_pressed(Key::PageUp) {
                    next = next.saturating_sub(10);
                }
                if i.key_pressed(Key::Home) {
                    next = 0;
                }
                if i.key_pressed(Key::End) {
                    next = count - 1;
                }
            });
            if ui.input(|i| i.key_pressed(Key::R)) {
                self.reset();
            }
        }
        if response.hovered() {
            let (wheel, factor, ctrl) = ui.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    i.zoom_delta(),
                    i.modifiers.ctrl || i.modifiers.command,
                )
            });
            if (factor - 1.0).abs() > 1e-3 {
                zoom = (response.hover_pos(), factor);
            } else if !ctrl {
                self.wheel += wheel;
                let steps = (self.wheel / 40.0).trunc();
                self.wheel -= steps * 40.0;
                next = next.saturating_add_signed(-(steps as isize));
            }
        }

        if response.double_clicked_by(PointerButton::Primary) {
            self.fit = true;
        }
        for (button, action) in [
            (PointerButton::Primary, tool),
            (PointerButton::Secondary, right_tool),
            (PointerButton::Middle, Tool::Move),
        ] {
            if action == Tool::Scroll {
                let released = response.drag_stopped_by(button);
                if response.dragged_by(button) || released {
                    if let Some(pos) = response.interact_pointer_pos() {
                        if response.drag_started_by(button) {
                            let y = ui.input(|i| i.pointer.press_origin()).map_or(pos.y, |origin| origin.y);
                            self.drag = Some(ScrollDrag {
                                button,
                                y,
                                index: self.index,
                            });
                        }
                        if let Some(drag) = self.drag.as_ref().filter(|drag| drag.button == button) {
                            next = drag_scroll_position(drag.index, pos.y - drag.y, response.rect.height(), count);
                        }
                    }
                }
                if released {
                    self.drag = None;
                }
            } else if response.dragged_by(button) {
                let d = response.drag_delta();
                match action {
                    Tool::WindowLevel => {
                        let factor = (self.view.window_width.abs() / 256.0).max(0.5);
                        self.view.window_center += d.y * factor;
                        self.view.window_width = (self.view.window_width + d.x * factor).max(1.0);
                    }
                    Tool::Move => self.view.pan += d,
                    Tool::Zoom => zoom = (Some(response.rect.center() + self.view.pan), (-d.y * 0.01).exp()),
                    Tool::Rotate => self.view.rotation_deg = (self.view.rotation_deg + d.x * 0.5).rem_euclid(360.0),
                    _ => {}
                }
            }
        }
        if (zoom.1 - 1.0).abs() > 1e-3 {
            if let Some(slice) = &self.displayed {
                let anchor = zoom.0.unwrap_or(response.rect.center());
                let mut t = ViewTransform::new(response.rect, slice.frame.width, slice.frame.height, 1.0, self.view);
                let before = t.screen_to_image(anchor);
                t.state.zoom = (t.state.zoom * zoom.1).clamp(0.02, 64.0);
                t.state.pan += anchor - t.image_to_screen(before);
                self.view = t.state;
            }
        }

        let next = next.min(count - 1);
        if next != self.index {
            self.index = next;
            let plane = self
                .stack
                .as_ref()
                .expect("the reformat stack is ready")
                .plane(next, 0.0);
            let normal = plane.normal();
            let p = self.position.unwrap_or(plane.centre);
            self.position = Some(add(p, scale(normal, dot(sub(plane.centre, p), normal))));
        }
    }
}

impl Drop for Reformat {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn draw_labels(painter: &egui::Painter, t: &ViewTransform, slice: &DisplaySlice, pointer: Option<Pos2>) {
    let rect = t.rect;
    let name = match slice.request.options.plane {
        OrthogonalPlane::Acquisition => "Acquisition",
        OrthogonalPlane::Axial => "Axial",
        OrthogonalPlane::Coronal => "Coronal",
        OrthogonalPlane::Sagittal => "Sagittal",
    };
    let mut label = format!("{name} · {}/{}", slice.request.index + 1, slice.count);
    if slice.plane.slab_mm > 0.0 {
        label.push_str(&format!(" · MIP {:.1} mm", slice.plane.slab_mm));
    }
    let color = Color32::from_gray(230);
    let font = FontId::proportional(13.0);
    shadow_text(
        painter,
        rect.left_top() + Vec2::splat(8.0),
        Align2::LEFT_TOP,
        &label,
        font.clone(),
        color,
    );
    let mut values = format!("WL {:.0}  WW {:.0}", t.state.window_center, t.state.window_width);
    if let Some(value) = pointer
        .and_then(|p| t.pixel_at(p))
        .and_then(|(x, y)| slice.frame.value_at(x, y))
    {
        values.push_str(&format!(" · Value {value:.0}"));
    }
    shadow_text(
        painter,
        rect.left_bottom() + Vec2::new(8.0, -8.0),
        Align2::LEFT_BOTTOM,
        &values,
        font.clone(),
        color,
    );

    let centre = t.screen_to_image(rect.center());
    let direction = |axis| {
        let d = t.screen_to_image(rect.center() + axis) - centre;
        normalize(add(
            scale(slice.plane.right, d.x as f64),
            scale(slice.plane.down, d.y as f64),
        ))
    };
    for (pos, anchor, axis) in [
        (rect.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, -Vec2::X),
        (rect.right_center() - Vec2::new(6.0, 0.0), Align2::RIGHT_CENTER, Vec2::X),
        (rect.center_top() + Vec2::new(0.0, 6.0), Align2::CENTER_TOP, -Vec2::Y),
        (
            rect.center_bottom() - Vec2::new(0.0, 6.0),
            Align2::CENTER_BOTTOM,
            Vec2::Y,
        ),
    ] {
        shadow_text(
            painter,
            pos,
            anchor,
            &orientation_label(direction(axis)),
            font.clone(),
            color,
        );
    }
}
