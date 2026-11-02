//! Visual theme.
//!
//! The database window is light, like the Horos (Aqua) browser; the viewer
//! window is dark with a black image area, like the Horos 2D viewer.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const TOOLBAR_HEIGHT: f32 = 58.0;
pub const TOOLBAR_ICON_SIZE: f32 = 22.0;
pub const TOOLBAR_LABEL_SIZE: f32 = 10.5;
pub const THUMB_SIZE: f32 = 100.0;

/// Default annotation text colour in Horos (white with a dark shadow).
pub const ANNOTATION_COLOR: Color32 = Color32::from_rgba_premultiplied(235, 235, 235, 235);
pub const ANNOTATION_SHADOW: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 200);
/// ROI colours: Horos draws unselected ROIs green and selected ones yellow-ish.
pub const ROI_COLOR: Color32 = Color32::from_rgb(0, 230, 0);
pub const ROI_SELECTED: Color32 = Color32::from_rgb(255, 230, 0);
pub const ROI_DRAFT: Color32 = Color32::from_rgb(120, 200, 255);

pub fn browser_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(236, 236, 236);
    v.window_fill = Color32::from_rgb(246, 246, 246);
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(243, 245, 250);
    v.selection.bg_fill = Color32::from_rgb(53, 112, 221);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(200, 200, 200));
    v.widgets.inactive.corner_radius = CornerRadius::same(4);
    v.widgets.hovered.corner_radius = CornerRadius::same(4);
    v.widgets.active.corner_radius = CornerRadius::same(4);
    v.striped = true;
    v
}

pub fn viewer_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(38, 38, 40);
    v.window_fill = Color32::from_rgb(45, 45, 48);
    v.extreme_bg_color = Color32::BLACK;
    v.faint_bg_color = Color32::from_rgb(50, 50, 54);
    v.selection.bg_fill = Color32::from_rgb(70, 110, 190);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 70, 74));
    v
}

/// The toolbar strip background, a soft vertical gradient in Horos.
pub fn toolbar_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(58, 58, 62)
    } else {
        Color32::from_rgb(222, 222, 222)
    }
}

pub fn toolbar_stroke(dark: bool) -> Stroke {
    if dark {
        Stroke::new(1.0, Color32::from_rgb(28, 28, 30))
    } else {
        Stroke::new(1.0, Color32::from_rgb(180, 180, 180))
    }
}
