//! AI segmentation in the Segments panel: the download, and a worker thread
//! that owns the model so the window never waits for it. It runs only when the
//! user asks, and no image leaves the machine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{Color32, RichText, Ui};

use ruthos_core::volume::Volume;
use ruthos_segment::ai::{self, to_slice, Click, Sam2, SAM2_TINY};
use ruthos_segment::Field;

/// Slices without the object before following stops in that direction.
const GIVE_UP_AFTER: usize = 3;

/// What is being segmented decides the window the model sees, whatever window
/// the user is viewing with, because it takes the slice as an 8-bit image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Bone,
    SoftTissue,
    Contrast,
}

impl Target {
    const ALL: [Target; 3] = [Target::Bone, Target::SoftTissue, Target::Contrast];

    fn label(self) -> &'static str {
        match self {
            Target::Bone => "Bone",
            Target::SoftTissue => "Soft tissue",
            Target::Contrast => "Contrast",
        }
    }

    /// Window centre and width.
    fn window(self) -> (f32, f32) {
        match self {
            Target::Bone => (300.0, 1500.0),
            Target::SoftTissue => (60.0, 400.0),
            Target::Contrast => (200.0, 700.0),
        }
    }
}

fn gray_slice(volume: &Volume, z: usize, (centre, width): (f32, f32)) -> Vec<f32> {
    let [w, h, _] = volume.dims;
    let low = centre - width / 2.0;
    (0..w * h)
        .map(|i| ((volume.at(i + w * h * z) - low) / width).clamp(0.0, 1.0))
        .collect()
}

/// A slice and the window it was encoded under.
type SliceKey = (usize, (u32, u32));

enum Request {
    Click {
        z: usize,
        clicks: Vec<Click>,
        window: (f32, f32),
    },
    Follow {
        z: usize,
        clicks: Vec<Click>,
        window: (f32, f32),
    },
}

pub enum Reply {
    Loaded {
        on_gpu: bool,
    },
    Failed(String),
    Mask {
        z: usize,
        mask: Vec<bool>,
    },
    /// Slices done so far.
    Followed(usize),
    Finished(usize),
}

struct Worker {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    cancel: Arc<AtomicBool>,
}

/// Dropping the requests ends the thread once it is idle; this ends a follow.
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn serve(
    volume: Arc<Volume>,
    root: PathBuf,
    requests: Receiver<Request>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
) {
    let mut model = match Sam2::load(&SAM2_TINY, &root) {
        Ok(m) => m,
        Err(e) => {
            let _ = replies.send(Reply::Failed(e.to_string()));
            return;
        }
    };
    let _ = replies.send(Reply::Loaded { on_gpu: model.on_gpu });
    let [w, h, d] = volume.dims;
    // Encoding is the expensive step, and clicks refine one slice repeatedly.
    let mut cached: Option<(SliceKey, ai::Features)> = None;
    while let Ok(request) = requests.recv() {
        let (z, clicks, window, follow) = match request {
            Request::Click { z, clicks, window } => (z, clicks, window, false),
            Request::Follow { z, clicks, window } => (z, clicks, window, true),
        };
        let run = (|| -> Result<(), ai::AiError> {
            let key = (z, (window.0.to_bits(), window.1.to_bits()));
            if cached.as_ref().is_none_or(|(k, _)| *k != key) {
                let features = model.encode(&gray_slice(&volume, z, window), w, h)?;
                cached = Some((key, features));
            }
            let Some((_, features)) = cached.as_ref() else {
                return Ok(());
            };
            let first = model.click(features, &clicks, w, h)?;
            let _ = replies.send(Reply::Mask {
                z,
                mask: to_slice(&first.mask, w, h),
            });
            if !follow {
                return Ok(());
            }
            let bank = model.begin(features, &first)?;
            let mut done = 1;
            let upward: Vec<usize> = (z + 1..d).collect();
            let downward: Vec<usize> = (0..z).rev().collect();
            for slices in [upward, downward] {
                let mut memory = bank.clone();
                let mut empty = 0;
                for (i, slice) in slices.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        let _ = replies.send(Reply::Finished(done));
                        return Ok(());
                    }
                    let f = model.encode(&gray_slice(&volume, slice, window), w, h)?;
                    let decoded = model.track(&f, &mut memory, i + 1)?;
                    let mask = to_slice(&decoded.mask, w, h);
                    empty = if decoded.score <= 0.0 || !mask.iter().any(|v| *v) {
                        empty + 1
                    } else {
                        0
                    };
                    let _ = replies.send(Reply::Mask { z: slice, mask });
                    done += 1;
                    let _ = replies.send(Reply::Followed(done));
                    if empty >= GIVE_UP_AFTER {
                        break;
                    }
                }
            }
            let _ = replies.send(Reply::Finished(done));
            Ok(())
        })();
        if let Err(e) = run {
            let _ = replies.send(Reply::Failed(e.to_string()));
        }
    }
}

struct Install {
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    finished: Receiver<Result<(), String>>,
}

pub struct AiPanel {
    worker: Option<Worker>,
    install: Option<Install>,
    /// The next click on a slice goes to the model, to include or exclude.
    pub picking: bool,
    pub include: bool,
    target: Target,
    /// The slice being prompted and its clicks.
    pub slice: Option<usize>,
    pub clicks: Vec<Click>,
    /// The label the model is writing to.
    pub label: Option<u8>,
    pub ready: bool,
    on_gpu: bool,
    /// Working: on a mask, or following with this many slices done.
    pub busy: bool,
    followed: Option<usize>,
    pub note: Option<String>,
    /// A new follow replaces what the last one wrote, once the panel sees it.
    pub restart: bool,
}

impl Default for AiPanel {
    fn default() -> Self {
        Self {
            worker: None,
            install: None,
            picking: false,
            include: true,
            target: Target::Bone,
            slice: None,
            clicks: Vec::new(),
            label: None,
            ready: false,
            on_gpu: false,
            busy: false,
            followed: None,
            note: None,
            restart: false,
        }
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

impl AiPanel {
    pub fn started(&self) -> bool {
        self.worker.is_some()
    }

    pub fn start_worker(&mut self, volume: &Arc<Volume>, root: &Path) {
        let (requests, serve_requests) = unbounded();
        let (serve_replies, replies) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let (v, r, c) = (volume.clone(), root.to_path_buf(), cancel.clone());
        let spawned = std::thread::Builder::new()
            .name("ruthos-segmentation-model".to_string())
            .spawn(move || serve(v, r, serve_requests, serve_replies, c));
        match spawned {
            Ok(_) => {
                self.worker = Some(Worker {
                    requests,
                    replies,
                    cancel,
                });
                self.busy = true;
                self.note = Some("Loading the model\u{2026}".to_string());
            }
            Err(e) => self.note = Some(e.to_string()),
        }
    }

    fn start_install(&mut self, root: &Path) {
        let (tx, finished) = unbounded();
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(SAM2_TINY.download_bytes(root)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d, t, c, r) = (done.clone(), total.clone(), cancel.clone(), root.to_path_buf());
        let spawned = std::thread::Builder::new()
            .name("ruthos-model-download".to_string())
            .spawn(move || {
                let progress = |so_far: u64, all: u64| {
                    d.store(so_far, Ordering::Relaxed);
                    t.store(all, Ordering::Relaxed);
                };
                let _ = tx.send(ai::install(&SAM2_TINY, &r, &progress, &c).map_err(|e| e.to_string()));
            });
        match spawned {
            Ok(_) => {
                self.install = Some(Install {
                    done,
                    total,
                    cancel,
                    finished,
                })
            }
            Err(e) => self.note = Some(e.to_string()),
        }
    }

    /// A click on a slice, at voxel `(x, y, z)`.
    pub fn click(&mut self, x: usize, y: usize, z: usize) {
        if self.slice != Some(z) {
            self.clicks.clear();
            self.slice = Some(z);
        }
        self.clicks.push(Click {
            x: x as f32 + 0.5,
            y: y as f32 + 0.5,
            include: self.include,
        });
        if let Some(w) = &self.worker {
            self.busy = true;
            let _ = w.requests.send(Request::Click {
                z,
                clicks: self.clicks.clone(),
                window: self.target.window(),
            });
        }
    }

    /// Follow the object from the clicked slice through the stack.
    pub fn follow(&mut self) {
        let (Some(z), Some(w)) = (self.slice, &self.worker) else {
            return;
        };
        if self.clicks.is_empty() {
            return;
        }
        w.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.restart = true;
        self.followed = Some(1);
        let _ = w.requests.send(Request::Follow {
            z,
            clicks: self.clicks.clone(),
            window: self.target.window(),
        });
    }

    /// Replies waiting, for the panel to act on.
    pub fn take_replies(&mut self) -> Vec<Reply> {
        let mut out = Vec::new();
        if let Some(w) = &self.worker {
            out.extend(w.replies.try_iter());
        }
        for r in &out {
            match r {
                Reply::Loaded { on_gpu } => {
                    self.ready = true;
                    self.busy = false;
                    self.on_gpu = *on_gpu;
                    self.note = Some(if *on_gpu {
                        "Ready. Click a structure on a slice".to_string()
                    } else {
                        "Ready, on the CPU: following through slices will be slow".to_string()
                    });
                }
                Reply::Failed(e) => {
                    self.busy = false;
                    self.followed = None;
                    self.note = Some(e.clone());
                }
                Reply::Mask { .. } => {
                    if self.followed.is_none() {
                        self.busy = false;
                    }
                }
                Reply::Followed(n) => self.followed = Some(*n),
                Reply::Finished(n) => {
                    self.busy = false;
                    self.followed = None;
                    self.note = Some(format!("Followed through {n} slices. Check them against the images"));
                }
            }
        }
        out
    }

    /// Start a new object: the next click makes a new segment.
    pub fn new_object(&mut self) {
        self.clicks.clear();
        self.slice = None;
        self.label = None;
    }

    pub fn ui(&mut self, ui: &mut Ui, root: &Path, volume: &Arc<Volume>) {
        ui.label(RichText::new("AI segment").strong());
        if self.busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(i) = &self.install {
            let (done, total) = (i.done.load(Ordering::Relaxed), i.total.load(Ordering::Relaxed));
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).text(format!(
                "{} of {}",
                megabytes(done),
                megabytes(total)
            )));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            if ui.button("Cancel").clicked() {
                i.cancel.store(true, Ordering::Relaxed);
            }
            if let Ok(result) = i.finished.try_recv() {
                self.install = None;
                self.note = result.err();
            }
            return;
        }
        if !SAM2_TINY.installed(root) {
            ui.label(RichText::new(format!("{} {}", SAM2_TINY.name, "is not installed")).small());
            ui.label(RichText::new(format!("Licence: {}", SAM2_TINY.licence)).small());
            ui.label(RichText::new(format!("Source: {}", SAM2_TINY.source)).small());
            ui.label(RichText::new(SAM2_TINY.training).small());
            ui.label(
                RichText::new("It runs on this machine. No image data leaves it.")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(format!("Download {}", megabytes(SAM2_TINY.download_bytes(root))))
                    .on_hover_text("The model and Microsoft's ONNX Runtime and DirectML, each checked by checksum")
                    .clicked()
                {
                    self.start_install(root);
                }
                if ui
                    .button("From a folder\u{2026}")
                    .on_hover_text("For a machine without internet access")
                    .clicked()
                {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.note = ai::install_from_folder(&SAM2_TINY, &folder, root)
                            .err()
                            .map(|e| e.to_string());
                    }
                }
            });
            if let Some(n) = &self.note {
                ui.label(RichText::new(n).small().color(Color32::from_rgb(240, 190, 90)));
            }
            return;
        }
        if self.worker.is_none() {
            if ui
                .button(format!("Start {}", SAM2_TINY.name))
                .on_hover_text("Loads the model. Nothing is segmented until you click")
                .clicked()
            {
                self.start_worker(volume, root);
            }
            return;
        }
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).small().color(Color32::from_gray(170)));
        }
        if !self.ready {
            ui.spinner();
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Finding");
            egui::ComboBox::from_id_salt("ai-target")
                .selected_text(self.target.label())
                .show_ui(ui, |ui| {
                    for t in Target::ALL {
                        ui.selectable_value(&mut self.target, t, t.label());
                    }
                });
        });
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.picking && self.include, "Include")
                .on_hover_text("Click on a slice: this is part of the object")
                .clicked()
            {
                self.picking = true;
                self.include = true;
            }
            if ui
                .selectable_label(self.picking && !self.include, "Exclude")
                .on_hover_text("Click on a slice: this is not part of the object")
                .clicked()
            {
                self.picking = true;
                self.include = false;
            }
            if ui.small_button("New object").clicked() {
                self.new_object();
            }
        });
        if let Some(n) = self.followed {
            ui.label(format!("Following: {n} slices"));
            if ui.button("Stop").clicked() {
                if let Some(w) = &self.worker {
                    w.cancel.store(true, Ordering::Relaxed);
                }
            }
        } else if ui
            .add_enabled(
                !self.busy && !self.clicks.is_empty(),
                egui::Button::new("Follow through slices"),
            )
            .on_hover_text("Follows the object up and down the stack until it ends")
            .clicked()
        {
            self.follow();
        }
    }
}
