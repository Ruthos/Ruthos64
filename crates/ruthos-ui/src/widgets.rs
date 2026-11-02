//! Purpose-built widgets that give the Horos look: the icon-over-label
//! toolbar button, the source-list row, and the thumbnail cell.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::theme;

/// A toolbar item: glyph above a label, Horos/Aqua style.
pub struct ToolbarButton<'a> {
    pub glyph: &'a str,
    pub label: &'a str,
    pub enabled: bool,
    pub selected: bool,
    pub tooltip: Option<&'a str>,
    pub width: f32,
}

impl<'a> ToolbarButton<'a> {
    pub fn new(glyph: &'a str, label: &'a str) -> Self {
        Self {
            glyph,
            label,
            enabled: true,
            selected: false,
            tooltip: None,
            width: 62.0,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn tooltip(mut self, tip: &'a str) -> Self {
        self.tooltip = Some(tip);
        self
    }

    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let label_width = ui
            .painter()
            .layout_no_wrap(self.label.to_string(), FontId::proportional(theme::TOOLBAR_LABEL_SIZE), Color32::BLACK)
            .size()
            .x;
        let width = self.width.max(label_width + 14.0);
        let size = Vec2::new(width, theme::TOOLBAR_HEIGHT - 10.0);
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let dark = ui.visuals().dark_mode;
            let hovered = self.enabled && response.hovered();
            let pressed = response.is_pointer_button_down_on();
            if self.selected || hovered || pressed {
                let fill = if pressed || self.selected {
                    if dark {
                        Color32::from_white_alpha(28)
                    } else {
                        Color32::from_black_alpha(34)
                    }
                } else if dark {
                    Color32::from_white_alpha(16)
                } else {
                    Color32::from_black_alpha(16)
                };
                painter.rect_filled(rect.shrink(2.0), 6.0, fill);
                if self.selected {
                    let stroke = if dark { Color32::from_white_alpha(40) } else { Color32::from_black_alpha(40) };
                    painter.rect_stroke(rect.shrink(2.0), 6.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
                }
            }
            let (icon_color, label_color) = if !self.enabled {
                if dark {
                    (Color32::from_gray(105), Color32::from_gray(105))
                } else {
                    (Color32::from_gray(165), Color32::from_gray(150))
                }
            } else if dark {
                (Color32::from_gray(232), Color32::from_gray(215))
            } else {
                (Color32::from_gray(55), Color32::from_gray(40))
            };
            painter.text(
                Pos2::new(rect.center().x, rect.top() + 6.0),
                Align2::CENTER_TOP,
                self.glyph,
                crate::icons::font(theme::TOOLBAR_ICON_SIZE),
                icon_color,
            );
            painter.text(
                Pos2::new(rect.center().x, rect.bottom() - 5.0),
                Align2::CENTER_BOTTOM,
                self.label,
                FontId::proportional(theme::TOOLBAR_LABEL_SIZE),
                label_color,
            );
        }
        let response = if let Some(tip) = self.tooltip {
            if self.enabled {
                response.on_hover_text(tip)
            } else {
                response.on_disabled_hover_text(tip)
            }
        } else {
            response
        };
        response
    }
}

/// Thin vertical separator between toolbar groups.
pub fn toolbar_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(11.0, theme::TOOLBAR_HEIGHT - 10.0), Sense::hover());
    let x = rect.center().x;
    let color = if ui.visuals().dark_mode {
        Color32::from_white_alpha(40)
    } else {
        Color32::from_black_alpha(50)
    };
    ui.painter().line_segment(
        [Pos2::new(x, rect.top() + 10.0), Pos2::new(x, rect.bottom() - 10.0)],
        Stroke::new(1.0, color),
    );
}

/// A label under a toolbar control (combo box, menu), matching button labels.
pub fn toolbar_caption(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::TOOLBAR_LABEL_SIZE)
            .color(if ui.visuals().dark_mode { Color32::from_gray(215) } else { Color32::from_gray(40) }),
    );
}

/// A source-list header such as "ALBUMS".
pub fn source_header(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .strong()
            .color(Color32::from_gray(120)),
    );
    ui.add_space(2.0);
}

/// A selectable source-list row with an optional count badge.
pub fn source_row(ui: &mut Ui, glyph: &str, text: &str, count: Option<i64>, selected: bool) -> Response {
    let desired = Vec2::new(ui.available_width(), 26.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 5.0, ui.visuals().selection.bg_fill);
        } else if response.hovered() {
            painter.rect_filled(rect, 5.0, Color32::from_black_alpha(14));
        }
        let color = if selected {
            Color32::WHITE
        } else {
            ui.visuals().text_color()
        };
        painter.text(
            Pos2::new(rect.left() + 9.0, rect.center().y),
            Align2::LEFT_CENTER,
            glyph,
            crate::icons::font(15.0),
            if selected { Color32::WHITE } else { Color32::from_gray(90) },
        );
        painter.text(
            Pos2::new(rect.left() + 32.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(14.0),
            color,
        );
        if let Some(n) = count {
            painter.text(
                Pos2::new(rect.right() - 9.0, rect.center().y),
                Align2::RIGHT_CENTER,
                n.to_string(),
                FontId::proportional(12.5),
                if selected { Color32::WHITE } else { Color32::from_gray(130) },
            );
        }
    }
    response
}

/// A thumbnail with a caption, used in the database strip and the viewer
/// drawer. Returns the response of the whole cell.
pub fn thumbnail_cell(
    ui: &mut Ui,
    texture: Option<&egui::TextureHandle>,
    caption: &str,
    subcaption: &str,
    selected: bool,
    size: f32,
) -> Response {
    let cell = Vec2::new(size + 14.0, size + 38.0);
    let (rect, response) = ui.allocate_exact_size(cell, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let dark = ui.visuals().dark_mode;
        if selected {
            painter.rect_filled(rect, 6.0, ui.visuals().selection.bg_fill.gamma_multiply(0.30));
            painter.rect_stroke(rect, 6.0, Stroke::new(1.5, ui.visuals().selection.bg_fill), egui::StrokeKind::Inside);
        } else if response.hovered() {
            painter.rect_filled(rect, 6.0, if dark { Color32::from_white_alpha(14) } else { Color32::from_black_alpha(12) });
        }
        let img_rect = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 7.0 + size / 2.0), Vec2::splat(size));
        painter.rect_filled(img_rect, 3.0, Color32::BLACK);
        match texture {
            Some(tex) => {
                let tex_size = tex.size_vec2();
                let scale = (size / tex_size.x).min(size / tex_size.y);
                let draw_size = tex_size * scale;
                let draw_rect = Rect::from_center_size(img_rect.center(), draw_size);
                painter.image(
                    tex.id(),
                    draw_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.text(
                    img_rect.center(),
                    Align2::CENTER_CENTER,
                    "…",
                    FontId::proportional(18.0),
                    Color32::from_gray(120),
                );
            }
        }
        let text_color = if dark { Color32::from_gray(225) } else { Color32::from_gray(35) };
        let caption = truncate(caption, 17);
        painter.text(
            Pos2::new(rect.center().x, img_rect.bottom() + 4.0),
            Align2::CENTER_TOP,
            caption,
            FontId::proportional(12.0),
            text_color,
        );
        painter.text(
            Pos2::new(rect.center().x, img_rect.bottom() + 19.0),
            Align2::CENTER_TOP,
            subcaption,
            FontId::proportional(11.0),
            Color32::from_gray(if dark { 165 } else { 115 }),
        );
    }
    response
}

/// Truncate a string to `max` characters with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Text with a one-pixel dark shadow, the Horos annotation style.
pub fn shadow_text(painter: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    painter.text(pos + Vec2::new(1.0, 1.0), anchor, text, font.clone(), theme::ANNOTATION_SHADOW);
    painter.text(pos, anchor, text, font, color)
}
