//! The Preferences window (Horos: PreferencesWindowController).
//!
//! A row of sections across the top, the section's controls below, and
//! Save / Cancel at the bottom. Edits go to a draft copy of the settings and
//! are written to `settings.json` on Save; the application then applies what
//! can change live (the listener, viewer defaults, measurement sets) and
//! says what needs a restart (the data folder, thumbnail size).

use egui::{Color32, RichText, Ui};

use ruthos_core::Settings;

use crate::measure_prefs::LibraryEditor;

use crate::icons::{self, Bitmap};
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{toolbar_separator, ToolbarButton};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Listener,
    Pacs,
    Routing,
    Viewer,
    Measurements,
}

pub struct PrefsState {
    section: Section,
    draft: Settings,
    library_editor: LibraryEditor,
    /// The PACS being edited, by index.
    pacs_edit: Option<usize>,
    /// The routing rule being edited, by index.
    route_edit: Option<usize>,
    pacs_tests: std::collections::HashMap<usize, crate::pacs::Task<Result<std::time::Duration, ruthos_net::NetError>>>,
    pacs_results: std::collections::HashMap<usize, Result<std::time::Duration, ruthos_net::NetError>>,
    pub close_requested: bool,
    /// The saved settings, handed to the app to apply.
    pub saved: Option<Settings>,
    note: Option<String>,
}

impl PrefsState {
    pub fn new(shared: &Shared) -> Self {
        let draft = shared.settings.lock().clone();
        // Developer option: `RUTHOS_OPEN_PREFS=listener|pacs|viewer|measurements`;
        // other windows can ask for a section through `prefs_section`.
        let asked = shared.prefs_section.lock().take().map(String::from);
        let section = match asked.or_else(|| std::env::var("RUTHOS_OPEN_PREFS").ok()).as_deref() {
            Some("listener") => Section::Listener,
            Some("pacs") => Section::Pacs,
            Some("routing") => Section::Routing,
            Some("viewer") => Section::Viewer,
            Some("measurements") => Section::Measurements,
            _ => Section::General,
        };
        Self {
            section,
            draft,
            library_editor: LibraryEditor::default(),
            pacs_edit: None,
            route_edit: None,
            pacs_tests: Default::default(),
            pacs_results: Default::default(),
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
                        (
                            Section::General,
                            crate::widgets::Icon::Glyph(icons::SMART_ALBUM),
                            "General",
                        ),
                        (
                            Section::Listener,
                            crate::widgets::Icon::Bitmap(Bitmap::Network),
                            "Listener",
                        ),
                        (Section::Pacs, crate::widgets::Icon::Bitmap(Bitmap::Query), "PACS"),
                        (Section::Routing, crate::widgets::Icon::Bitmap(Bitmap::Send), "Routing"),
                        (Section::Viewer, crate::widgets::Icon::Bitmap(Bitmap::Viewer), "Viewer"),
                        (
                            Section::Measurements,
                            crate::widgets::Icon::Bitmap(Bitmap::Length),
                            "Measurements",
                        ),
                    ] {
                        if ToolbarButton::new(icon, label)
                            .selected(self.section == section)
                            .width(88.0)
                            .show(ui)
                            .clicked()
                        {
                            self.section = section;
                        }
                    }
                    toolbar_separator(ui);
                });
            });

        egui::Panel::bottom("prefs-buttons")
            .exact_size(46.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(236, 236, 236))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
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
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(246, 246, 246))
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.section {
                        Section::General => self.general(ui, shared),
                        Section::Listener => self.listener(ui),
                        Section::Pacs => self.pacs(ui, shared),
                        Section::Routing => self.routing(ui, shared),
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
        let current = self
            .draft
            .data_root
            .clone()
            .unwrap_or_else(ruthos_core::DataPaths::default_root);
        ui.horizontal(|ui| {
            ui.label(RichText::new(current.display().to_string()).monospace().size(12.5));
        });
        ui.horizontal(|ui| {
            if ui.button("Change…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose the Ruthos database folder")
                    .pick_folder()
                {
                    self.draft.data_root = Some(dir);
                }
            }
            if self.draft.data_root.is_some() && ui.button("Use default").clicked() {
                self.draft.data_root = None;
            }
        });
        Self::hint(ui, "Imported files, the database and thumbnails live here. A change takes effect the next time Ruthos starts; nothing is moved.");
        if let Some(service) = ruthos_core::paths::sync_service(&current) {
            ui.label(
                RichText::new(format!(
                    "This folder is inside {service}. A synced folder can corrupt the database when the sync \
                     uploads it and its log at different moments; choose a folder that is not synced."
                ))
                .size(12.0)
                .color(Color32::from_rgb(180, 40, 30)),
            );
        }
        if std::env::var_os("RUTHOS_DATA_ROOT").is_some() {
            Self::hint(
                ui,
                &format!(
                    "RUTHOS_DATA_ROOT is set, so this run uses {}.",
                    shared.paths.root.display()
                ),
            );
        }
        ui.add_space(16.0);

        Self::heading(ui, "Importing");
        ui.checkbox(
            &mut self.draft.copy_files_on_import,
            "Copy imported files into the database folder",
        );
        Self::hint(ui, "Off links to files where they are, as Horos' \"Link\" mode does. Files received by the listener are always moved in.");
        ui.add_space(16.0);

        Self::heading(ui, "Teaching copies");
        ui.horizontal(|ui| {
            ui.label("Reference prefix");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.teaching_reference_prefix)
                    .desired_width(180.0)
                    .hint_text("RUTHOS-2026"),
            );
        });
        Self::hint(
            ui,
            "Suggested references are the prefix and a running number, for example EQ-CARD-2026-0014.",
        );
        ui.add_space(16.0);

        Self::heading(ui, "Thumbnails");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.draft.thumbnail_size, 64..=160).suffix(" px"));
        });
        Self::hint(ui, "Applies to thumbnails made after the next start.");
    }

    fn pacs(&mut self, ui: &mut Ui, shared: &Shared) {
        use crate::pacs::{admin_text, clean_ae, me, node, show_problem, Task, PRESETS};
        let listener_on = shared.net_status.lock().running;
        let me = me(&self.draft, listener_on);

        Self::heading(ui, "This computer");
        egui::Grid::new("pacs-me")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("AE title");
                ui.label(RichText::new(&me.ae_title).monospace());
                ui.end_row();
                ui.label("Address");
                ui.label(RichText::new(&me.address).monospace());
                ui.end_row();
                ui.label("Port");
                ui.label(RichText::new(me.port.to_string()).monospace());
                ui.end_row();
                ui.label("Listener");
                ui.label(if listener_on { "On" } else { "Off" });
                ui.end_row();
            });
        ui.add_space(4.0);
        if ui.button("Copy details for your PACS administrator").clicked() {
            ui.ctx().copy_text(admin_text(&me));
            self.note = Some("Copied. Paste it into an email to whoever runs the PACS.".into());
        }
        Self::hint(
            ui,
            "A PACS has to be told about Ruthos before it lets Ruthos search it. The AE title and port are set under Listener.",
        );
        ui.add_space(14.0);

        Self::heading(ui, "PACS");
        // Finished connection tests.
        let done: Vec<usize> = self.pacs_tests.keys().copied().collect();
        for i in done {
            if let Some(r) = self.pacs_tests.get(&i).and_then(|t| t.take()) {
                self.pacs_tests.remove(&i);
                self.pacs_results.insert(i, r);
            }
        }
        if self.draft.pacs_nodes.is_empty() {
            Self::hint(
                ui,
                "None yet. Add one to search it, retrieve studies from it and send studies to it.",
            );
        }
        let mut remove = None;
        let mut test = None;
        for i in 0..self.draft.pacs_nodes.len() {
            let n = self.draft.pacs_nodes[i].clone();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&n.name).strong());
                        ui.label(
                            RichText::new(format!("{}:{}  \u{b7}  AE title {}", n.host, n.port, n.ae_title))
                                .size(12.0)
                                .color(Color32::from_gray(110)),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                        if ui
                            .button(if self.pacs_edit == Some(i) { "Close" } else { "Edit" })
                            .clicked()
                        {
                            self.pacs_edit = if self.pacs_edit == Some(i) { None } else { Some(i) };
                        }
                        let testing = self.pacs_tests.contains_key(&i);
                        if ui
                            .add_enabled(
                                !testing,
                                egui::Button::new(if testing { "Testing\u{2026}" } else { "Test" }),
                            )
                            .clicked()
                        {
                            test = Some(i);
                        }
                    });
                });
                match self.pacs_results.get(&i) {
                    Some(Ok(d)) => {
                        ui.label(
                            RichText::new(format!("Connected: the PACS answered in {} ms.", d.as_millis().max(1)))
                                .color(Color32::from_rgb(30, 120, 50)),
                        );
                    }
                    Some(Err(e)) => show_problem(ui, ("pacs-problem", i), &crate::pacs::problem(e, &n, &me)),
                    None => {}
                }
            });
            ui.add_space(4.0);
        }
        if let Some(i) = test {
            let n = node(&self.draft.pacs_nodes[i]);
            let ae = self.draft.listener_ae_title.clone();
            self.pacs_results.remove(&i);
            self.pacs_tests.insert(
                i,
                Task::spawn(ui.ctx(), "ruthos-echo", move || ruthos_net::echo(&n, &ae)),
            );
        }
        if let Some(i) = remove {
            self.draft.pacs_nodes.remove(i);
            self.pacs_edit = None;
            self.pacs_tests.clear();
            self.pacs_results.clear();
        }
        if ui.button("Add a PACS").clicked() {
            self.draft.pacs_nodes.push(ruthos_core::settings::PacsNode::default());
            self.pacs_edit = Some(self.draft.pacs_nodes.len() - 1);
        }

        // The editor.
        let Some(i) = self.pacs_edit.filter(|i| *i < self.draft.pacs_nodes.len()) else {
            return;
        };
        ui.add_space(12.0);
        let mut test_now = false;
        let n = &mut self.draft.pacs_nodes[i];
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("Edit {}", n.name)).size(14.0).strong());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Start from");
                egui::ComboBox::from_id_salt("pacs-preset")
                    .selected_text("Choose a system\u{2026}")
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (name, port, ae, note) in PRESETS {
                            if ui.selectable_label(false, *name).on_hover_text(*note).clicked() {
                                n.port = *port;
                                if !ae.is_empty() {
                                    n.ae_title = ae.to_string();
                                }
                                if n.name == "New PACS" {
                                    n.name = name.to_string();
                                }
                            }
                        }
                    });
            });
            ui.add_space(4.0);
            egui::Grid::new("pacs-edit").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(240.0).hint_text("Anything you like"));
                ui.end_row();
                ui.label("Address");
                ui.add(
                    egui::TextEdit::singleline(&mut n.host)
                        .desired_width(240.0)
                        .hint_text("pacs.clinic.local or 192.168.1.20"),
                );
                ui.end_row();
                ui.label("Port");
                let mut port = n.port as u32;
                ui.add(egui::DragValue::new(&mut port).range(1..=65535));
                n.port = port as u16;
                ui.end_row();
                ui.label("AE title");
                let mut ae = n.ae_title.clone();
                if ui
                    .add(egui::TextEdit::singleline(&mut ae).desired_width(160.0).hint_text("The PACS's DICOM name"))
                    .changed()
                {
                    n.ae_title = clean_ae(&ae);
                }
                ui.end_row();
            });
            Self::hint(
                ui,
                "The address, port and AE title come from whoever runs the PACS. AE titles must match exactly, capitals included.",
            );
            ui.add_space(8.0);
            ui.label("Retrieve with");
            for (value, label, explain) in [
                (
                    "auto",
                    "Automatic (recommended)",
                    "Uses C-GET, which needs nothing more set up on the PACS, and C-MOVE when the PACS only offers that.",
                ),
                ("get", "C-GET", "The PACS sends the images back over the same connection."),
                (
                    "move",
                    "C-MOVE",
                    "The PACS connects back to Ruthos to send the images, so it must know Ruthos's address and port, and the listener must be on.",
                ),
            ] {
                ui.radio_value(&mut n.retrieve, value.to_string(), label);
                ui.label(RichText::new(format!("    {explain}")).size(11.5).color(Color32::from_gray(110)));
            }
            if n.retrieve == "move" && !self.draft.listener_enabled {
                ui.label(
                    RichText::new("The listener is off, so C-MOVE cannot work. Turn it on under Listener.")
                        .color(Color32::from_rgb(190, 40, 30)),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !n.host.trim().is_empty() && !n.ae_title.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new("Test connection")).clicked() {
                    test_now = true;
                }
                if !ready {
                    ui.label(RichText::new("Fill in the address and AE title to test.").size(12.0).color(Color32::from_gray(110)));
                }
            });
        });
        if test_now {
            let n = node(&self.draft.pacs_nodes[i]);
            let ae = self.draft.listener_ae_title.clone();
            self.pacs_results.remove(&i);
            self.pacs_tests.insert(
                i,
                Task::spawn(ui.ctx(), "ruthos-echo", move || ruthos_net::echo(&n, &ae)),
            );
        }
        Self::hint(ui, "Changes are kept when you press Save.");
    }

    fn routing(&mut self, ui: &mut Ui, shared: &Shared) {
        use ruthos_core::routing::{RouteRule, RouteSource};
        Self::heading(ui, "Routing");
        Self::hint(
            ui,
            "Send images on to a PACS as they arrive. Ruthos waits until a study has been quiet for a few seconds and sends it in one go; a send that fails is tried again three times, a minute apart. A study retrieved from a PACS is never sent back to that PACS.",
        );
        ui.add_space(8.0);
        let names: Vec<String> = self.draft.pacs_nodes.iter().map(|n| n.name.clone()).collect();
        if names.is_empty() {
            ui.label("Routing needs a PACS to send to.");
            if ui.button("Set up a PACS first").clicked() {
                self.section = Section::Pacs;
            }
            return;
        }
        let mut remove = None;
        for i in 0..self.draft.routing_rules.len() {
            let r = &mut self.draft.routing_rules[i];
            ui.horizontal(|ui| {
                ui.checkbox(&mut r.enabled, "");
                let text = RichText::new(r.sentence());
                ui.label(if r.enabled {
                    text
                } else {
                    text.color(Color32::from_gray(140))
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Remove").clicked() {
                        remove = Some(i);
                    }
                    let editing = self.route_edit == Some(i);
                    if ui.button(if editing { "Close" } else { "Edit" }).clicked() {
                        self.route_edit = if editing { None } else { Some(i) };
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.draft.routing_rules.remove(i);
            self.route_edit = None;
        }
        if ui.button("Add a rule").clicked() {
            self.draft.routing_rules.push(RouteRule {
                destination: names[0].clone(),
                ..Default::default()
            });
            self.route_edit = Some(self.draft.routing_rules.len() - 1);
        }
        if let Some(i) = self.route_edit.filter(|i| *i < self.draft.routing_rules.len()) {
            ui.add_space(10.0);
            let r = &mut self.draft.routing_rules[i];
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("route-edit")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("When images arrive");
                        egui::ComboBox::from_id_salt("route-source")
                            .selected_text(match r.source {
                                RouteSource::Network => "over the network",
                                RouteSource::Any => "from any import",
                            })
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut r.source, RouteSource::Network, "over the network")
                                    .on_hover_text(
                                        "From modalities through the listener, and studies retrieved from a PACS",
                                    );
                                ui.selectable_value(&mut r.source, RouteSource::Any, "from any import")
                                    .on_hover_text("Also folders, files and pictures imported by hand");
                            });
                        ui.end_row();
                        ui.label("Modalities");
                        ui.add(
                            egui::TextEdit::singleline(&mut r.modalities)
                                .desired_width(200.0)
                                .hint_text("US, CR (empty for all)"),
                        );
                        ui.end_row();
                        ui.label("Send to");
                        egui::ComboBox::from_id_salt("route-dest")
                            .selected_text(if r.destination.is_empty() {
                                "Choose a PACS".to_string()
                            } else {
                                r.destination.clone()
                            })
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for n in &names {
                                    ui.selectable_value(&mut r.destination, n.clone(), n);
                                }
                            });
                        ui.end_row();
                    });
                ui.label(RichText::new(r.sentence()).italics());
                if !names.iter().any(|n| n.eq_ignore_ascii_case(r.destination.trim())) {
                    ui.label(
                        RichText::new("No PACS has that name any more; choose one.")
                            .color(Color32::from_rgb(190, 40, 30)),
                    );
                }
            });
        }
        Self::hint(ui, "Changes take effect when you press Save.");

        ui.add_space(14.0);
        Self::heading(ui, "Recent activity");
        let log = shared.route_log.lock().clone();
        if log.is_empty() {
            Self::hint(ui, "Nothing routed since Ruthos started.");
        }
        egui::ScrollArea::vertical()
            .id_salt("route-log")
            .max_height(180.0)
            .show(ui, |ui| {
                for e in log.iter().take(50) {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&e.when)
                                .monospace()
                                .size(11.5)
                                .color(Color32::from_gray(120)),
                        );
                        ui.label(RichText::new(&e.text).size(12.0).color(if e.ok {
                            Color32::from_rgb(30, 120, 50)
                        } else {
                            Color32::from_rgb(170, 60, 20)
                        }));
                    });
                }
            });
    }

    fn listener(&mut self, ui: &mut Ui) {
        Self::heading(ui, "DICOM listener");
        ui.checkbox(
            &mut self.draft.listener_enabled,
            "Receive studies from modalities and other viewers",
        );
        ui.add_space(6.0);
        egui::Grid::new("listener-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("AE title");
                let mut ae = self.draft.listener_ae_title.clone();
                if ui
                    .add(egui::TextEdit::singleline(&mut ae).desired_width(160.0))
                    .changed()
                {
                    ae = ae
                        .chars()
                        .filter(|c| c.is_ascii_graphic())
                        .take(16)
                        .collect::<String>()
                        .to_uppercase();
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
                    ui.add(
                        egui::DragValue::new(&mut self.draft.incoming_scan_seconds)
                            .range(1..=60)
                            .suffix(" s"),
                    );
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
        Self::hint(
            ui,
            "The corner annotations a viewer opens with. The Annotations button cycles them per window.",
        );
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
        Self::heading(ui, "Zoom loupe");
        ui.checkbox(
            &mut self.draft.loupe_enabled,
            "Show a magnified view beside the pointer while placing a point",
        );
        ui.horizontal(|ui| {
            ui.label("Magnification");
            egui::ComboBox::from_id_salt("prefs-loupe-mag")
                .selected_text(format!("{}×", self.draft.loupe_magnification))
                .width(70.0)
                .show_ui(ui, |ui| {
                    for m in crate::loupe::MAGNIFICATIONS {
                        ui.selectable_value(&mut self.draft.loupe_magnification, m, format!("{m}×"));
                    }
                });
            ui.add_space(12.0);
            ui.label("Size");
            egui::ComboBox::from_id_salt("prefs-loupe-size")
                .selected_text(format!("{} px", self.draft.loupe_size))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for v in crate::loupe::SIZES {
                        ui.selectable_value(&mut self.draft.loupe_size, v, format!("{v} px"));
                    }
                });
        });
        ui.checkbox(
            &mut self.draft.loupe_fine_movement,
            "Shift slows the pointer for fine placement",
        );
        Self::hint(ui, "Hold Alt over the image with a measuring tool to preview the loupe. With Shift held, the point moves by the pointer's movement divided by the magnification.");
        ui.add_space(16.0);

        Self::heading(ui, "Echocardiography");
        ui.horizontal(|ui| {
            ui.label("Lengths in");
            for (v, label) in [("mm", "millimetres"), ("cm", "centimetres")] {
                ui.selectable_value(&mut self.draft.echo_length_unit, v.to_string(), label);
            }
        });
        Self::hint(
            ui,
            "The unit of LV M-mode values on the image and in reports. Calculations work in centimetres regardless.",
        );
        ui.add_space(16.0);

        Self::heading(ui, "Definitions, sets and calculations");
        Self::hint(ui, "What can be measured, how measurements are grouped when naming one, and the numbers calculated from them. Changes apply when saved.");
        ui.add_space(6.0);
        self.library_editor.show(ui, &mut self.draft.measurements);
        if let Some(note) = self.library_editor.note.take() {
            self.note = Some(note);
        }
    }

    fn save(&mut self, shared: &Shared) {
        self.library_editor.flush(&mut self.draft.measurements);
        let problems = ruthos_core::measure::validate(&self.draft.library());
        if let Some(p) = problems.first() {
            self.section = Section::Measurements;
            self.note = Some(format!("Not saved: {p}"));
            return;
        }
        if self.draft.listener_ae_title.trim().is_empty() {
            self.draft.listener_ae_title = "RUTHOS".into();
        }
        match self.draft.save() {
            Ok(()) => {
                let previous = shared.settings.lock().clone();
                *shared.settings.lock() = self.draft.clone();
                *shared.library.lock() = std::sync::Arc::new(self.draft.library());
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
