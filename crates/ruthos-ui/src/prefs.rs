//! The Preferences window (Horos: PreferencesWindowController).
//!
//! A row of sections across the top, the section's controls below, and
//! Save / Cancel at the bottom. Edits go to a draft copy of the settings and
//! are written to `settings.json` on Save; the application then applies what
//! can change live (the listener, viewer defaults, measurement sets) and
//! says what needs a restart (the data folder, thumbnail size).

use egui::{Color32, RichText, Ui};

use ruthos_core::{settings::default_measurement_sets, MeasurementSet, Settings};

use crate::icons::{self, Bitmap};
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{toolbar_separator, ToolbarButton};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Listener,
    Viewer,
    Measurements,
}

pub struct PrefsState {
    section: Section,
    draft: Settings,
    /// Measurement sets are edited as text, one item per line.
    set_text: Vec<String>,
    pub close_requested: bool,
    /// The saved settings, handed to the app to apply.
    pub saved: Option<Settings>,
    note: Option<String>,
}

impl PrefsState {
    pub fn new(shared: &Shared) -> Self {
        let draft = shared.settings.lock().clone();
        let set_text = draft.measurement_sets.iter().map(|s| s.items.join("\n")).collect();
        // Developer option: `RUTHOS_OPEN_PREFS=listener|viewer|measurements`.
        let section = match std::env::var("RUTHOS_OPEN_PREFS").as_deref() {
            Ok("listener") => Section::Listener,
            Ok("viewer") => Section::Viewer,
            Ok("measurements") => Section::Measurements,
            _ => Section::General,
        };
        Self {
            section,
            draft,
            set_text,
            close_requested: false,
            saved: None,
            note: None,
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.style_mut().visuals = theme::browser_visuals();
        if ui.input(|i| i.viewport().close_requested()) {
            self.close_requested = true;
        }

        egui::Panel::top("prefs-sections")
            .exact_size(theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(theme::toolbar_stroke(false))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                theme::paint_toolbar_background(ui, false, egui::vec2(6.0, 3.0));
                ui.horizontal(|ui| {
                    for (section, icon, label) in [
                        (Section::General, crate::widgets::Icon::Glyph(icons::SMART_ALBUM), "General"),
                        (Section::Listener, crate::widgets::Icon::Bitmap(Bitmap::Network), "Listener"),
                        (Section::Viewer, crate::widgets::Icon::Bitmap(Bitmap::Viewer), "Viewer"),
                        (Section::Measurements, crate::widgets::Icon::Bitmap(Bitmap::Length), "Measurements"),
                    ] {
                        if ToolbarButton::new(icon, label).selected(self.section == section).width(88.0).show(ui).clicked() {
                            self.section = section;
                        }
                    }
                    toolbar_separator(ui);
                });
            });

        egui::Panel::bottom("prefs-buttons")
            .exact_size(46.0)
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(236, 236, 236)).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(note) = &self.note {
                        ui.label(RichText::new(note).size(12.0).color(Color32::from_rgb(170, 90, 0)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_sized([90.0, 28.0], egui::Button::new("Save")).clicked() {
                            self.save(shared);
                        }
                        if ui.add_sized([90.0, 28.0], egui::Button::new("Cancel")).clicked() {
                            self.close_requested = true;
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(246, 246, 246)).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.section {
                    Section::General => self.general(ui, shared),
                    Section::Listener => self.listener(ui),
                    Section::Viewer => self.viewer(ui, shared),
                    Section::Measurements => self.measurements(ui),
                });
            });
    }

    fn heading(ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).size(16.0).strong());
        ui.add_space(6.0);
    }

    fn hint(ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).size(12.0).color(Color32::from_gray(110)));
    }

    fn general(&mut self, ui: &mut Ui, shared: &Shared) {
        Self::heading(ui, "Database folder");
        let current = self.draft.data_root.clone().unwrap_or_else(ruthos_core::DataPaths::default_root);
        ui.horizontal(|ui| {
            ui.label(RichText::new(current.display().to_string()).monospace().size(12.5));
        });
        ui.horizontal(|ui| {
            if ui.button("Change…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Choose the Ruthos database folder").pick_folder() {
                    self.draft.data_root = Some(dir);
                }
            }
            if self.draft.data_root.is_some() && ui.button("Use default").clicked() {
                self.draft.data_root = None;
            }
        });
        Self::hint(ui, "Imported files, the database and thumbnails live here. A change takes effect the next time Ruthos starts; nothing is moved.");
        if std::env::var_os("RUTHOS_DATA_ROOT").is_some() {
            Self::hint(ui, &format!("RUTHOS_DATA_ROOT is set, so this run uses {}.", shared.paths.root.display()));
        }
        ui.add_space(16.0);

        Self::heading(ui, "Importing");
        ui.checkbox(&mut self.draft.copy_files_on_import, "Copy imported files into the database folder");
        Self::hint(ui, "Off links to files where they are, as Horos' \"Link\" mode does. Files received by the listener are always moved in.");
        ui.add_space(16.0);

        Self::heading(ui, "Thumbnails");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.draft.thumbnail_size, 64..=160).suffix(" px"));
        });
        Self::hint(ui, "Applies to thumbnails made after the next start.");
    }

    fn listener(&mut self, ui: &mut Ui) {
        Self::heading(ui, "DICOM listener");
        ui.checkbox(&mut self.draft.listener_enabled, "Receive studies from modalities and other viewers");
        ui.add_space(6.0);
        egui::Grid::new("listener-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("AE title");
            let mut ae = self.draft.listener_ae_title.clone();
            if ui.add(egui::TextEdit::singleline(&mut ae).desired_width(160.0)).changed() {
                ae = ae.chars().filter(|c| c.is_ascii_graphic()).take(16).collect::<String>().to_uppercase();
                self.draft.listener_ae_title = ae;
            }
            ui.end_row();
            ui.label("Port");
            let mut port = self.draft.listener_port as u32;
            ui.add(egui::DragValue::new(&mut port).range(1..=65535));
            self.draft.listener_port = port as u16;
            ui.end_row();
            ui.label("Check incoming folder every");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.draft.incoming_scan_seconds).range(1..=60).suffix(" s"));
            });
            ui.end_row();
        });
        Self::hint(ui, "Point the modality at this computer's address, the AE title and the port. Changes to the listener apply as soon as you save. Windows Firewall must allow Ruthos on the port.");
    }

    fn viewer(&mut self, ui: &mut Ui, shared: &Shared) {
        Self::heading(ui, "Annotations");
        ui.horizontal(|ui| {
            for (v, label) in [(2u8, "Full"), (1, "Partial"), (0, "None")] {
                ui.selectable_value(&mut self.draft.annotations_level, v, label);
            }
        });
        Self::hint(ui, "The corner annotations a viewer opens with. The Annotations button cycles them per window.");
        ui.add_space(16.0);

        Self::heading(ui, "Colour lookup table");
        egui::ComboBox::from_id_salt("prefs-clut")
            .selected_text(self.draft.default_clut.clone())
            .width(200.0)
            .show_ui(ui, |ui| {
                for name in shared.cluts.names() {
                    ui.selectable_value(&mut self.draft.default_clut, name.to_string(), name);
                }
            });
        Self::hint(ui, "Applied to new viewer windows. The CLUT menu changes it per cell.");
    }

    fn measurements(&mut self, ui: &mut Ui) {
        Self::heading(ui, "Measurement sets");
        Self::hint(ui, "Clicking a measurement's number in the viewer offers these names. One name per line.");
        ui.add_space(8.0);
        let mut remove: Option<usize> = None;
        let n = self.draft.measurement_sets.len();
        for i in 0..n {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.draft.measurement_sets[i].name).desired_width(220.0).hint_text("Set name"));
                if ui.small_button("Remove set").clicked() {
                    remove = Some(i);
                }
            });
            ui.add(egui::TextEdit::multiline(&mut self.set_text[i]).desired_width(360.0).desired_rows(4).hint_text("One measurement per line"));
            ui.add_space(10.0);
        }
        if let Some(i) = remove {
            self.draft.measurement_sets.remove(i);
            self.set_text.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.button("Add set").clicked() {
                self.draft.measurement_sets.push(MeasurementSet { name: "New set".into(), items: Vec::new() });
                self.set_text.push(String::new());
            }
            if ui.button("Restore defaults").clicked() {
                self.draft.measurement_sets = default_measurement_sets();
                self.set_text = self.draft.measurement_sets.iter().map(|s| s.items.join("\n")).collect();
            }
        });
    }

    fn save(&mut self, shared: &Shared) {
        for (set, text) in self.draft.measurement_sets.iter_mut().zip(&self.set_text) {
            set.items = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
        }
        self.draft.measurement_sets.retain(|s| !s.name.trim().is_empty());
        if self.draft.listener_ae_title.trim().is_empty() {
            self.draft.listener_ae_title = "RUTHOS".into();
        }
        match self.draft.save() {
            Ok(()) => {
                let previous = shared.settings.lock().clone();
                *shared.settings.lock() = self.draft.clone();
                let mut notes = Vec::new();
                if previous.data_root != self.draft.data_root {
                    notes.push("the database folder");
                }
                if previous.thumbnail_size != self.draft.thumbnail_size {
                    notes.push("the thumbnail size");
                }
                self.saved = Some(self.draft.clone());
                if notes.is_empty() {
                    self.close_requested = true;
                } else {
                    self.note = Some(format!("Saved. Restart Ruthos to apply {}.", notes.join(" and ")));
                }
            }
            Err(e) => self.note = Some(format!("Could not save: {e}")),
        }
    }
}
