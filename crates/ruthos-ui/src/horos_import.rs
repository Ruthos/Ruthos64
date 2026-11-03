//! File > Import Horos Database: bring a Horos (or OsiriX) library over,
//! with its albums, comments, status and key images.
//!
//! Three steps in one window: Ruthos reads the library and says what it
//! found, the user chooses to copy the images or use them where they are,
//! and the import runs with progress and Cancel. Horos's own files are never
//! changed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{Color32, Context, RichText};
use parking_lot::Mutex;

use ruthos_core::horos::{self, HorosCatalog, HorosLibrary, MigrateMode, MigrationSummary};

use crate::pacs::Task;
use crate::shared::Shared;

type Progress = Arc<Mutex<(usize, usize, String)>>;

enum Stage {
    Reading(Task<Result<HorosCatalog, String>>),
    Found(Box<HorosCatalog>, u64),
    Importing(Task<Result<MigrationSummary, String>>),
    Done(Box<MigrationSummary>),
    Failed(String),
}

pub struct HorosImport {
    library: HorosLibrary,
    stage: Stage,
    mode: MigrateMode,
    progress: Progress,
    cancel: Arc<AtomicBool>,
    /// Developer option: start importing as soon as the library is read.
    pub auto_start: bool,
}

/// A fault on a worker thread must reach the window, or it would wait for ever.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("an internal error stopped the import; see crash.log".into()))
}

fn size_text(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else if bytes < 1_000_000 {
        "under 1 MB".to_string()
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl HorosImport {
    /// Ask for the library folder and start reading it. `None` when the user
    /// cancelled the folder dialog; an error when the folder holds no library.
    pub fn pick(ctx: &Context, shared: &Shared) -> Option<Result<Self, String>> {
        let dir = rfd::FileDialog::new()
            .set_title("Choose the Horos Data folder (or the folder that contains it)")
            .pick_folder()?;
        Some(Self::open(ctx, shared, dir))
    }

    pub fn open(ctx: &Context, shared: &Shared, dir: PathBuf) -> Result<Self, String> {
        let Some(library) = horos::locate(&dir) else {
            return Err(format!(
                "No Horos library in {}. Choose the folder called \"Horos Data\" (it holds Database.sql and DATABASE.noindex), or the folder that contains it.",
                dir.display()
            ));
        };
        let lib = library.clone();
        let paths = shared.paths.clone();
        let task = Task::spawn(ctx, "ruthos-horos-read", move || {
            guarded(|| horos::read_catalog(&lib, &paths).map_err(|e| e.to_string()))
        });
        Ok(Self {
            library,
            stage: Stage::Reading(task),
            mode: MigrateMode::Copy,
            progress: Arc::new(Mutex::new((0, 0, String::new()))),
            cancel: Arc::new(AtomicBool::new(false)),
            auto_start: false,
        })
    }

    fn start(&mut self, ctx: &Context, shared: &Shared, catalog: HorosCatalog) {
        let db = shared.db.clone();
        let paths = shared.paths.clone();
        let mode = self.mode;
        let progress = self.progress.clone();
        let cancel = self.cancel.clone();
        let repaint = ctx.clone();
        self.stage = Stage::Importing(Task::spawn(ctx, "ruthos-horos-import", move || {
            guarded(|| {
                horos::migrate(
                    &db,
                    &paths,
                    &catalog,
                    mode,
                    &|done, total, what| {
                        *progress.lock() = (done, total, what.to_string());
                        repaint.request_repaint();
                    },
                    &cancel,
                )
                .map_err(|e| e.to_string())
            })
        }));
    }

    /// Draw the window. Returns `Some(changed)` when it should close;
    /// `changed` says the archive gained studies.
    pub fn show(&mut self, ctx: &Context, shared: &Shared) -> Option<bool> {
        // Finished work moves the window on.
        match &self.stage {
            Stage::Reading(t) => {
                if let Some(r) = t.take() {
                    self.stage = match r {
                        Ok(c) => {
                            let bytes = c.bytes();
                            Stage::Found(Box::new(c), bytes)
                        }
                        Err(e) => Stage::Failed(e),
                    };
                }
            }
            Stage::Importing(t) => {
                if let Some(r) = t.take() {
                    self.stage = match r {
                        Ok(s) => Stage::Done(Box::new(s)),
                        Err(e) => Stage::Failed(e),
                    };
                }
            }
            _ => {}
        }
        if matches!(self.stage, Stage::Reading(_) | Stage::Importing(_)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        let mut close = None;
        let mut start = None;
        egui::Modal::new(egui::Id::unique("horos-import")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading("Import a Horos database");
            ui.label(
                RichText::new(self.library.data_dir.display().to_string())
                    .size(12.0)
                    .color(Color32::from_gray(110)),
            );
            ui.separator();
            match &mut self.stage {
                Stage::Reading(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the Horos database and finding its images…");
                    });
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        close = Some(false);
                    }
                }
                Stage::Found(catalog, bytes) => {
                    let c = &**catalog;
                    let smart = c.albums.iter().filter(|a| a.smart).count();
                    ui.label(RichText::new("Found").strong());
                    for line in [
                        format!(
                            "{}, {} ({})",
                            plural(c.studies.len(), "study", "studies"),
                            plural(c.importable(), "image", "images"),
                            size_text(*bytes)
                        ),
                        format!(
                            "{} and {}",
                            plural(c.albums.len() - smart, "album", "albums"),
                            plural(smart, "smart album", "smart albums")
                        ),
                        format!(
                            "{} with comments, {}",
                            plural(c.commented_studies(), "study", "studies"),
                            plural(c.key_images(), "key image", "key images")
                        ),
                    ] {
                        ui.label(format!("•  {line}"));
                    }
                    if c.missing() > 0 {
                        ui.label(
                            RichText::new(format!(
                                "•  {} not found on this computer. Horos recorded {} outside its library (on another disk or a CD), so {} skipped unless Ruthos already has {}.",
                                plural(c.missing(), "image is", "images are"),
                                if c.missing() == 1 { "it" } else { "them" },
                                if c.missing() == 1 { "it is" } else { "they are" },
                                if c.missing() == 1 { "it" } else { "them" },
                            ))
                            .color(Color32::from_rgb(170, 90, 0)),
                        );
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new("The images").strong());
                    ui.radio_value(
                        &mut self.mode,
                        MigrateMode::Copy,
                        "Copy them into Ruthos (recommended). The Horos library is left as it is.",
                    );
                    ui.radio_value(
                        &mut self.mode,
                        MigrateMode::Link,
                        "Use them where they are. Saves space, but Ruthos then needs this folder to stay put.",
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Comments, status, key images and albums come over too. Where Ruthos already has a study, its own comment and status are kept. Running the import again only adds what is new.",
                        )
                        .size(12.0)
                        .color(Color32::from_gray(100)),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = Some(false);
                        }
                        let can = c.importable() > 0 || !c.albums.is_empty() || c.missing() > 0;
                        if ui.add_enabled(can, egui::Button::new("Import")).clicked() {
                            start = Some(());
                        }
                    });
                }
                Stage::Importing(_) => {
                    let (done, total, what) = self.progress.lock().clone();
                    let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                    let stopping = self.cancel.load(Ordering::Relaxed);
                    ui.label(if stopping {
                        "Stopping after the images already started…".to_string()
                    } else if total == 0 {
                        "Starting…".to_string()
                    } else {
                        format!("Importing {what}")
                    });
                    ui.add(egui::ProgressBar::new(frac).show_percentage());
                    ui.add_space(8.0);
                    if ui.add_enabled(!stopping, egui::Button::new("Cancel")).clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                }
                Stage::Done(s) => {
                    Self::summary(ui, s);
                    ui.add_space(10.0);
                    if ui.button("Close").clicked() {
                        close = Some(s.images_imported > 0 || s.albums_created > 0 || s.smart_albums_created > 0);
                    }
                }
                Stage::Failed(e) => {
                    ui.label(
                        RichText::new(format!("The import stopped: {e}")).color(Color32::from_rgb(170, 40, 30)),
                    );
                    ui.add_space(10.0);
                    if ui.button("Close").clicked() {
                        close = Some(true);
                    }
                }
            }
        });
        if self.auto_start && matches!(self.stage, Stage::Found(..)) {
            self.auto_start = false;
            start = Some(());
        }
        if start.is_some() {
            if let Stage::Found(c, _) = std::mem::replace(&mut self.stage, Stage::Failed(String::new())) {
                self.start(ctx, shared, *c);
            }
        }
        close
    }

    fn summary(ui: &mut egui::Ui, s: &MigrationSummary) {
        ui.label(
            RichText::new(if s.cancelled { "Stopped" } else { "Done" })
                .strong()
                .size(15.0),
        );
        let mut lines = vec![format!(
            "{} imported into {} ({} new)",
            plural(s.images_imported, "image", "images"),
            plural(s.studies, "study", "studies"),
            s.new_studies
        )];
        if s.images_present > 0 {
            lines.push(format!(
                "{} already in Ruthos",
                plural(s.images_present, "image was", "images were")
            ));
        }
        lines.push(format!(
            "{} and {} applied, {} marked",
            plural(s.comments, "comment", "comments"),
            plural(s.states, "status", "statuses"),
            plural(s.key_images, "key image", "key images")
        ));
        let mut albums = format!(
            "{} and {} created",
            plural(s.albums_created, "album", "albums"),
            plural(s.smart_albums_created, "smart album", "smart albums")
        );
        if s.albums_merged > 0 {
            albums.push_str(&format!(
                "; {} added to albums of the same name",
                plural(s.albums_merged, "album's studies were", "albums' studies were")
            ));
        }
        lines.push(albums);
        for l in lines {
            ui.label(format!("•  {l}"));
        }
        let warn = Color32::from_rgb(170, 90, 0);
        if s.images_missing > 0 {
            ui.label(
                RichText::new(format!(
                    "•  {} not found on this computer",
                    plural(s.images_missing, "image was", "images were")
                ))
                .color(warn),
            );
        }
        if s.reports_not_copied > 0 {
            ui.label(
                RichText::new(format!(
                    "•  {} a Horos report file, which is not copied; it stays in the Horos REPORTS folder",
                    plural(s.reports_not_copied, "study has", "studies have")
                ))
                .color(warn),
            );
        }
        if s.horos_private_skipped > 0 {
            ui.label(
                RichText::new(format!(
                    "•  {} Horos measurement or report {} skipped (Horos's own format)",
                    s.horos_private_skipped,
                    if s.horos_private_skipped == 1 {
                        "object was"
                    } else {
                        "objects were"
                    }
                ))
                .color(Color32::from_gray(110)),
            );
        }
        if !s.smart_albums_skipped.is_empty() {
            ui.label(RichText::new("Smart albums Ruthos cannot express yet (make them again by hand):").color(warn));
            for (name, rule) in &s.smart_albums_skipped {
                ui.label(RichText::new(format!("     {name}: {rule}")).size(12.0).monospace());
            }
        }
        if !s.errors.is_empty() {
            ui.label(
                RichText::new(format!(
                    "{} could not be imported:",
                    plural(s.errors.len(), "file", "files")
                ))
                .color(Color32::from_rgb(170, 40, 30)),
            );
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                for (f, e) in s.errors.iter().take(100) {
                    ui.label(RichText::new(format!("{}: {e}", f.display())).size(11.5));
                }
            });
        }
    }
}
