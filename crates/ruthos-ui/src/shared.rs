//! State shared between the database window and every viewer window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::TextureHandle;
use parking_lot::Mutex;

use ruthos_core::{DataPaths, Database, ImportEvent, Settings, ThumbnailService};
use ruthos_dicom::{ClutLibrary, WindowPresets};

use crate::loader::FrameLoader;

/// Status of the DICOM listener, for the source list.
#[derive(Debug, Clone, Default)]
pub struct NetStatus {
    pub running: bool,
    pub ae_title: String,
    pub port: u16,
    pub last_peer: Option<String>,
    pub received: u64,
    pub last_error: Option<String>,
}

pub struct Shared {
    pub db: Arc<Database>,
    pub paths: DataPaths,
    pub settings: Mutex<Settings>,
    /// The effective measurement library (built-in plus the user's),
    /// rebuilt when Preferences are saved.
    pub library: Mutex<Arc<ruthos_core::measure::Library>>,
    pub render_state: egui_wgpu::RenderState,
    pub loader: FrameLoader,
    pub thumbs: ThumbnailService,
    pub cluts: ClutLibrary,
    pub presets: WindowPresets,
    /// egui textures for series thumbnails, keyed by series id.
    pub thumb_textures: Mutex<HashMap<i64, TextureHandle>>,
    pub import_tx: Sender<ImportEvent>,
    pub import_rx: Receiver<ImportEvent>,
    pub net_status: Mutex<NetStatus>,
    /// Set by a viewer's Database button; the app focuses the browser window.
    pub focus_browser: AtomicBool,
    /// Studies a viewer asked to open (not used yet by viewers, reserved).
    pub open_requests: Mutex<Vec<i64>>,
    /// Monotonic id source for viewers and paint callbacks.
    next_id: AtomicU64,
    pub viewer_keys_to_forget: Mutex<Vec<u64>>,
    /// Internal errors caught at a window boundary, shown by the database
    /// window in a dialog.
    pub errors: Mutex<Vec<String>>,
    /// A note about the graphics adapter when it is worth knowing, e.g. a
    /// software renderer is in use.
    pub renderer_note: Mutex<Option<String>>,
    /// Set by the Preferences button; the app opens the window.
    pub open_prefs: AtomicBool,
    /// The viewer window that last had keyboard focus, for menu actions.
    pub focused_viewer: AtomicU64,
    /// Studies whose report window should open.
    pub open_reports: Mutex<Vec<i64>>,
}

impl Shared {
    pub fn new(
        db: Arc<Database>,
        paths: DataPaths,
        settings: Settings,
        render_state: egui_wgpu::RenderState,
    ) -> Arc<Self> {
        let (import_tx, import_rx) = unbounded();
        let thumbs = ThumbnailService::new(paths.clone(), settings.thumbnail_size);
        Arc::new(Self {
            db,
            paths,
            library: Mutex::new(Arc::new(settings.library())),
            settings: Mutex::new(settings),
            render_state,
            loader: FrameLoader::new(),
            thumbs,
            cluts: ClutLibrary::load(),
            presets: WindowPresets::default(),
            thumb_textures: Mutex::new(HashMap::new()),
            import_tx,
            import_rx,
            net_status: Mutex::new(NetStatus::default()),
            focus_browser: AtomicBool::new(false),
            open_requests: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            viewer_keys_to_forget: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            renderer_note: Mutex::new(None),
            open_prefs: AtomicBool::new(false),
            focused_viewer: AtomicU64::new(0),
            open_reports: Mutex::new(Vec::new()),
        })
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn resolve(&self, stored: &str) -> PathBuf {
        self.paths.resolve(stored)
    }

    /// Upload finished thumbnails into egui textures. Call once per frame.
    pub fn pump_thumbnails(&self, ctx: &egui::Context) {
        for u in self.thumbs.poll() {
            let image = egui::ColorImage::from_rgba_unmultiplied([u.width as usize, u.height as usize], &u.rgba);
            let handle = ctx.load_texture(format!("thumb-{}", u.series_id), image, egui::TextureOptions::LINEAR);
            self.thumb_textures.lock().insert(u.series_id, handle);
        }
    }

    /// Thumbnail texture for a series, requesting it if missing.
    pub fn thumbnail(&self, series_id: i64) -> Option<TextureHandle> {
        if let Some(t) = self.thumb_textures.lock().get(&series_id) {
            return Some(t.clone());
        }
        if let Ok(Some(img)) = self.db.representative_image(series_id) {
            let frame = (img.number_of_frames / 2).max(0) as u32;
            self.thumbs.request(series_id, self.resolve(&img.path), frame);
        }
        None
    }
}
