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
use crate::shared::Shared;
use crate::theme;
use crate::viewer::ViewerState;

pub struct RuthosApp {
    shared: Arc<Shared>,
    browser: BrowserState,
    viewers: Vec<Arc<Mutex<ViewerState>>>,
    scp: Option<StoreScp>,
    incoming_stop: Arc<AtomicBool>,
    startup_error: Option<String>,
    /// Viewer window to bring to the front on the next frame.
    pending_focus: Option<ViewportId>,
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

        let settings = Settings::load();
        let paths = DataPaths::new(settings.data_root());
        paths.ensure()?;
        let db = Database::open(&paths)?;
        tracing::info!("database at {}", paths.database_file().display());

        // Register the image pipeline with egui-wgpu once.
        {
            let mut renderer = render_state.renderer.write();
            renderer
                .callback_resources
                .insert(ImageRenderer::new(&render_state.device, render_state.target_format));
        }

        let shared = Shared::new(db.clone(), paths.clone(), settings.clone(), render_state);

        // DICOM listener writes to the incoming folder; the scanner imports.
        let mut startup_error = None;
        let scp = if settings.listener_enabled {
            let cfg = ListenerConfig {
                port: settings.listener_port,
                ae_title: settings.listener_ae_title.clone(),
                out_dir: paths.incoming_dir(),
            };
            match StoreScp::start(cfg) {
                Ok(s) => {
                    let mut st = shared.net_status.lock();
                    st.running = true;
                    st.ae_title = settings.listener_ae_title.clone();
                    st.port = settings.listener_port;
                    Some(s)
                }
                Err(e) => {
                    let msg = format!("DICOM listener could not start on port {}: {e}", settings.listener_port);
                    tracing::warn!("{msg}");
                    shared.net_status.lock().last_error = Some(msg.clone());
                    startup_error = Some(msg);
                    None
                }
            }
        } else {
            None
        };

        let incoming_stop = Arc::new(AtomicBool::new(false));
        IncomingScanner::spawn(
            db,
            paths,
            Duration::from_secs(settings.incoming_scan_seconds.max(1)),
            shared.import_tx.clone(),
            incoming_stop.clone(),
        );

        let mut browser = BrowserState::new();
        if !startup_paths.is_empty() {
            browser.open_next_import = true;
            browser.import_paths(&shared, startup_paths);
        }

        Ok(Self {
            shared,
            browser,
            viewers: Vec::new(),
            scp,
            incoming_stop,
            startup_error,
            pending_focus: None,
        })
    }

    fn open_viewer(&mut self, study_id: i64, series_id: Option<i64>) {
        tracing::info!("open viewer: study {study_id}, series {series_id:?}, {} open", self.viewers.len());
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
        let Ok(Some(study)) = self.shared.db.study(study_id) else { return };
        match ViewerState::open(&self.shared, study, series_id) {
            Ok(v) => self.viewers.push(Arc::new(Mutex::new(v))),
            Err(e) => tracing::warn!("could not open viewer: {e}"),
        }
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

        self.browser.show(ui, &self.shared);

        for (study, series) in self.browser.take_open_requests() {
            self.open_viewer(study, series);
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
                    v.lock().show(ui, &shared);
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
