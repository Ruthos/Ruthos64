//! Editing the 3D pane's colours and opacity: the points of its transfer
//! function over a histogram of the volume.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};

use ruthos_render::volume3d::{Stop, TransferFunction};

const WIDTH: f32 = 560.0;
const HEIGHT: f32 = 190.0;
const STRIP: f32 = 18.0;
const REACH: f32 = 8.0;
const BINS: usize = 256;

/// What the person did in the editor.
pub enum Outcome {
    Nothing,
    /// The function changed and needs baking again.
    Changed,
    /// Save the function as a preset under this name.
    Save(String),
    /// Forget the saved preset being edited.
    Delete,
}

pub struct TransferEditor {
    pub open: bool,
    /// How many voxels fall in each of `BINS` bins over the range, on a log
    /// scale from 0 to 1.
    histogram: Vec<f32>,
    held: Option<usize>,
    chosen: Option<usize>,
    /// The function as it was when editing began, for Undo.
    original: Option<TransferFunction>,
    name: String,
}

impl TransferEditor {
    pub fn new() -> Self {
        Self {
            open: false,
            histogram: Vec::new(),
            held: None,
            chosen: None,
            original: None,
            name: String::new(),
        }
    }

    /// Start editing, from the function on screen.
    pub fn begin(&mut self, f: &TransferFunction, values: impl Iterator<Item = f32>, range: (f32, f32)) {
        self.open = true;
        self.held = None;
        self.chosen = None;
        self.original = Some(f.clone());
        self.name = if f.group == "Saved" {
            f.name.clone()
        } else {
            format!("{} (mine)", f.name)
        };
        let mut counts = vec![0u64; BINS];
        let span = (range.1 - range.0).max(1e-6);
        for v in values {
            let bin = (((v - range.0) / span) * (BINS - 1) as f32).round();
            counts[(bin.max(0.0) as usize).min(BINS - 1)] += 1;
        }
        let top = counts
            .iter()
            .map(|c| (*c as f32 + 1.0).ln())
            .fold(0.0, f32::max)
            .max(1e-6);
        self.histogram = counts.iter().map(|c| (*c as f32 + 1.0).ln() / top).collect();
    }

    pub fn window(&mut self, ctx: &egui::Context, id: u64, f: &mut TransferFunction, range: (f32, f32)) -> Outcome {
        if !self.open {
            return Outcome::Nothing;
        }
        let mut outcome = Outcome::Nothing;
        let mut open = true;
        egui::Window::new("3D colours and opacity")
            .id(egui::Id::unique(("transfer-editor", id)))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos([160.0, 160.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Drag a point to move it, click to add one, right-click to take one away. \
                         Higher is more opaque, on Horos's scale. Grey bars show how much of the volume has each value.",
                    )
                    .size(12.0)
                    .color(Color32::from_gray(150)),
                );
                ui.add_space(4.0);
                if self.plot(ui, f, range) {
                    outcome = Outcome::Changed;
                }
                ui.add_space(6.0);
                if self.point_controls(ui, f, range, id) {
                    outcome = Outcome::Changed;
                }
                ui.add_space(4.0);
                if ui
                    .checkbox(&mut f.ends_clear, "Clear at the lowest and highest values")
                    .on_hover_text("As Horos's curve presets are: opacity slopes to nothing at the ends of the range")
                    .changed()
                {
                    outcome = Outcome::Changed;
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Save as");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(180.0));
                    let name = self.name.trim().to_string();
                    if ui.add_enabled(!name.is_empty(), egui::Button::new("Save")).clicked() {
                        outcome = Outcome::Save(name);
                    }
                    if ui
                        .add_enabled(self.original.is_some(), egui::Button::new("Undo changes"))
                        .clicked()
                    {
                        if let Some(o) = &self.original {
                            *f = o.clone();
                            self.chosen = None;
                            outcome = Outcome::Changed;
                        }
                    }
                    if f.group == "Saved" && ui.button("Delete this preset").clicked() {
                        outcome = Outcome::Delete;
                    }
                });
            });
        if !open {
            self.open = false;
        }
        outcome
    }

    /// The histogram, the opacity curve and its points, and a strip of the
    /// colours. True when a point moved, appeared or went.
    fn plot(&mut self, ui: &mut egui::Ui, f: &mut TransferFunction, range: (f32, f32)) -> bool {
        let (response, painter) = ui.allocate_painter(Vec2::new(WIDTH, HEIGHT + STRIP + 4.0), Sense::click_and_drag());
        let plot = Rect::from_min_size(response.rect.min, Vec2::new(WIDTH, HEIGHT));
        let strip = Rect::from_min_size(plot.left_bottom() + Vec2::new(0.0, 4.0), Vec2::new(WIDTH, STRIP));
        let span = (range.1 - range.0).max(1e-6);
        let to_x = |value: f32| plot.left() + (value - range.0) / span * WIDTH;
        // Height is the square root of opacity, as in Horos's editor, so the
        // faint opacities that matter most are not squashed at the bottom.
        let to_y = |alpha: f32| plot.bottom() - alpha.clamp(0.0, 1.0).sqrt() * HEIGHT;
        let from = |p: Pos2| {
            (
                range.0 + ((p.x - plot.left()) / WIDTH).clamp(0.0, 1.0) * span,
                ((plot.bottom() - p.y) / HEIGHT).clamp(0.0, 1.0).powi(2),
            )
        };

        painter.rect_filled(plot, 2.0, Color32::from_gray(24));
        let bar = WIDTH / BINS as f32;
        for (i, h) in self.histogram.iter().enumerate() {
            let x = plot.left() + i as f32 * bar;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x, plot.bottom() - h * HEIGHT),
                    Pos2::new(x + bar, plot.bottom()),
                ),
                0.0,
                Color32::from_gray(70),
            );
        }
        let columns = WIDTH as usize / 2;
        for c in 0..columns {
            let value = range.0 + span * c as f32 / (columns - 1) as f32;
            let [r, g, b, _] = f.at(value);
            let x = strip.left() + c as f32 * 2.0;
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(2.0, STRIP)),
                0.0,
                Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8),
            );
        }

        let mut points: Vec<Pos2> = f
            .stops
            .iter()
            .map(|s| Pos2::new(to_x(s.value), to_y(s.rgba[3])))
            .collect();
        if f.ends_clear {
            if let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) {
                let dashed = Stroke::new(1.0, Color32::from_gray(150));
                painter.add(egui::Shape::dashed_line(
                    &[Pos2::new(plot.left(), plot.bottom()), first],
                    dashed,
                    4.0,
                    3.0,
                ));
                painter.add(egui::Shape::dashed_line(
                    &[last, Pos2::new(plot.right(), plot.bottom())],
                    dashed,
                    4.0,
                    3.0,
                ));
            }
        }
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(1.5, Color32::from_gray(230)),
        ));
        for (i, (p, s)) in points.iter().zip(&f.stops).enumerate() {
            let [r, g, b, _] = s.rgba;
            let fill = Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
            let ring = if self.chosen == Some(i) {
                Stroke::new(2.0, Color32::from_rgb(120, 220, 255))
            } else {
                Stroke::new(1.0, Color32::BLACK)
            };
            painter.circle(*p, 5.5, fill, ring);
        }

        let near = |at: Pos2, points: &[Pos2]| points.iter().position(|p| p.distance(at) <= REACH);
        let mut changed = false;
        if response.drag_started() {
            self.held = ui.input(|i| i.pointer.press_origin()).and_then(|at| near(at, &points));
            self.chosen = self.held.or(self.chosen);
        }
        if let (Some(i), Some(at)) = (self.held, response.interact_pointer_pos()) {
            if response.dragged() && i < f.stops.len() {
                let (value, alpha) = from(at);
                // A point stays between its neighbours.
                let low = if i > 0 { f.stops[i - 1].value } else { f32::MIN };
                let high = f.stops.get(i + 1).map_or(f32::MAX, |s| s.value);
                f.stops[i].value = value.clamp(low, high);
                f.stops[i].rgba[3] = alpha;
                points[i] = Pos2::new(to_x(f.stops[i].value), to_y(alpha));
                changed = true;
            }
        }
        if response.drag_stopped() {
            self.held = None;
        }
        if let (true, Some(at)) = (response.clicked(), response.interact_pointer_pos()) {
            match near(at, &points) {
                Some(i) => self.chosen = Some(i),
                None if plot.contains(at) => {
                    let (value, alpha) = from(at);
                    let [r, g, b, _] = f.at(value);
                    let i = f.stops.iter().position(|s| s.value > value).unwrap_or(f.stops.len());
                    f.stops.insert(
                        i,
                        Stop {
                            value,
                            rgba: [r, g, b, alpha],
                        },
                    );
                    self.chosen = Some(i);
                    changed = true;
                }
                None => {}
            }
        }
        if let (true, Some(at)) = (response.secondary_clicked(), response.interact_pointer_pos()) {
            if let Some(i) = near(at, &points).filter(|_| f.stops.len() > 2) {
                f.stops.remove(i);
                self.chosen = None;
                changed = true;
            }
        }
        if let Some(i) = self.held.or(self.chosen).filter(|i| *i < f.stops.len()) {
            let s = f.stops[i];
            response.on_hover_text_at_pointer(format!("{:.0}, opacity {:.2}", s.value, s.rgba[3]));
        }
        changed
    }

    /// The chosen point's value, opacity and colour.
    fn point_controls(&mut self, ui: &mut egui::Ui, f: &mut TransferFunction, range: (f32, f32), id: u64) -> bool {
        let Some(i) = self.chosen.filter(|i| *i < f.stops.len()) else {
            ui.label(egui::RichText::new("Click a point to choose its colour.").size(12.0));
            return false;
        };
        let low = if i > 0 {
            f.stops[i - 1].value
        } else {
            range.0.min(f.stops[i].value)
        };
        let high = f.stops.get(i + 1).map_or(range.1.max(f.stops[i].value), |s| s.value);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Value");
            changed |= ui
                .add(egui::DragValue::new(&mut f.stops[i].value).range(low..=high).speed(1.0))
                .changed();
            ui.label("Opacity");
            changed |= ui
                .add(egui::Slider::new(&mut f.stops[i].rgba[3], 0.0..=1.0).fixed_decimals(2))
                .changed();
        });
        let [r, g, b, _] = f.stops[i].rgba;
        let mut colour = [r, g, b].map(|c| (c * 255.0).round() as u8);
        if crate::colour_pick::editor(ui, egui::Id::unique(("transfer-point", id)), &mut colour) {
            for (k, c) in colour.iter().enumerate() {
                f.stops[i].rgba[k] = *c as f32 / 255.0;
            }
            changed = true;
        }
        changed
    }
}

impl Default for TransferEditor {
    fn default() -> Self {
        Self::new()
    }
}
