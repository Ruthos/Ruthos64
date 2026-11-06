//! Packages on screen: starting as a viewer, the page a package opens on, the
//! Share dialog that makes one, and the choice offered for a DICOMDIR.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver};
use egui::{Context, ProgressBar, RichText, Ui, ViewportCommand};

use ruthos_core::package::{self, KeyImage, Size, Spec};
use ruthos_core::portable::{self, ViewerRun};
use ruthos_core::report::ReportDocument;
use ruthos_core::{ImportEvent, ImportMode, Importer, StudyRow};
use ruthos_dicom::dicomdir::{self, DicomDir};

use crate::shared::Shared;

/// The argument that makes a run a viewer, for a Ruthos opening a disc.
const VIEW_ARGUMENT: &str = "--view";
const INI_NAME: &str = "ruthos.ini";

fn own_folder() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Decide whether this run is a viewer. Only a `ruthos.ini` beside the program
/// counts, so that nothing elsewhere on a computer can turn someone's own
/// Ruthos into one.
pub fn start_viewer(args: &mut Vec<OsString>) -> bool {
    if let Some(at) = args.iter().position(|a| a == VIEW_ARGUMENT) {
        args.remove(at);
        if at < args.len() {
            let source = PathBuf::from(args.remove(at));
            portable::enter(source);
            return true;
        }
    }
    let Some(folder) = own_folder() else { return false };
    let Ok(text) = std::fs::read_to_string(folder.join(INI_NAME)) else {
        return false;
    };
    let Some(open) = ruthos_session::deployment::ini_value(&text, "viewer", "open") else {
        return false;
    };
    // A missing or unusable name falls back to reading the folder.
    let source = dicomdir::locate(&folder, Path::new(&open)).unwrap_or(folder);
    portable::enter(source);
    true
}

/// True for a package's program that has lost its images: run from inside a
/// zip, or copied somewhere on its own. It must not start as a full Ruthos.
pub fn stranded() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let named = exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(package::PROGRAM_NAME));
    named || portable::run_from_unextracted_zip(&exe, &std::env::temp_dir())
}

pub const STRANDED_TEXT: &str = "This program opens the images in its own folder, and it can't find them.\n\n\
     If this came in a zip file, close this message, right-click the zip file and choose Extract All. \
     Then open the extracted folder and double-click Ruthos Viewer.";

fn patient_label(study: &StudyRow) -> String {
    study.patient_name.replace('^', " ").trim().to_string()
}

fn study_label(study: &StudyRow) -> String {
    let what = if study.study_description.is_empty() {
        study.modalities.replace('\\', ", ")
    } else {
        study.study_description.clone()
    };
    match &study.study_date {
        Some(date) => format!("{date}  {what}"),
        None => what,
    }
}

fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// What a viewer run shows in place of the database window.
pub struct Home {
    root: PathBuf,
    progress: Option<(usize, usize)>,
    ready: bool,
    studies: Vec<StudyRow>,
    reports: Vec<PathBuf>,
    failed: usize,
    missing: usize,
    shot_taken: bool,
}

impl Home {
    pub fn start(run: &ViewerRun, shared: &Shared) -> Self {
        let is_file = run.source.is_file();
        let root = if is_file {
            run.source.parent().map(Path::to_path_buf).unwrap_or_default()
        } else {
            run.source.clone()
        };
        let mut missing = 0;
        let mut sources = vec![root.clone()];
        if is_file {
            match dicomdir::open(&run.source) {
                Ok(dir) => {
                    let (found, gone) = dir.locate_all(&root);
                    missing = gone + dir.refused;
                    if !found.is_empty() {
                        sources = found;
                    }
                }
                Err(e) => tracing::warn!("reading the folder instead of its DICOMDIR: {e}"),
            }
        }
        let importer = Importer::new(shared.db.clone(), shared.paths.clone());
        importer.spawn(sources, ImportMode::Link, shared.import_tx.clone());
        Self {
            root,
            progress: Some((0, 0)),
            ready: false,
            studies: Vec::new(),
            reports: Vec::new(),
            failed: 0,
            missing,
            shot_taken: false,
        }
    }

    fn finish(&mut self, shared: &Shared, failed: usize) {
        self.failed = failed;
        self.progress = None;
        self.ready = true;
        if let Err(e) = package::apply_notes(&shared.db, &self.root) {
            tracing::warn!("the package's notes could not be read: {e}");
        }
        self.studies = shared.db.list_studies(&Default::default()).unwrap_or_default();
        let entries = std::fs::read_dir(&self.root).map(|d| d.flatten().map(|e| e.path()).collect());
        let mut reports: Vec<PathBuf> = entries.unwrap_or_default();
        reports.retain(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        });
        reports.sort();
        self.reports = reports;
    }

    /// Draw the page. Returns a study to open.
    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) -> Option<i64> {
        let ctx = ui.ctx().clone();
        let mut open = None;
        while let Ok(event) = shared.import_rx.try_recv() {
            match event {
                ImportEvent::Started { total } => self.progress = Some((0, total)),
                ImportEvent::Progress { done, total, .. } => self.progress = Some((done, total)),
                ImportEvent::Finished(summary) => {
                    self.finish(shared, summary.errors.len());
                    if let Some(first) = self.studies.first() {
                        ctx.send_viewport_cmd(ViewportCommand::Title(patient_label(first)));
                    }
                    // One study has nothing to choose between.
                    if let [only] = self.studies.as_slice() {
                        open = Some(only.id);
                    }
                }
            }
        }
        if self.progress.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(ui.max_rect(), 0.0, fill);
        egui::Frame::new().inner_margin(28).show(ui, |ui| {
            ui.set_max_width(640.0);
            if let Some((done, total)) = self.progress {
                ui.label(RichText::new("Getting the images ready").size(20.0).strong());
                ui.add_space(10.0);
                let share = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                ui.add(ProgressBar::new(share).text(format!("{done} of {total}")));
                return;
            }
            open = self.page(ui).or(open);
        });
        // `RUTHOS_HOME_SCREENSHOT=<file.png>` saves this page once it is ready.
        if self.ready && !self.shot_taken {
            self.shot_taken = true;
            if let Some(path) = std::env::var_os("RUTHOS_HOME_SCREENSHOT") {
                ctx.request_screenshot(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if let Some(png) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                        let _ = png.save(&path);
                    }
                });
            }
        }
        open
    }

    fn page(&self, ui: &mut Ui) -> Option<i64> {
        let mut open = None;
        match self.studies.first() {
            Some(first) => {
                ui.label(RichText::new(patient_label(first)).size(24.0).strong());
                let id = &first.patient_id;
                if !id.is_empty() {
                    ui.label(format!("ID {id}"));
                }
            }
            None => {
                ui.label(RichText::new("No images were found here").size(20.0).strong());
                ui.label("The folder may be incomplete. Ask whoever sent it for another copy.");
            }
        }
        ui.add_space(14.0);
        for study in &self.studies {
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Open").size(15.0)).clicked() {
                    open = Some(study.id);
                }
                ui.label(RichText::new(study_label(study)).size(15.0));
            });
            ui.add_space(4.0);
        }
        if !self.reports.is_empty() {
            ui.add_space(10.0);
        }
        for report in &self.reports {
            let name = report.file_stem().and_then(|n| n.to_str()).unwrap_or("Report");
            if ui.button(RichText::new(format!("Read: {name}")).size(15.0)).clicked() {
                crate::platform::open_path(report);
            }
        }
        if self.failed + self.missing > 0 {
            ui.add_space(12.0);
            let n = self.failed + self.missing;
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{n} image{} could not be read, so this may not be everything.",
                    if n == 1 { "" } else { "s" }
                ),
            );
        }
        ui.add_space(18.0);
        ui.label(
            RichText::new(
                "Nothing here changes your computer or these images. Any measurements you add go when this closes.",
            )
            .size(12.5)
            .weak(),
        );
        open
    }
}

struct Running {
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<PathBuf, String>>,
}

enum Target {
    Folder(PathBuf),
    Zip(PathBuf),
}

/// Make a package.
pub struct ShareDialog {
    choices: Vec<(StudyRow, bool)>,
    title: String,
    with_viewer: bool,
    remove_owner: bool,
    drives: Vec<PathBuf>,
    size: Size,
    has_extras: bool,
    running: Option<Running>,
    outcome: Option<Result<PathBuf, String>>,
}

fn suggested_title(study: &StudyRow) -> String {
    let name = package::name_without_owner(&study.patient_name);
    let what = study.modalities.split('\\').next().unwrap_or("");
    let date = study
        .study_date
        .as_deref()
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map(|d| d.format("%-d %b %Y").to_string())
        .unwrap_or_default();
    [name.as_str(), what, date.as_str()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn report_of(shared: &Shared, study: &StudyRow) -> Option<ReportDocument> {
    let report = crate::report::ReportState::open(shared, study.clone());
    let document = report.document(shared);
    let empty = document.findings.trim().is_empty()
        && document.conclusion.trim().is_empty()
        && document.measurements.is_empty()
        && document.images.is_empty();
    (!empty).then_some(document)
}

fn jpeg(image: &ruthos_core::report::ReportImage) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .write_image(&image.rgb, image.width, image.height, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// The notices that must travel with the program, where it was unpacked.
fn licence_files() -> Vec<PathBuf> {
    let Some(folder) = own_folder() else { return Vec::new() };
    ["LICENSE", "THIRD_PARTY_LICENSES.html", "THIRD_PARTY_LICENSES.txt"]
        .into_iter()
        .map(|n| folder.join(n))
        .filter(|p| p.is_file())
        .collect()
}

#[cfg(windows)]
fn removable_drives() -> Vec<PathBuf> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOVABLE: u32 = 2;
    ruthos_core::catalogue::volumes()
        .into_iter()
        .filter(|p| unsafe { GetDriveTypeW(&HSTRING::from(p.as_os_str())) } == DRIVE_REMOVABLE)
        .collect()
}

#[cfg(not(windows))]
fn removable_drives() -> Vec<PathBuf> {
    ruthos_core::catalogue::volumes()
}

impl ShareDialog {
    pub fn open(shared: &Shared, study_id: i64) -> Option<Self> {
        let chosen = shared.db.study(study_id).ok().flatten()?;
        let mut all = shared.db.list_studies(&Default::default()).unwrap_or_default();
        all.retain(|s| s.patient_id == chosen.patient_id && s.patient_name == chosen.patient_name);
        all.sort_by(|a, b| b.study_date.cmp(&a.study_date));
        let choices: Vec<(StudyRow, bool)> = all
            .into_iter()
            .map(|s| {
                let on = s.id == study_id;
                (s, on)
            })
            .collect();
        let mut dialog = Self {
            title: suggested_title(&chosen),
            choices,
            with_viewer: cfg!(windows),
            remove_owner: false,
            drives: removable_drives(),
            size: Size::default(),
            has_extras: false,
            running: None,
            outcome: None,
        };
        dialog.refresh(shared);
        Some(dialog)
    }

    /// Developer option: start writing to a folder without a click.
    pub fn start_in(&mut self, shared: &Shared, parent: PathBuf) {
        self.begin(shared, Target::Folder(parent));
    }

    fn chosen(&self) -> Vec<i64> {
        self.choices.iter().filter(|(_, on)| *on).map(|(s, _)| s.id).collect()
    }

    fn refresh(&mut self, shared: &Shared) {
        let chosen = self.chosen();
        self.size = package::size_of(&shared.db, &chosen).unwrap_or_default();
        self.has_extras = self
            .choices
            .iter()
            .filter(|(_, on)| *on)
            .any(|(s, _)| report_of(shared, s).is_some());
    }

    fn spec(&self, shared: &Shared) -> Spec {
        let mut spec = Spec {
            studies: self.chosen(),
            title: self.title.clone(),
            remove_owner: self.remove_owner,
            ..Spec::default()
        };
        if self.with_viewer {
            spec.program = std::env::current_exe().ok();
            spec.licences = licence_files();
        }
        let chosen: Vec<&StudyRow> = self.choices.iter().filter(|(_, on)| *on).map(|(s, _)| s).collect();
        for study in &chosen {
            let Some(mut document) = report_of(shared, study) else {
                continue;
            };
            for image in &document.images {
                if let Some(jpeg) = jpeg(image) {
                    spec.key_images.push(KeyImage {
                        caption: image.caption.clone(),
                        jpeg,
                    });
                }
            }
            if self.remove_owner {
                document.header.retain(|(label, _)| label != "Owner");
                document.title = format!("Report: {}", package::name_without_owner(&study.patient_name));
            }
            let name = match (chosen.len(), &study.study_date) {
                (1, _) | (_, None) => "Report".to_string(),
                (_, Some(date)) => format!("Report {date}"),
            };
            let taken = spec.reports.iter().any(|(n, _)| *n == name);
            let name = if taken { format!("{name} {}", study.id) } else { name };
            spec.reports.push((name, document));
        }
        spec
    }

    fn begin(&mut self, shared: &Shared, target: Target) {
        let spec = self.spec(shared);
        let db = shared.db.clone();
        let paths = shared.paths.clone();
        let done = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(self.size.files));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = bounded(1);
        let (seen, stop) = (done.clone(), cancel.clone());
        std::thread::spawn(move || {
            let progress = move |n: usize, _: usize| seen.store(n, Ordering::Relaxed);
            let outcome = match target {
                Target::Folder(parent) => package::write_folder(&db, &paths, &spec, &parent, &progress, &stop),
                Target::Zip(path) => {
                    package::write_zip(&db, &paths, &spec, &path, &progress, &stop).map(|()| path.clone())
                }
            };
            let _ = tx.send(outcome.map_err(|e| e.to_string()));
        });
        self.running = Some(Running {
            done,
            total,
            cancel,
            result,
        });
    }

    fn choose_zip(&mut self, shared: &Shared) {
        let name = format!("{}.zip", package::folder_name(&self.title));
        let picked = rfd::FileDialog::new()
            .set_title("Save the package as a zip file")
            .add_filter("Zip file", &["zip"])
            .set_file_name(name)
            .save_file();
        if let Some(path) = picked {
            self.begin(shared, Target::Zip(path));
        }
    }

    fn form(&mut self, ui: &mut Ui, shared: &Shared) -> bool {
        let mut close = false;
        ui.label(RichText::new("Share these images").size(16.0).strong());
        ui.add_space(6.0);
        let mut changed = false;
        if self.choices.len() > 1 {
            for (study, on) in &mut self.choices {
                changed |= ui.checkbox(on, study_label(study)).changed();
            }
        } else if let Some((study, _)) = self.choices.first() {
            ui.label(study_label(study));
        }
        if changed {
            self.refresh(shared);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut self.title).desired_width(320.0));
        });
        ui.add_space(6.0);
        if cfg!(windows) {
            ui.checkbox(
                &mut self.with_viewer,
                "Include a viewer, so it opens on any Windows computer",
            );
        }
        ui.checkbox(&mut self.remove_owner, "Leave out the owner's name and contact details");
        if self.remove_owner {
            ui.label(
                RichText::new("Words typed into the report, and names burned into the images, stay as they are.")
                    .size(12.5)
                    .weak(),
            );
        }
        ui.add_space(8.0);
        ui.label(format!(
            "{} images, about {}{}",
            self.size.files,
            size_label(self.size.bytes),
            if self.with_viewer {
                ", plus 45 MB for the viewer"
            } else {
                ""
            }
        ));
        if !self.has_extras {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No report or key images: someone without Windows will have nothing to look at.",
            );
        }
        if self.size.over_4gb > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "One file is over 4 GB, which won't fit on most sticks as they come.",
            );
        }
        ui.label(
            RichText::new("It isn't encrypted: anyone who has it can read it.")
                .size(12.5)
                .weak(),
        );
        ui.add_space(10.0);
        let ready = self.size.files > 0 && !self.title.trim().is_empty();
        let mut target = None;
        ui.horizontal_wrapped(|ui| {
            for drive in &self.drives {
                if ui
                    .add_enabled(ready, egui::Button::new(format!("Save to {}", drive.display())))
                    .clicked()
                {
                    target = Some(drive.clone());
                }
            }
            if ui
                .add_enabled(ready, egui::Button::new("Save to a folder\u{2026}"))
                .clicked()
            {
                target = rfd::FileDialog::new().set_title("Save the package in").pick_folder();
            }
        });
        if let Some(parent) = target {
            self.begin(shared, Target::Folder(parent));
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Save as a zip file\u{2026}"))
                .clicked()
            {
                self.choose_zip(shared);
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
        close
    }

    /// Draw the dialog. Returns true when it is finished with.
    pub fn show(&mut self, ctx: &Context, shared: &Shared) -> bool {
        if let Some(running) = &self.running {
            if let Ok(result) = running.result.try_recv() {
                self.outcome = Some(result);
                self.running = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        let mut close = false;
        egui::Modal::new(egui::Id::unique("share-modal")).show(ctx, |ui| {
            ui.set_width(480.0);
            if let Some(running) = &self.running {
                let done = running.done.load(Ordering::Relaxed);
                let total = running.total.load(Ordering::Relaxed).max(1);
                ui.label(RichText::new("Writing and checking every file").size(16.0).strong());
                ui.add_space(8.0);
                ui.add(ProgressBar::new(done as f32 / total as f32).text(format!("{done} of {total}")));
                ui.add_space(8.0);
                if ui.button("Stop").clicked() {
                    running.cancel.store(true, Ordering::Relaxed);
                }
            } else if let Some(outcome) = &self.outcome {
                match outcome {
                    Ok(path) => {
                        ui.label(RichText::new("Done, and every file checked").size(16.0).strong());
                        ui.add_space(4.0);
                        ui.label(path.display().to_string());
                        ui.label("On a stick, use Safely Remove Hardware before you unplug it.");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Show it").clicked() {
                                let shown = if path.is_dir() {
                                    path.clone()
                                } else {
                                    path.parent().map(Path::to_path_buf).unwrap_or_default()
                                };
                                crate::platform::open_path(&shown);
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    }
                    Err(error) if error == "cancelled" => close = true,
                    Err(error) => {
                        ui.label(RichText::new("That didn't work").size(16.0).strong());
                        ui.add_space(4.0);
                        ui.label(error);
                        ui.label("Nothing was left behind.");
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                }
            } else {
                close = self.form(ui, shared);
            }
        });
        close
    }
}

/// What to do with a DICOMDIR.
pub enum DiscChoice {
    Import(Vec<PathBuf>, PathBuf),
    View(PathBuf),
    Cancel,
}

/// The list of what is on a disc or stick, shown before anything is copied.
pub struct DiscDialog {
    path: PathBuf,
    root: PathBuf,
    dir: DicomDir,
    /// One flag for each study, in listing order.
    ticks: Vec<bool>,
}

impl DiscDialog {
    /// The DICOMDIR among what is being imported, if that is all there is.
    pub fn for_sources(sources: &[PathBuf]) -> Option<Self> {
        let [source] = sources else { return None };
        let path = if dicomdir::is_dicomdir(source) {
            source.clone()
        } else if source.is_dir() {
            dicomdir::find_in(source)?
        } else {
            return None;
        };
        // One that cannot be read leaves the ordinary import, which walks the folder.
        let dir = dicomdir::open(&path).ok().filter(|d| d.file_count() > 0)?;
        let studies = dir.patients.iter().map(|p| p.studies.len()).sum();
        Some(Self {
            root: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            path,
            dir,
            ticks: vec![true; studies],
        })
    }

    fn chosen_files(&self) -> Vec<PathBuf> {
        let studies = self.dir.patients.iter().flat_map(|p| &p.studies);
        studies
            .zip(&self.ticks)
            .filter(|(_, on)| **on)
            .flat_map(|(study, _)| &study.series)
            .flat_map(|series| &series.files)
            .filter_map(|file| dicomdir::locate(&self.root, &file.path))
            .collect()
    }

    pub fn show(&mut self, ctx: &Context) -> Option<DiscChoice> {
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("disc-modal")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(RichText::new("What's here").size(16.0).strong());
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                let mut tick = 0;
                for patient in &self.dir.patients {
                    let name = patient.name.replace('^', " ");
                    ui.label(RichText::new(name.trim()).strong());
                    for study in &patient.studies {
                        let images: usize = study.series.iter().map(|s| s.files.len()).sum();
                        let date = match (study.date.get(..4), study.date.get(4..6), study.date.get(6..8)) {
                            (Some(y), Some(m), Some(d)) => format!("{y}-{m}-{d}"),
                            _ => study.date.clone(),
                        };
                        let what = if study.description.is_empty() {
                            "Study"
                        } else {
                            study.description.as_str()
                        };
                        let label = format!("{date}  {what}, {images} images");
                        if let Some(on) = self.ticks.get_mut(tick) {
                            ui.checkbox(on, label);
                        }
                        tick += 1;
                    }
                    ui.add_space(4.0);
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let any = self.ticks.iter().any(|t| *t);
                if ui.add_enabled(any, egui::Button::new("Import")).clicked() {
                    choice = Some(DiscChoice::Import(self.chosen_files(), self.root.clone()));
                }
                if ui
                    .button("View without importing")
                    .on_hover_text("Opens a separate window. Nothing is added to your archive.")
                    .clicked()
                {
                    choice = Some(DiscChoice::View(self.path.clone()));
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(DiscChoice::Cancel);
                }
            });
        });
        choice
    }
}

/// Open a DICOMDIR in a separate viewer run, away from this archive.
pub fn view_separately(dicomdir: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg(VIEW_ARGUMENT)
        .arg(dicomdir)
        .spawn()
        .map(|_| ())
}
