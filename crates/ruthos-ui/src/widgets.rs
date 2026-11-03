//! Purpose-built widgets that give the Horos look: the icon-over-label
//! toolbar button, the source-list row, and the thumbnail cell.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::icons::{self, Bitmap};
use crate::theme;

/// An icon: a glyph from the icon font, or a Horos bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Glyph(&'static str),
    Bitmap(Bitmap),
}

impl From<&'static str> for Icon {
    fn from(g: &'static str) -> Self {
        Icon::Glyph(g)
    }
}

impl From<Bitmap> for Icon {
    fn from(b: Bitmap) -> Self {
        Icon::Bitmap(b)
    }
}

/// Paint an icon centred on `center`. `size` is the glyph size; bitmaps are
/// drawn a little larger since their artwork has margins. `alpha` dims a
/// disabled bitmap.
pub fn paint_icon(ui: &Ui, icon: Icon, center: Pos2, size: f32, color: Color32, alpha: u8) {
    match icon {
        Icon::Glyph(g) => {
            ui.painter()
                .text(center, Align2::CENTER_CENTER, g, icons::font(size), color);
        }
        Icon::Bitmap(b) => {
            let tex = icons::texture(ui.ctx(), b);
            let rect = Rect::from_center_size(center, Vec2::splat(size * 1.2));
            let tint = Color32::from_rgba_unmultiplied(255, 255, 255, alpha);
            ui.painter()
                .image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
        }
    }
}

/// A toolbar item: glyph above a label, Horos/Aqua style.
pub struct ToolbarButton<'a> {
    pub icon: Icon,
    pub label: &'a str,
    pub enabled: bool,
    pub selected: bool,
    pub tooltip: Option<&'a str>,
    pub width: f32,
}

impl<'a> ToolbarButton<'a> {
    pub fn new(icon: impl Into<Icon>, label: &'a str) -> Self {
        Self {
            icon: icon.into(),
            label,
            enabled: true,
            selected: false,
            tooltip: None,
            width: 56.0,
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
            .layout_no_wrap(
                self.label.to_string(),
                FontId::proportional(theme::TOOLBAR_LABEL_SIZE),
                Color32::BLACK,
            )
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
                    let stroke = if dark {
                        Color32::from_white_alpha(40)
                    } else {
                        Color32::from_black_alpha(40)
                    };
                    painter.rect_stroke(
                        rect.shrink(2.0),
                        6.0,
                        Stroke::new(1.0, stroke),
                        egui::StrokeKind::Inside,
                    );
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
            let icon_center = Pos2::new(rect.center().x, rect.top() + 6.0 + theme::TOOLBAR_ICON_SIZE * 0.55);
            paint_icon(
                ui,
                self.icon,
                icon_center,
                theme::TOOLBAR_ICON_SIZE,
                icon_color,
                if self.enabled { 255 } else { 80 },
            );
            ui.painter().text(
                Pos2::new(rect.center().x, rect.bottom() - 5.0),
                Align2::CENTER_BOTTOM,
                self.label,
                FontId::proportional(theme::TOOLBAR_LABEL_SIZE),
                label_color,
            );
        }
        if let Some(tip) = self.tooltip {
            if self.enabled {
                response.on_hover_text(tip)
            } else {
                response.on_disabled_hover_text(tip)
            }
        } else {
            response
        }
    }
}

/// Thin vertical separator between toolbar groups.
pub fn toolbar_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(7.0, theme::TOOLBAR_HEIGHT - 10.0), Sense::hover());
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
            .color(if ui.visuals().dark_mode {
                Color32::from_gray(215)
            } else {
                Color32::from_gray(40)
            }),
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
pub fn source_row(ui: &mut Ui, icon: impl Into<Icon>, text: &str, count: Option<i64>, selected: bool) -> Response {
    let icon = icon.into();
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
        paint_icon(
            ui,
            icon,
            Pos2::new(rect.left() + 17.0, rect.center().y),
            15.0,
            if selected {
                Color32::WHITE
            } else {
                Color32::from_gray(90)
            },
            255,
        );
        let painter = ui.painter();
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
                if selected {
                    Color32::WHITE
                } else {
                    Color32::from_gray(130)
                },
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
            painter.rect_stroke(
                rect,
                6.0,
                Stroke::new(1.5, ui.visuals().selection.bg_fill),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            painter.rect_filled(
                rect,
                6.0,
                if dark {
                    Color32::from_white_alpha(14)
                } else {
                    Color32::from_black_alpha(12)
                },
            );
        }
        let img_rect = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.top() + 7.0 + size / 2.0),
            Vec2::splat(size),
        );
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
        let text_color = if dark {
            Color32::from_gray(225)
        } else {
            Color32::from_gray(35)
        };
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
pub fn shadow_text(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font: FontId,
    color: Color32,
) -> Rect {
    painter.text(
        pos + Vec2::new(1.0, 1.0),
        anchor,
        text,
        font.clone(),
        theme::ANNOTATION_SHADOW,
    );
    painter.text(pos, anchor, text, font, color)
}

/// Which mouse button a menu row is assigned to, for [`mouse_glyph`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseKey {
    Left,
    Right,
}

/// A small two-button mouse outline with one key darkened, drawn into `rect`.
pub fn mouse_glyph(painter: &egui::Painter, rect: Rect, key: MouseKey, dark: bool) {
    let outline = if dark {
        Color32::from_gray(205)
    } else {
        Color32::from_gray(70)
    };
    let key_fill = if dark {
        Color32::from_gray(225)
    } else {
        Color32::from_gray(60)
    };
    let body = if dark {
        Color32::from_white_alpha(18)
    } else {
        Color32::from_black_alpha(14)
    };
    let w = rect.width().min(rect.height() * 0.68);
    let h = w / 0.68;
    let body_rect = Rect::from_center_size(rect.center(), Vec2::new(w, h));
    let radius = w * 0.5;
    painter.rect(
        body_rect,
        radius,
        body,
        Stroke::new(1.0, outline),
        egui::StrokeKind::Inside,
    );
    // The two keys occupy the upper half; the darker one is the assigned key.
    let key_h = h * 0.46;
    let key_rect = match key {
        MouseKey::Left => Rect::from_min_size(body_rect.min, Vec2::new(w * 0.5, key_h)),
        MouseKey::Right => Rect::from_min_size(
            Pos2::new(body_rect.center().x, body_rect.top()),
            Vec2::new(w * 0.5, key_h),
        ),
    };
    let rounding = match key {
        MouseKey::Left => egui::CornerRadius {
            nw: radius as u8,
            ne: 0,
            sw: 0,
            se: 0,
        },
        MouseKey::Right => egui::CornerRadius {
            nw: 0,
            ne: radius as u8,
            sw: 0,
            se: 0,
        },
    };
    painter.rect_filled(key_rect.shrink(0.5), rounding, key_fill);
    let mid_x = body_rect.center().x;
    painter.line_segment(
        [
            Pos2::new(mid_x, body_rect.top() + 0.5),
            Pos2::new(mid_x, body_rect.top() + key_h),
        ],
        Stroke::new(1.0, outline),
    );
    painter.line_segment(
        [
            Pos2::new(body_rect.left() + 0.5, body_rect.top() + key_h),
            Pos2::new(body_rect.right() - 0.5, body_rect.top() + key_h),
        ],
        Stroke::new(1.0, outline),
    );
}

/// A menu row for a tool: a mouse glyph on the left when the tool is on the
/// left button, the tool's icon and name, and a mouse glyph on the right when
/// it is on the right button. Left-click and right-click are both reported.
pub fn tool_menu_row(ui: &mut Ui, icon: impl Into<Icon>, label: &str, on_left: bool, on_right: bool) -> Response {
    let icon = icon.into();
    let desired = Vec2::new(200.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let dark = ui.visuals().dark_mode;
        let active = on_left || on_right;
        if response.hovered() {
            painter.rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
        } else if active {
            painter.rect_filled(
                rect,
                4.0,
                if dark {
                    Color32::from_white_alpha(16)
                } else {
                    Color32::from_black_alpha(12)
                },
            );
        }
        let text_color = if response.hovered() {
            Color32::WHITE
        } else {
            ui.visuals().text_color()
        };
        let glyph_dark = dark || response.hovered();
        let slot = 20.0;
        if on_left {
            let r = Rect::from_min_size(
                Pos2::new(rect.left() + 5.0, rect.top() + 4.0),
                Vec2::new(slot - 6.0, rect.height() - 8.0),
            );
            mouse_glyph(painter, r, MouseKey::Left, glyph_dark);
        }
        if on_right {
            let r = Rect::from_min_size(
                Pos2::new(rect.right() - slot + 1.0, rect.top() + 4.0),
                Vec2::new(slot - 6.0, rect.height() - 8.0),
            );
            mouse_glyph(painter, r, MouseKey::Right, glyph_dark);
        }
        paint_icon(
            ui,
            icon,
            Pos2::new(rect.left() + slot + 12.0, rect.center().y),
            15.0,
            text_color,
            255,
        );
        let painter = ui.painter();
        painter.text(
            Pos2::new(rect.left() + slot + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            text_color,
        );
    }
    response
}

/// A grid pictogram button for the Tiling menu: `rows` by `cols` cells.
pub fn tiling_button(ui: &mut Ui, rows: usize, cols: usize, selected: bool, enabled: bool) -> Response {
    let size = Vec2::new(52.0, 44.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let dark = ui.visuals().dark_mode;
        if selected {
            painter.rect_filled(rect, 5.0, ui.visuals().selection.bg_fill);
        } else if enabled && response.hovered() {
            painter.rect_filled(
                rect,
                5.0,
                if dark {
                    Color32::from_white_alpha(20)
                } else {
                    Color32::from_black_alpha(14)
                },
            );
        }
        let cell_color = if !enabled {
            if dark {
                Color32::from_gray(80)
            } else {
                Color32::from_gray(190)
            }
        } else if selected {
            Color32::WHITE
        } else if dark {
            Color32::from_gray(210)
        } else {
            Color32::from_gray(70)
        };
        let grid = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 17.0), Vec2::new(26.0, 20.0));
        let gap = 2.0;
        let cw = (grid.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let ch = (grid.height() - gap * (rows as f32 - 1.0)) / rows as f32;
        for r in 0..rows {
            for c in 0..cols {
                let cell = Rect::from_min_size(
                    Pos2::new(grid.left() + c as f32 * (cw + gap), grid.top() + r as f32 * (ch + gap)),
                    Vec2::new(cw, ch),
                );
                painter.rect_filled(cell, 1.0, cell_color);
            }
        }
        painter.text(
            Pos2::new(rect.center().x, rect.bottom() - 3.0),
            Align2::CENTER_BOTTOM,
            format!("{rows}\u{00d7}{cols}"),
            FontId::proportional(10.5),
            cell_color,
        );
    }
    response
}

/// Width shared by the items of one menu: the widest item seen last frame.
#[derive(Clone, Copy, Default)]
struct MenuWidth {
    frame: u64,
    current: f32,
    previous: f32,
}

fn menu_item_width(ui: &mut Ui, needed: f32) -> f32 {
    let key = ui.scope_id().with("menu-width");
    let frame = ui.ctx().cumulative_frame_nr();
    let mut mw: MenuWidth = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    if mw.frame != frame {
        mw.previous = if mw.frame + 1 == frame { mw.current } else { 0.0 };
        mw.current = 0.0;
        mw.frame = frame;
    }
    mw.current = mw.current.max(needed);
    ui.data_mut(|d| d.insert_temp(key, mw));
    if mw.previous < mw.current {
        ui.ctx().request_repaint();
    }
    mw.previous.max(needed).max(ui.spacing().menu_width)
}

/// A menu row in the macOS style: full width, blue highlight with white
/// text under the pointer, a tick when `selected`.
pub fn menu_item(ui: &mut Ui, text: impl Into<egui::WidgetText>, selected: bool) -> Response {
    menu_item_enabled(ui, text, selected, true)
}

pub fn menu_item_enabled(ui: &mut Ui, text: impl Into<egui::WidgetText>, selected: bool, enabled: bool) -> Response {
    menu_item_full(ui, text, "", selected, enabled)
}

/// A menu row with a keyboard shortcut shown at the right.
pub fn menu_item_shortcut(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    shortcut: &str,
    selected: bool,
    enabled: bool,
) -> Response {
    menu_item_full(ui, text, shortcut, selected, enabled)
}

fn menu_item_full(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    shortcut: &str,
    selected: bool,
    enabled: bool,
) -> Response {
    let galley = text.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let shortcut_galley = (!shortcut.is_empty()).then(|| {
        egui::WidgetText::from(egui::RichText::new(shortcut).size(11.5)).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Button,
        )
    });
    let extra = shortcut_galley.as_ref().map(|g| g.size().x + 24.0).unwrap_or(0.0);
    let width = menu_item_width(ui, galley.size().x + 44.0 + extra);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, 24.0),
        if enabled { Sense::click() } else { Sense::hover() },
    );
    if ui.is_rect_visible(rect) {
        let dark = ui.visuals().dark_mode;
        let hovered = enabled && response.hovered();
        if hovered {
            ui.painter().rect_filled(rect, 5.0, theme::SELECTION_BLUE);
        }
        let color = if hovered {
            Color32::WHITE
        } else if !enabled {
            if dark {
                Color32::from_gray(120)
            } else {
                Color32::from_gray(160)
            }
        } else if dark {
            Color32::from_gray(230)
        } else {
            Color32::from_gray(25)
        };
        if selected {
            let c = Pos2::new(rect.left() + 12.0, rect.center().y);
            let stroke = Stroke::new(1.8, color);
            ui.painter()
                .line_segment([c + Vec2::new(-4.0, 0.0), c + Vec2::new(-1.0, 3.0)], stroke);
            ui.painter()
                .line_segment([c + Vec2::new(-1.0, 3.0), c + Vec2::new(5.0, -4.0)], stroke);
        }
        let pos = Pos2::new(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0);
        if hovered || !enabled {
            ui.painter().galley_with_override_text_color(pos, galley, color);
        } else {
            ui.painter().galley(pos, galley, color);
        }
        if let Some(g) = shortcut_galley {
            let c = if hovered {
                Color32::from_white_alpha(200)
            } else {
                Color32::from_gray(140)
            };
            let pos = Pos2::new(rect.right() - 12.0 - g.size().x, rect.center().y - g.size().y / 2.0);
            ui.painter().galley_with_override_text_color(pos, g, c);
        }
    }
    response
}

/// A small grey heading inside a menu.
pub fn menu_heading(ui: &mut Ui, text: &str) {
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .strong()
            .color(Color32::from_gray(140)),
    );
}

/// A menu row with a check box state that toggles on click.
pub fn menu_check(ui: &mut Ui, value: &mut bool, text: &str) -> Response {
    let r = menu_item(ui, text, *value);
    if r.clicked() {
        *value = !*value;
    }
    r
}
