//! The colour map menu: every map drawn as a strip of its own colours.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use ruthos_dicom::clut::{Clut, ClutGroup, ClutLibrary};

use crate::theme;

/// What was chosen.
pub enum Pick {
    /// Gray, and the window the image came with.
    ImageDefault,
    /// A map, and the window it needs if its colours stand for fixed values.
    Table { name: String, window: Option<(f32, f32)> },
}

const STRIP: Vec2 = Vec2::new(190.0, 16.0);
const ROW: Vec2 = Vec2::new(370.0, 24.0);
const AMBER: Color32 = Color32::from_rgb(240, 190, 90);

fn strip(ui: &Ui, rect: Rect, clut: &Clut) {
    let mut mesh = egui::Mesh::default();
    let columns = clut.table.len().max(1);
    let step = rect.width() / columns as f32;
    for (i, [r, g, b]) in clut.table.iter().enumerate() {
        let left = rect.left() + i as f32 * step;
        let column = Rect::from_min_max(Pos2::new(left, rect.top()), Pos2::new(left + step + 0.5, rect.bottom()));
        mesh.add_colored_rect(column, Color32::from_rgb(*r, *g, *b));
    }
    ui.painter().add(mesh);
}

fn row(ui: &mut Ui, label: &str, clut: &Clut, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(ROW, Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let left = rect.left_center() + Vec2::new(6.0, 0.0);
    let bar = Rect::from_min_size(left - Vec2::new(0.0, STRIP.y / 2.0), STRIP);
    strip(ui, bar, clut);
    if selected {
        ui.painter().rect_stroke(
            bar.expand(2.0),
            1.0,
            Stroke::new(2.0, theme::SELECTION_BLUE),
            egui::StrokeKind::Middle,
        );
    }
    ui.painter().text(
        Pos2::new(bar.right() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    if clut.about.is_empty() {
        response
    } else {
        response.on_hover_text(clut.about)
    }
}

fn group(ui: &mut Ui, lib: &ClutLibrary, group: ClutGroup, current: &str, picked: &mut Option<Pick>) {
    for clut in lib.cluts.iter().filter(|c| c.group == group) {
        if row(ui, &clut.name, clut, clut.name == current).clicked() {
            *picked = Some(Pick::Table {
                name: clut.name.clone(),
                window: clut.window(),
            });
            ui.close();
        }
    }
}

/// The menu's contents. The maps for CT appear only on a CT series, because
/// their colours stand for Hounsfield units.
pub fn show(ui: &mut Ui, lib: &ClutLibrary, current: &str, modality: &str) -> Option<Pick> {
    let mut picked = None;
    let gray = lib.get("No CLUT").cloned().unwrap_or_else(Clut::grayscale);
    if row(ui, "Default (image)", &gray, false).clicked() {
        picked = Some(Pick::ImageDefault);
        ui.close();
    }
    if row(ui, "Gray", &gray, current == gray.name).clicked() {
        picked = Some(Pick::Table {
            name: gray.name.clone(),
            window: None,
        });
        ui.close();
    }
    if modality == "CT" {
        ui.separator();
        ui.label(egui::RichText::new("CT").small().weak());
        group(ui, lib, ClutGroup::Ct, current, &mut picked);
    }
    for (name, kind) in [
        ("Classic", ClutGroup::Classic),
        ("Scientific", ClutGroup::Scientific),
        ("Rendering", ClutGroup::Rendering),
    ] {
        ui.separator();
        ui.menu_button(name, |ui| {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                group(ui, lib, kind, current, &mut picked);
            });
        });
    }
    if picked.is_some() {
        ui.close();
    }
    picked
}

/// A map whose colours stand for fixed values says so when the window has moved
/// off them. True when asked to put the window back.
pub fn moved_warning(ui: &mut Ui, clut: Option<&Clut>, window: (f32, f32)) -> bool {
    let Some(clut) = clut else { return false };
    if clut.window().is_none() || clut.shows_true_values(window.0, window.1) {
        return false;
    }
    ui.add(egui::Button::new(egui::RichText::new("Colours off").small().color(AMBER)).frame(false))
        .on_hover_text("The window has moved, so these colours no longer mean what the map says. Click to put it back")
        .clicked()
}
