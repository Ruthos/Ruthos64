//! Preferences editors for measurement definitions, sets and calculations.
//!
//! The user's library is edited in place; built-in entries are shown
//! read-only until "Edit a copy" puts a copy with the same key into the
//! user's library, which then stands in for the built-in one. Deleting
//! that copy restores the built-in entry.

use std::path::PathBuf;

use egui::{Color32, RichText, Ui};
use ruthos_core::measure::{self, Calculation, Definition, Library, MergeReport, Set};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Definitions,
    Sets,
    Calculations,
}

/// Editor state kept between frames.
pub struct LibraryEditor {
    tab: Tab,
    sel_def: Option<String>,
    sel_set: Option<String>,
    sel_calc: Option<String>,
    /// Keys of the selected user set, one per line, while it is edited.
    set_text: Option<(String, String)>,
    /// A file read for import, waiting for confirmation.
    pending_import: Option<(PathBuf, Library, MergeReport)>,
    pub note: Option<String>,
}

impl Default for LibraryEditor {
    fn default() -> Self {
        Self { tab: Tab::Definitions, sel_def: None, sel_set: None, sel_calc: None, set_text: None, pending_import: None, note: None }
    }
}

const TOOLS: [&str; 5] = ["length", "angle", "area", "point", "lv-mmode"];
const QUANTITIES: [&str; 4] = ["length", "angle", "area", "value"];

fn grey(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(Color32::from_gray(110))
}

fn red(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(Color32::from_rgb(180, 40, 30))
}

fn green(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(Color32::from_rgb(30, 120, 50))
}

impl LibraryEditor {
    /// Parse the edited set text back into the user library.
    pub fn flush(&mut self, user: &mut Library) {
        if let Some((name, text)) = self.set_text.take() {
            if let Some(set) = user.sets.iter_mut().find(|s| s.name == name) {
                set.keys = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui, user: &mut Library) {
        ui.horizontal(|ui| {
            for (tab, label) in [(Tab::Definitions, "Definitions"), (Tab::Sets, "Sets"), (Tab::Calculations, "Calculations")] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.flush(user);
                    self.tab = tab;
                }
            }
            ui.add_space(16.0);
            if ui.button("Export JSON…").clicked() {
                self.flush(user);
                self.export(user);
            }
            if ui.button("Import JSON…").clicked() {
                self.flush(user);
                self.import_prompt(user);
            }
        });
        ui.add_space(6.0);
        let effective = measure::effective(user);
        match self.tab {
            Tab::Definitions => self.definitions(ui, user, &effective),
            Tab::Sets => self.sets(ui, user, &effective),
            Tab::Calculations => self.calculations(ui, user, &effective),
        }
        self.import_modal(ui, user);
        ui.add_space(8.0);
        let problems = measure::validate(&measure::effective(user));
        if let Some(p) = problems.first() {
            ui.label(red(format!("{p}{}", if problems.len() > 1 { format!(" (and {} more)", problems.len() - 1) } else { String::new() })));
        }
    }

    // --------------------------------------------------------- definitions

    fn definitions(&mut self, ui: &mut Ui, user: &mut Library, effective: &Library) {
        ui.label(grey("What can be measured. Built-in entries are read-only: edit a copy to change one, delete the copy to restore it."));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("defs-list").max_height(190.0).show(ui, |ui| {
            for d in &effective.definitions {
                let custom = user.definition(&d.key).is_some();
                let builtin = measure::is_builtin_definition(&d.key);
                let tag = match (builtin, custom) {
                    (true, true) => "  (built-in, edited)",
                    (true, false) => "  (built-in)",
                    _ => "",
                };
                let text = format!("{:<28} {}{}", d.key, d.label, tag);
                if ui.selectable_label(self.sel_def.as_deref() == Some(&d.key), RichText::new(text).monospace().size(12.0)).clicked() {
                    self.sel_def = Some(d.key.clone());
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("New definition").clicked() {
                let key = unique_key(effective, "custom.new");
                user.definitions.push(Definition { key: key.clone(), label: "New measurement".into(), ..Default::default() });
                self.sel_def = Some(key);
            }
            let Some(sel) = self.sel_def.clone() else { return };
            let custom = user.definition(&sel).is_some();
            let builtin = measure::is_builtin_definition(&sel);
            if !custom && ui.button("Edit a copy").clicked() {
                if let Some(d) = effective.definition(&sel).cloned() {
                    user.definitions.push(d);
                }
            }
            if ui.button("Duplicate").clicked() {
                if let Some(d) = effective.definition(&sel).cloned() {
                    let key = unique_key(effective, &format!("{}_copy", d.key));
                    user.definitions.push(Definition { key: key.clone(), label: format!("{} (copy)", d.label), ..d });
                    self.sel_def = Some(key);
                }
            }
            if custom {
                let label = if builtin { "Restore built-in" } else { "Delete" };
                if ui.button(label).clicked() {
                    user.definitions.retain(|d| d.key != sel);
                    if !builtin {
                        self.sel_def = None;
                    }
                }
            }
        });
        ui.add_space(6.0);
        let Some(sel) = self.sel_def.clone() else { return };
        if let Some(i) = user.definitions.iter().position(|d| d.key == sel) {
            let others: Vec<String> = effective.definitions.iter().map(|d| d.key.clone()).filter(|k| *k != sel).collect();
            let d = &mut user.definitions[i];
            egui::Grid::new("def-form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Key");
                let r = ui.add(egui::TextEdit::singleline(&mut d.key).desired_width(260.0));
                if r.changed() {
                    self.sel_def = Some(d.key.clone());
                }
                ui.end_row();
                ui.label("Label");
                ui.add(egui::TextEdit::singleline(&mut d.label).desired_width(260.0));
                ui.end_row();
                ui.label("Tool");
                egui::ComboBox::from_id_salt("def-tool").selected_text(d.tool.clone()).width(140.0).show_ui(ui, |ui| {
                    for t in TOOLS {
                        ui.selectable_value(&mut d.tool, t.to_string(), t);
                    }
                });
                ui.end_row();
                ui.label("Quantity");
                egui::ComboBox::from_id_salt("def-quantity").selected_text(d.quantity.clone()).width(140.0).show_ui(ui, |ui| {
                    for q in QUANTITIES {
                        ui.selectable_value(&mut d.quantity, q.to_string(), q);
                    }
                });
                ui.end_row();
                ui.label("Unit");
                ui.add(egui::TextEdit::singleline(&mut d.unit).desired_width(80.0).hint_text("mm, deg, mm2"));
                ui.end_row();
                ui.label("Decimals");
                ui.add(egui::DragValue::new(&mut d.decimals).range(0..=4));
                ui.end_row();
                ui.label("Coded concept");
                let mut concept = d.concept.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut concept).desired_width(200.0).hint_text("LN:29436-3 (optional)")).changed() {
                    d.concept = (!concept.trim().is_empty()).then(|| concept.trim().to_string());
                }
                ui.end_row();
                ui.label("Species");
                let mut species = d.species.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut species).desired_width(200.0).hint_text("any")).changed() {
                    d.species = (!species.trim().is_empty()).then(|| species.trim().to_string());
                }
                ui.end_row();
            });
            if !measure::valid_key(&d.key) {
                ui.label(red("The key must be lower-case letters, digits, dots and underscores, like lv.lvidd"));
            } else if others.contains(&d.key) {
                ui.label(red("Another definition already has this key"));
            } else if d.label.trim().is_empty() {
                ui.label(red("Give the definition a label"));
            } else {
                ui.label(green("Valid"));
            }
        } else if let Some(d) = effective.definition(&sel) {
            egui::Grid::new("def-view").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Key", d.key.clone()),
                    ("Label", d.label.clone()),
                    ("Tool", d.tool.clone()),
                    ("Quantity", d.quantity.clone()),
                    ("Unit", d.unit.clone()),
                    ("Decimals", d.decimals.to_string()),
                    ("Coded concept", d.concept.clone().unwrap_or_else(|| "none".into())),
                    ("Species", d.species.clone().unwrap_or_else(|| "any".into())),
                ] {
                    ui.label(grey(k));
                    ui.label(v);
                    ui.end_row();
                }
            });
        }
    }

    // ---------------------------------------------------------------- sets

    fn sets(&mut self, ui: &mut Ui, user: &mut Library, effective: &Library) {
        ui.label(grey("Groups of definitions offered when naming a measurement. One key per line, in the order shown."));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("sets-list").max_height(150.0).show(ui, |ui| {
            for s in &effective.sets {
                let custom = user.set(&s.name).is_some();
                let builtin = measure::is_builtin_set(&s.name);
                let tag = match (builtin, custom) {
                    (true, true) => "  (built-in, edited)",
                    (true, false) => "  (built-in)",
                    _ => "",
                };
                let text = format!("{}  ({} measurements){tag}", s.name, s.keys.len());
                if ui.selectable_label(self.sel_set.as_deref() == Some(&s.name), text).clicked() {
                    self.flush(user);
                    self.sel_set = Some(s.name.clone());
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("New set").clicked() {
                self.flush(user);
                let mut name = "New set".to_string();
                let mut n = 2;
                while effective.set(&name).is_some() {
                    name = format!("New set {n}");
                    n += 1;
                }
                user.sets.push(Set { name: name.clone(), keys: Vec::new() });
                self.sel_set = Some(name);
            }
            let Some(sel) = self.sel_set.clone() else { return };
            let custom = user.set(&sel).is_some();
            let builtin = measure::is_builtin_set(&sel);
            if !custom && ui.button("Edit a copy").clicked() {
                if let Some(s) = effective.set(&sel).cloned() {
                    user.sets.push(s);
                }
            }
            if ui.button("Duplicate").clicked() {
                self.flush(user);
                if let Some(s) = effective.set(&sel).cloned() {
                    let name = format!("{} (copy)", s.name);
                    user.sets.push(Set { name: name.clone(), keys: s.keys });
                    self.sel_set = Some(name);
                }
            }
            if custom {
                let label = if builtin { "Restore built-in" } else { "Delete" };
                if ui.button(label).clicked() {
                    self.set_text = None;
                    user.sets.retain(|s| s.name != sel);
                    if !builtin {
                        self.sel_set = None;
                    }
                }
            }
        });
        ui.add_space(6.0);
        let Some(sel) = self.sel_set.clone() else { return };
        if let Some(i) = user.sets.iter().position(|s| s.name == sel) {
            if self.set_text.as_ref().map(|(n, _)| n != &sel).unwrap_or(true) {
                self.set_text = Some((sel.clone(), user.sets[i].keys.join("\n")));
            }
            let (name, text) = self.set_text.as_mut().unwrap();
            ui.horizontal(|ui| {
                ui.label("Name");
                let mut new_name = user.sets[i].name.clone();
                if ui.add(egui::TextEdit::singleline(&mut new_name).desired_width(240.0)).changed() && !new_name.trim().is_empty() {
                    user.sets[i].name = new_name.clone();
                    *name = new_name.clone();
                    self.sel_set = Some(new_name);
                }
            });
            ui.label("Keys, one per line");
            ui.add(egui::TextEdit::multiline(text).desired_width(320.0).desired_rows(6).font(egui::TextStyle::Monospace));
            let unknown: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && effective.definition(l).is_none()).collect();
            if unknown.is_empty() {
                ui.label(green("All keys known"));
            } else {
                ui.label(red(format!("Unknown key{}: {}", if unknown.len() == 1 { "" } else { "s" }, unknown.join(", "))));
            }
            ui.label(grey(format!(
                "Available: {}",
                effective.definitions.iter().map(|d| d.key.as_str()).collect::<Vec<_>>().join("  ")
            )));
        } else if let Some(s) = effective.set(&sel) {
            for k in &s.keys {
                ui.label(RichText::new(format!("{:<28} {}", k, effective.label(k))).monospace().size(12.0));
            }
        }
    }

    // --------------------------------------------------------- calculations

    fn calculations(&mut self, ui: &mut Ui, user: &mut Library, effective: &Library) {
        ui.label(grey("Numbers derived from measurements. Expressions use keys, numbers, + − × ÷, ^, parentheses, sqrt, abs, min and max."));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("calcs-list").max_height(150.0).show(ui, |ui| {
            for c in &effective.calculations {
                let custom = user.calculation(&c.key).is_some();
                let builtin = measure::is_builtin_calculation(&c.key);
                let tag = match (builtin, custom) {
                    (true, true) => "  (built-in, edited)",
                    (true, false) => "  (built-in)",
                    _ => "",
                };
                let text = format!("{:<22} {}{}", c.key, c.label, tag);
                if ui.selectable_label(self.sel_calc.as_deref() == Some(&c.key), RichText::new(text).monospace().size(12.0)).clicked() {
                    self.sel_calc = Some(c.key.clone());
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("New calculation").clicked() {
                let key = unique_calc_key(effective, "custom.calc");
                user.calculations.push(Calculation { key: key.clone(), label: "New calculation".into(), ..Default::default() });
                self.sel_calc = Some(key);
            }
            let Some(sel) = self.sel_calc.clone() else { return };
            let custom = user.calculation(&sel).is_some();
            let builtin = measure::is_builtin_calculation(&sel);
            if !custom && ui.button("Edit a copy").clicked() {
                if let Some(c) = effective.calculation(&sel).cloned() {
                    user.calculations.push(c);
                }
            }
            if ui.button("Duplicate").clicked() {
                if let Some(c) = effective.calculation(&sel).cloned() {
                    let key = unique_calc_key(effective, &format!("{}_copy", c.key));
                    user.calculations.push(Calculation { key: key.clone(), label: format!("{} (copy)", c.label), ..c });
                    self.sel_calc = Some(key);
                }
            }
            if custom {
                let label = if builtin { "Restore built-in" } else { "Delete" };
                if ui.button(label).clicked() {
                    user.calculations.retain(|c| c.key != sel);
                    if !builtin {
                        self.sel_calc = None;
                    }
                }
            }
        });
        ui.add_space(6.0);
        let Some(sel) = self.sel_calc.clone() else { return };
        if let Some(i) = user.calculations.iter().position(|c| c.key == sel) {
            let others: Vec<String> = effective.calculations.iter().map(|c| c.key.clone()).filter(|k| *k != sel).collect();
            let c = &mut user.calculations[i];
            egui::Grid::new("calc-form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Key");
                if ui.add(egui::TextEdit::singleline(&mut c.key).desired_width(260.0)).changed() {
                    self.sel_calc = Some(c.key.clone());
                }
                ui.end_row();
                ui.label("Label");
                ui.add(egui::TextEdit::singleline(&mut c.label).desired_width(260.0));
                ui.end_row();
                ui.label("Expression");
                ui.add(egui::TextEdit::singleline(&mut c.expression).desired_width(420.0).font(egui::TextStyle::Monospace));
                ui.end_row();
                ui.label("Lengths in");
                ui.horizontal(|ui| {
                    for u in ["mm", "cm"] {
                        ui.selectable_value(&mut c.input_unit, u.to_string(), u);
                    }
                });
                ui.end_row();
                ui.label("Result unit");
                ui.add(egui::TextEdit::singleline(&mut c.result_unit).desired_width(80.0).hint_text("%, ml, none"));
                ui.end_row();
                ui.label("Decimals");
                ui.add(egui::DragValue::new(&mut c.decimals).range(0..=4));
                ui.end_row();
                ui.label("Scope");
                let mut scope = c.scope.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut scope).desired_width(200.0).hint_text("echo, thorax, canine… (optional)")).changed() {
                    c.scope = (!scope.trim().is_empty()).then(|| scope.trim().to_string());
                }
                ui.end_row();
            });
            if !measure::valid_key(&c.key) {
                ui.label(red("The key must be lower-case letters, digits, dots and underscores"));
            } else if others.contains(&c.key) || effective.definition(&c.key).is_some() {
                ui.label(red("Another entry already has this key"));
            } else {
                match measure::describe_calculation(effective, c) {
                    Ok(text) => ui.label(green(format!("Valid; {text}"))),
                    Err(e) => ui.label(red(e)),
                };
            }
        } else if let Some(c) = effective.calculation(&sel) {
            egui::Grid::new("calc-view").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Key", c.key.clone()),
                    ("Label", c.label.clone()),
                    ("Expression", c.expression.clone()),
                    ("Lengths in", c.input_unit.clone()),
                    ("Result unit", if c.result_unit.is_empty() { "none".into() } else { c.result_unit.clone() }),
                    ("Decimals", c.decimals.to_string()),
                    ("Scope", c.scope.clone().unwrap_or_else(|| "any".into())),
                ] {
                    ui.label(grey(k));
                    ui.label(v);
                    ui.end_row();
                }
            });
            match measure::describe_calculation(effective, c) {
                Ok(text) => ui.label(grey(text)),
                Err(e) => ui.label(red(e)),
            };
        }
    }

    // ------------------------------------------------------- import/export

    fn export(&mut self, user: &Library) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export measurement library")
            .add_filter("JSON", &["json"])
            .set_file_name("ruthos-measurements.json")
            .save_file()
        else {
            return;
        };
        let lib = measure::effective(user);
        match std::fs::write(&path, lib.to_json()) {
            Ok(()) => self.note = Some(format!("Exported {} definitions, {} sets and {} calculations to {}", lib.definitions.len(), lib.sets.len(), lib.calculations.len(), path.display())),
            Err(e) => self.note = Some(format!("Could not export: {e}")),
        }
    }

    fn import_prompt(&mut self, user: &Library) {
        let Some(path) = rfd::FileDialog::new().set_title("Import measurement library").add_filter("JSON", &["json"]).pick_file() else {
            return;
        };
        let incoming: Library = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Library::from_json(&t)) {
            Ok(l) => l,
            Err(e) => {
                self.note = Some(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let problems = measure::validate(&incoming);
        if let Some(p) = problems.first() {
            self.note = Some(format!("The file is not a sound library: {p}"));
            return;
        }
        let report = measure::effective(user).preview_merge(&incoming);
        self.pending_import = Some((path, incoming, report));
    }

    fn import_modal(&mut self, ui: &mut Ui, user: &mut Library) {
        let Some((path, incoming, report)) = self.pending_import.as_ref() else { return };
        let (path, incoming, report) = (path.clone(), incoming.clone(), report.clone());
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::unique("import-library")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.heading("Import measurement library");
            ui.label(grey(path.display().to_string()));
            ui.add_space(6.0);
            ui.label(format!(
                "{} definitions, {} sets and {} calculations; {} new.",
                incoming.definitions.len(),
                incoming.sets.len(),
                incoming.calculations.len(),
                report.added
            ));
            if report.replaces_anything() {
                ui.add_space(4.0);
                ui.label(RichText::new("These existing entries will be replaced:").strong());
                for k in report.replaced_definitions.iter().chain(&report.replaced_calculations) {
                    ui.label(RichText::new(format!("  {k}")).monospace().size(12.0));
                }
                for n in &report.replaced_sets {
                    ui.label(RichText::new(format!("  set '{n}'")).size(12.0));
                }
            } else {
                ui.label(grey("Nothing already in the library changes."));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Import").clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                // Built-in entries unchanged by the file need not be stored.
                let builtin = measure::builtin();
                let mut trimmed = incoming;
                trimmed.definitions.retain(|d| builtin.definition(&d.key) != Some(d));
                trimmed.sets.retain(|s| builtin.set(&s.name) != Some(s));
                trimmed.calculations.retain(|c| builtin.calculation(&c.key) != Some(c));
                let r = user.merge(trimmed);
                self.note = Some(format!(
                    "Imported: {} new, {} replaced. Save to keep it.",
                    r.added,
                    r.replaced_definitions.len() + r.replaced_sets.len() + r.replaced_calculations.len()
                ));
                self.pending_import = None;
            }
            Some(false) => self.pending_import = None,
            None => {}
        }
    }
}

fn unique_key(lib: &Library, base: &str) -> String {
    let mut key = base.to_string();
    let mut n = 2;
    while lib.definition(&key).is_some() || lib.calculation(&key).is_some() {
        key = format!("{base}{n}");
        n += 1;
    }
    key
}

fn unique_calc_key(lib: &Library, base: &str) -> String {
    unique_key(lib, base)
}
