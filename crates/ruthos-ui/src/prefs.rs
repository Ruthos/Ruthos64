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
    Watch,
    Viewer,
    Measurements,
    GuidedReview,
    Advanced,
}

pub struct PrefsState {
    section: Section,
    draft: Settings,
    library_editor: LibraryEditor,
    /// The PACS being edited, by index.
    pacs_edit: Option<usize>,
    /// The routing rule being edited, by index.
    route_edit: Option<usize>,
    /// Why the last folder could not be added to the watch list.
    watch_message: Option<String>,
    pacs_tests: std::collections::HashMap<usize, crate::pacs::Task<Result<std::time::Duration, ruthos_net::NetError>>>,
    pacs_results: std::collections::HashMap<usize, Result<std::time::Duration, ruthos_net::NetError>>,
    pub close_requested: bool,
    /// The dial settings when the window opened. Dial changes apply at
    /// once, so the user can turn the dial and compare; closing without
    /// saving puts these back.
    dial_original: ruthos_core::settings::DialSettings,
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
            Some("watch") => Section::Watch,
            Some("viewer") => Section::Viewer,
            Some("advanced") => Section::Advanced,
            Some("measurements") => Section::Measurements,
            Some("guided") => Section::GuidedReview,
            _ => Section::General,
        };
        Self {
            section,
            draft,
            library_editor: LibraryEditor::default(),
            pacs_edit: None,
            route_edit: None,
            watch_message: None,
            pacs_tests: Default::default(),
            pacs_results: Default::default(),
            close_requested: false,
            dial_original: shared.settings.lock().dial.clone(),
            saved: None,
            note: None,
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        self.show_inner(ui, shared);
        if self.close_requested {
            shared.settings.lock().dial = self.dial_original.clone();
        }
    }

    fn show_inner(&mut self, ui: &mut Ui, shared: &Shared) {
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
                        (
                            Section::Watch,
                            crate::widgets::Icon::Bitmap(Bitmap::Import),
                            "Watch folders",
                        ),
                        (Section::Viewer, crate::widgets::Icon::Bitmap(Bitmap::Viewer), "Viewer"),
                        (
                            Section::Measurements,
                            crate::widgets::Icon::Bitmap(Bitmap::Length),
                            "Measurements",
                        ),
                        (
                            Section::GuidedReview,
                            crate::widgets::Icon::Bitmap(Bitmap::Network),
                            "Guided Review",
                        ),
                        (
                            Section::Advanced,
                            crate::widgets::Icon::Bitmap(Bitmap::Presets),
                            "Advanced",
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
                        Section::Listener => self.listener(ui, shared),
                        Section::Pacs => self.pacs(ui, shared),
                        Section::Routing => self.routing(ui, shared),
                        Section::Watch => self.watch_folders(ui, shared),
                        Section::Viewer => self.viewer(ui, shared),
                        Section::Advanced => self.advanced(ui),
                        Section::Measurements => self.measurements(ui),
                        Section::GuidedReview => {
                            let ctx = ui.ctx().clone();
                            shared.session.lock().tab(ui, shared, &ctx);
                        }
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
        ui.horizontal(|ui| {
            ui.label("Your name");
            ui.text_edit_singleline(&mut self.draft.user_name);
        });
        Self::hint(
            ui,
            "Recorded as the creator of segments you save to a study. Leave it empty to record no name.",
        );
        ui.add_space(10.0);
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

    fn watch_folders(&mut self, ui: &mut Ui, shared: &Shared) {
        use ruthos_core::watch::{refuse, WatchFolder};
        Self::heading(ui, "Watch folders");
        Self::hint(
            ui,
            "Ruthos imports new DICOM files that appear in these folders and their subfolders: a modality's export share, a CR reader's output folder, a scanner's drop folder. A file is imported once it has stopped changing, and only once. The files themselves are never moved or deleted.",
        );
        ui.add_space(8.0);
        let status = shared.watch.status.lock().clone();
        let mut remove = None;
        if self.draft.watch_folders.is_empty() {
            Self::hint(ui, "No folders are watched.");
        }
        for (i, f) in self.draft.watch_folders.iter_mut().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.enabled, "");
                    // A long path is shortened so Remove stays in view.
                    let room = (ui.available_width() - 80.0).max(60.0);
                    ui.add_sized(
                        [room, 18.0],
                        egui::Label::new(RichText::new(f.path.display().to_string()).strong()).truncate(),
                    )
                    .on_hover_text(f.path.display().to_string());
                    if ui.button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut f.copy, true, "Copy files into Ruthos");
                    ui.radio_value(&mut f.copy, false, "Use them where they are");
                });
                let line = match status.get(&f.path) {
                    _ if !f.enabled => RichText::new("Paused").color(Color32::from_gray(130)),
                    Some(s) if !s.reachable => RichText::new(format!(
                        "Not reachable at {}: {}",
                        s.checked,
                        s.last_error.clone().unwrap_or_default()
                    ))
                    .color(Color32::from_rgb(170, 90, 0)),
                    Some(s) => RichText::new(format!(
                        "Checked {}; {} imported since Ruthos started{}",
                        s.checked,
                        s.imported,
                        s.last_error
                            .as_ref()
                            .map(|e| format!(". Last problem: {e}"))
                            .unwrap_or_default()
                    ))
                    .color(Color32::from_rgb(30, 120, 50)),
                    None => RichText::new("Watched once you press Save").color(Color32::from_gray(130)),
                };
                ui.label(line.size(12.0));
            });
            ui.add_space(4.0);
        }
        if let Some(i) = remove {
            self.draft.watch_folders.remove(i);
        }
        if ui.button("Add a folder…").clicked() {
            if let Some(dir) = rfd::FileDialog::new().set_title("Folder to watch").pick_folder() {
                if let Some(why) = refuse(&dir, &shared.paths) {
                    self.watch_message = Some(why);
                } else if self.draft.watch_folders.iter().any(|f| f.path == dir) {
                    self.watch_message = Some("That folder is already watched.".into());
                } else {
                    self.watch_message = None;
                    self.draft.watch_folders.push(WatchFolder {
                        path: dir,
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(m) = &self.watch_message {
            ui.label(RichText::new(m).color(Color32::from_rgb(190, 40, 30)));
        }
        Self::hint(ui, "Changes take effect when you press Save.");
    }

    /// The dial: Surface Dial, ASUS Dial or the DialPad (Windows only).
    fn dial_section(&mut self, ui: &mut Ui, shared: &Shared) {
        use ruthos_core::settings::DialMode;
        Self::heading(ui, "Dial");
        let before = self.draft.dial.clone();
        let d = &mut self.draft.dial;
        ui.checkbox(&mut d.enabled, "Use a dial (Surface Dial, ASUS Dial or ProArt DialPad)");
        ui.add_enabled_ui(d.enabled, |ui| {
            ui.label("A press cycles through");
            ui.horizontal_wrapped(|ui| {
                for m in DialMode::ALL {
                    let mut on = d.modes.contains(&m);
                    if ui.checkbox(&mut on, m.label()).changed() {
                        if on {
                            d.modes.push(m);
                        } else if d.modes.len() > 1 {
                            d.modes.retain(|x| *x != m);
                        }
                        d.modes.sort_by_key(|x| DialMode::ALL.iter().position(|y| y == x));
                    }
                }
            });
            if !d.modes.contains(&d.start_mode) {
                d.start_mode = d.modes[0];
            }
            egui::Grid::new("prefs-dial")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Mode at start");
                    egui::ComboBox::from_id_salt("prefs-dial-start")
                        .selected_text(d.start_mode.label())
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for m in d.modes.clone() {
                                ui.selectable_value(&mut d.start_mode, m, m.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Degrees per step");
                    ui.horizontal(|ui| {
                        for v in [5u32, 10, 15, 20] {
                            ui.selectable_value(&mut d.degrees_per_step, v, format!("{v}°"));
                        }
                    });
                    ui.end_row();
                });
            ui.checkbox(
                &mut d.accelerate,
                "Fast turns in Navigate move five frames or images a step",
            );
            ui.checkbox(&mut d.reverse, "Reverse direction");
            ui.checkbox(&mut d.show_indicator, "Show the mode and value on the image");
        });
        Self::hint(
            ui,
            "Navigate steps frames in a loop and images in a stack; width and level steps are a fixed share of the image's value range, so the right-drag sets the window and the dial refines it. Set the ASUS Dial to Microsoft Wheel Device for the dial to appear here, or use the keyboard shortcuts in docs/dial.md. Dial settings apply at once; Save keeps them.",
        );
        if self.draft.dial != before {
            shared.settings.lock().dial = self.draft.dial.clone();
        }
        if let Some(e) = shared.dial.lock().last_error() {
            Self::hint(ui, &format!("Windows did not give Ruthos a dial for one window: {e}"));
        }
    }

    /// Limits on the images Ruthos will decode: guards against damaged or
    /// crafted files. Up to four times the defaults.
    fn advanced(&mut self, ui: &mut Ui) {
        use ruthos_core::settings::DecodeLimits;
        use ruthos_dicom::guard::Limits;
        Self::heading(ui, "Image limits");
        Self::hint(
            ui,
            "Ruthos refuses an image whose size or description is out of all proportion, before any decoder sees it: a damaged or deliberately crafted file cannot then make it run out of memory or misread its data. The defaults sit well above real studies (a large DR plate is about 4,300 by 3,500 pixels; an echo loop a few hundred frames). Raise a limit only for a genuine study that needs it; nothing in a file can raise them, and each stops at four times its default.",
        );
        ui.add_space(8.0);
        let ceiling = DecodeLimits::from_limits(Limits::ceiling());
        let mut l = self.draft.decode_limits.unwrap_or_default();
        let before = l;
        egui::Grid::new("prefs-limits")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Largest side");
                ui.add(
                    egui::DragValue::new(&mut l.side)
                        .range(1024..=ceiling.side)
                        .suffix(" pixels"),
                );
                ui.end_row();
                ui.label("Largest frame");
                ui.add(
                    egui::DragValue::new(&mut l.megapixels)
                        .range(8..=ceiling.megapixels)
                        .suffix(" megapixels"),
                );
                ui.end_row();
                ui.label("Most frames in one image");
                ui.add(egui::DragValue::new(&mut l.frames).range(100..=ceiling.frames));
                ui.end_row();
                ui.label("Largest compressed frame");
                ui.add(
                    egui::DragValue::new(&mut l.fragment_mb)
                        .range(16..=ceiling.fragment_mb)
                        .suffix(" MB"),
                );
                ui.end_row();
                ui.label("Memory for one loop or series");
                ui.add(
                    egui::DragValue::new(&mut l.series_mb)
                        .range(256..=ceiling.series_mb)
                        .suffix(" MB"),
                );
                ui.end_row();
            });
        if l != before {
            self.draft.decode_limits = Some(l);
        }
        ui.add_space(6.0);
        if ui
            .add_enabled(
                self.draft.decode_limits.is_some(),
                egui::Button::new("Restore the defaults"),
            )
            .clicked()
        {
            self.draft.decode_limits = None;
        }
        Self::hint(
            ui,
            "By default the memory limit is the smaller of 2 GB and half this computer's memory. A loop larger than it is shown a frame at a time. Changes take effect when you press Save.",
        );
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

    fn listener(&mut self, ui: &mut Ui, shared: &Shared) {
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
        self.listener_allow(ui, shared);
    }

    /// The devices allowed to send, and those recently refused.
    fn listener_allow(&mut self, ui: &mut Ui, shared: &Shared) {
        use ruthos_core::settings::ListenerPeer;
        ui.add_space(14.0);
        Self::heading(ui, "Devices allowed to send");
        Self::hint(
            ui,
            "The listener accepts images only from devices listed here, by the AE title they call from and, if you give one, the address or subnet they connect from (192.168.1.20 or 192.168.1.0/24). Anything else is refused before any data is sent. The PACS in the PACS section are allowed too, so retrieving from them works. An AE title is not a password: this closes the open door, it does not authenticate a device.",
        );
        ui.add_space(4.0);
        let mut remove = None;
        if self.draft.listener_allow.is_empty() {
            Self::hint(ui, "No devices listed yet: switch the listener on, send a test image from the modality, and add it from the refused list below.");
        } else {
            egui::Grid::new("listener-allow")
                .num_columns(4)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Name").strong());
                    ui.label(RichText::new("AE title").strong());
                    ui.label(RichText::new("Address (optional)").strong());
                    ui.label("");
                    ui.end_row();
                    for (i, p) in self.draft.listener_allow.iter_mut().enumerate() {
                        ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(170.0));
                        let mut ae = p.ae_title.clone();
                        if ui
                            .add(egui::TextEdit::singleline(&mut ae).desired_width(120.0))
                            .changed()
                        {
                            p.ae_title = ae.chars().filter(|c| c.is_ascii_graphic()).take(16).collect();
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut p.address)
                                .desired_width(150.0)
                                .hint_text("any"),
                        );
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
        }
        if let Some(i) = remove {
            self.draft.listener_allow.remove(i);
        }
        if ui.button("Add a device").clicked() {
            self.draft.listener_allow.push(ListenerPeer::default());
        }

        let refused = shared.net_status.lock().refused.clone();
        if !refused.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new("Recently refused").strong());
            for (ae, addr, when) in refused {
                let listed = self
                    .draft
                    .listener_allow
                    .iter()
                    .any(|p| p.ae_title.eq_ignore_ascii_case(&ae));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&when)
                            .monospace()
                            .size(11.5)
                            .color(Color32::from_gray(120)),
                    );
                    ui.label(format!("{ae} from {addr}"));
                    if ui.add_enabled(!listed, egui::Button::new("Add to list")).clicked() {
                        self.draft.listener_allow.push(ListenerPeer {
                            name: ae.clone(),
                            ae_title: ae.clone(),
                            address: addr.clone(),
                        });
                    }
                });
            }
            Self::hint(ui, "Add to list, then Save: the device can send straight away.");
        }

        ui.add_space(10.0);
        ui.checkbox(
            &mut self.draft.listener_promiscuous,
            "Advanced: accept kinds of object Ruthos does not recognise",
        );
        Self::hint(
            ui,
            "Off by default. Only images Ruthos knows how to store are accepted otherwise.",
        );
    }

    fn segmentation(&mut self, ui: &mut Ui) {
        Self::heading(ui, "Segmentation");
        ui.horizontal(|ui| {
            ui.label("Refinement band");
            let mut band = self.draft.refine_band as u32;
            let range = ruthos_segment::refine::BAND_RANGE;
            let bounds = *range.start() as u32..=*range.end() as u32;
            if ui.add(egui::Slider::new(&mut band, bounds).suffix(" voxels")).changed() {
                self.draft.refine_band = band as u8;
            }
        });
        Self::hint(
            ui,
            "How far from the model's edge the edge read from the image may lie. 3 suits most anatomy;              a wider band reaches further but sees more of what is next to the object.",
        );
        ui.checkbox(
            &mut self.draft.load_newest_segments,
            "Load the newest saved segments on opening MPR, without asking",
        );
        ui.checkbox(&mut self.draft.research_mode, "Research mode");
        Self::hint(
            ui,
            "Offers AI models still being tested, and those whose licences allow only research and other \
             non-commercial use. These are installed from a folder for now. A model with such a licence shows \
             its terms before first use, and whether your use is allowed is your responsibility.",
        );
        ui.checkbox(&mut self.draft.save_unrefined_masks, "Save unrefined AI masks");
        Self::hint(
            ui,
            "A developer option for benchmarking: each AI segment is saved twice, so a case can be scored              with and without refinement without running the model again. Leave it off in normal use.",
        );
        ui.add_space(16.0);
    }

    fn viewer(&mut self, ui: &mut Ui, shared: &Shared) {
        self.segmentation(ui);
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
        if cfg!(windows) {
            ui.add_space(16.0);
            self.dial_section(ui, shared);
        }
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
        // Set from the sidebar, perhaps while this window was open.
        {
            let now = shared.settings.lock();
            self.draft.hidden_smart_albums = now.hidden_smart_albums.clone();
            // Set from the Segments panel.
            self.draft.research_agreed = now.research_agreed.clone();
            self.draft.ai_model = now.ai_model.clone();
        }
        match self.draft.save() {
            Ok(()) => {
                let previous = shared.settings.lock().clone();
                *shared.settings.lock() = self.draft.clone();
                *shared.library.lock() = std::sync::Arc::new(self.draft.library());
                crate::platform::apply_decode_limits(&self.draft);
                let mut notes = Vec::new();
                if previous.data_root != self.draft.data_root {
                    notes.push("the database folder");
                }
                if previous.thumbnail_size != self.draft.thumbnail_size {
                    notes.push("the thumbnail size");
                }
                self.saved = Some(self.draft.clone());
                self.dial_original = self.draft.dial.clone();
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
