//! The database window (Horos: BrowserController).
//!
//! Toolbar across the top, source list on the left (local database, albums,
//! listener), the study table in the middle with series as expandable rows,
//! and the series thumbnail strip along the bottom.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver};
use egui::{Align2, Color32, Context, FontId, Pos2, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};

use ruthos_core::db::StudySort;
use ruthos_core::import::ImportSummary;
use ruthos_core::{
    AlbumRow, DateFilter, ImageRow, ImportEvent, ImportMode, Importer, SeriesRow, StudyFilter, StudyRow,
};
use ruthos_dicom::Frame;
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::annotations::{self, AnnotationContext, AnnotationLevel};
use crate::anonymize::AnonymizeState;
use crate::icons;
use crate::icons::Bitmap;
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{
    menu_item, shadow_text, source_header, source_row, thumbnail_cell, toolbar_caption, toolbar_separator, truncate,
    ToolbarButton,
};

/// Paint-callback key of the preview pane; viewers use small numbers.
const PREVIEW_KEY: u64 = u64::MAX - 7;

/// The preview of the selected series at the bottom right, as in Horos.
#[derive(Default)]
struct Preview {
    image: Option<ImageRow>,
    series: Option<SeriesRow>,
    study: Option<StudyRow>,
    frame: Option<Arc<Frame>>,
    gpu: Option<Arc<GpuImage>>,
    rx: Option<Receiver<(i64, Result<Frame, String>)>>,
    requested: Option<i64>,
    error: Option<String>,
}

/// Horos's built-in smart albums: a name and the filter behind it.
#[derive(Clone)]
struct BuiltinAlbum {
    name: String,
    filter: StudyFilter,
    count: i64,
}

#[derive(Debug, Clone)]
enum RowKind {
    Study(usize),
    Series(usize, usize),
}

// A modal is one value at a time; the picture form's size does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum Modal {
    DeleteStudy(i64),
    NewAlbum {
        name: String,
    },
    RenameAlbum {
        id: i64,
        name: String,
    },
    EditComment {
        study_id: i64,
        text: String,
    },
    ImportPictures {
        files: Vec<PathBuf>,
        info: ruthos_dicom::sc::PictureInfo,
        add_to_selected: bool,
    },
}

#[derive(Debug, Clone)]
enum Action {
    Anonymize(i64),
    Select(i64),
    SelectSeries(i64, i64),
    ToggleExpand(i64),
    Open(i64, Option<i64>),
    Delete(i64),
    AddToAlbum(i64, i64),
    RemoveFromAlbum(i64, i64),
    SetState(i64, i64),
    EditComment(i64),
}

pub struct BrowserState {
    pub filter: StudyFilter,
    studies: Vec<StudyRow>,
    series_cache: HashMap<i64, Vec<SeriesRow>>,
    expanded: HashSet<i64>,
    pub selected_study: Option<i64>,
    pub selected_series: Option<i64>,
    albums: Vec<AlbumRow>,
    selected_album: Option<i64>,
    builtin_albums: Vec<BuiltinAlbum>,
    selected_builtin: Option<usize>,
    /// Studies that have report text, for the Report column.
    reports: HashSet<i64>,
    /// Source studies (by UID) that have teaching cases; shown only here.
    teaching_sources: HashSet<String>,
    anonymize: Option<Box<AnonymizeState>>,
    preview: Preview,
    modalities: Vec<String>,
    dirty: bool,
    sidebar_visible: bool,
    import_progress: Option<(usize, usize, String)>,
    last_import: Option<(ImportSummary, Instant)>,
    status: Option<(String, Instant)>,
    modal: Option<Modal>,
    open_requests: Vec<(i64, Option<i64>)>,
    counts: (i64, i64, i64),
    /// Open the first study touched by the next finished import (used when
    /// files are passed on the command line).
    pub open_next_import: bool,
}

impl BrowserState {
    pub fn new() -> Self {
        Self {
            filter: StudyFilter::default(),
            studies: Vec::new(),
            series_cache: HashMap::new(),
            expanded: HashSet::new(),
            selected_study: None,
            selected_series: None,
            albums: Vec::new(),
            selected_album: None,
            builtin_albums: Vec::new(),
            selected_builtin: None,
            reports: HashSet::new(),
            teaching_sources: HashSet::new(),
            anonymize: None,
            preview: Preview::default(),
            modalities: Vec::new(),
            dirty: true,
            sidebar_visible: true,
            import_progress: None,
            last_import: None,
            status: None,
            modal: None,
            open_requests: Vec::new(),
            counts: (0, 0, 0),
            open_next_import: false,
        }
    }

    /// Import files or folders (for example from the command line).
    pub fn import_paths(&mut self, shared: &Shared, sources: Vec<PathBuf>) {
        self.start_import(shared, sources);
    }

    /// Studies the user asked to open since the last call.
    pub fn take_open_requests(&mut self) -> Vec<(i64, Option<i64>)> {
        std::mem::take(&mut self.open_requests)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The built-in albums, recomputed with the database.
    fn builtin_albums(&self, shared: &Shared) -> Vec<BuiltinAlbum> {
        let mut list = vec![
            (
                "Cases with comments".to_string(),
                StudyFilter {
                    has_comment: true,
                    ..Default::default()
                },
            ),
            (
                "Just Added (last hour)".to_string(),
                StudyFilter {
                    added_within_secs: Some(3600),
                    ..Default::default()
                },
            ),
            (
                "Just Opened (today)".to_string(),
                StudyFilter {
                    opened_within_secs: Some(24 * 3600),
                    ..Default::default()
                },
            ),
        ];
        for m in &self.modalities {
            list.push((
                format!("Today {m}"),
                StudyFilter {
                    modality: m.clone(),
                    date: DateFilter::Today,
                    ..Default::default()
                },
            ));
        }
        list.into_iter()
            .map(|(name, filter)| {
                let count = shared.db.list_studies(&filter).map(|v| v.len() as i64).unwrap_or(0);
                BuiltinAlbum { name, filter, count }
            })
            .collect()
    }

    fn reload(&mut self, shared: &Shared) {
        let mut filter = self.filter.clone();
        filter.album_id = None;
        if let Some(b) = self.selected_builtin.and_then(|i| self.builtin_albums.get(i)) {
            let smart = &b.filter;
            filter.has_comment = smart.has_comment;
            filter.added_within_secs = smart.added_within_secs;
            filter.opened_within_secs = smart.opened_within_secs;
            if filter.modality.trim().is_empty() {
                filter.modality = smart.modality.clone();
            }
            if filter.date == DateFilter::Any {
                filter.date = smart.date;
            }
        }
        if let Some(album_id) = self.selected_album {
            if let Some(album) = self.albums.iter().find(|a| a.id == album_id) {
                if let Some(smart) = ruthos_core::Database::smart_album_filter(album) {
                    // Smart album: its own filter, narrowed further by the toolbar.
                    if filter.text.trim().is_empty() {
                        filter.text = smart.text;
                    }
                    if filter.modality.trim().is_empty() {
                        filter.modality = smart.modality;
                    }
                    if filter.date == DateFilter::Any {
                        filter.date = smart.date;
                    }
                } else {
                    filter.album_id = Some(album_id);
                }
            }
        }
        match shared.db.list_studies(&filter) {
            Ok(s) => self.studies = s,
            Err(e) => self.set_status(format!("Database error: {e}")),
        }
        self.albums = shared.db.albums().unwrap_or_default();
        self.modalities = shared.db.modalities().unwrap_or_default();
        self.counts = shared.db.counts().unwrap_or((0, 0, 0));
        self.reports = shared
            .db
            .studies_with_reports()
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.teaching_sources = shared
            .db
            .teaching_source_uids()
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.builtin_albums = self.builtin_albums(shared);
        if self.selected_builtin.is_some_and(|i| i >= self.builtin_albums.len()) {
            self.selected_builtin = None;
        }
        self.series_cache.clear();
        if let Some(sel) = self.selected_study {
            if !self.studies.iter().any(|s| s.id == sel) {
                self.selected_study = None;
                self.selected_series = None;
            }
        }
        self.dirty = false;
    }

    fn series_for(&mut self, shared: &Shared, study_id: i64) -> &Vec<SeriesRow> {
        self.series_cache
            .entry(study_id)
            .or_insert_with(|| shared.db.series_for_study(study_id).unwrap_or_default())
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    fn pump_events(&mut self, shared: &Shared) {
        while let Ok(ev) = shared.import_rx.try_recv() {
            match ev {
                ImportEvent::Started { total } => self.import_progress = Some((0, total, String::new())),
                ImportEvent::Progress { done, total, current } => self.import_progress = Some((done, total, current)),
                ImportEvent::Finished(summary) => {
                    self.import_progress = None;
                    if summary.total_files() > 0 {
                        let msg = format!(
                            "Imported {} image{}{}{}",
                            summary.imported,
                            if summary.imported == 1 { "" } else { "s" },
                            if summary.duplicates > 0 {
                                format!(", {} already in the database", summary.duplicates)
                            } else {
                                String::new()
                            },
                            if summary.errors.is_empty() {
                                String::new()
                            } else {
                                format!(", {} could not be read", summary.errors.len())
                            },
                        );
                        self.set_status(msg);
                        for (path, err) in summary.errors.iter().take(20) {
                            tracing::warn!("import error for {}: {err}", path.display());
                        }
                    }
                    for id in &summary.touched_study_ids {
                        self.series_cache.remove(id);
                        if let Ok(series) = shared.db.series_for_study(*id) {
                            for s in series {
                                shared.thumb_textures.lock().remove(&s.id);
                                shared.thumbs.invalidate(s.id);
                            }
                        }
                    }
                    if self.open_next_import {
                        if let Some(id) = summary.new_study_ids.first().or(summary.touched_study_ids.first()) {
                            self.selected_study = Some(*id);
                            self.open_requests.push((*id, None));
                            self.open_next_import = false;
                        }
                    }
                    self.last_import = Some((summary, Instant::now()));
                    self.dirty = true;
                }
            }
        }
    }

    /// Convert pictures to DICOM on a background thread, then import them.
    fn start_picture_import(&mut self, shared: &Shared, files: Vec<PathBuf>, info: ruthos_dicom::sc::PictureInfo) {
        let db = shared.db.clone();
        let paths = shared.paths.clone();
        let tx = shared.import_tx.clone();
        let out_dir = paths
            .temp_dir()
            .join(format!("pictures-{}", ruthos_dicom::sc::new_uid()));
        self.import_progress = Some((0, files.len(), "Converting pictures…".to_string()));
        std::thread::Builder::new()
            .name("ruthos-pictures".into())
            .spawn(
                move || match ruthos_dicom::sc::convert_pictures(&files, &info, &out_dir) {
                    Ok(written) => {
                        let importer = Importer::new(db, paths);
                        let handle = importer.spawn(written, ImportMode::Move, tx);
                        let _ = handle.join();
                        let _ = std::fs::remove_dir_all(&out_dir);
                    }
                    Err(e) => {
                        let mut summary = ImportSummary::default();
                        summary
                            .errors
                            .push((out_dir.clone(), format!("picture conversion failed: {e}")));
                        let _ = tx.send(ImportEvent::Finished(summary));
                        let _ = std::fs::remove_dir_all(&out_dir);
                    }
                },
            )
            .expect("spawn picture import thread");
    }

    fn start_import(&mut self, shared: &Shared, sources: Vec<PathBuf>) {
        if sources.is_empty() {
            return;
        }
        let mode = if shared.settings.lock().copy_files_on_import {
            ImportMode::Copy
        } else {
            ImportMode::Link
        };
        let importer = Importer::new(shared.db.clone(), shared.paths.clone());
        importer.spawn(sources, mode, shared.import_tx.clone());
        self.import_progress = Some((0, 0, String::new()));
    }

    // ---------------------------------------------------------------- show

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        self.pump_events(shared);
        if self.dirty {
            self.reload(shared);
        }
        ui.style_mut().visuals = theme::browser_visuals();
        let ctx = ui.ctx().clone();

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
            shared.open_prefs.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        // Files dropped onto the window are imported.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.start_import(shared, dropped);
        }

        egui::Panel::top("browser-toolbar")
            .exact_size(theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(theme::toolbar_stroke(false))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                theme::paint_toolbar_background(ui, false, egui::vec2(6.0, 3.0));
                self.toolbar(ui, shared)
            });

        egui::Panel::bottom("browser-status")
            .exact_size(24.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::symmetric(10, 3)),
            )
            .show(ui, |ui| self.status_bar(ui, shared));

        self.update_preview(shared);
        if self.selected_study.is_some() {
            egui::Panel::bottom("browser-thumbs")
                .exact_size(theme::PREVIEW_HEIGHT)
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(228, 228, 228))
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| {
                    let avail = ui.available_rect_before_wrap();
                    let preview_w = (avail.width() * 0.42).clamp(260.0, 640.0);
                    let strip =
                        egui::Rect::from_min_max(avail.min, Pos2::new(avail.right() - preview_w - 8.0, avail.bottom()));
                    let pane = egui::Rect::from_min_max(Pos2::new(avail.right() - preview_w, avail.top()), avail.max);
                    let mut strip_ui = ui.new_child(egui::UiBuilder::new().max_rect(strip));
                    self.thumbnail_strip(&mut strip_ui, shared);
                    self.preview_pane(ui, shared, pane);
                });
        }

        if self.sidebar_visible {
            egui::Panel::left("browser-sources")
                .default_size(210.0)
                .size_range(160.0..=320.0)
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(230, 232, 236))
                        .inner_margin(egui::Margin::same(8)),
                )
                .show(ui, |ui| self.source_list(ui, shared));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show(ui, |ui| self.study_table(ui, shared));

        self.show_modal(&ctx, shared);
        if let Some(mut a) = self.anonymize.take() {
            if a.show(&ctx, shared) {
                self.dirty = true;
                self.set_status("Teaching copy written");
            } else {
                self.anonymize = Some(a);
            }
        }
    }

    /// Developer aid: open the anonymiser at a step without clicking.
    pub fn dev_open_anonymize(&mut self, shared: &Shared, study_id: i64, step: &str) {
        if self.dirty {
            self.reload(shared);
        }
        if let Some(study) = self.studies.iter().find(|s| s.id == study_id).cloned() {
            let mut a = AnonymizeState::open(shared, study);
            a.dev_jump(shared, step);
            self.anonymize = Some(Box::new(a));
        }
    }

    /// Open the anonymiser for a study; a teaching copy is re-exported unchanged.
    fn start_anonymize(&mut self, shared: &Shared, study_id: i64) {
        let Some(study) = self.studies.iter().find(|s| s.id == study_id).cloned() else {
            return;
        };
        if study.teaching_reference.is_some() {
            let Some(dir) = rfd::FileDialog::new()
                .set_title("Folder for the teaching copy")
                .pick_folder()
            else {
                return;
            };
            match ruthos_core::teaching::reexport_copy(&shared.db, &shared.paths, study.id, &dir) {
                Ok(p) => self.set_status(format!("Teaching copy written to {}", p.display())),
                Err(e) => self.set_status(format!("Could not export: {e}")),
            }
            return;
        }
        self.anonymize = Some(Box::new(AnonymizeState::open(shared, study)));
    }

    fn toolbar(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            if ToolbarButton::new(icons::SIDEBAR, "Sidebar")
                .tooltip("Show or hide the source list")
                .show(ui)
                .clicked()
            {
                self.sidebar_visible = !self.sidebar_visible;
            }
            if ToolbarButton::new(Bitmap::Import, "Import")
                .tooltip("Import a folder of DICOM files (a CD, a USB stick, an export)")
                .show(ui)
                .clicked()
            {
                if let Some(paths) = rfd::FileDialog::new().set_title("Import DICOM folder").pick_folders() {
                    self.start_import(shared, paths);
                }
            }
            if ToolbarButton::new(Bitmap::ImportImages, "Import Files")
                .tooltip("Import individual DICOM files")
                .show(ui)
                .clicked()
            {
                if let Some(paths) = rfd::FileDialog::new().set_title("Import DICOM files").pick_files() {
                    self.start_import(shared, paths);
                }
            }
            if ToolbarButton::new(Bitmap::AddImages, "Pictures")
                .tooltip("Import JPEG or PNG pictures as a DICOM series, asking for the patient")
                .show(ui)
                .clicked()
            {
                if let Some(mut files) = rfd::FileDialog::new()
                    .set_title("Import pictures")
                    .add_filter("Pictures", &["jpg", "jpeg", "png", "bmp"])
                    .pick_files()
                {
                    files.sort();
                    let mut info = ruthos_dicom::sc::PictureInfo {
                        modality: "XC".to_string(),
                        max_edge: Some(4096),
                        ..Default::default()
                    };
                    if let Some(study) = self
                        .selected_study
                        .and_then(|id| self.studies.iter().find(|s| s.id == id))
                    {
                        info.patient_name = study.patient_name.clone();
                        info.patient_id = study.patient_id.clone();
                        info.birth_date = study.patient_birth_date.clone().unwrap_or_default();
                        info.sex = study.patient_sex.clone();
                        info.species = study.patient_species.clone().unwrap_or_default();
                        info.breed = study.patient_breed.clone().unwrap_or_default();
                        info.responsible_person = study.responsible_person.clone().unwrap_or_default();
                    }
                    self.modal = Some(Modal::ImportPictures {
                        files,
                        info,
                        add_to_selected: false,
                    });
                }
            }
            let has_sel = self.selected_study.is_some();
            if ToolbarButton::new(Bitmap::Export, "Export")
                .enabled(has_sel)
                .tooltip("Export the selected study as DICOM files")
                .show(ui)
                .clicked()
            {
                self.export_selected(shared);
            }
            ToolbarButton::new(Bitmap::Query, "Query")
                .enabled(false)
                .tooltip("PACS query/retrieve is next on the Tier 1 list")
                .show(ui);
            ToolbarButton::new(Bitmap::Send, "Send")
                .enabled(false)
                .tooltip("Send to PACS is next on the Tier 1 list")
                .show(ui);
            if ToolbarButton::new(Bitmap::Anonymize, "Anonymize")
                .enabled(has_sel)
                .tooltip("Make a de-identified teaching copy of the selected study")
                .show(ui)
                .clicked()
            {
                if let Some(id) = self.selected_study {
                    self.start_anonymize(shared, id);
                }
            }
            if ToolbarButton::new(Bitmap::Trash, "Delete")
                .enabled(has_sel)
                .tooltip("Delete the selected study")
                .show(ui)
                .clicked()
            {
                if let Some(id) = self.selected_study {
                    self.modal = Some(Modal::DeleteStudy(id));
                }
            }
            toolbar_separator(ui);
            if ToolbarButton::new(Bitmap::Viewer, "2D Viewer")
                .enabled(has_sel)
                .tooltip("Open the selected study")
                .show(ui)
                .clicked()
            {
                if let Some(id) = self.selected_study {
                    self.open_requests.push((id, self.selected_series));
                }
            }
            if ToolbarButton::new(Bitmap::ReportCustom, "Report")
                .enabled(has_sel)
                .tooltip("Open the report for the selected study")
                .show(ui)
                .clicked()
            {
                if let Some(id) = self.selected_study {
                    shared.open_reports.lock().push(id);
                }
            }

            // Right-aligned filter group: sort, date, modality, search. Each
            // column has a fixed width so the labels never wrap.
            const SORT_W: f32 = 118.0;
            const DATE_W: f32 = 118.0;
            const MOD_W: f32 = 84.0;
            let spacing = ui.spacing().item_spacing.x;
            // The search field takes what is left, within reason.
            let fixed_w = SORT_W + DATE_W + MOD_W + spacing * 3.0;
            let search_w = (ui.available_width() - fixed_w - 6.0).clamp(96.0, 230.0);
            let gap = ui.available_width() - fixed_w - search_w;
            if gap > 0.0 {
                ui.add_space(gap);
            }
            let mut changed = false;
            ui.vertical(|ui| {
                ui.set_width(SORT_W);
                ui.add_space(4.0);
                toolbar_caption(ui, "Sort");
                let label = match self.filter.sort {
                    StudySort::DateAddedDesc => "Date added",
                    StudySort::StudyDateDesc => "Study date",
                    StudySort::PatientName => "Patient name",
                    StudySort::PatientId => "Patient ID",
                    StudySort::Modality => "Modality",
                };
                egui::ComboBox::from_id_salt("sort-popup")
                    .selected_text(label)
                    .width(SORT_W - 4.0)
                    .show_ui(ui, |ui| {
                        for (s, l) in [
                            (StudySort::DateAddedDesc, "Date added"),
                            (StudySort::StudyDateDesc, "Study date"),
                            (StudySort::PatientName, "Patient name"),
                            (StudySort::PatientId, "Patient ID"),
                            (StudySort::Modality, "Modality"),
                        ] {
                            if ui.selectable_label(self.filter.sort == s, l).clicked() {
                                self.filter.sort = s;
                                changed = true;
                            }
                        }
                    });
            });
            ui.vertical(|ui| {
                ui.set_width(DATE_W);
                ui.add_space(4.0);
                toolbar_caption(ui, "Time Interval");
                egui::ComboBox::from_id_salt("date-filter")
                    .selected_text(self.filter.date.label())
                    .width(DATE_W - 4.0)
                    .show_ui(ui, |ui| {
                        for d in DateFilter::ALL {
                            if ui.selectable_label(self.filter.date == d, d.label()).clicked() {
                                self.filter.date = d;
                                changed = true;
                            }
                        }
                    });
            });
            ui.vertical(|ui| {
                ui.set_width(MOD_W);
                ui.add_space(4.0);
                toolbar_caption(ui, "Modality");
                let text = if self.filter.modality.is_empty() {
                    "All".to_string()
                } else {
                    self.filter.modality.clone()
                };
                egui::ComboBox::from_id_salt("modality-filter")
                    .selected_text(text)
                    .width(MOD_W - 4.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(self.filter.modality.is_empty(), "All").clicked() {
                            self.filter.modality.clear();
                            changed = true;
                        }
                        for m in &self.modalities.clone() {
                            if ui.selectable_label(&self.filter.modality == m, m).clicked() {
                                self.filter.modality = m.clone();
                                changed = true;
                            }
                        }
                    });
            });
            ui.vertical(|ui| {
                ui.set_width(search_w);
                ui.add_space(4.0);
                toolbar_caption(ui, "Search");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.filter.text)
                        .hint_text("Name, ID, description…")
                        .desired_width(search_w - 4.0),
                );
                changed |= r.changed();
            });
            if changed {
                self.dirty = true;
            }
        });
    }

    fn source_list(&mut self, ui: &mut Ui, shared: &Shared) {
        source_header(ui, "LOCAL");
        let all_selected = self.selected_album.is_none() && self.selected_builtin.is_none();
        if source_row(ui, Bitmap::Database, "Database", Some(self.counts.0), all_selected).clicked() {
            self.selected_album = None;
            self.selected_builtin = None;
            self.dirty = true;
        }
        let builtins = self.builtin_albums.clone();
        for (i, b) in builtins.iter().enumerate() {
            if source_row(
                ui,
                Bitmap::SmartAlbum,
                &b.name,
                Some(b.count),
                self.selected_builtin == Some(i),
            )
            .clicked()
            {
                self.selected_builtin = Some(i);
                self.selected_album = None;
                self.dirty = true;
            }
        }

        ui.horizontal(|ui| {
            source_header(ui, "ALBUMS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(icons::rich(icons::PLUS, 13.0)).small())
                    .on_hover_text("New album")
                    .clicked()
                {
                    self.modal = Some(Modal::NewAlbum { name: String::new() });
                }
            });
        });
        let albums = self.albums.clone();
        if albums.is_empty() {
            ui.label(RichText::new("No albums yet").size(11.5).color(Color32::from_gray(140)));
        }
        for album in &albums {
            let glyph = if album.is_smart {
                Bitmap::SmartAlbum
            } else {
                Bitmap::Album
            };
            let r = source_row(
                ui,
                glyph,
                &album.name,
                Some(album.number_of_studies),
                self.selected_album == Some(album.id),
            );
            if r.clicked() {
                self.selected_album = Some(album.id);
                self.selected_builtin = None;
                self.dirty = true;
            }
            r.context_menu(|ui| {
                if menu_item(ui, "Rename…", false).clicked() {
                    self.modal = Some(Modal::RenameAlbum {
                        id: album.id,
                        name: album.name.clone(),
                    });
                    ui.close();
                }
                if menu_item(ui, "Delete album", false).clicked() {
                    let _ = shared.db.delete_album(album.id);
                    if self.selected_album == Some(album.id) {
                        self.selected_album = None;
                    }
                    self.dirty = true;
                    ui.close();
                }
            });
        }
        if ui.small_button("Save current search as smart album").clicked() {
            let name = smart_album_name(&self.filter);
            let mut f = self.filter.clone();
            f.album_id = None;
            match shared.db.create_album(&name, Some(&f)) {
                Ok(_) => self.dirty = true,
                Err(e) => self.set_status(format!("Could not create album: {e}")),
            }
        }

        source_header(ui, "SOURCES");
        let net = shared.net_status.lock().clone();
        let (glyph, text) = if net.running {
            (icons::LISTENER_ON, format!("{} : {}", net.ae_title, net.port))
        } else {
            (icons::LISTENER_OFF, "Listener off".to_string())
        };
        let r = source_row(ui, glyph, &text, Some(net.received as i64), false);
        let mut tip = String::new();
        if net.running {
            tip.push_str(&format!(
                "DICOM listener running. AE title {}, port {}.\n",
                net.ae_title, net.port
            ));
            tip.push_str("Point your X-ray system or PACS at this computer's IP address.\n");
        } else {
            tip.push_str("The DICOM listener is not running.\n");
        }
        if let Some(p) = &net.last_peer {
            tip.push_str(&format!("Last connection from {p}\n"));
        }
        if let Some(e) = &net.last_error {
            tip.push_str(&format!("Last error: {e}"));
        }
        r.on_hover_text(tip);

        ui.add_space(8.0);
        if source_row(ui, icons::SMART_ALBUM, "Preferences…", None, false)
            .on_hover_text("Settings (Ctrl+,)")
            .clicked()
        {
            shared.open_prefs.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("{}", shared.paths.root.display()))
                .size(10.0)
                .color(Color32::from_gray(130)),
        );
        if let Some(note) = shared.renderer_note.lock().clone() {
            ui.add_space(4.0);
            ui.label(RichText::new(note).size(10.0).color(Color32::from_rgb(190, 120, 20)));
        }
    }

    fn study_table(&mut self, ui: &mut Ui, shared: &Shared) {
        // Flatten studies and expanded series into rows.
        let mut rows: Vec<RowKind> = Vec::new();
        for (i, study) in self.studies.iter().enumerate() {
            rows.push(RowKind::Study(i));
            if self.expanded.contains(&study.id) {
                let n = self.series_cache.get(&study.id).map(|s| s.len()).unwrap_or(0);
                for j in 0..n {
                    rows.push(RowKind::Series(i, j));
                }
            }
        }
        // Make sure expanded studies have their series cached.
        let expanded: Vec<i64> = self.expanded.iter().copied().collect();
        for id in expanded {
            self.series_for(shared, id);
        }
        // Recompute rows now that series are cached.
        rows.clear();
        for (i, study) in self.studies.iter().enumerate() {
            rows.push(RowKind::Study(i));
            if self.expanded.contains(&study.id) {
                let n = self.series_cache.get(&study.id).map(|s| s.len()).unwrap_or(0);
                for j in 0..n {
                    rows.push(RowKind::Series(i, j));
                }
            }
        }

        if self.studies.is_empty() {
            ui.centered_and_justified(|ui| {
                let msg = if self.counts.0 == 0 {
                    "The database is empty.\n\nClick Import, or drop DICOM files and folders onto this window.\nImages sent to the DICOM listener appear here automatically."
                } else {
                    "No studies match the current search."
                };
                ui.label(RichText::new(msg).size(14.0).color(Color32::from_gray(120)));
            });
            return;
        }

        let mut actions: Vec<Action> = Vec::new();
        let studies = &self.studies;
        let series_cache = &self.series_cache;
        let expanded = &self.expanded;
        let selected_study = self.selected_study;
        let selected_series = self.selected_series;
        let albums = &self.albums;
        let selected_album = self.selected_album;

        let reports = &self.reports;
        let teaching_sources = &self.teaching_sources;
        let ctx = ui.ctx().clone();

        let text_color = Color32::from_gray(30);
        let row_h = theme::TABLE_ROW_HEIGHT;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(20.0))
            .column(Column::initial(200.0).at_least(100.0).clip(true))
            .column(Column::exact(46.0))
            .column(Column::initial(90.0).at_least(60.0).clip(true))
            .column(Column::initial(46.0).at_least(36.0))
            .column(Column::initial(96.0).at_least(50.0).clip(true))
            .column(Column::initial(130.0).at_least(80.0).clip(true))
            .column(Column::initial(66.0).at_least(40.0).clip(true))
            .column(Column::initial(200.0).at_least(80.0).clip(true))
            .column(Column::initial(56.0).at_least(40.0))
            .column(Column::initial(125.0).at_least(80.0).clip(true))
            .column(Column::initial(150.0).at_least(60.0).clip(true))
            .column(Column::remainder().at_least(70.0).clip(true))
            .header(theme::TABLE_HEADER_HEIGHT, |mut header| {
                for title in [
                    "",
                    "Patient Name",
                    "Report",
                    "Patient ID",
                    "Age",
                    "Accession",
                    "Study Date",
                    "Modality",
                    "Description",
                    "Images",
                    "Date Added",
                    "Comments",
                    "Status",
                ] {
                    header.col(|ui| {
                        ui.label(RichText::new(title).strong().color(Color32::from_gray(80)));
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let idx = row.index();
                    match rows[idx].clone() {
                        RowKind::Study(i) => {
                            let s = &studies[i];
                            let is_sel = selected_study == Some(s.id) && selected_series.is_none();
                            row.set_selected(is_sel);
                            let is_exp = expanded.contains(&s.id);
                            row.col(|ui| {
                                let glyph = if is_exp { icons::CARET_DOWN } else { icons::CARET_RIGHT };
                                if ui
                                    .add(
                                        egui::Label::new(icons::rich(glyph, 13.0).color(Color32::from_gray(90)))
                                            .sense(Sense::click()),
                                    )
                                    .clicked()
                                {
                                    actions.push(Action::ToggleExpand(s.id));
                                }
                            });
                            let cells: [String; 12] = [
                                s.patient_name.clone(),
                                String::new(),
                                s.patient_id.clone(),
                                s.age_at_study().unwrap_or_default(),
                                s.accession_number.clone(),
                                match (&s.study_date, &s.study_time) {
                                    (Some(d), Some(t)) => format!("{d} {t}"),
                                    (Some(d), None) => d.clone(),
                                    _ => String::new(),
                                },
                                s.modalities.replace('\\', " "),
                                s.study_description.clone(),
                                s.number_of_images.to_string(),
                                unix_to_local(s.date_added),
                                s.comment.clone(),
                                state_label(s.state).to_string(),
                            ];
                            for (ci, text) in cells.iter().enumerate() {
                                if ci == 1 {
                                    let has_report = reports.contains(&s.id);
                                    row.col(|ui| {
                                        if has_report {
                                            ui.add(icons::image(&ctx, Bitmap::ReportCustom, 17.0));
                                        }
                                    });
                                    continue;
                                }
                                row.col(|ui| {
                                    let mut rt =
                                        RichText::new(text).color(if is_sel { Color32::WHITE } else { text_color });
                                    if ci == 0 {
                                        rt = rt.strong();
                                        if s.teaching_reference.is_some() {
                                            ui.label(icons::rich(icons::ANONYMIZE, 13.0).color(if is_sel {
                                                Color32::WHITE
                                            } else {
                                                Color32::from_gray(90)
                                            }))
                                            .on_hover_text(format!(
                                                "Teaching copy {}",
                                                s.teaching_reference.clone().unwrap_or_default()
                                            ));
                                        }
                                    }
                                    ui.label(rt);
                                    if ci == 0 && teaching_sources.contains(&s.study_instance_uid) {
                                        let r = ui.label(
                                            icons::rich(icons::ANONYMIZE, 11.0).color(Color32::from_rgb(190, 120, 20)),
                                        );
                                        r.on_hover_ui(|ui| {
                                            ui.label("Teaching copies exist for this study:");
                                            for c in shared
                                                .db
                                                .teaching_cases_for_study(&s.study_instance_uid)
                                                .unwrap_or_default()
                                            {
                                                ui.label(format!("{}  ({})", c.reference, c.name.replace('^', " ")));
                                            }
                                        });
                                    }
                                });
                            }
                            let r = row.response();
                            if r.clicked() {
                                actions.push(Action::Select(s.id));
                            }
                            if r.double_clicked() {
                                actions.push(Action::Open(s.id, None));
                            }
                            r.context_menu(|ui| {
                                if menu_item(ui, "Open in 2D Viewer", false).clicked() {
                                    actions.push(Action::Open(s.id, None));
                                    ui.close();
                                }
                                if menu_item(ui, if is_exp { "Collapse series" } else { "Show series" }, false)
                                    .clicked()
                                {
                                    actions.push(Action::ToggleExpand(s.id));
                                    ui.close();
                                }
                                ui.separator();
                                ui.menu_button("Add to album", |ui| {
                                    let plain: Vec<&AlbumRow> = albums.iter().filter(|a| !a.is_smart).collect();
                                    if plain.is_empty() {
                                        ui.label("No albums. Create one in the sidebar.");
                                    }
                                    for a in plain {
                                        if menu_item(ui, a.name.as_str(), false).clicked() {
                                            actions.push(Action::AddToAlbum(a.id, s.id));
                                            ui.close();
                                        }
                                    }
                                });
                                if let Some(album_id) = selected_album {
                                    if albums.iter().any(|a| a.id == album_id && !a.is_smart)
                                        && menu_item(ui, "Remove from this album", false).clicked()
                                    {
                                        actions.push(Action::RemoveFromAlbum(album_id, s.id));
                                        ui.close();
                                    }
                                }
                                ui.menu_button("Set status", |ui| {
                                    for (v, l) in [
                                        (0, "None"),
                                        (1, "Unread"),
                                        (2, "Reviewed"),
                                        (3, "Dictated"),
                                        (4, "Validated"),
                                    ] {
                                        if menu_item(ui, l, s.state == v).clicked() {
                                            actions.push(Action::SetState(s.id, v));
                                            ui.close();
                                        }
                                    }
                                });
                                if menu_item(ui, "Report…", false).clicked() {
                                    shared.open_reports.lock().push(s.id);
                                    ui.close();
                                }
                                if menu_item(ui, "Anonymise for teaching…", false).clicked() {
                                    actions.push(Action::Anonymize(s.id));
                                    ui.close();
                                }
                                if menu_item(ui, "Edit comment…", false).clicked() {
                                    actions.push(Action::EditComment(s.id));
                                    ui.close();
                                }
                                ui.separator();
                                if menu_item(
                                    ui,
                                    RichText::new("Delete study…").color(Color32::from_rgb(190, 30, 30)),
                                    false,
                                )
                                .clicked()
                                {
                                    actions.push(Action::Delete(s.id));
                                    ui.close();
                                }
                            });
                        }
                        RowKind::Series(i, j) => {
                            let study = &studies[i];
                            let Some(se) = series_cache.get(&study.id).and_then(|v| v.get(j)) else {
                                row.col(|_| {});
                                return;
                            };
                            let is_sel = selected_series == Some(se.id);
                            row.set_selected(is_sel);
                            row.col(|_| {});
                            let cells: [String; 12] = [
                                format!("    {}", se.display_name()),
                                String::new(),
                                String::new(),
                                String::new(),
                                String::new(),
                                se.series_date.clone().unwrap_or_default(),
                                se.modality.clone(),
                                {
                                    let mut d = Vec::new();
                                    if !se.body_part_examined.is_empty() {
                                        d.push(se.body_part_examined.clone());
                                    }
                                    if !se.view_position.is_empty() {
                                        d.push(se.view_position.clone());
                                    }
                                    d.join(" ")
                                },
                                se.number_of_images.to_string(),
                                unix_to_local(se.date_added),
                                se.comment.clone(),
                                if se.number_of_key_images > 0 {
                                    format!("{} key", se.number_of_key_images)
                                } else {
                                    String::new()
                                },
                            ];
                            for text in cells.iter() {
                                row.col(|ui| {
                                    ui.label(RichText::new(text).color(if is_sel {
                                        Color32::WHITE
                                    } else {
                                        Color32::from_gray(75)
                                    }));
                                });
                            }
                            let r = row.response();
                            if r.clicked() {
                                actions.push(Action::SelectSeries(study.id, se.id));
                            }
                            if r.double_clicked() {
                                actions.push(Action::Open(study.id, Some(se.id)));
                            }
                        }
                    }
                });
            });

        for action in actions {
            match action {
                Action::Anonymize(id) => self.start_anonymize(shared, id),
                Action::Select(id) => {
                    self.selected_study = Some(id);
                    self.selected_series = None;
                }
                Action::SelectSeries(study, series) => {
                    self.selected_study = Some(study);
                    self.selected_series = Some(series);
                }
                Action::ToggleExpand(id) => {
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                }
                Action::Open(id, series) => {
                    self.selected_study = Some(id);
                    self.open_requests.push((id, series));
                }
                Action::Delete(id) => self.modal = Some(Modal::DeleteStudy(id)),
                Action::AddToAlbum(album, study) => {
                    let _ = shared.db.add_study_to_album(album, study);
                    self.dirty = true;
                }
                Action::RemoveFromAlbum(album, study) => {
                    let _ = shared.db.remove_study_from_album(album, study);
                    self.dirty = true;
                }
                Action::SetState(id, state) => {
                    let _ = shared.db.set_study_state(id, state);
                    self.dirty = true;
                }
                Action::EditComment(id) => {
                    let text = self
                        .studies
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| s.comment.clone())
                        .unwrap_or_default();
                    self.modal = Some(Modal::EditComment { study_id: id, text });
                }
            }
        }
    }

    /// Keep the preview on the selected series' representative image,
    /// decoding on a thread so the window never waits.
    fn update_preview(&mut self, shared: &Shared) {
        let Some(study_id) = self.selected_study else {
            self.preview = Preview::default();
            return;
        };
        let series_id = match self.selected_series {
            Some(id) => Some(id),
            None => self.series_for(shared, study_id).first().map(|s| s.id),
        };
        let Some(series_id) = series_id else {
            self.preview = Preview::default();
            return;
        };
        let wanted = self.preview.series.as_ref().map(|s| s.id) != Some(series_id);
        if wanted {
            let series = self
                .series_for(shared, study_id)
                .iter()
                .find(|s| s.id == series_id)
                .cloned();
            let image = shared.db.representative_image(series_id).ok().flatten();
            self.preview = Preview {
                study: self.studies.iter().find(|s| s.id == study_id).cloned(),
                series,
                ..Preview::default()
            };
            if let Some(img) = image {
                let (tx, rx) = unbounded();
                let path = shared.resolve(&img.path);
                let id = img.id;
                std::thread::Builder::new()
                    .name("ruthos-preview".into())
                    .spawn(move || {
                        let result = ruthos_dicom::decode_frame(&path, 0).map_err(|e| e.to_string());
                        let _ = tx.send((id, result));
                    })
                    .ok();
                self.preview.requested = Some(id);
                self.preview.rx = Some(rx);
                self.preview.image = Some(img);
            }
        }
        if let Some(rx) = &self.preview.rx {
            if let Ok((id, result)) = rx.try_recv() {
                if Some(id) == self.preview.requested {
                    match result {
                        Ok(frame) => {
                            let frame = Arc::new(frame);
                            self.preview.gpu = Some(Arc::new(GpuImage::upload(
                                &shared.render_state.device,
                                &shared.render_state.queue,
                                &frame,
                            )));
                            self.preview.frame = Some(frame);
                        }
                        Err(e) => self.preview.error = Some(e),
                    }
                }
                self.preview.rx = None;
            }
        }
    }

    /// The preview pane: the image with Horos' corner annotations.
    fn preview_pane(&mut self, ui: &mut Ui, _shared: &Shared, rect: egui::Rect) {
        let response = ui.allocate_rect(rect, Sense::click());
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 3.0, Color32::BLACK);
        let (Some(frame), Some(gpu), Some(series), Some(study), Some(image)) = (
            self.preview.frame.clone(),
            self.preview.gpu.clone(),
            self.preview.series.clone(),
            self.preview.study.clone(),
            self.preview.image.clone(),
        ) else {
            let text = match &self.preview.error {
                Some(e) => format!("Could not decode image:\n{e}"),
                None if self.preview.requested.is_some() => "Loading\u{2026}".to_string(),
                None => "No image".to_string(),
            };
            shadow_text(
                &painter,
                rect.center(),
                Align2::CENTER_CENTER,
                &text,
                FontId::proportional(13.0),
                Color32::from_gray(160),
            );
            return;
        };
        let (c, w) = match (series.window_center, series.window_width) {
            (Some(c), Some(w)) if w > 0.0 => (c as f32, w as f32),
            _ => frame.default_window(),
        };
        let aspect = frame
            .meta
            .pixel_spacing
            .map(|[row, col]| if col > 0.0 { (row / col) as f32 } else { 1.0 })
            .unwrap_or(1.0);
        let mut view = ViewState {
            window_center: c,
            window_width: w,
            ..ViewState::default()
        };
        view.rotation_deg = series.rotation_angle as f32;
        view.flip_horizontal = series.x_flipped;
        view.flip_vertical = series.y_flipped;
        view.fit(rect.shrink(2.0), frame.width, frame.height, aspect);
        let transform = ViewTransform::new(rect, frame.width, frame.height, aspect, view);
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ImageCallback {
                key: PREVIEW_KEY,
                image: gpu,
                clut: Arc::new(ruthos_dicom::Clut::grayscale()),
                transform,
                inverted: frame.inverted,
                interpolation: Interpolation::Bilinear,
            },
        ));
        let count = series.number_of_images.max(1) as usize;
        let index = image
            .instance_number
            .map(|n| (n.max(1) - 1) as usize)
            .unwrap_or(0)
            .min(count - 1);
        let ctx = AnnotationContext {
            study: &study,
            series: &series,
            image: &image,
            frame: &frame,
            image_index: index,
            image_count: count,
            transform: &transform,
            mouse_pixel: None,
            level: AnnotationLevel::Full,
            bottom_right_inset: 0.0,
        };
        annotations::draw(&painter, rect, &ctx);
        let _ = Vec2::ZERO;
        if response.double_clicked() {
            self.open_requests.push((study.id, Some(series.id)));
        }
        response.on_hover_text("Double-click to open in the 2D viewer");
    }

    fn thumbnail_strip(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(study_id) = self.selected_study else { return };
        let series = self.series_for(shared, study_id).clone();
        let mut open: Option<i64> = None;
        let mut select: Option<i64> = None;
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for s in &series {
                        let tex = shared.thumbnail(s.id);
                        let sub = format!("{} · {}", s.modality, s.count_label());
                        let r = thumbnail_cell(
                            ui,
                            tex.as_ref(),
                            &s.display_name(),
                            &sub,
                            self.selected_series == Some(s.id),
                            theme::THUMB_SIZE,
                        );
                        if r.clicked() {
                            select = Some(s.id);
                        }
                        if r.double_clicked() {
                            open = Some(s.id);
                        }
                    }
                });
            });
        if let Some(id) = select {
            self.selected_series = Some(id);
        }
        if let Some(id) = open {
            self.open_requests.push((study_id, Some(id)));
        }
    }

    fn status_bar(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.horizontal(|ui| {
            let (st, se, im) = self.counts;
            ui.label(
                RichText::new(format!("{st} studies · {se} series · {im} images"))
                    .size(11.5)
                    .color(Color32::from_gray(80)),
            );
            if let Some((done, total, current)) = &self.import_progress {
                ui.separator();
                let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(180.0)
                        .text(format!("Importing {done}/{total}")),
                );
                ui.label(
                    RichText::new(truncate(current, 40))
                        .size(11.0)
                        .color(Color32::from_gray(110)),
                );
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            } else if let Some((text, when)) = &self.status {
                if when.elapsed().as_secs() < 8 {
                    ui.separator();
                    ui.label(RichText::new(text).size(11.5).color(Color32::from_gray(60)));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let net = shared.net_status.lock().clone();
                let text = if net.running {
                    format!("Listener {}:{}", net.ae_title, net.port)
                } else {
                    "Listener off".to_string()
                };
                ui.label(RichText::new(text).size(11.0).color(Color32::from_gray(110)));
            });
        });
    }

    fn export_selected(&mut self, shared: &Shared) {
        let Some(study_id) = self.selected_study else { return };
        let Some(dir) = rfd::FileDialog::new().set_title("Export study to folder").pick_folder() else {
            return;
        };
        let series = shared.db.series_for_study(study_id).unwrap_or_default();
        let mut n = 0usize;
        for s in series {
            let sub = dir.join(sanitize(&s.display_name()));
            let _ = std::fs::create_dir_all(&sub);
            for img in shared.db.images_for_series(s.id).unwrap_or_default() {
                let src = shared.resolve(&img.path);
                let name = format!("{:04}.dcm", img.instance_number.unwrap_or(n as i64 + 1));
                if std::fs::copy(&src, sub.join(name)).is_ok() {
                    n += 1;
                }
            }
        }
        self.set_status(format!("Exported {n} files to {}", dir.display()));
    }

    fn show_modal(&mut self, ctx: &Context, shared: &Shared) {
        let Some(modal) = self.modal.clone() else { return };
        let mut close = false;
        let mut next = modal.clone();
        egui::Modal::new(egui::Id::unique("browser-modal")).show(ctx, |ui| {
            ui.set_min_width(360.0);
            match &mut next {
                Modal::DeleteStudy(id) => {
                    let name = self.studies.iter().find(|s| s.id == *id).map(|s| s.patient_name.clone()).unwrap_or_default();
                    ui.heading("Delete study?");
                    ui.label(format!("\"{name}\" and all its images will be removed from the database. Files stored inside the Ruthos data folder are deleted; linked files are left where they are."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button(RichText::new("Delete").color(Color32::from_rgb(190, 30, 30))).clicked() {
                            match shared.db.delete_study(*id) {
                                Ok(files) => {
                                    for f in files {
                                        let _ = std::fs::remove_file(f);
                                    }
                                    self.set_status(format!("Deleted {name}"));
                                }
                                Err(e) => self.set_status(format!("Delete failed: {e}")),
                            }
                            if self.selected_study == Some(*id) {
                                self.selected_study = None;
                                self.selected_series = None;
                            }
                            self.dirty = true;
                            close = true;
                        }
                    });
                }
                Modal::NewAlbum { name } => {
                    ui.heading("New album");
                    let r = ui.text_edit_singleline(name);
                    r.request_focus();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        let ok = ui.button("Create").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if ok && !name.trim().is_empty() {
                            let _ = shared.db.create_album(name.trim(), None);
                            self.dirty = true;
                            close = true;
                        }
                    });
                }
                Modal::RenameAlbum { id, name } => {
                    ui.heading("Rename album");
                    ui.text_edit_singleline(name);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Rename").clicked() && !name.trim().is_empty() {
                            let _ = shared.db.rename_album(*id, name.trim());
                            self.dirty = true;
                            close = true;
                        }
                    });
                }
                Modal::ImportPictures { files, info, add_to_selected } => {
                    ui.set_min_width(460.0);
                    ui.heading(format!(
                        "Import {} picture{}",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" }
                    ));
                    ui.label(
                        RichText::new("The pictures become a DICOM series in the archive. Fill in the patient.")
                            .size(12.0)
                            .color(Color32::from_gray(90)),
                    );
                    ui.add_space(6.0);
                    let selected = self.selected_study.and_then(|id| self.studies.iter().find(|s| s.id == id)).cloned();
                    if let Some(study) = &selected {
                        ui.checkbox(
                            add_to_selected,
                            format!("Add to the selected study ({}, {})", study.patient_name, study.study_date.clone().unwrap_or_default()),
                        );
                        ui.add_space(4.0);
                    }
                    let locked = *add_to_selected && selected.is_some();
                    egui::Grid::new("picture-form").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        let field = |ui: &mut Ui, label: &str, value: &mut String, hint: &str, enabled: bool| {
                            ui.label(label);
                            ui.add_enabled(enabled, egui::TextEdit::singleline(value).hint_text(hint).desired_width(300.0));
                            ui.end_row();
                        };
                        field(ui, "Patient name", &mut info.patient_name, "Smith, Rex", !locked);
                        field(ui, "Patient ID", &mut info.patient_id, "Practice record number", !locked);
                        field(ui, "Species", &mut info.species, "Canine, Feline, Equine…", !locked);
                        field(ui, "Breed", &mut info.breed, "", !locked);
                        ui.label("Sex");
                        ui.add_enabled_ui(!locked, |ui| {
                            ui.horizontal(|ui| {
                                for (v, l) in [("M", "Male"), ("F", "Female"), ("O", "Other"), ("", "Unknown")] {
                                    ui.selectable_value(&mut info.sex, v.to_string(), l);
                                }
                                let mut neutered = info.neutered.unwrap_or(false);
                                if ui.checkbox(&mut neutered, "Neutered").changed() {
                                    info.neutered = Some(neutered);
                                }
                            });
                        });
                        ui.end_row();
                        field(ui, "Date of birth", &mut info.birth_date, "YYYY-MM-DD", !locked);
                        field(ui, "Owner", &mut info.responsible_person, "Responsible person", !locked);
                        field(ui, "Study description", &mut info.study_description, "e.g. Dermatology photos", !locked);
                        field(ui, "Series description", &mut info.series_description, "Pictures", true);
                        field(ui, "Study date", &mut info.study_date, "Today", !locked);
                        ui.label("Modality");
                        ui.horizontal(|ui| {
                            for (v, l) in [("XC", "Photo (XC)"), ("ES", "Endoscopy (ES)"), ("GM", "Microscopy (GM)"), ("OT", "Other (OT)")] {
                                ui.selectable_value(&mut info.modality, v.to_string(), l);
                            }
                        });
                        ui.end_row();
                        ui.label("Size limit");
                        ui.horizontal(|ui| {
                            let mut cap = info.max_edge.is_some();
                            if ui.checkbox(&mut cap, "Scale pictures down to 4096 px on the long edge").changed() {
                                info.max_edge = if cap { Some(4096) } else { None };
                            }
                        });
                        ui.end_row();
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        let ready = !info.patient_name.trim().is_empty() || locked;
                        if ui.add_enabled(ready, egui::Button::new("Import")).clicked() {
                            let mut info = info.clone();
                            if locked {
                                if let Some(study) = &selected {
                                    info.existing_study_uid = Some(study.study_instance_uid.clone());
                                    info.study_date = study.study_date.clone().unwrap_or_default();
                                    info.study_description = study.study_description.clone();
                                }
                            }
                            self.open_next_import = true;
                            self.start_picture_import(shared, files.clone(), info);
                            close = true;
                        }
                    });
                }
                Modal::EditComment { study_id, text } => {
                    ui.heading("Comment");
                    ui.add(egui::TextEdit::multiline(text).desired_rows(3).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Save").clicked() {
                            let _ = shared.db.set_study_comment(*study_id, text.trim());
                            self.dirty = true;
                            close = true;
                        }
                    });
                }
            }
        });
        if close {
            self.modal = None;
        } else {
            self.modal = Some(next);
        }
    }
}

impl Default for BrowserState {
    fn default() -> Self {
        Self::new()
    }
}

fn state_label(state: i64) -> &'static str {
    match state {
        1 => "Unread",
        2 => "Reviewed",
        3 => "Dictated",
        4 => "Validated",
        _ => "",
    }
}

fn unix_to_local(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim().to_string();
    if out.is_empty() {
        "Series".to_string()
    } else {
        out
    }
}

fn smart_album_name(f: &StudyFilter) -> String {
    let mut parts = Vec::new();
    if !f.text.trim().is_empty() {
        parts.push(format!("\"{}\"", f.text.trim()));
    }
    if !f.modality.trim().is_empty() {
        parts.push(f.modality.trim().to_string());
    }
    if f.date != DateFilter::Any {
        parts.push(f.date.label().to_string());
    }
    if parts.is_empty() {
        "All studies".to_string()
    } else {
        parts.join(" · ")
    }
}
