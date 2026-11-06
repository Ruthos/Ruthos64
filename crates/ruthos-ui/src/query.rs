//! The Query/Retrieve window (Horos: the Query window): search a PACS and
//! bring studies into the archive.
//!
//! One search box takes a patient name or ID; dates and modality narrow
//! it. Each result says whether the study is already in the database,
//! shows retrieval progress, and opens the study once it has arrived.
//! Retrieved images go to the incoming folder, where the importer files
//! them as it does for the listener.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui, ViewportId};
use egui_extras::{Column, TableBuilder};
use parking_lot::Mutex;
use ruthos_net::{NetError, Problem, RetrieveOutcome, RetrieveProgress, SeriesResult, StudyQuery, StudyResult};

use crate::pacs::{effective_method, me, node, problem, show_problem, Task};
use crate::shared::Shared;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateRange {
    Any,
    Today,
    Yesterday,
    Week,
    Month,
    Year,
}

impl DateRange {
    const ALL: [DateRange; 6] = [
        DateRange::Any,
        DateRange::Today,
        DateRange::Yesterday,
        DateRange::Week,
        DateRange::Month,
        DateRange::Year,
    ];

    fn label(self) -> &'static str {
        match self {
            DateRange::Any => "Any date",
            DateRange::Today => "Today",
            DateRange::Yesterday => "Yesterday",
            DateRange::Week => "Last 7 days",
            DateRange::Month => "Last 30 days",
            DateRange::Year => "Last 12 months",
        }
    }

    /// The DICOM date range, `YYYYMMDD-YYYYMMDD`.
    fn dicom(self) -> String {
        let today = chrono::Local::now().date_naive();
        let fmt = |d: chrono::NaiveDate| d.format("%Y%m%d").to_string();
        let back = |days: i64| today - chrono::Duration::days(days);
        match self {
            DateRange::Any => String::new(),
            DateRange::Today => fmt(today),
            DateRange::Yesterday => fmt(back(1)),
            DateRange::Week => format!("{}-{}", fmt(back(6)), fmt(today)),
            DateRange::Month => format!("{}-{}", fmt(back(29)), fmt(today)),
            DateRange::Year => format!("{}-{}", fmt(back(364)), fmt(today)),
        }
    }
}

const MODALITIES: [&str; 11] = ["", "US", "CR", "DX", "CT", "MR", "NM", "XA", "RF", "MG", "OT"];

/// A search in progress.
type SearchTask = Task<Result<Vec<StudyResult>, NetError>>;

struct RetrieveJob {
    progress: Arc<Mutex<RetrieveProgress>>,
    task: Task<Result<RetrieveOutcome, NetError>>,
    cancel: Arc<AtomicBool>,
}

/// The series of an expanded study.
enum SeriesState {
    Loading(Task<Result<Vec<SeriesResult>, NetError>>),
    Loaded(Vec<SeriesResult>),
    Failed(Problem),
}

/// One line of the results table: a study, or a series under it.
#[derive(Clone, Copy)]
enum Line {
    Study(usize),
    Series(usize, usize),
    Note(usize),
}

/// A node's name in the list, marked with its protocol.
fn node_label(n: &ruthos_core::settings::PacsNode) -> String {
    if n.is_web() {
        format!("{} (DICOMweb)", n.name)
    } else {
        n.name.clone()
    }
}

/// The key of a retrieval: the study, or the study and one series.
fn job_key(study: &str, series: Option<&str>) -> String {
    match series {
        Some(s) => format!("{study}|{s}"),
        None => study.to_string(),
    }
}

enum RowState {
    Retrieving,
    Done(RetrieveOutcome),
    Failed(Problem),
}

pub struct QueryState {
    pub viewport_id: ViewportId,
    pub close_requested: bool,
    node: usize,
    patient: String,
    accession: String,
    date: DateRange,
    modality: &'static str,
    search: Option<(SearchTask, Arc<AtomicBool>)>,
    /// A DICOMweb search had more results than can be shown.
    narrow: Arc<AtomicBool>,
    results: Vec<StudyResult>,
    searched: bool,
    error: Option<Problem>,
    selected: HashSet<String>,
    jobs: HashMap<String, RetrieveJob>,
    rows: HashMap<String, RowState>,
    /// Study UID to local study id, refreshed every couple of seconds.
    local: HashMap<String, i64>,
    local_checked: Option<Instant>,
    /// Studies whose series are shown.
    expanded: HashSet<String>,
    series: HashMap<String, SeriesState>,
    /// Series already in the archive, by Series Instance UID.
    local_series: HashSet<String>,
    pending_expand: Option<String>,
    dev_searched: bool,
}

impl QueryState {
    pub fn new(node: Option<usize>) -> Self {
        Self {
            viewport_id: ViewportId::from_hash_of("ruthos-query"),
            close_requested: false,
            node: node.unwrap_or(0),
            patient: String::new(),
            accession: String::new(),
            date: DateRange::Any,
            modality: "",
            search: None,
            narrow: Arc::default(),
            results: Vec::new(),
            searched: false,
            error: None,
            selected: HashSet::new(),
            jobs: HashMap::new(),
            rows: HashMap::new(),
            local: HashMap::new(),
            local_checked: None,
            expanded: HashSet::new(),
            series: HashMap::new(),
            local_series: HashSet::new(),
            pending_expand: None,
            dev_searched: false,
        }
    }

    pub fn select_node(&mut self, node: usize) {
        self.node = node;
    }

    fn start_search(&mut self, ctx: &egui::Context, shared: &Shared) {
        let settings = shared.settings.lock().clone();
        let Some(n) = settings.pacs_nodes.get(self.node).cloned() else {
            return;
        };
        let q = StudyQuery {
            patient: self.patient.trim().to_string(),
            accession: self.accession.trim().to_string(),
            study_date: self.date.dicom(),
            modality: self.modality.to_string(),
            ..StudyQuery::default()
        };
        let ae = settings.listener_ae_title.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        self.error = None;
        let narrow = self.narrow.clone();
        narrow.store(false, std::sync::atomic::Ordering::Relaxed);
        self.search = Some((
            Task::spawn(ctx, "ruthos-find", move || {
                crate::pacs::search(&n, &ae, &q, &c2, &narrow)
            }),
            cancel,
        ));
    }

    fn start_retrieve(&mut self, ctx: &egui::Context, shared: &Shared, study_uid: &str, series_uid: Option<&str>) {
        let key = job_key(study_uid, series_uid);
        if self.jobs.contains_key(&key) {
            return;
        }
        let settings = shared.settings.lock().clone();
        let Some(n) = settings.pacs_nodes.get(self.node).cloned() else {
            return;
        };
        let listener_on = shared.net_status.lock().running;
        let method = match effective_method(&n, listener_on) {
            Ok(m) => m,
            Err(p) => {
                self.rows.insert(key, RowState::Failed(p));
                return;
            }
        };
        // Routing must not send this study straight back.
        shared
            .retrieved_from
            .lock()
            .insert(study_uid.to_string(), n.name.trim().to_string());
        let progress = Arc::new(Mutex::new(RetrieveProgress::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p2, c2, ctx2) = (progress.clone(), cancel.clone(), ctx.clone());
        let target = node(&n);
        let db = shared.db.clone();
        let ae = settings.listener_ae_title.clone();
        let out = shared.paths.incoming_dir();
        let study = study_uid.to_string();
        let series = series_uid.map(String::from);
        let task = Task::spawn(ctx, "ruthos-retrieve", move || {
            let report = |p: RetrieveProgress| {
                *p2.lock() = p;
                ctx2.request_repaint();
            };
            if n.is_web() {
                return crate::pacs::web_retrieve(&n, &db, &study, series.as_deref(), &out, &report, &c2);
            }
            match &series {
                Some(se) => ruthos_net::retrieve_series(&target, &ae, &study, se, method, &out, &report, &c2),
                None => ruthos_net::retrieve_study(&target, &ae, &study, method, &out, &report, &c2),
            }
        });
        self.rows.insert(key.clone(), RowState::Retrieving);
        self.jobs.insert(key, RetrieveJob { progress, task, cancel });
    }

    /// Show or hide a study's series, asking the PACS the first time.
    fn toggle_series(&mut self, ctx: &egui::Context, shared: &Shared, study_uid: &str) {
        if !self.expanded.remove(study_uid) {
            self.expanded.insert(study_uid.to_string());
            if !matches!(self.series.get(study_uid), Some(SeriesState::Loaded(_))) {
                let settings = shared.settings.lock().clone();
                let Some(n) = settings.pacs_nodes.get(self.node).cloned() else {
                    return;
                };
                let ae = settings.listener_ae_title.clone();
                let uid = study_uid.to_string();
                let task = Task::spawn(ctx, "ruthos-find-series", move || {
                    crate::pacs::series_of(&n, &ae, &uid, &AtomicBool::new(false))
                });
                self.series.insert(study_uid.to_string(), SeriesState::Loading(task));
            }
        }
    }

    fn poll(&mut self, shared: &Shared) {
        let settings = shared.settings.lock().clone();
        let listener_on = shared.net_status.lock().running;
        let me = me(&settings, listener_on);
        let n = settings.pacs_nodes.get(self.node).cloned().unwrap_or_default();
        if let Some((task, _)) = &self.search {
            if let Some(r) = task.take() {
                self.search = None;
                self.searched = true;
                match r {
                    Ok(results) => {
                        self.selected.retain(|u| results.iter().any(|s| &s.study_uid == u));
                        self.results = results;
                        if self.narrow.load(std::sync::atomic::Ordering::Relaxed) {
                            self.error = Some(Problem {
                                summary: "Only the first 1,000 results are shown.".into(),
                                fix: "Narrow the search to see the rest.".into(),
                                technical: "stopped at the result limit".into(),
                            });
                        }
                        self.local_checked = None;
                        self.expanded.clear();
                        self.series.clear();
                        // Developer option: `RUTHOS_QUERY_EXPAND=1` shows the
                        // first study's series, for screenshots.
                        if std::env::var_os("RUTHOS_QUERY_EXPAND").is_some() {
                            self.pending_expand = self.results.first().map(|s| s.study_uid.clone());
                        }
                    }
                    Err(NetError::Cancelled) => {}
                    Err(e) => {
                        self.results.clear();
                        self.error = Some(problem(&e, &n, &me));
                    }
                }
            }
        }
        let loading: Vec<String> = self
            .series
            .iter()
            .filter(|(_, st)| matches!(st, SeriesState::Loading(_)))
            .map(|(k, _)| k.clone())
            .collect();
        for uid in loading {
            let Some(SeriesState::Loading(t)) = self.series.get(&uid) else {
                continue;
            };
            let Some(r) = t.take() else { continue };
            let state = match r {
                Ok(list) => {
                    // Many PACS leave the study's modalities and image count
                    // blank; the series say what they are.
                    if let Some(study) = self.results.iter_mut().find(|s| s.study_uid == uid) {
                        if study.modalities.trim().is_empty() {
                            let mut mods: Vec<String> = Vec::new();
                            for m in list.iter().map(|s| s.modality.clone()).filter(|m| !m.is_empty()) {
                                if !mods.contains(&m) {
                                    mods.push(m);
                                }
                            }
                            study.modalities = mods.join(" ");
                        }
                        if study.instances.is_none() {
                            let sum: u32 = list.iter().filter_map(|s| s.instances).sum();
                            if sum > 0 {
                                study.instances = Some(sum);
                            }
                        }
                    }
                    SeriesState::Loaded(list)
                }
                Err(e) => SeriesState::Failed(problem(&e, &n, &me)),
            };
            self.series.insert(uid, state);
            self.local_checked = None;
        }
        let finished: Vec<String> = self.jobs.keys().cloned().collect();
        for uid in finished {
            let Some(r) = self.jobs.get(&uid).and_then(|j| j.task.take()) else {
                continue;
            };
            self.jobs.remove(&uid);
            let state = match r {
                Ok(o) => RowState::Done(o),
                Err(NetError::NotSupported(_)) if !listener_on => RowState::Failed(Problem {
                    summary: "This PACS only sends images by C-MOVE, which needs Ruthos's listener.".into(),
                    fix: "Turn the listener on in Preferences > Listener, and make sure the PACS knows Ruthos's address and port.".into(),
                    technical: "C-GET not offered; listener off".into(),
                }),
                Err(e) => RowState::Failed(problem(&e, &n, &me)),
            };
            self.rows.insert(uid, state);
            self.local_checked = None;
        }
        // Which results are already in the archive. Retrieved images take
        // a moment to be imported, so this is refreshed while jobs run.
        let stale = self.local_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if stale {
            self.local.clear();
            for s in &self.results {
                if let Ok(Some(id)) = shared.db.study_id_for_uid(&s.study_uid) {
                    self.local.insert(s.study_uid.clone(), id);
                }
            }
            self.local_series.clear();
            for st in self.series.values() {
                if let SeriesState::Loaded(list) = st {
                    for se in list {
                        if shared.db.series_in_archive(&se.series_uid).unwrap_or(false) {
                            self.local_series.insert(se.series_uid.clone());
                        }
                    }
                }
            }
            self.local_checked = Some(Instant::now());
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.style_mut().visuals = theme::browser_visuals();
        if ui.input(|i| i.viewport().close_requested()) {
            self.close_requested = true;
        }
        self.poll(shared);
        let ctx = ui.ctx().clone();
        if let Some(uid) = self.pending_expand.take() {
            self.toggle_series(&ctx, shared, &uid);
        }
        if !self.searched
            && self.search.is_none()
            && std::env::var_os("RUTHOS_OPEN_QUERY").is_some()
            && !self.dev_searched
        {
            self.dev_searched = true;
            self.start_search(&ctx, shared);
        }
        let settings = shared.settings.lock().clone();
        if !self.jobs.is_empty() || self.search.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(246, 246, 246)).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| {
                if settings.pacs_nodes.is_empty() {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No PACS set up yet").size(18.0).strong());
                        ui.add_space(6.0);
                        ui.label("Add your PACS once in Preferences, then search it from here.");
                        ui.add_space(10.0);
                        if ui.button("Set up a PACS\u{2026}").clicked() {
                            *shared.prefs_section.lock() = Some("pacs");
                            shared.open_prefs.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    });
                    return;
                }
                if self.node >= settings.pacs_nodes.len() {
                    self.node = 0;
                }

                // Search controls.
                let mut go = false;
                ui.horizontal_wrapped(|ui| {
                    ui.label("Search");
                    egui::ComboBox::from_id_salt("query-node")
                        .selected_text(node_label(&settings.pacs_nodes[self.node]))
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for (i, n) in settings.pacs_nodes.iter().enumerate() {
                                ui.selectable_value(&mut self.node, i, node_label(n));
                            }
                        });
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.patient)
                            .desired_width(220.0)
                            .hint_text("Patient name or ID"),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.accession)
                            .desired_width(110.0)
                            .hint_text("Accession"),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    egui::ComboBox::from_id_salt("query-date")
                        .selected_text(self.date.label())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for d in DateRange::ALL {
                                ui.selectable_value(&mut self.date, d, d.label());
                            }
                        });
                    egui::ComboBox::from_id_salt("query-modality")
                        .selected_text(if self.modality.is_empty() { "All" } else { self.modality })
                        .width(60.0)
                        .show_ui(ui, |ui| {
                            for m in MODALITIES {
                                ui.selectable_value(&mut self.modality, m, if m.is_empty() { "All" } else { m });
                            }
                        });
                    if self.search.is_some() {
                        ui.spinner();
                        if ui.button("Stop").clicked() {
                            if let Some((_, c)) = &self.search {
                                c.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    } else if ui.button(RichText::new("Search").strong()).clicked() {
                        go = true;
                    }
                });
                if go && self.search.is_none() {
                    self.start_search(&ctx, shared);
                }
                ui.label(
                    RichText::new("Leave everything empty to list the PACS's recent studies. A name can be part of a name; an ID is matched as typed.")
                        .size(11.5)
                        .color(Color32::from_gray(120)),
                );
                ui.add_space(6.0);
                if let Some(p) = &self.error {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        show_problem(ui, "query-error", p);
                    });
                    ui.add_space(6.0);
                }

                // Actions on the selection.
                let n_sel = self.selected.len();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(n_sel > 0, egui::Button::new(format!("Retrieve selected ({n_sel})")))
                        .clicked()
                    {
                        let uids: Vec<String> = self.selected.iter().cloned().collect();
                        for u in uids {
                            self.start_retrieve(&ctx, shared, &u, None);
                        }
                    }
                    if self.searched {
                        ui.label(
                            RichText::new(match self.results.len() {
                                0 => "No studies matched.".to_string(),
                                1 => "1 study".to_string(),
                                n => format!("{n} studies"),
                            })
                            .color(Color32::from_gray(110)),
                        );
                    }
                });
                ui.add_space(4.0);
                self.table(ui, shared);
            });
    }

    fn table(&mut self, ui: &mut Ui, shared: &Shared) {
        let ctx = ui.ctx().clone();
        // (study, series) to retrieve, a local study to open, a job to cancel.
        let mut retrieve: Option<(String, Option<String>)> = None;
        let mut open: Option<i64> = None;
        let mut cancel: Option<String> = None;
        let mut toggle: Option<String> = None;
        let results = self.results.clone();
        let mut lines = Vec::new();
        for (i, s) in results.iter().enumerate() {
            lines.push(Line::Study(i));
            if self.expanded.contains(&s.study_uid) {
                match self.series.get(&s.study_uid) {
                    Some(SeriesState::Loaded(list)) => {
                        for j in 0..list.len() {
                            lines.push(Line::Series(i, j));
                        }
                    }
                    _ => lines.push(Line::Note(i)),
                }
            }
        }
        let red = Color32::from_rgb(190, 40, 30);
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(46.0))
            .column(Column::initial(190.0).at_least(100.0))
            .column(Column::initial(90.0))
            .column(Column::initial(84.0))
            .column(Column::initial(70.0))
            .column(Column::initial(200.0).at_least(80.0))
            .column(Column::initial(56.0))
            .column(Column::remainder().at_least(160.0))
            .header(22.0, |mut h| {
                for t in [
                    "",
                    "Patient",
                    "ID",
                    "Date",
                    "Modality",
                    "Description",
                    "Images",
                    "Status",
                ] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, lines.len(), |mut row| {
                    let line = lines[row.index()];
                    match line {
                        Line::Study(i) => {
                            let s = &results[i];
                            let uid = s.study_uid.clone();
                            row.col(|ui| {
                                let open_arrow = crate::icons::rich(
                                    if self.expanded.contains(&uid) {
                                        crate::icons::CARET_DOWN
                                    } else {
                                        crate::icons::CARET_RIGHT
                                    },
                                    13.0,
                                );
                                if ui
                                    .add(egui::Button::new(open_arrow).frame(false))
                                    .on_hover_text("Show or hide this study's series")
                                    .clicked()
                                {
                                    toggle = Some(uid.clone());
                                }
                                let mut on = self.selected.contains(&uid);
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        self.selected.insert(uid.clone());
                                    } else {
                                        self.selected.remove(&uid);
                                    }
                                }
                            });
                            row.col(|ui| {
                                ui.label(s.patient_name.replace('^', ", "));
                            });
                            row.col(|ui| {
                                ui.label(&s.patient_id);
                            });
                            row.col(|ui| {
                                ui.label(pretty_date(&s.study_date));
                            });
                            row.col(|ui| {
                                ui.label(&s.modalities);
                            });
                            row.col(|ui| {
                                ui.label(&s.description);
                            });
                            row.col(|ui| {
                                ui.label(s.instances.map(|n| n.to_string()).unwrap_or_default());
                            });
                            row.col(|ui| {
                                let local = self.local.get(&uid).copied();
                                self.status_cell(ui, &uid, local.is_some(), &mut cancel, &mut retrieve, (&uid, None));
                                if let (Some(id), false) = (local, self.jobs.contains_key(&uid)) {
                                    if ui.small_button("Open").clicked() {
                                        open = Some(id);
                                    }
                                }
                            });
                        }
                        Line::Series(i, j) => {
                            let study = &results[i];
                            let Some(SeriesState::Loaded(list)) = self.series.get(&study.study_uid) else {
                                return;
                            };
                            let se = list[j].clone();
                            let key = job_key(&study.study_uid, Some(&se.series_uid));
                            row.col(|_| {});
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "      Series {}",
                                        se.number.map(|n| n.to_string()).unwrap_or_else(|| "\u{2014}".into())
                                    ))
                                    .color(Color32::from_gray(90)),
                                );
                            });
                            row.col(|_| {});
                            row.col(|_| {});
                            row.col(|ui| {
                                ui.label(&se.modality);
                            });
                            row.col(|ui| {
                                ui.label(&se.description);
                            });
                            row.col(|ui| {
                                ui.label(se.instances.map(|n| n.to_string()).unwrap_or_default());
                            });
                            row.col(|ui| {
                                self.status_cell(
                                    ui,
                                    &key,
                                    self.local_series.contains(&se.series_uid),
                                    &mut cancel,
                                    &mut retrieve,
                                    (&study.study_uid, Some(&se.series_uid)),
                                );
                            });
                        }
                        Line::Note(i) => {
                            let study = &results[i];
                            row.col(|_| {});
                            row.col(|ui| match self.series.get(&study.study_uid) {
                                Some(SeriesState::Failed(p)) => {
                                    ui.label(RichText::new(format!("      {}", p.summary)).color(red).size(12.0))
                                        .on_hover_text(&p.fix);
                                }
                                _ => {
                                    ui.spinner();
                                    ui.label(
                                        RichText::new("Asking for the series\u{2026}").color(Color32::from_gray(110)),
                                    );
                                }
                            });
                            for _ in 0..6 {
                                row.col(|_| {});
                            }
                        }
                    }
                });
            });
        if let Some(uid) = toggle {
            self.toggle_series(&ctx, shared, &uid);
        }
        if let Some((study, series)) = retrieve {
            self.rows.remove(&job_key(&study, series.as_deref()));
            self.start_retrieve(&ctx, shared, &study, series.as_deref());
        }
        if let Some(key) = cancel {
            if let Some(j) = self.jobs.get(&key) {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Some(id) = open {
            shared.open_requests.lock().push(id);
        }
    }

    /// The status of a study or series: progress, failure with the reason,
    /// in the archive, or a Retrieve button.
    fn status_cell(
        &self,
        ui: &mut Ui,
        key: &str,
        in_archive: bool,
        cancel: &mut Option<String>,
        retrieve: &mut Option<(String, Option<String>)>,
        target: (&str, Option<&str>),
    ) {
        let want = || (target.0.to_string(), target.1.map(String::from));
        if let Some(job) = self.jobs.get(key) {
            let p = *job.progress.lock();
            let text = match p.total() {
                Some(t) if t > 0 => format!("Retrieving {} of {t}", p.completed.max(p.received)),
                _ if p.received > 0 => format!("Retrieving: {} received", p.received),
                _ => "Retrieving\u{2026}".to_string(),
            };
            ui.spinner();
            ui.label(text);
            if ui.small_button("Cancel").clicked() {
                *cancel = Some(key.to_string());
            }
            return;
        }
        match self.rows.get(key) {
            Some(RowState::Failed(p)) => {
                ui.label(RichText::new("Failed").color(Color32::from_rgb(190, 40, 30)))
                    .on_hover_text(format!("{}\n{}", p.summary, p.fix));
                if ui.small_button("Retry").clicked() {
                    *retrieve = Some(want());
                }
            }
            _ if in_archive => {
                ui.label(RichText::new("In database").color(Color32::from_rgb(30, 120, 50)));
            }
            Some(RowState::Done(o)) => {
                let n = o.progress.completed.max(o.progress.received);
                ui.label(format!("{n} images received; importing\u{2026}"));
            }
            _ => {
                if ui.small_button("Retrieve").clicked() {
                    *retrieve = Some(want());
                }
            }
        }
    }
}

fn pretty_date(d: &str) -> String {
    if d.len() == 8 {
        format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
    } else {
        d.to_string()
    }
}
