//! AI segmentation in the Segments panel: the download, and a worker thread
//! that owns the model so the window never waits for it. It runs only when the
//! user asks, and no image leaves the machine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{Color32, RichText, Ui};

use ruthos_core::volume::Volume;
use ruthos_segment::ai::{self, Click, Engine, Manifest, Region, Sam2, AVAILABLE};
use ruthos_segment::refine::Edge;
use ruthos_segment::volume_ai::{
    histogram, hold_to_range, Click3, Found, VolumeModel, Walk, HISTOGRAM_LOW, HISTOGRAM_STEP,
};
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

    /// The term a new segment starts with, from the list in `ruthos_dicom::seg`.
    fn term(self) -> &'static str {
        match self {
            Target::Bone => "Bone",
            Target::SoftTissue => "Soft tissue",
            Target::Contrast => "Other tissue",
        }
    }

    fn edge(self) -> Edge {
        match self {
            Target::Bone => Edge::Bone,
            Target::SoftTissue => Edge::SoftTissue,
            Target::Contrast => Edge::Contrast,
        }
    }

    /// The span of the range control in HU, and where its handles start. A handle at
    /// the top of the bone control means no ceiling.
    fn scale(self) -> ((f32, f32), (f32, f32)) {
        match self {
            Target::Bone => ((0.0, 2000.0), (250.0, 2000.0)),
            Target::SoftTissue => ((-200.0, 300.0), (-50.0, 150.0)),
            Target::Contrast => ((0.0, 800.0), (130.0, 600.0)),
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

/// A slice, the window it was encoded under and the part of it encoded.
type SliceKey = (usize, (u32, u32), Region);

/// More than this share of a box empty is worth a warning.
const MOSTLY_EMPTY: f32 = 0.6;
/// Above this many pixels to a mask cell, thin bone comes out noticeably thick:
/// on a 256 pixel slice, at 1.0, a 3 pixel wall came out 47% too large.
const FINE: f32 = 0.5;
/// A box whose object spans less than this of it could be much tighter.
const LOOSE: f32 = 0.4;
/// The model stretches a box square; past this it distorts what it sees.
const STRETCHED: f32 = 2.0;

/// What the model is given: part of each slice, and the slices it may follow
/// through, first and last included.
#[derive(Clone, Copy, PartialEq)]
pub struct Limits {
    pub region: Region,
    pub slices: (usize, usize),
}

enum Request {
    Click {
        z: usize,
        clicks: Vec<Click>,
        window: (f32, f32),
        limits: Limits,
        refine: Option<(Edge, usize)>,
    },
    Follow {
        z: usize,
        clicks: Vec<Click>,
        window: (f32, f32),
        limits: Limits,
        refine: Option<(Edge, usize)>,
    },
    /// All the clicks on the object so far, for a 3D model, whether to aim them at
    /// bone, and the range to hold the result to. Only the slices clicked on come back.
    Volume(Vec<Click3>, bool, Option<(f32, f32)>),
    /// Hold the result in hand to another range, which needs no model.
    Level(Option<(f32, f32)>),
    /// The rest of the slices of the result in hand.
    Reveal,
    /// Forget the object, for a 3D model.
    NewObject,
}

pub enum Reply {
    Loaded {
        on_gpu: bool,
    },
    Failed(String),
    Mask {
        z: usize,
        mask: Vec<bool>,
        /// The model's own mask, when `mask` has been refined from it.
        raw: Option<Vec<bool>>,
        /// The object ran into a side of the box.
        edge: bool,
        /// The share of the box it spans.
        extent: f32,
    },
    /// Slices done so far.
    Followed(usize),
    Finished(usize),
    /// What a 3D model's result holds, for the range control.
    Histogram(Vec<u32>),
    /// A 3D model has shown the slices clicked on.
    Preview {
        seconds: f32,
        on_gpu: bool,
    },
    /// How far a 3D model has got in mapping its result through the stack.
    Mapping {
        done: usize,
        total: usize,
    },
    /// A 3D model has written the whole of its result, or was stopped.
    Done {
        slices: usize,
        edge: bool,
        stopped: bool,
    },
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

/// A 3D model's thread: each request is every click so far.
fn serve_volume(
    manifest: Manifest,
    volume: Arc<Volume>,
    root: PathBuf,
    requests: Receiver<Request>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
) {
    let mut model = match VolumeModel::load(&manifest, &root) {
        Ok(m) => m,
        Err(e) => {
            let _ = replies.send(Reply::Failed(e.to_string()));
            return;
        }
    };
    model.prepare(volume.as_ref());
    let _ = replies.send(Reply::Loaded { on_gpu: model.on_gpu() });
    // The result in hand, as the model gave it: the whole object is worked out with
    // each click, and the slices beyond those clicked on wait to be asked for. A new
    // level is applied to this, so it costs no time in the model.
    struct Result3 {
        raw: Found,
        aimed: Vec<Click3>,
        clicked: Vec<usize>,
        level: Option<(f32, f32)>,
        revealed: bool,
        walk: Walk,
    }
    let mut latest: Option<Result3> = None;
    let hold = |r: &Result3| match r.level {
        Some((low, high)) => hold_to_range(&r.raw, volume.as_ref(), &r.aimed, low, high),
        None => r.raw.clone(),
    };
    let mask = |z: usize, mask: Vec<bool>| Reply::Mask {
        z,
        mask,
        raw: None,
        edge: false,
        extent: 0.0,
    };
    while let Ok(request) = requests.recv() {
        let begun = std::time::Instant::now();
        let reveal = matches!(request, Request::Reveal);
        let (clicks, bone, level) = match request {
            Request::Volume(clicks, bone, level) => (clicks, bone, level),
            Request::Reveal | Request::Level(_) => {
                let new_level = if let Request::Level(l) = request { Some(l) } else { None };
                // Follow goes on past the window as far as the object does. Stopped,
                // it still shows what it has, and can carry on from there.
                let mut stopped = false;
                if let Some(r) = latest.as_mut() {
                    r.revealed |= reveal;
                    r.level = new_level.unwrap_or(r.level);
                    if reveal {
                        let walked = model.follow(
                            volume.as_ref(),
                            &r.aimed,
                            r.level,
                            &mut r.raw,
                            &mut r.walk,
                            |done, total| {
                                let _ = replies.send(Reply::Mapping { done, total });
                            },
                            &cancel,
                        );
                        match walked {
                            Ok(s) => stopped = s,
                            Err(e) => {
                                let _ = replies.send(Reply::Failed(e.to_string()));
                            }
                        }
                        cancel.store(false, Ordering::Relaxed);
                    }
                }
                if let Some(r) = latest.as_ref() {
                    let Found { slices, edge } = hold(r);
                    if r.revealed {
                        let total = slices.len();
                        let mut done = 0;
                        for (z, on) in slices {
                            let _ = replies.send(mask(z, on));
                            done += 1;
                        }
                        let _ = total;
                        let _ = replies.send(Reply::Done {
                            slices: done,
                            edge,
                            stopped,
                        });
                    } else {
                        for (z, on) in slices.into_iter().filter(|(z, _)| r.clicked.contains(z)) {
                            let _ = replies.send(mask(z, on));
                        }
                        let _ = replies.send(Reply::Preview {
                            seconds: begun.elapsed().as_secs_f32(),
                            on_gpu: model.on_gpu(),
                        });
                    }
                }
                continue;
            }
            Request::NewObject => {
                model.reset();
                latest = None;
                continue;
            }
            _ => continue,
        };
        match model.click(volume.as_ref(), &clicks, bone) {
            Ok((raw, aimed)) => {
                let result = Result3 {
                    raw,
                    aimed,
                    clicked: clicks.iter().map(|c| c.z).collect(),
                    level,
                    revealed: false,
                    walk: Walk::default(),
                };
                let shown = hold(&result);
                for (z, on) in shown.slices.into_iter().filter(|(z, _)| result.clicked.contains(z)) {
                    let _ = replies.send(mask(z, on));
                }
                let _ = replies.send(Reply::Histogram(histogram(&result.raw, volume.as_ref())));
                latest = Some(result);
                let _ = replies.send(Reply::Preview {
                    seconds: begun.elapsed().as_secs_f32(),
                    on_gpu: model.on_gpu(),
                });
            }
            Err(e) => {
                let _ = replies.send(Reply::Failed(e.to_string()));
            }
        }
    }
}

fn serve(
    manifest: Manifest,
    volume: Arc<Volume>,
    root: PathBuf,
    requests: Receiver<Request>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
) {
    let mut model = match Sam2::load(&manifest, &root) {
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
        let (z, clicks, window, limits, refine, follow) = match request {
            Request::Volume(..) | Request::Level(_) | Request::Reveal | Request::NewObject => continue,
            Request::Click {
                z,
                clicks,
                window,
                limits,
                refine,
            } => (z, clicks, window, limits, refine, false),
            Request::Follow {
                z,
                clicks,
                window,
                limits,
                refine,
            } => (z, clicks, window, limits, refine, true),
        };
        let region = limits.region;
        let (rw, rh) = (region.width, region.height);
        let local: Vec<Click> = clicks.iter().map(|c| region.local(*c)).collect();
        let cut = |slice: usize| region.take(&gray_slice(&volume, slice, window), w);
        // Back into the whole slice, with what the panel warns about.
        let reply = |z: usize, decoded: &ai::Decoded| {
            let values = || {
                let slice: Vec<f32> = (0..w * h).map(|i| volume.at(i + w * h * z)).collect();
                region.take(&slice, w)
            };
            let (inner, own) = ai::masks(&decoded.mask, values, rw, rh, refine);
            let judged = own.as_ref().unwrap_or(&inner);
            Reply::Mask {
                z,
                mask: region.place(&inner, w, h),
                raw: own.as_ref().map(|m| region.place(m, w, h)),
                edge: region.reaches_edge(judged, w, h),
                extent: region.extent(judged),
            }
        };
        let run = (|| -> Result<(), ai::AiError> {
            let key = (z, (window.0.to_bits(), window.1.to_bits()), region);
            if cached.as_ref().is_none_or(|(k, _)| *k != key) {
                let features = model.encode(&cut(z), rw, rh)?;
                cached = Some((key, features));
            }
            let Some((_, features)) = cached.as_ref() else {
                return Ok(());
            };
            let first = model.click(features, &local, rw, rh)?;
            let _ = replies.send(reply(z, &first));
            if !follow {
                return Ok(());
            }
            let bank = model.begin(features, &first)?;
            let mut done = 1;
            let upward: Vec<usize> = (z + 1..=limits.slices.1.min(d - 1)).collect();
            let downward: Vec<usize> = (limits.slices.0..z).rev().collect();
            for slices in [upward, downward] {
                let mut memory = bank.clone();
                let mut empty = 0;
                for (i, slice) in slices.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        let _ = replies.send(Reply::Finished(done));
                        return Ok(());
                    }
                    let f = model.encode(&cut(slice), rw, rh)?;
                    let decoded = model.track(&f, &mut memory, i + 1)?;
                    let found = reply(slice, &decoded);
                    let any = matches!(&found, Reply::Mask { mask, .. } if mask.iter().any(|v| *v));
                    empty = if decoded.score <= 0.0 || !any { empty + 1 } else { 0 };
                    let _ = replies.send(found);
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
    /// The slices to go through, for the progress bar.
    follow_total: usize,
    pub note: Option<String>,
    /// A new follow replaces what the last one wrote, once the panel sees it.
    pub restart: bool,
    /// The volume's size, once a worker has started.
    dims: Option<[usize; 3]>,
    /// The cut box, from the 3D pane, when there is one.
    pub cut_box: Option<Limits>,
    /// Keep to the cut box.
    use_box: bool,
    /// The object reached a side of the box.
    edge: bool,
    /// The most of the box the object spanned.
    extent: f32,
    /// How empty the box is, and what that was worked out for.
    emptiness: Option<(Region, usize, Target, f32)>,
    /// Replace the model's edge with one read from the image.
    pub refine: bool,
    /// Band width in voxels, from Preferences.
    pub band: usize,
    /// Draw the model's own outline over refined segments.
    pub show_outline: bool,
    /// The model in use, from `AVAILABLE`.
    model: usize,
    /// Every click on the object, for a 3D model.
    clicks3: Vec<Click3>,
    /// A 3D model's result is on the clicked slices, waiting for Follow.
    pub previewing: bool,
    /// A 3D model's result has been mapped through the stack.
    mapped: bool,
    /// For a 3D model, hold what it finds to the range for the target.
    keep_to_range: bool,
    /// Bottom and top of the range, for each target.
    ranges: [(f32, f32); 3],
    /// What the result holds, for the range control.
    values: Vec<u32>,
    /// The range has changed since the result on show.
    level_changed: bool,
    /// The next click on an image sets this handle: the top if true.
    picking_handle: Option<bool>,
    /// Research mode is on in Preferences.
    pub research_mode: bool,
    /// Models whose terms have been agreed to, by id.
    pub agreed: Vec<String>,
    /// The user has just agreed to this model's terms, or chosen this model. The
    /// window saves them.
    pub newly_agreed: Option<&'static str>,
    pub newly_chosen: Option<&'static str>,
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
            follow_total: 0,
            note: None,
            restart: false,
            dims: None,
            cut_box: None,
            use_box: true,
            edge: false,
            extent: 0.0,
            emptiness: None,
            refine: false,
            band: ruthos_segment::refine::BAND,
            show_outline: false,
            model: 0,
            clicks3: Vec::new(),
            previewing: false,
            mapped: false,
            keep_to_range: true,
            ranges: Target::ALL.map(|t| t.scale().1),
            values: Vec::new(),
            picking_handle: None,
            level_changed: false,
            research_mode: false,
            agreed: Vec::new(),
            newly_agreed: None,
            newly_chosen: None,
        }
    }
}

/// A line from dark to bright with a handle at each end of a range. No numbers: the
/// bars show what is there, and a handle is dragged to where it should go. True if
/// the range changed. The value in HU is on hover, for those who want it.
fn range_line(ui: &mut Ui, range: &mut (f32, f32), (low, high): (f32, f32), bins: &[u32]) -> bool {
    use egui::{pos2, vec2, Pos2, Rect, Sense, Stroke};
    let id = ui.make_persistent_id("range-line");
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click_and_drag());
    let line = Rect::from_min_max(
        pos2(rect.left() + 7.0, rect.top() + 2.0),
        pos2(rect.right() - 7.0, rect.bottom() - 10.0),
    );
    let at = |v: f32| line.left() + (v - low) / (high - low) * line.width();
    let value = |x: f32| low + ((x - line.left()) / line.width()).clamp(0.0, 1.0) * (high - low);
    let painter = ui.painter_at(rect);

    const STEPS: usize = 64;
    for k in 0..STEPS {
        let (a, b) = (k as f32 / STEPS as f32, (k + 1) as f32 / STEPS as f32);
        let (x0, x1) = (line.left() + a * line.width(), line.left() + b * line.width());
        let mid = low + (a + b) / 2.0 * (high - low);
        let grey = (((a + b) / 2.0) * 255.0) as u8;
        let chosen = mid >= range.0 && mid <= range.1;
        let colour = if chosen {
            Color32::from_gray(grey)
        } else {
            Color32::from_gray(grey / 3)
        };
        painter.rect_filled(
            Rect::from_min_max(pos2(x0, line.top()), pos2(x1 + 0.5, line.bottom())),
            0.0,
            colour,
        );
    }
    // What the model found, gathered into the bars of the line.
    const BARS: usize = 64;
    let mut bars = [0f32; BARS];
    for (i, n) in bins.iter().enumerate() {
        let v = HISTOGRAM_LOW + (i as f32 + 0.5) * HISTOGRAM_STEP;
        if v >= low && v < high {
            bars[(((v - low) / (high - low)) * BARS as f32) as usize % BARS] += *n as f32;
        }
    }
    let tallest = bars.iter().copied().fold(1.0, f32::max);
    for (k, n) in bars.iter().enumerate().filter(|(_, n)| **n > 0.0) {
        let x0 = line.left() + k as f32 / BARS as f32 * line.width();
        let x1 = line.left() + (k + 1) as f32 / BARS as f32 * line.width();
        let height = (n / tallest).sqrt() * line.height();
        painter.rect_filled(
            Rect::from_min_max(pos2(x0 + 0.5, line.bottom() - height), pos2(x1 - 0.5, line.bottom())),
            0.0,
            Color32::from_rgba_unmultiplied(240, 170, 60, 170),
        );
    }

    let handle = |v: f32| {
        let x = at(v);
        painter.line_segment(
            [pos2(x, line.top()), pos2(x, line.bottom())],
            Stroke::new(2.0, Color32::from_rgb(90, 170, 240)),
        );
        let tip = |y: f32| [pos2(x, y), pos2(x - 6.0, y + 7.0), pos2(x + 6.0, y + 7.0)];
        let points: [Pos2; 3] = tip(line.bottom());
        painter.add(egui::Shape::convex_polygon(
            points.to_vec(),
            Color32::from_rgb(90, 170, 240),
            Stroke::NONE,
        ));
    };
    handle(range.0);
    handle(range.1);

    // The handle nearest the press is the one that moves, for as long as it is held.
    let mut changed = false;
    if let Some(pointer) = response.interact_pointer_pos() {
        let grabbed: Option<bool> = ui.memory(|m| m.data.get_temp(id));
        let top = grabbed.unwrap_or_else(|| (pointer.x - at(range.1)).abs() < (pointer.x - at(range.0)).abs());
        ui.memory_mut(|m| m.data.insert_temp(id, top));
        let gap = (high - low) / 50.0;
        let v = value(pointer.x);
        let before = *range;
        if top {
            range.1 = v.max(range.0 + gap);
        } else {
            range.0 = v.min(range.1 - gap);
        }
        changed = *range != before;
    } else {
        ui.memory_mut(|m| m.data.remove::<bool>(id));
    }
    response.on_hover_text(format!(
        "Keeps what lies between the handles. Drag them towards the bars: the bars show what the model found, \
         dark on the left and bright on the right. Now {:.0} to {:.0} HU",
        range.0, range.1
    ));
    changed
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

impl AiPanel {
    pub fn started(&self) -> bool {
        self.worker.is_some()
    }

    /// The model in use.
    pub fn manifest(&self) -> Manifest {
        AVAILABLE[self.model]
    }

    fn is_3d(&self) -> bool {
        self.manifest().engine != Engine::Slices
    }

    /// Use the model with this id, as long as it is offered. Whatever was loaded
    /// goes, and with it the object in hand.
    pub fn choose(&mut self, id: &str) {
        let wanted = AVAILABLE
            .iter()
            .position(|m| m.id == id && m.offered(self.research_mode));
        if let Some(i) = wanted.filter(|i| *i != self.model) {
            self.model = i;
            self.worker = None;
            self.ready = false;
            self.busy = false;
            self.followed = None;
            self.note = None;
            self.new_object();
        }
    }

    /// The panel's list is what research mode allows, so turning that off drops a
    /// research model that was in use.
    pub fn keep_to_offered(&mut self) {
        if !self.manifest().offered(self.research_mode) {
            self.choose(AVAILABLE[0].id);
        }
    }

    pub fn start_worker(&mut self, volume: &Arc<Volume>, root: &Path) {
        let (requests, serve_requests) = unbounded();
        let (serve_replies, replies) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        self.dims = Some(volume.dims);
        let (v, r, c) = (volume.clone(), root.to_path_buf(), cancel.clone());
        let manifest = self.manifest();
        let spawned = std::thread::Builder::new()
            .name("ruthos-segmentation-model".to_string())
            .spawn(move || {
                if manifest.engine == Engine::Slices {
                    serve(manifest, v, r, serve_requests, serve_replies, c)
                } else {
                    serve_volume(manifest, v, r, serve_requests, serve_replies, c)
                }
            });
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
        let manifest = self.manifest();
        let (tx, finished) = unbounded();
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(manifest.download_bytes(root)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d, t, c, r) = (done.clone(), total.clone(), cancel.clone(), root.to_path_buf());
        let spawned = std::thread::Builder::new()
            .name("ruthos-model-download".to_string())
            .spawn(move || {
                let progress = |so_far: u64, all: u64| {
                    d.store(so_far, Ordering::Relaxed);
                    t.store(all, Ordering::Relaxed);
                };
                let _ = tx.send(ai::install(&manifest, &r, &progress, &c).map_err(|e| e.to_string()));
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

    /// What the model will be given.
    fn limits(&self) -> Option<Limits> {
        if let (true, Some(b)) = (self.use_box, self.cut_box) {
            return Some(b);
        }
        let [w, h, d] = self.dims?;
        Some(Limits {
            region: Region::whole(w, h),
            slices: (0, d.saturating_sub(1)),
        })
    }

    /// What the segment being made is, as far as the panel knows.
    pub fn term(&self) -> &'static str {
        self.target.term()
    }

    /// A progress bar and a Stop button, while slices are being mapped. True if
    /// that is what is happening.
    fn progress_ui(&mut self, ui: &mut Ui) -> bool {
        let Some(done) = self.followed else {
            return false;
        };
        let total = self.follow_total.max(done).max(1);
        ui.add(
            egui::ProgressBar::new(done as f32 / total as f32)
                .desired_width(ui.available_width())
                .text(if self.is_3d() {
                    format!("Following: step {} of {total}", (done + 1).min(total))
                } else {
                    format!("{done} of {total} slices")
                }),
        );
        if ui.button("Stop").clicked() {
            if let Some(w) = &self.worker {
                w.cancel.store(true, Ordering::Relaxed);
            }
        }
        true
    }

    /// The range for the target in use.
    fn range(&mut self) -> &mut (f32, f32) {
        let at = Target::ALL.iter().position(|t| *t == self.target).unwrap_or(0);
        &mut self.ranges[at]
    }

    /// The range a 3D model's result is held to.
    fn level(&mut self) -> Option<(f32, f32)> {
        if !self.keep_to_range {
            return None;
        }
        let ((_, top), _) = self.target.scale();
        let (low, high) = *self.range();
        // At the top of the bone scale, nothing is too bright.
        Some((
            low,
            if self.target == Target::Bone && high >= top {
                f32::MAX
            } else {
                high
            },
        ))
    }

    /// Apply a changed level, once the last one has finished, so that dragging the
    /// slider never builds up a queue.
    fn apply_level(&mut self) {
        if !self.level_changed || self.busy || !(self.previewing || self.mapped) {
            return;
        }
        let level = self.level();
        if let Some(w) = &self.worker {
            self.level_changed = false;
            self.busy = true;
            if self.mapped {
                w.cancel.store(false, Ordering::Relaxed);
                self.followed = Some(0);
                self.follow_total = 0;
            }
            let _ = w.requests.send(Request::Level(level));
        }
    }

    /// How far a 3D model's result is held to a range of values: a line from dark to
    /// bright, as on the image, with what the model found drawn over it and a handle
    /// for the bottom and the top of the range.
    fn level_ui(&mut self, ui: &mut Ui) {
        let name = match self.target {
            Target::Bone => "Keep to bone",
            Target::SoftTissue => "Keep to soft tissue",
            Target::Contrast => "Keep to contrast",
        };
        let kept = ui
            .checkbox(&mut self.keep_to_range, name)
            .on_hover_text("Off shows everything the model found");
        self.level_changed |= kept.changed();
        if !self.keep_to_range {
            return;
        }
        let ((low, high), _) = self.target.scale();
        let values = self.values.clone();
        let range = self.range();
        let changed = range_line(ui, range, (low, high), &values);
        self.level_changed |= changed;
        ui.horizontal(|ui| {
            let hover = "Then click a point on an image: this handle goes to that point's value";
            for (top, label) in [(false, "Pick bottom"), (true, "Pick top")] {
                if ui
                    .selectable_label(self.picking_handle == Some(top), label)
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.picking_handle = (self.picking_handle != Some(top)).then_some(top);
                }
            }
        });
    }

    /// A click on an image is waiting to set a handle.
    pub fn picking_handle(&self) -> bool {
        self.picking_handle.is_some()
    }

    /// Set the waiting handle to a point's value. A handle cannot pass the other,
    /// and the point's value is kept just inside the range.
    pub fn pick_handle(&mut self, value: f32) {
        let Some(top) = self.picking_handle.take() else { return };
        let ((low, high), _) = self.target.scale();
        let gap = (high - low) / 50.0;
        let value = value.clamp(low, high);
        let range = self.range();
        if top {
            range.1 = value.max(range.0 + gap);
        } else {
            range.0 = value.min(range.1 - gap);
        }
        self.level_changed = true;
    }

    fn refinement(&self) -> Option<(Edge, usize)> {
        self.refine.then_some((self.target.edge(), self.band))
    }

    /// A click on a slice, at voxel `(x, y, z)`.
    pub fn click(&mut self, x: usize, y: usize, z: usize) {
        if self.is_3d() {
            self.clicks3.push(Click3 {
                x,
                y,
                z,
                include: self.include,
            });
            self.slice = Some(z);
            // Correcting a mapped object goes back to working on the slice. nnInteractive
            // builds on what it has found, so that stays on screen until Follow redraws
            // it. VISTA3D starts afresh, so its old result goes.
            self.restart |= self.mapped && self.manifest().engine == Engine::Vista;
            self.mapped = false;
            let level = self.level();
            if let Some(w) = &self.worker {
                self.busy = true;
                self.previewing = true;
                let bone = self.target == Target::Bone;
                let _ = w.requests.send(Request::Volume(self.clicks3.clone(), bone, level));
            }
            return;
        }
        let Some(limits) = self.limits() else { return };
        let inside = z >= limits.slices.0 && z <= limits.slices.1;
        if !inside || !limits.region.holds(x as f32 + 0.5, y as f32 + 0.5) {
            self.note = Some("That click is outside the box".to_string());
            return;
        }
        self.edge = false;
        self.extent = 0.0;
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
                limits,
                refine: self.refinement(),
            });
        }
    }

    /// Follow the object from the clicked slice through the stack.
    pub fn follow(&mut self) {
        if self.is_3d() {
            if let Some(w) = self.worker.as_ref().filter(|_| self.previewing) {
                w.cancel.store(false, Ordering::Relaxed);
                self.busy = true;
                self.followed = Some(0);
                self.follow_total = 0;
                self.previewing = false;
                self.mapped = true;
                let _ = w.requests.send(Request::Reveal);
            }
            return;
        }
        let Some(limits) = self.limits() else { return };
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
        self.follow_total = limits.slices.1.saturating_sub(limits.slices.0) + 1;
        let _ = w.requests.send(Request::Follow {
            z,
            clicks: self.clicks.clone(),
            window: self.target.window(),
            limits,
            refine: self.refinement(),
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
                Reply::Mask { edge, extent, .. } => {
                    self.edge |= *edge;
                    self.extent = self.extent.max(*extent);
                    if self.followed.is_none() {
                        self.busy = false;
                    }
                }
                Reply::Followed(n) => self.followed = Some(*n),
                Reply::Mapping { done, total } => {
                    self.followed = Some(*done);
                    self.follow_total = *total;
                }
                Reply::Histogram(bins) => self.values = bins.clone(),
                Reply::Preview { seconds, on_gpu } => {
                    self.busy = false;
                    self.on_gpu = *on_gpu;
                    let device = if *on_gpu { "GPU" } else { "CPU" };
                    self.note = Some(format!(
                        "{seconds:.1} s on the {device}. Correct it here, then Follow through slices"
                    ));
                }
                Reply::Done { slices, edge, stopped } => {
                    self.busy = false;
                    self.followed = None;
                    if *stopped {
                        // Follow can finish the job.
                        self.previewing = true;
                        self.mapped = false;
                        self.note = Some(format!("Stopped after {slices} slices"));
                        continue;
                    }
                    let mut note = format!("Mapped through {slices} slices. Check them against the images");
                    if *edge {
                        note.push_str(
                            ". It reaches the side of the model's window, so it may carry on past it: \
                             click nearer that end",
                        );
                    }
                    self.note = Some(note);
                }
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
        self.clicks3.clear();
        self.previewing = false;
        self.mapped = false;
        self.level_changed = false;
        self.picking_handle = None;
        self.values.clear();
        self.slice = None;
        self.label = None;
        if let Some(w) = &self.worker {
            let _ = w.requests.send(Request::NewObject);
        }
    }

    /// Choose a model. A model in research mode is listed with that said.
    fn model_ui(&mut self, ui: &mut Ui) {
        let mut picked = None;
        let current = self.manifest();
        ui.add_enabled_ui(!self.busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Model");
                egui::ComboBox::from_id_salt("ai-model")
                    .selected_text(current.name)
                    .show_ui(ui, |ui| {
                        for m in AVAILABLE.iter().filter(|m| m.offered(self.research_mode)) {
                            let label = if m.research.is_some() {
                                format!("{} (research)", m.name)
                            } else {
                                m.name.to_string()
                            };
                            if ui.selectable_label(m.id == current.id, label).clicked() {
                                picked = Some(m.id);
                            }
                        }
                    });
            });
        });
        if let Some(id) = picked.filter(|id| *id != current.id) {
            self.choose(id);
            self.newly_chosen = Some(id);
        }
    }

    /// How much detail the model will see, and what would improve it.
    fn detail_ui(&mut self, ui: &mut Ui, volume: &Volume) {
        let warn = |ui: &mut Ui, text: String| {
            ui.label(RichText::new(text).small().color(Color32::from_rgb(240, 190, 90)));
        };
        if self.cut_box.is_some() {
            ui.checkbox(&mut self.use_box, "Keep to the cut box")
                .on_hover_text("The model sees only the box, and follows only through its slices");
        }
        let Some(limits) = self.limits() else { return };
        let region = limits.region;
        let boxed = self.use_box && self.cut_box.is_some();
        let cell = region.pixels_per_cell();
        let what = if boxed {
            format!("Box {} \u{d7} {}", region.width, region.height)
        } else {
            "Whole slice".to_string()
        };
        ui.label(RichText::new(format!("{what}: {cell:.1} pixels to each mask cell")).small())
            .on_hover_text(
                "The model stretches what it is given to a square and makes its mask in 256 cells, \
                 so a smaller area means finer detail",
            );
        if !boxed {
            if cell > FINE {
                let what = if self.refine {
                    "A fracture line can be filled in"
                } else {
                    "Thin bone can come out thicker and a fracture line can be filled in"
                };
                warn(
                    ui,
                    format!("{what}. For detail, draw a Cut box around the object in the 3D pane"),
                );
            }
            return;
        }
        let z = self.slice.unwrap_or((limits.slices.0 + limits.slices.1) / 2);
        let empty = match self.emptiness {
            Some((r, s, t, share)) if r == region && s == z && t == self.target => share,
            _ => {
                let share = region.empty_share(&gray_slice(volume, z, self.target.window()), volume.dims[0]);
                self.emptiness = Some((region, z, self.target, share));
                share
            }
        };
        if empty > MOSTLY_EMPTY {
            let percent = (empty * 100.0).round();
            warn(
                ui,
                format!("About {percent}% of this box is empty. A tighter box gives more detail"),
            );
        }
        if region.stretch() > STRETCHED {
            warn(
                ui,
                "This box is long and thin, and the model stretches it square. A squarer box works better".to_string(),
            );
        }
        if self.extent > 0.0 && self.extent < LOOSE {
            let percent = (self.extent * 100.0).round();
            warn(
                ui,
                format!("The object spans only {percent}% of this box. A tighter box gives more detail"),
            );
        }
        if self.edge {
            warn(
                ui,
                "The object reaches the side of the box, so it may be cut off. Make the box larger and click again"
                    .to_string(),
            );
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, root: &Path, volume: &Arc<Volume>) {
        ui.label(RichText::new("AI segment").strong());
        self.model_ui(ui);
        let manifest = self.manifest();
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
        if let Some(terms) = manifest.research {
            if !self.agreed.iter().any(|id| id == manifest.id) {
                ui.label(
                    RichText::new("Research model")
                        .strong()
                        .color(Color32::from_rgb(240, 190, 90)),
                );
                ui.label(RichText::new(terms).small());
                ui.label(RichText::new(format!("Licence: {}", manifest.licence)).small());
                if ui.button("I agree: my use is non-commercial").clicked() {
                    self.agreed.push(manifest.id.to_string());
                    self.newly_agreed = Some(manifest.id);
                }
                return;
            }
        }
        if !manifest.installed(root) {
            ui.label(RichText::new(format!("{} {}", manifest.name, "is not installed")).small());
            ui.label(RichText::new(format!("Licence: {}", manifest.licence)).small());
            ui.label(RichText::new(format!("Source: {}", manifest.source)).small());
            ui.label(RichText::new(manifest.training).small());
            ui.label(
                RichText::new("It runs on this machine. No image data leaves it.")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.horizontal_wrapped(|ui| {
                let bytes = manifest.download_bytes(root);
                if bytes > 0
                    && ui
                        .button(format!("Download {}", megabytes(bytes)))
                        .on_hover_text("From each publisher, each file checked by checksum")
                        .clicked()
                {
                    self.start_install(root);
                }
                if !manifest.has_model(root)
                    && ui
                        .button("From a folder\u{2026}")
                        .on_hover_text("Each file is checked by checksum before it is copied")
                        .clicked()
                {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.note = ai::install_from_folder(&manifest, &folder, root)
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
                .button(format!("Start {}", manifest.name))
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
        self.apply_level();
        let three_d = self.is_3d();
        if !three_d {
            self.detail_ui(ui, volume);
            ui.checkbox(&mut self.refine, "Refine edges").on_hover_text(
                "Keeps the object the model chose and reads its edge from the image's own values. \
                 Applies to the next click or follow. Off by default: on real anatomy it can drop thin bone",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Finding");
            egui::ComboBox::from_id_salt("ai-target")
                .selected_text(self.target.label())
                .show_ui(ui, |ui| {
                    for t in Target::ALL {
                        self.level_changed |= ui.selectable_value(&mut self.target, t, t.label()).changed();
                    }
                });
        });
        if three_d {
            self.level_ui(ui);
        }
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
        if three_d {
            ui.label(
                RichText::new(if self.target == Target::Bone {
                    "Click a structure, and correct it on the slice with Include and Exclude. Finding Bone takes \
                     the nearest bone, and only bone joined to the click"
                } else {
                    "Click a structure, and correct it on the slice with Include and Exclude"
                })
                .small()
                .color(Color32::from_gray(170)),
            );
            if self.progress_ui(ui) {
                return;
            }
            let hover = "Maps the object through the stack, moving the model along as far as the object goes";
            if ui
                .add_enabled(
                    !self.busy && self.previewing,
                    egui::Button::new("Follow through slices"),
                )
                .on_hover_text(hover)
                .clicked()
            {
                self.follow();
            }
            return;
        }
        if self.progress_ui(ui) {
            return;
        }
        if ui
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
