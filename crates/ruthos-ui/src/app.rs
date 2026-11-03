//! The eframe application: owns the database window (root viewport), the
//! viewer windows (deferred viewports), the DICOM listener and the incoming
//! folder scanner.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use egui::{ViewportBuilder, ViewportCommand, ViewportId};
use parking_lot::Mutex;

use ruthos_core::incoming::IncomingScanner;
use ruthos_core::{DataPaths, Database, Settings};
use ruthos_net::{ListenerConfig, NetEvent, StoreScp};
use ruthos_render::ImageRenderer;

use crate::browser::BrowserState;
use crate::prefs::PrefsState;
use crate::query::QueryState;
use crate::report::ReportState;
use crate::shared::Shared;
use crate::theme;
use crate::viewer::ViewerState;

pub struct RuthosApp {
    shared: Arc<Shared>,
    browser: BrowserState,
    viewers: Vec<Arc<Mutex<ViewerState>>>,
    mprs: Vec<Arc<Mutex<crate::mpr::MprState>>>,
    reports: Vec<Arc<Mutex<ReportState>>>,
    scp: Option<StoreScp>,
    incoming_stop: Arc<AtomicBool>,
    startup_error: Option<String>,
    /// Viewer window to bring to the front on the next frame.
    pending_focus: Option<ViewportId>,
    /// Study to open on the first frame (developer option `RUTHOS_OPEN_STUDY`).
    startup_open: Option<i64>,
    /// Internal errors waiting to be acknowledged in a dialog.
    error_dialog: Vec<String>,
    prefs: Option<PrefsState>,
    query: Option<QueryState>,
    router: crate::routing::Router,
}

impl RuthosApp {
    /// `startup_paths` are files or folders given on the command line; they
    /// are imported and the first resulting study is opened in a viewer.
    pub fn new(cc: &eframe::CreationContext<'_>, startup_paths: Vec<std::path::PathBuf>) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Ruthos needs the wgpu renderer"))?;

        crate::icons::install(&cc.egui_ctx);
        theme::apply_global_style(&cc.egui_ctx);
        crate::platform::install_native();
        // Developer option: `RUTHOS_ZOOM=0.8` scales the whole interface,
        // for screenshots on small screens.
        if let Some(z) = std::env::var("RUTHOS_ZOOM")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|z| (0.3..=3.0).contains(z))
        {
            cc.egui_ctx.set_zoom_factor(z);
        }

        let settings = Settings::load();
        let paths = DataPaths::new(settings.data_root());
        paths.ensure()?;
        let db = Database::open(&paths)?;
        tracing::info!("database at {}", paths.database_file().display());
        // Measurements named before definitions existed get their key.
        match db.assign_def_keys_by_label(&ruthos_core::measure::label_keys(&settings.library())) {
            Ok(0) => {}
            Ok(n) => tracing::info!("{n} stored measurements linked to their definitions"),
            Err(e) => tracing::warn!("could not link stored measurements to definitions: {e}"),
        }
        crate::crash::install(paths.root.join("crash.log"));
        // A teaching export interrupted by a crash leaves its temporary
        // folder behind; nothing can be in progress at startup, so clear them.
        if let Ok(entries) = std::fs::read_dir(paths.temp_dir()) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("export-") && e.path().is_dir() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }

        // Register the image pipeline with egui-wgpu once.
        {
            let mut renderer = render_state.renderer.write();
            renderer
                .callback_resources
                .insert(ImageRenderer::new(&render_state.device, render_state.target_format));
        }

        let shared = Shared::new(db.clone(), paths.clone(), settings.clone(), render_state);
        {
            let info = shared.render_state.adapter.get_info();
            tracing::info!(
                "renderer: {} via {:?} ({:?}), max texture {}",
                info.name,
                info.backend,
                info.device_type,
                shared.render_state.device.limits().max_texture_dimension_2d
            );
            if info.device_type == egui_wgpu::wgpu::DeviceType::Cpu {
                *shared.renderer_note.lock() = Some(format!("Software rendering ({}): no GPU found", info.name));
            }
        }

        // DICOM listener writes to the incoming folder; the scanner imports.
        let mut startup_error = None;
        let scp = start_listener(&shared, &settings);
        if settings.listener_enabled && scp.is_none() {
            startup_error = shared.net_status.lock().last_error.clone();
        }

        let incoming_stop = Arc::new(AtomicBool::new(false));
        *shared.watch.folders.lock() = settings.watch_folders.clone();
        ruthos_core::watch::WatchScanner::spawn(
            db.clone(),
            paths.clone(),
            shared.watch.clone(),
            Duration::from_secs(5),
            shared.import_tx.clone(),
            incoming_stop.clone(),
        );
        IncomingScanner::spawn(
            db,
            paths,
            Duration::from_secs(settings.incoming_scan_seconds.max(1)),
            shared.import_tx.clone(),
            incoming_stop.clone(),
        );

        // Developer option: open a study's report at startup for screenshots.
        if let Some(v) = std::env::var("RUTHOS_OPEN_REPORT").ok().filter(|v| !v.is_empty()) {
            let id = v.trim().parse::<i64>().ok().or_else(|| {
                let filter = ruthos_core::StudyFilter {
                    text: v.clone(),
                    ..Default::default()
                };
                shared.db.list_studies(&filter).ok()?.first().map(|s| s.id)
            });
            if let Some(id) = id {
                shared.open_reports.lock().push(id);
            }
        }

        // Developer option: open Preferences at startup for screenshots.
        // Developer option: `RUTHOS_OPEN_QUERY=<PACS index>` opens the
        // Query/Retrieve window on that PACS and searches straight away.
        if let Some(i) = std::env::var("RUTHOS_OPEN_QUERY")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            *shared.open_query.lock() = Some(Some(i));
        }
        if std::env::var_os("RUTHOS_OPEN_PREFS").is_some() {
            shared.open_prefs.store(true, Ordering::Relaxed);
        }

        let mut browser = BrowserState::new();
        // Developer option: select a study in the database window at startup.
        if let Some(v) = std::env::var("RUTHOS_SELECT_STUDY").ok().filter(|v| !v.is_empty()) {
            let id = v.trim().parse::<i64>().ok().or_else(|| {
                let filter = ruthos_core::StudyFilter {
                    text: v.clone(),
                    ..Default::default()
                };
                shared.db.list_studies(&filter).ok()?.first().map(|s| s.id)
            });
            browser.selected_study = id;
        }

        // Developer option: `RUTHOS_OPEN_MPR=<study>:<series>` opens an MPR window.
        if let Some((st, se)) = std::env::var("RUTHOS_OPEN_MPR").ok().and_then(|v| {
            v.split_once(':')
                .map(|(a, b)| (a.trim().parse::<i64>(), b.trim().parse::<i64>()))
        }) {
            if let (Ok(st), Ok(se)) = (st, se) {
                shared.open_mpr.lock().push((st, se));
            }
        }

        // Developer option: `RUTHOS_IMPORT_HOROS=<folder>` opens the Horos import on that folder.
        if let Some(v) = std::env::var("RUTHOS_IMPORT_HOROS").ok().filter(|v| !v.is_empty()) {
            browser.request_horos_import(Some(std::path::PathBuf::from(v)));
        }

        // Developer option: `RUTHOS_OPEN_ANON=<study>[:pixels|verify]` opens the anonymiser.
        if let Some(v) = std::env::var("RUTHOS_OPEN_ANON").ok().filter(|v| !v.is_empty()) {
            let (who, step) = v
                .split_once(':')
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .unwrap_or((v.clone(), String::new()));
            let id = who.trim().parse::<i64>().ok().or_else(|| {
                let filter = ruthos_core::StudyFilter {
                    text: who.clone(),
                    ..Default::default()
                };
                shared.db.list_studies(&filter).ok()?.first().map(|s| s.id)
            });
            if let Some(id) = id {
                browser.dev_open_anonymize(&shared, id, &step);
            }
        }
        if !startup_paths.is_empty() {
            browser.open_next_import = true;
            browser.import_paths(&shared, startup_paths);
        }

        // Developer option: open a study at startup by id or by name/ID text,
        // so a viewer can be screenshotted without clicking.
        let startup_open = std::env::var("RUTHOS_OPEN_STUDY").ok().and_then(|v| {
            if let Ok(id) = v.trim().parse::<i64>() {
                return Some(id);
            }
            let filter = ruthos_core::StudyFilter {
                text: v,
                ..Default::default()
            };
            shared.db.list_studies(&filter).ok()?.first().map(|s| s.id)
        });

        Ok(Self {
            shared,
            browser,
            viewers: Vec::new(),
            mprs: Vec::new(),
            reports: Vec::new(),
            scp,
            incoming_stop,
            startup_error,
            pending_focus: None,
            startup_open,
            error_dialog: Vec::new(),
            prefs: None,
            query: None,
            router: Default::default(),
        })
    }

    fn open_viewer(&mut self, study_id: i64, series_id: Option<i64>) {
        tracing::info!(
            "open viewer: study {study_id}, series {series_id:?}, {} open",
            self.viewers.len()
        );
        // Reuse an existing window for this study: switch series and focus it.
        if let Some(existing) = self.viewers.iter().find(|v| v.lock().study.id == study_id) {
            let mut v = existing.lock();
            if let Some(sid) = series_id {
                if let Some(idx) = v.series.iter().position(|s| s.id == sid) {
                    let shared = self.shared.clone();
                    v.select_series(&shared, idx);
                }
            }
            let vid = v.viewport_id;
            drop(v);
            // Focus is sent during update via the viewport command.
            self.pending_focus = Some(vid);
            return;
        }
        let Ok(Some(study)) = self.shared.db.study(study_id) else {
            return;
        };
        match ViewerState::open(&self.shared, study, series_id) {
            Ok(v) => self.viewers.push(Arc::new(Mutex::new(v))),
            Err(e) => tracing::warn!("could not open viewer: {e}"),
        }
    }

    /// An action from the menu bar (native on macOS) by its item id.
    fn run_app_action(&mut self, id: &str) {
        use crate::platform::AppAction;
        match crate::platform::action_for(id) {
            Some(AppAction::Preferences) => self.shared.open_prefs.store(true, Ordering::Relaxed),
            Some(AppAction::ImportFolder) => self.browser.import_folder_dialog(&self.shared),
            Some(AppAction::ImportFiles) => self.browser.import_files_dialog(&self.shared),
            Some(AppAction::ImportHoros) => self.browser.request_horos_import(None),
            Some(AppAction::DatabaseWindow) => self.shared.focus_browser.store(true, Ordering::Relaxed),
            Some(AppAction::Url(url)) => crate::platform::open_url(url),
            Some(AppAction::Viewer(item)) => {
                let Some(action) = ViewerState::menu_from_id(item) else {
                    return;
                };
                let focused = self.shared.focused_viewer.load(Ordering::Relaxed);
                let target = self
                    .viewers
                    .iter()
                    .find(|v| v.lock().id == focused)
                    .or_else(|| self.viewers.last())
                    .cloned();
                match target {
                    Some(v) => v.lock().request_menu(action),
                    None => tracing::info!("menu item {id} needs an open viewer"),
                }
            }
            None => tracing::warn!("unknown menu item {id}"),
        }
    }

    /// The Query/Retrieve window, opened from the toolbar or a PACS in the
    /// source list.
    fn query_window(&mut self, ctx: &egui::Context) {
        let id = ViewportId::from_hash_of("ruthos-query");
        if let Some(node) = self.shared.open_query.lock().take() {
            match self.query.as_mut() {
                Some(q) => {
                    if let Some(n) = node {
                        q.select_node(n);
                    }
                }
                None => self.query = Some(QueryState::new(node)),
            }
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
        }
        let Some(query) = self.query.as_mut() else { return };
        let shared = self.shared.clone();
        ctx.show_viewport_immediate(
            id,
            ViewportBuilder::default()
                .with_title("Query/Retrieve \u{2014} Ruthos")
                .with_inner_size([1040.0, 640.0])
                .with_min_inner_size([720.0, 420.0]),
            |ctx, _class| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    if let Err(msg) = crate::crash::guarded(|| query.show(ui, &shared)) {
                        shared.errors.lock().push(format!(
                            "The Query/Retrieve window was closed after an internal error: {msg}"
                        ));
                        query.close_requested = true;
                    }
                });
            },
        );
        if query.close_requested {
            self.query = None;
        }
    }

    /// The Preferences window: opened on request, closed on Cancel, Save or
    /// the window's close button. Saved settings are applied live where
    /// they can be.
    fn preferences(&mut self, ctx: &egui::Context) {
        let id = ViewportId::from_hash_of("ruthos-prefs");
        if self.shared.open_prefs.swap(false, Ordering::Relaxed) {
            if self.prefs.is_none() {
                self.prefs = Some(PrefsState::new(&self.shared));
            }
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
        }
        let Some(prefs) = self.prefs.as_mut() else { return };
        let shared = self.shared.clone();
        let mut close = false;
        let mut saved = None;
        ctx.show_viewport_immediate(
            id,
            ViewportBuilder::default()
                .with_title("Ruthos Preferences")
                // Wide enough for the seven section buttons.
                .with_inner_size([720.0, 540.0])
                .with_min_inner_size([680.0, 420.0])
                .with_resizable(true),
            |ctx, _class| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    if let Err(msg) = crate::crash::guarded(|| prefs.show(ui, &shared)) {
                        shared.errors.lock().push(format!(
                            "The Preferences window was closed after an internal error: {msg}"
                        ));
                        prefs.close_requested = true;
                    }
                });
                close = prefs.close_requested;
                saved = prefs.saved.take();
            },
        );
        if let Some(settings) = saved {
            self.apply_settings(&settings);
        }
        if close {
            self.prefs = None;
        }
    }

    /// Apply saved preferences to the running application.
    fn apply_settings(&mut self, settings: &ruthos_core::Settings) {
        let (running_port, running_ae) = {
            let st = self.shared.net_status.lock();
            (st.port, st.ae_title.clone())
        };
        let wanted = (
            settings.listener_enabled,
            settings.listener_port,
            settings.listener_ae_title.clone(),
        );
        let current = (self.scp.is_some(), running_port, running_ae);
        if wanted != current {
            if let Some(mut scp) = self.scp.take() {
                scp.stop();
                self.shared.net_status.lock().running = false;
            }
            self.scp = start_listener(&self.shared, settings);
            if settings.listener_enabled && self.scp.is_none() {
                let last_error = self.shared.net_status.lock().last_error.clone();
                if let Some(msg) = last_error {
                    self.report_error(msg);
                }
            }
        }
        self.browser.mark_dirty();
    }

    /// Queue an internal error for the dialog, without repeating the same
    /// message every frame.
    fn report_error(&mut self, msg: String) {
        if self.error_dialog.last() != Some(&msg) {
            self.error_dialog.push(msg);
        }
    }

    /// The "something went wrong" dialog in the database window.
    fn error_dialog_ui(&mut self, ctx: &egui::Context) {
        if self.error_dialog.is_empty() {
            return;
        }
        let mut dismiss = false;
        egui::Modal::new(egui::Id::unique("ruthos-error-dialog")).show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.set_max_width(560.0);
            ui.heading("Ruthos hit an internal error");
            ui.add_space(6.0);
            for msg in &self.error_dialog {
                ui.label(msg.as_str());
                ui.add_space(4.0);
            }
            if let Some(path) = crate::crash::log_path() {
                ui.label(
                    egui::RichText::new(format!("Details and a backtrace were written to {}", path.display()))
                        .size(12.0)
                        .color(egui::Color32::from_gray(120)),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Continue").clicked() {
                    dismiss = true;
                }
                if ui.button("Quit Ruthos").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
        });
        if dismiss {
            self.error_dialog.clear();
        }
    }

    fn open_report(&mut self, study_id: i64) {
        if let Some(existing) = self.reports.iter().find(|r| r.lock().study.id == study_id) {
            self.pending_focus = Some(existing.lock().viewport_id);
            return;
        }
        let Ok(Some(study)) = self.shared.db.study(study_id) else {
            return;
        };
        let report = ReportState::open(&self.shared, study);
        self.reports.push(Arc::new(Mutex::new(report)));
    }

    fn pump_net(&mut self) {
        let Some(scp) = &self.scp else { return };
        let mut status = self.shared.net_status.lock();
        while let Ok(ev) = scp.events.try_recv() {
            match ev {
                NetEvent::Started { .. } => status.running = true,
                NetEvent::Association { peer_ae, peer_addr } => {
                    status.last_peer = Some(format!("{peer_ae} ({peer_addr})"));
                }
                NetEvent::Stored { .. } => status.received += 1,
                NetEvent::Echo { peer_ae } => status.last_peer = Some(format!("{peer_ae} (echo)")),
                NetEvent::Released { .. } => {}
                NetEvent::Error(e) => status.last_error = Some(e),
                NetEvent::Stopped => status.running = false,
            }
        }
    }
}

impl eframe::App for RuthosApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(theme::browser_visuals());
        self.shared.pump_thumbnails(&ctx);
        self.pump_net();

        if let Some(err) = self.startup_error.take() {
            self.browser.mark_dirty();
            tracing::warn!("{err}");
        }

        if let Err(msg) = crate::crash::guarded(|| self.browser.show(ui, &self.shared)) {
            self.report_error(format!("The database window hit an internal error: {msg}"));
        }

        // Files the system handed to the running application (Finder's
        // "Open with", a drop on the Dock icon) are imported and opened.
        let handed = crate::platform::take_open_files();
        if !handed.is_empty() {
            self.browser.open_next_import = true;
            self.browser.import_paths(&self.shared, handed);
        }
        for id in crate::platform::poll_native_menu() {
            self.run_app_action(&id);
        }

        if let Some(id) = self.startup_open.take() {
            self.browser.selected_study = Some(id);
            // `RUTHOS_OPEN_SERIES=<series id>` picks the series to open on.
            let series = std::env::var("RUTHOS_OPEN_SERIES")
                .ok()
                .and_then(|v| v.trim().parse::<i64>().ok());
            self.open_viewer(id, series);
        }
        for (study, series) in self.browser.take_open_requests() {
            self.open_viewer(study, series);
        }
        let viewer_requests: Vec<i64> = std::mem::take(&mut *self.shared.open_requests.lock());
        for study in viewer_requests {
            self.open_viewer(study, None);
        }
        // MPR windows asked for by a viewer (or the developer option).
        let mpr_requests: Vec<(i64, i64)> = std::mem::take(&mut *self.shared.open_mpr.lock());
        for (study_id, series_id) in mpr_requests {
            if let Some(existing) = self.mprs.iter().find(|m| m.lock().series_id == series_id) {
                self.pending_focus = Some(existing.lock().viewport_id);
                continue;
            }
            let study = self.shared.db.study(study_id).ok().flatten();
            let series = self.shared.db.series(series_id).ok().flatten();
            if let (Some(study), Some(series)) = (study, series) {
                let m = crate::mpr::MprState::open(&self.shared, study, series, &ctx);
                self.mprs.push(Arc::new(Mutex::new(m)));
            }
        }
        self.mprs.retain(|m| !m.lock().close_requested);
        for m in &self.mprs {
            let (id, title) = {
                let g = m.lock();
                (g.viewport_id, g.title())
            };
            let m = m.clone();
            let shared = self.shared.clone();
            ctx.show_viewport_deferred(
                id,
                ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([1500.0, 640.0])
                    .with_min_inner_size([800.0, 360.0]),
                move |ui, _class| {
                    if let Err(msg) = crate::crash::guarded(|| m.lock().show(ui, &shared)) {
                        m.lock().close_requested = true;
                        shared
                            .errors
                            .lock()
                            .push(format!("An MPR window was closed after an internal error: {msg}"));
                    }
                },
            );
        }

        let report_requests: Vec<i64> = std::mem::take(&mut *self.shared.open_reports.lock());
        for study in report_requests {
            self.open_report(study);
        }

        // Viewer windows.
        self.viewers.retain(|v| !v.lock().close_requested);
        for v in &self.viewers {
            let (id, title) = {
                let g = v.lock();
                (g.viewport_id, g.title())
            };
            let v = v.clone();
            let shared = self.shared.clone();
            ctx.show_viewport_deferred(
                id,
                ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([1200.0, 860.0])
                    .with_min_inner_size([700.0, 500.0]),
                move |ui, _class| {
                    if let Err(msg) = crate::crash::guarded(|| v.lock().show(ui, &shared)) {
                        v.lock().close_requested = true;
                        shared
                            .errors
                            .lock()
                            .push(format!("A viewer window was closed after an internal error: {msg}"));
                    }
                },
            );
        }

        self.preferences(&ctx);
        self.query_window(&ctx);
        self.router.pump(&ctx, &self.shared);
        // Watch folders follow Preferences as soon as they are saved.
        {
            let wanted = self.shared.settings.lock().watch_folders.clone();
            let mut current = self.shared.watch.folders.lock();
            if *current != wanted {
                *current = wanted;
            }
        }

        let caught = std::mem::take(&mut *self.shared.errors.lock());
        for msg in caught {
            self.report_error(msg);
        }
        self.error_dialog_ui(&ctx);

        // Report windows.
        self.reports.retain(|r| !r.lock().close_requested);
        for r in &self.reports {
            let (id, title) = {
                let g = r.lock();
                (g.viewport_id, g.title())
            };
            let r = r.clone();
            let shared = self.shared.clone();
            ctx.show_viewport_deferred(
                id,
                ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([760.0, 820.0])
                    .with_min_inner_size([560.0, 500.0]),
                move |ui, _class| {
                    if let Err(msg) = crate::crash::guarded(|| r.lock().show(ui, &shared)) {
                        r.lock().close_requested = true;
                        shared
                            .errors
                            .lock()
                            .push(format!("A report window was closed after an internal error: {msg}"));
                    }
                },
            );
        }

        if let Some(vid) = self.pending_focus.take() {
            ctx.send_viewport_cmd_to(vid, ViewportCommand::Focus);
        }
        if self.shared.focus_browser.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
        }

        // Keep polling background work at a gentle rate.
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn on_exit(&mut self) {
        for v in &self.viewers {
            v.lock().persist_series_display(&self.shared);
        }
        self.incoming_stop.store(true, Ordering::Relaxed);
        if let Some(mut scp) = self.scp.take() {
            scp.stop();
        }
        let _ = self.shared.settings.lock().save();
    }
}

/// Start the DICOM listener from the settings, recording its state.
fn start_listener(shared: &Shared, settings: &ruthos_core::Settings) -> Option<StoreScp> {
    if !settings.listener_enabled {
        return None;
    }
    let cfg = ListenerConfig {
        port: settings.listener_port,
        ae_title: settings.listener_ae_title.clone(),
        out_dir: shared.paths.incoming_dir(),
    };
    match StoreScp::start(cfg) {
        Ok(s) => {
            let mut st = shared.net_status.lock();
            st.running = true;
            st.ae_title = settings.listener_ae_title.clone();
            st.port = settings.listener_port;
            st.last_error = None;
            Some(s)
        }
        Err(e) => {
            let msg = format!("DICOM listener could not start on port {}: {e}", settings.listener_port);
            tracing::warn!("{msg}");
            shared.net_status.lock().last_error = Some(msg);
            None
        }
    }
}
