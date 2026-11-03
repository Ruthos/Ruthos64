//! Visual theme.
//!
//! The database window is light, like the Horos (Aqua) browser; the viewer
//! window is dark with a black image area, like the Horos 2D viewer. Text is
//! set in Inter, which sits close to the system fonts Horos inherits on a
//! Mac, with Phosphor glyphs for icons.

use egui::{Color32, CornerRadius, FontFamily, FontId, Mesh, Rect, Stroke, TextStyle, Ui, Visuals};

pub const TOOLBAR_HEIGHT: f32 = 68.0;
pub const TOOLBAR_ICON_SIZE: f32 = 25.0;
pub const TOOLBAR_LABEL_SIZE: f32 = 11.0;
pub const THUMB_SIZE: f32 = 100.0;
/// Height of the database window's bottom pane: thumbnails and preview.
pub const PREVIEW_HEIGHT: f32 = 262.0;
pub const TABLE_ROW_HEIGHT: f32 = 26.0;
pub const TABLE_HEADER_HEIGHT: f32 = 28.0;

/// Default annotation text colour in Horos (white with a dark shadow).
pub const ANNOTATION_COLOR: Color32 = Color32::from_rgba_premultiplied(235, 235, 235, 235);
pub const ANNOTATION_SHADOW: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 200);
/// ROI colours: Horos draws unselected ROIs green and selected ones yellow-ish.
pub const ROI_COLOR: Color32 = Color32::from_rgb(0, 230, 0);
pub const ROI_SELECTED: Color32 = Color32::from_rgb(255, 230, 0);
pub const ROI_DRAFT: Color32 = Color32::from_rgb(120, 200, 255);

/// Mac-style selection blue.
pub const SELECTION_BLUE: Color32 = Color32::from_rgb(0, 103, 214);

/// Text sizes and spacing shared by both windows. Applied once at start-up.
pub fn apply_global_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(19.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.combo_width = 120.0;
        style.spacing.menu_margin = egui::Margin::symmetric(5, 6);
        style.spacing.menu_width = 180.0;
        style.interaction.selectable_labels = false;
    });
}

fn rounded(v: &mut Visuals, r: u8) {
    let cr = CornerRadius::same(r);
    v.widgets.noninteractive.corner_radius = cr;
    v.widgets.inactive.corner_radius = cr;
    v.widgets.hovered.corner_radius = cr;
    v.widgets.active.corner_radius = cr;
    v.widgets.open.corner_radius = cr;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
}

pub fn browser_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(236, 236, 236);
    v.window_fill = Color32::from_rgb(248, 248, 248);
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(244, 246, 250);
    v.selection.bg_fill = SELECTION_BLUE;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.override_text_color = Some(Color32::from_gray(30));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(205, 205, 205));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(60));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(253, 253, 253);
    v.widgets.inactive.bg_fill = Color32::from_rgb(253, 253, 253);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(190, 190, 190));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_gray(30));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(242, 242, 242);
    v.widgets.hovered.bg_fill = Color32::from_rgb(242, 242, 242);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(160, 160, 160));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_gray(10));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(225, 225, 225);
    v.widgets.active.bg_fill = Color32::from_rgb(225, 225, 225);
    v.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(140, 140, 140));
    v.widgets.open.weak_bg_fill = Color32::from_rgb(235, 235, 235);
    v.window_shadow.color = Color32::from_black_alpha(60);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(200, 200, 200));
    menu_look(
        &mut v,
        Color32::from_rgb(247, 247, 247),
        Color32::from_rgb(196, 196, 196),
    );
    v.striped = true;
    rounded(&mut v, 6);
    v
}

/// Menus and popups: a soft shadow, rounded corners and a plain fill, as on
/// macOS.
fn menu_look(v: &mut Visuals, fill: Color32, stroke: Color32) {
    v.window_fill = fill;
    v.window_stroke = Stroke::new(1.0, stroke);
    v.menu_corner_radius = CornerRadius::same(8);
    v.popup_shadow = egui::Shadow {
        offset: [0, 5],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(75),
    };
}

pub fn viewer_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(38, 38, 40);
    v.window_fill = Color32::from_rgb(48, 48, 52);
    v.extreme_bg_color = Color32::BLACK;
    v.faint_bg_color = Color32::from_rgb(52, 52, 56);
    v.selection.bg_fill = Color32::from_rgb(60, 110, 200);
    v.override_text_color = Some(Color32::from_gray(228));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 70, 74));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(66, 66, 72);
    v.widgets.inactive.bg_fill = Color32::from_rgb(66, 66, 72);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(90, 90, 96));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(82, 82, 90);
    v.widgets.hovered.bg_fill = Color32::from_rgb(82, 82, 90);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(100, 100, 110);
    menu_look(&mut v, Color32::from_rgb(46, 46, 50), Color32::from_rgb(86, 86, 92));
    rounded(&mut v, 6);
    v
}

/// Top and bottom colours of the toolbar gradient.
pub fn toolbar_gradient_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (Color32::from_rgb(72, 72, 78), Color32::from_rgb(52, 52, 56))
    } else {
        (Color32::from_rgb(238, 238, 238), Color32::from_rgb(214, 214, 214))
    }
}

pub fn toolbar_stroke(dark: bool) -> Stroke {
    if dark {
        Stroke::new(1.0, Color32::from_rgb(24, 24, 26))
    } else {
        Stroke::new(1.0, Color32::from_rgb(168, 168, 168))
    }
}

/// Paint the Aqua-style vertical gradient behind a toolbar. Call first thing
/// inside the toolbar panel; `inset` is the panel's inner margin.
pub fn paint_toolbar_background(ui: &Ui, dark: bool, inset: egui::Vec2) {
    let rect = ui.max_rect().expand2(inset);
    let (top, bottom) = toolbar_gradient_colors(dark);
    gradient_rect(ui.painter(), rect, top, bottom);
    let y = rect.bottom() - 0.5;
    ui.painter().line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        toolbar_stroke(dark),
    );
}

/// A vertical two-colour gradient.
pub fn gradient_rect(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}
