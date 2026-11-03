//! The zoom loupe: a magnified view of the image around the point being
//! placed, drawn beside the pointer while a measurement is created or a
//! handle dragged, and on request while hovering with Alt held.
//!
//! The loupe is the same GPU image drawn a second time through a second
//! `ViewTransform`, so it shows exactly the pixels, window and colour table
//! of the cell, with nearest-neighbour sampling so individual pixels stay
//! visible. Above six screen pixels per image pixel a grid outlines them.

use std::sync::Arc;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Ui, Vec2};
use ruthos_core::Settings;
use ruthos_dicom::Clut;
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewTransform};

use crate::theme;

/// The Preferences > Measurements loupe settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoupeSettings {
    pub enabled: bool,
    /// 2, 3, 4, 6 or 8 times the cell's zoom.
    pub magnification: f32,
    /// Side of the square loupe in screen points.
    pub size: f32,
    /// Shift scales pointer movement by the reciprocal of the magnification.
    pub fine_movement: bool,
}

pub const MAGNIFICATIONS: [u32; 5] = [2, 3, 4, 6, 8];
pub const SIZES: [u32; 3] = [120, 160, 200];

impl Default for LoupeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            magnification: 4.0,
            size: 160.0,
            fine_movement: true,
        }
    }
}

impl From<&Settings> for LoupeSettings {
    fn from(s: &Settings) -> Self {
        Self {
            enabled: s.loupe_enabled,
            magnification: if MAGNIFICATIONS.contains(&s.loupe_magnification) {
                s.loupe_magnification as f32
            } else {
                4.0
            },
            size: if SIZES.contains(&s.loupe_size) {
                s.loupe_size as f32
            } else {
                160.0
            },
            fine_movement: s.loupe_fine_movement,
        }
    }
}

/// Where the loupe is being shown: the image point under magnification and
/// the pointer it sits beside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoupeTarget {
    pub image_point: Vec2,
    pub pointer: Pos2,
}

/// Place the loupe beside the pointer, below and to the right, flipping to
/// the other side where it would leave the cell.
pub fn placement(cell: Rect, pointer: Pos2, size: f32) -> Rect {
    const GAP: f32 = 24.0;
    let mut x = pointer.x + GAP;
    if x + size > cell.right() - 4.0 {
        x = pointer.x - GAP - size;
    }
    let mut y = pointer.y + GAP;
    if y + size > cell.bottom() - 4.0 {
        y = pointer.y - GAP - size;
    }
    let x = x.max(cell.left() + 4.0);
    let y = y.max(cell.top() + 4.0);
    Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size))
}

/// What the loupe draws: the cell's image and the geometry on it.
pub struct LoupeSource {
    pub key: u64,
    pub image: Arc<GpuImage>,
    pub clut: Arc<Clut>,
    pub inverted: bool,
}

/// Draw the loupe for `target`. `overlay` draws the measurement geometry
/// through the loupe's transform so the line being placed is visible in it.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &Ui,
    painter: &Painter,
    cell: Rect,
    cell_transform: &ViewTransform,
    target: LoupeTarget,
    settings: &LoupeSettings,
    source: &LoupeSource,
    overlay: &dyn Fn(&Painter, &ViewTransform),
) {
    let rect = placement(cell, target.pointer, settings.size);
    let t = cell_transform.loupe(target.image_point, rect, settings.magnification);
    let p = painter.with_clip_rect(rect);
    p.rect_filled(rect, 0.0, Color32::BLACK);
    p.add(egui_wgpu::Callback::new_paint_callback(
        rect,
        ImageCallback {
            key: source.key,
            image: source.image.clone(),
            clut: source.clut.clone(),
            transform: t,
            inverted: source.inverted,
            interpolation: Interpolation::Nearest,
        },
    ));

    // Pixel grid once pixels are large enough to tell apart.
    let px = t.state.zoom;
    if px >= 6.0 {
        let grid = Stroke::new(1.0, Color32::from_white_alpha(40));
        let corners = [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
        ]
        .map(|c| t.screen_to_image(c));
        let (x0, x1) = corners
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
        let (y0, y1) = corners
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), c| (lo.min(c.y), hi.max(c.y)));
        for x in (x0.floor() as i64)..=(x1.ceil() as i64) {
            let a = t.image_to_screen(Vec2::new(x as f32, y0));
            let b = t.image_to_screen(Vec2::new(x as f32, y1));
            p.line_segment([a, b], grid);
        }
        for y in (y0.floor() as i64)..=(y1.ceil() as i64) {
            let a = t.image_to_screen(Vec2::new(x0, y as f32));
            let b = t.image_to_screen(Vec2::new(x1, y as f32));
            p.line_segment([a, b], grid);
        }
    }

    overlay(&p, &t);

    // Crosshair with a clear centre, and the frame.
    let c = rect.center();
    let hair = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200));
    for (d, len) in [(Vec2::X, 10.0), (Vec2::Y, 10.0)] {
        p.line_segment([c - d * (len + 4.0), c - d * 4.0], hair);
        p.line_segment([c + d * 4.0, c + d * (len + 4.0)], hair);
    }
    p.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.5, theme::ROI_SELECTED),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.right_bottom() - Vec2::new(4.0, 2.0),
        Align2::RIGHT_BOTTOM,
        format!("{}\u{d7}", settings.magnification as u32),
        FontId::proportional(11.0),
        Color32::from_white_alpha(200),
    );
    ui.ctx().request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_flips_near_edges() {
        let cell = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let r = placement(cell, Pos2::new(100.0, 100.0), 160.0);
        assert_eq!(r.min, Pos2::new(124.0, 124.0));
        let r = placement(cell, Pos2::new(750.0, 100.0), 160.0);
        assert!(r.right() <= 800.0 && r.left() < 750.0);
        let r = placement(cell, Pos2::new(100.0, 580.0), 160.0);
        assert!(r.bottom() <= 600.0 && r.top() < 580.0);
        let r = placement(cell, Pos2::new(790.0, 590.0), 160.0);
        assert!(cell.contains_rect(r));
    }

    #[test]
    fn settings_fall_back_to_allowed_values() {
        let s = Settings {
            loupe_magnification: 5,
            loupe_size: 999,
            ..Default::default()
        };
        let l = LoupeSettings::from(&s);
        assert_eq!((l.magnification, l.size), (4.0, 160.0));
    }
}
