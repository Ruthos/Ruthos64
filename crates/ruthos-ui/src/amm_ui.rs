//! Anatomical M-mode in the viewer: a line or curve on a loop, its live strip,
//! and saving the strip to the study.

use std::sync::Arc;

use egui::{Color32, Pos2, Sense, Stroke, Ui, Vec2};

use ruthos_dicom::amm::{calibration, read_strip, Strip};
use ruthos_dicom::Frame;
use ruthos_render::ViewTransform;

/// How close a press must be to a dot, in screen points.
const REACH: f32 = 10.0;
const MOST_POINTS: usize = 24;
const LINE: Color32 = Color32::from_rgb(120, 220, 255);
const AMBER: Color32 = Color32::from_rgb(240, 190, 90);
/// The strip's width on screen, in points.
const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 300.0;

/// What the person asked for in the strip's window.
pub enum Action {
    Nothing,
    Jump(usize),
    Save,
    Close,
}

pub struct AmmView {
    pub image_id: i64,
    points: Vec<Vec2>,
    held: Option<usize>,
    /// What the strip was read from.
    read: Option<(Vec<Vec2>, (f32, f32))>,
    pub strip: Option<Strip>,
    problem: Option<String>,
    texture: Option<egui::TextureHandle>,
    pub message: Option<String>,
}

impl AmmView {
    pub fn new(image_id: i64) -> Self {
        Self {
            image_id,
            points: Vec::new(),
            held: None,
            read: None,
            strip: None,
            problem: None,
            texture: None,
            message: None,
        }
    }

    fn dot_at(&self, t: &ViewTransform, at: Pos2) -> Option<usize> {
        self.points
            .iter()
            .position(|p| t.image_to_screen(*p).distance(at) <= REACH)
    }

    /// Clicks add points, dots drag, and a right-click takes a dot away.
    pub fn input(&mut self, ui: &Ui, response: &egui::Response, t: &ViewTransform) {
        let pointer = response.interact_pointer_pos();
        if response.drag_started_by(egui::PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.held = origin.and_then(|at| self.dot_at(t, at));
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let (Some(i), Some(at)) = (self.held, pointer) {
                self.points[i] = t.screen_to_image(at);
            }
        }
        if response.drag_stopped() {
            self.held = None;
        }
        if let (true, Some(at)) = (response.clicked(), pointer) {
            if self.dot_at(t, at).is_none() && self.points.len() < MOST_POINTS {
                self.points.push(t.screen_to_image(at));
            }
        }
        if let (true, Some(at)) = (response.secondary_clicked(), pointer) {
            if let Some(i) = self.dot_at(t, at) {
                self.points.remove(i);
            }
        }
    }

    pub fn draw(&self, painter: &egui::Painter, t: &ViewTransform) {
        let path: Vec<[f64; 2]> = self.points.iter().map(|p| [p.x as f64, p.y as f64]).collect();
        let line: Vec<Pos2> = ruthos_dicom::amm::smooth(&path)
            .iter()
            .map(|p| t.image_to_screen(Vec2::new(p[0] as f32, p[1] as f32)))
            .collect();
        painter.add(egui::Shape::line(line, Stroke::new(1.5, LINE)));
        for p in &self.points {
            painter.circle(t.image_to_screen(*p), 5.0, LINE, Stroke::new(1.0, Color32::BLACK));
        }
    }

    /// Read the strip again if the line or the window has moved.
    pub fn refresh(&mut self, ctx: &egui::Context, frames: &[Arc<Frame>], window: (f32, f32)) {
        if self.read.as_ref() == Some(&(self.points.clone(), window)) {
            return;
        }
        self.read = Some((self.points.clone(), window));
        self.strip = None;
        self.problem = None;
        self.message = None;
        self.texture = None;
        let Some(first) = frames.first() else { return };
        if self.points.len() < 2 {
            return;
        }
        let path: Vec<[f64; 2]> = self.points.iter().map(|p| [p.x as f64, p.y as f64]).collect();
        match calibration(first, &path) {
            Ok(scale) => {
                let strip = read_strip(frames, &path, scale);
                let shade = |v: f32| ((v - window.0) / window.1.max(1.0) + 0.5).clamp(0.0, 1.0);
                let mut pixels = Vec::with_capacity(strip.frames * strip.samples);
                for sample in 0..strip.samples {
                    for frame in 0..strip.frames {
                        pixels.push(Color32::from_gray((shade(strip.at(frame, sample)) * 255.0) as u8));
                    }
                }
                let image = egui::ColorImage::new([strip.frames, strip.samples], pixels);
                // Nearest: a frame is a block, and must not look like more.
                self.texture = Some(ctx.load_texture("ruthos-amm", image, egui::TextureOptions::NEAREST));
                self.strip = Some(strip);
            }
            Err(e) => self.problem = Some(e),
        }
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// The strip's window.
    pub fn window(&mut self, ctx: &egui::Context, frame: usize, can_save: bool, id: u64) -> Action {
        let mut action = Action::Nothing;
        let mut open = true;
        let title = match &self.strip {
            Some(s) => s.label(),
            None => "Anatomical M-mode".to_string(),
        };
        egui::Window::new(title)
            .id(egui::Id::unique(("ruthos-amm", id)))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos([120.0, 140.0])
            .show(ctx, |ui| {
                match (&self.strip, &self.texture) {
                    (Some(strip), Some(texture)) => {
                        let size = Vec2::new(WIDTH, HEIGHT);
                        let picture = egui::Image::new((texture.id(), size)).sense(Sense::click_and_drag());
                        let response = ui.add(picture);
                        let frames = strip.frames.max(1);
                        let x = response.rect.left() + (frame as f32 + 0.5) / frames as f32 * WIDTH;
                        ui.painter().line_segment(
                            [
                                Pos2::new(x, response.rect.top()),
                                Pos2::new(x, response.rect.bottom()),
                            ],
                            Stroke::new(1.5, LINE),
                        );
                        if let (true, Some(at)) = (
                            response.clicked() || response.dragged(),
                            response.interact_pointer_pos(),
                        ) {
                            let share = ((at.x - response.rect.left()) / WIDTH).clamp(0.0, 0.999);
                            action = Action::Jump((share * frames as f32) as usize);
                        }
                        ui.label(format!(
                            "{:.1} mm along a {}, {} frames, {:.0} ms a frame. The 2D loop is the reference.",
                            strip.length_mm(),
                            if strip.curved { "curve" } else { "line" },
                            strip.frames,
                            strip.ms_per_frame,
                        ));
                        if let Some(warning) = strip.warning() {
                            ui.colored_label(AMBER, warning);
                        }
                    }
                    _ => {
                        let text = match &self.problem {
                            Some(p) => p.as_str(),
                            None if self.points.is_empty() => {
                                "Click the loop to start a line. Two points make a straight line, and more make a curve."
                            }
                            None if self.points.len() == 1 => "Click a second point.",
                            None => "Reading the loop\u{2026}",
                        };
                        ui.set_min_width(WIDTH / 2.0);
                        ui.label(text);
                    }
                }
                if let Some(message) = &self.message {
                    ui.label(message.as_str());
                }
                ui.horizontal(|ui| {
                    if can_save
                        && ui
                            .add_enabled(self.strip.is_some(), egui::Button::new("Save to study"))
                            .on_hover_text("Saves the strip as a new series, where the M-mode tools measure it")
                            .clicked()
                    {
                        action = Action::Save;
                    }
                    if ui.add_enabled(!self.points.is_empty(), egui::Button::new("Clear line")).clicked() {
                        self.points.clear();
                    }
                });
            });
        if !open {
            action = Action::Close;
        }
        action
    }
}
