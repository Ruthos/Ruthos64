//! The viewer's side of a guided session, as schema 1 defines it: the
//! controller's frame-by-frame difference in view state becomes coalesced
//! changes, so no input handler holds session code, and a follower renders
//! the shared cells exactly.
//!
//! A cell's viewport travels as the image point at its centre and the
//! visible extent in image pixels rather than a zoom and a pan, which are
//! relative to the sender's screen. Each follower fits that region to its own
//! cell, so a laptop and a 4K monitor look at the same anatomy.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Rect, Ui, Vec2};
use serde_json::{json, Map, Value};

use ruthos_core::RoiRecord;
use ruthos_render::{ViewState, ViewTransform};
use ruthos_session::proto::{round, MAX_SET, SCHEMA};
use ruthos_session::sync::simplify;
use ruthos_session::{Change, Command, SharedModel};

use super::*;
use crate::session::{pointer_command, Role};

/// Measurements are compared this often while nothing is being dragged.
const MEASURE_INTERVAL: Duration = Duration::from_millis(100);
/// Tolerance for freehand paths, in image pixels.
const PATH_TOLERANCE: f32 = 0.25;

/// What the viewer remembers about the session.
#[derive(Default)]
pub struct SessionView {
    pub following: bool,
    pub role: Option<Role>,
    /// What the relay has been told, by cell.
    sent_layout: Map<String, Value>,
    sent_cells: BTreeMap<usize, Map<String, Value>>,
    /// Measurements published, by id: their JSON.
    sent_measures: HashMap<String, Value>,
    sent_pointer: Option<(usize, [f32; 2])>,
    measured_at: Option<Instant>,
    rebuild_seen: u64,
    /// Other people's measurements, drawn over this viewer's own.
    pub overlay: HashMap<FrameKey, Vec<(Roi, Color32)>>,
    pub pointer: Option<(usize, [f32; 2], String)>,
    pub note: Option<String>,
}

/// The viewport of a cell, in the image's own terms.
pub fn viewport_of(t: &ViewTransform) -> ([f32; 2], [f32; 2]) {
    let c = t.screen_to_image(t.rect.center());
    let zoom = t.state.zoom.max(1e-6);
    ([c.x, c.y], [t.rect.width() / zoom, t.rect.height() / zoom])
}

/// Fit a shared viewport to this cell: the whole region stays in view, with
/// `centre` at the middle of `rect`.
pub fn fit_viewport(view: &mut ViewState, rect: Rect, w: u32, h: u32, aspect: f32, centre: [f32; 2], extent: [f32; 2]) {
    view.zoom = (rect.width() / extent[0].max(1e-3)).min(rect.height() / extent[1].max(1e-3));
    view.pan = Vec2::ZERO;
    let at = ViewTransform::new(rect, w, h, aspect, *view).image_to_screen(Vec2::new(centre[0], centre[1]));
    view.pan = rect.center() - at;
}

fn flip_word(h: bool, v: bool) -> &'static str {
    match (h, v) {
        (false, false) => "none",
        (true, false) => "h",
        (false, true) => "v",
        (true, true) => "hv",
    }
}

fn number(m: &Map<String, Value>, key: &str) -> Option<f32> {
    m.get(key).and_then(Value::as_f64).map(|v| v as f32)
}

fn pair(m: &Map<String, Value>, key: &str) -> Option<[f32; 2]> {
    let a = m.get(key)?.as_array()?;
    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
}

/// A colour for an author, the same on every machine.
fn author_colour(sub: &str) -> Color32 {
    const COLOURS: [Color32; 6] = [
        Color32::from_rgb(120, 200, 255),
        Color32::from_rgb(255, 170, 90),
        Color32::from_rgb(150, 230, 140),
        Color32::from_rgb(240, 130, 190),
        Color32::from_rgb(200, 170, 255),
        Color32::from_rgb(250, 225, 110),
    ];
    let n: u32 = sub.bytes().fold(0, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    COLOURS[n as usize % COLOURS.len()]
}

/// A shared measurement, for drawing.
fn roi_of(m: &Map<String, Value>) -> Option<Roi> {
    let points = m
        .get("points")?
        .as_array()?
        .iter()
        .map(|p| {
            let p = p.as_array()?;
            Some([p.first()?.as_f64()? as f32, p.get(1)?.as_f64()? as f32])
        })
        .collect::<Option<Vec<[f32; 2]>>>()?;
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
    Roi::from_record(&RoiRecord {
        id: 0,
        image_id: 0,
        frame: 0,
        kind: text("kind")?,
        points,
        name: text("label"),
        value: None,
        unit: String::new(),
        basis: None,
        def_key: text("def"),
    })
}

impl ViewerState {
    /// Each frame, before input: follow, or publish.
    pub(super) fn session_sync(&mut self, ui: &Ui, shared: &Shared) {
        let (role, sub, model, target, rebuild, pointer, drags) = {
            let hub = shared.session.lock();
            let focused = shared.focused_viewer.load(std::sync::atomic::Ordering::Relaxed);
            let target = match hub.role() {
                Role::Controller => focused == self.id || (focused == 0 && hub.viewer == Some(self.id)),
                _ => hub.viewer == Some(self.id),
            };
            let controller = hub
                .presence
                .members
                .iter()
                .find(|m| Some(&m.sub) == hub.presence.control.as_ref())
                .map(|m| m.name.clone())
                .unwrap_or_default();
            (
                hub.role(),
                hub.sub.clone(),
                hub.model.clone(),
                target,
                hub.rebuild,
                hub.pointer.map(|g| (g.cell as usize, g.position(), controller)),
                hub.drags.clone(),
            )
        };
        let was_active = self.session.role.is_some_and(|r| r != Role::None);
        self.session.role = Some(role);
        if role == Role::None {
            if was_active {
                self.session = SessionView::default();
            }
            return;
        }
        self.session.following = role == Role::Follower && target;
        self.session.pointer = if role == Role::Follower { pointer } else { None };
        let Some(model) = model else { return };
        if role == Role::Controller {
            if target {
                // The first frame after gaining control or a rollback starts
                // from what the relay holds, not from the last thing sent.
                if rebuild != self.session.rebuild_seen {
                    self.session.rebuild_seen = rebuild;
                    self.session.sent_layout = model.layout.clone();
                    self.session.sent_cells = model.cells.iter().map(|(k, v)| (*k as usize, v.clone())).collect();
                    self.follow(&model, shared);
                }
                self.publish(ui, shared, sub.as_deref());
            }
        } else if self.session.following {
            if !model.understood() {
                self.session.note = Some("This session needs a newer Ruthos".to_string());
                return;
            }
            self.session.note = None;
            self.follow(&model, shared);
        }
        self.overlay_measurements(&model, &drags);
    }

    fn shared_cell(&self, k: usize, shared: &Shared, reference: Option<&str>) -> Option<Map<String, Value>> {
        let p = self.panes.get(k)?;
        let key = p.key()?;
        let image = p.image()?;
        let frame = self.frames.get(&key)?;
        let rect = *self.cell_rects.get(k)?;
        let t = ViewTransform::new(rect, frame.width, frame.height, aspect_of(frame), p.view);
        let (centre, extent) = viewport_of(&t);
        let v = &p.view;
        let playing = self.playing && k == self.active;
        let mut m = Map::new();
        m.insert("case".into(), json!(reference));
        m.insert(
            "series".into(),
            json!(self.series.get(p.series_idx)?.series_instance_uid),
        );
        m.insert("instance".into(), json!(image.sop_instance_uid));
        // A follower plays the loop itself, so frames stay out of the stream.
        if !playing {
            m.insert("frame".into(), json!(p.frame_idx));
        }
        m.insert("wc".into(), json!(round(v.window_center, 1)));
        m.insert("ww".into(), json!(round(v.window_width, 1)));
        let lut = shared.cluts.get(&p.clut).map(|_| p.clut.clone());
        m.insert("lut".into(), json!(lut));
        m.insert("invert".into(), json!(frame.inverted));
        m.insert("rotate".into(), json!(v.rotation_deg));
        m.insert("flip".into(), json!(flip_word(v.flip_horizontal, v.flip_vertical)));
        m.insert("centre".into(), json!([round(centre[0], 2), round(centre[1], 2)]));
        m.insert("extent".into(), json!([round(extent[0], 2), round(extent[1], 2)]));
        m.insert("cine".into(), json!({"playing": playing, "fps": round(self.fps, 1)}));
        Some(m)
    }

    /// Send what differs from what was last sent.
    fn publish(&mut self, ui: &Ui, shared: &Shared, sub: Option<&str>) {
        // Only a teaching copy leaves the machine: anything else carries the
        // study's real identifiers.
        let Some(reference) = self.study.teaching_reference.clone() else {
            self.session.note = Some("Open a teaching copy to share it in the session".to_string());
            return;
        };
        self.session.note = None;
        let mut layout = Map::new();
        layout.insert("schema".into(), json!(SCHEMA));
        layout.insert("rows".into(), json!(self.tiling.rows));
        layout.insert("cols".into(), json!(self.tiling.cols));
        layout.insert("case".into(), json!(reference));
        layout.insert("study".into(), json!(self.study.study_instance_uid));
        let diff = difference(&self.session.sent_layout, &layout);
        let mut outgoing = Vec::new();
        if !diff.is_empty() {
            outgoing.push(Change::Layout(diff));
            self.session.sent_layout = layout;
        }
        for k in 0..self.panes.len() {
            let Some(cell) = self.shared_cell(k, shared, Some(&reference)) else {
                continue;
            };
            let sent = self.session.sent_cells.entry(k).or_default();
            let diff = difference(sent, &cell);
            if !diff.is_empty() {
                outgoing.push(Change::Cell {
                    cell: k as u8,
                    set: diff,
                });
                *sent = cell;
            }
        }
        if !outgoing.is_empty() {
            let hub = shared.session.lock();
            for c in outgoing {
                hub.send(Command::Change(c));
            }
        }
        self.publish_pointer(ui, shared);
        self.publish_measurements(shared, sub);
    }

    fn publish_pointer(&mut self, ui: &Ui, shared: &Shared) {
        let hover = ui.input(|i| i.pointer.hover_pos());
        let now = hover.and_then(|pos| {
            let k = self.cell_rects.iter().position(|r| r.contains(pos))?;
            let p = self.panes.get(k)?;
            let frame = self.frames.get(&p.key()?)?;
            let t = ViewTransform::new(self.cell_rects[k], frame.width, frame.height, aspect_of(frame), p.view);
            let q = t.screen_to_image(pos);
            Some((k, [q.x, q.y]))
        });
        let moved = match (self.session.sent_pointer, now) {
            (Some((a, p)), Some((b, q))) => a != b || (p[0] - q[0]).abs() + (p[1] - q[1]).abs() > 0.25,
            (None, None) => false,
            _ => true,
        };
        if moved {
            self.session.sent_pointer = now;
            shared.session.lock().send(pointer_command(now));
        }
    }

    fn publish_measurements(&mut self, shared: &Shared, sub: Option<&str>) {
        if let Some(d) = &self.handle_drag {
            let (index, handle) = (d.index, d.handle);
            if let Some(key) = self.current_key() {
                let roi = self.rois.get(&key).and_then(|l| l.get(index));
                if let Some((id, p)) = roi.and_then(|r| Some((r.shared_id.clone()?, *r.handles().get(handle)?))) {
                    shared.session.lock().send(Command::Drag {
                        id,
                        i: handle as u16,
                        x: p.x,
                        y: p.y,
                    });
                }
            }
            return;
        }
        if self.move_drag.is_some() || self.measured_recently() {
            return;
        }
        self.session.measured_at = Some(Instant::now());
        let mut live = HashMap::new();
        let mut too_large = false;
        let answer_key = self.study.teaching_answer_key;
        let keys: Vec<FrameKey> = self.rois.keys().copied().collect();
        for key in keys {
            // The sop UID and the cell come from whichever cell shows it.
            let Some((cell, sop)) = self.panes.iter().enumerate().find_map(|(k, p)| {
                p.images
                    .iter()
                    .find(|i| i.id == key.image_id)
                    .map(|i| (k, i.sop_instance_uid.clone()))
            }) else {
                continue;
            };
            let frame = self.frames.get(&key).cloned();
            let Some(list) = self.rois.get_mut(&key) else { continue };
            for roi in list.iter_mut() {
                // What was already in the archive is on everyone's copy.
                if roi.from_archive {
                    continue;
                }
                let id = roi
                    .shared_id
                    .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
                    .clone();
                let r = roi.to_record(key.image_id, key.frame, frame.as_deref());
                let points: Vec<[f32; 2]> = if matches!(r.kind.as_str(), "pencil" | "polygon") {
                    simplify(&r.points, PATH_TOLERANCE)
                } else {
                    r.points.clone()
                };
                let points: Vec<Value> = points.iter().map(|p| json!([round(p[0], 2), round(p[1], 2)])).collect();
                let author = sub.unwrap_or_default();
                let v = json!({
                    "kind": r.kind, "cell": cell, "instance": sop, "frame": key.frame,
                    "points": points, "def": r.def_key, "label": r.name, "value": r.value,
                    "unit": r.unit, "basis": r.basis, "author": author, "key": answer_key,
                });
                if v.to_string().len() > MAX_SET {
                    too_large = true;
                    continue;
                }
                live.insert(id, v);
            }
        }
        if too_large {
            self.session.note = Some("A measurement is too detailed to share in the session".to_string());
        }
        let mut outgoing = Vec::new();
        for (id, v) in &live {
            if self.session.sent_measures.get(id) != Some(v) {
                if let Some(set) = v.as_object() {
                    outgoing.push(Change::MeasUpsert {
                        id: id.clone(),
                        set: set.clone(),
                    });
                }
            }
        }
        for id in self.session.sent_measures.keys() {
            if !live.contains_key(id) {
                outgoing.push(Change::MeasDelete { id: id.clone() });
            }
        }
        self.session.sent_measures = live;
        if !outgoing.is_empty() {
            let hub = shared.session.lock();
            for c in outgoing {
                hub.send(Command::Change(c));
            }
        }
    }

    fn measured_recently(&self) -> bool {
        self.session.measured_at.is_some_and(|t| t.elapsed() < MEASURE_INTERVAL)
    }

    /// Make the screen match the shared cells.
    fn follow(&mut self, model: &SharedModel, shared: &Shared) {
        let study = model.layout.get("study").and_then(Value::as_str);
        if study.is_some_and(|uid| uid != self.study.study_instance_uid) {
            return;
        }
        let dim = |k: &str| model.layout.get(k).and_then(Value::as_u64).map(|v| v as usize);
        if let (Some(rows), Some(cols)) = (dim("rows"), dim("cols")) {
            let wanted = Tiling {
                mode: TilingMode::Study,
                rows: rows.max(1),
                cols: cols.max(1),
            };
            if wanted.cells() != self.panes.len() || wanted.rows != self.tiling.rows {
                self.set_tiling(shared, wanted);
                while self.panes.len() < wanted.cells() {
                    let p = self.panes[0].clone();
                    self.panes.push(p);
                }
            }
        }
        for (n, cell) in &model.cells {
            self.follow_cell(*n as usize, cell, shared);
        }
    }

    fn follow_cell(&mut self, k: usize, cell: &Map<String, Value>, shared: &Shared) {
        if k >= self.panes.len() {
            return;
        }
        if let Some(uid) = cell.get("series").and_then(Value::as_str) {
            let Some(idx) = self.series.iter().position(|s| s.series_instance_uid == uid) else {
                return;
            };
            if self.panes[k].series_idx != idx || self.panes[k].images.is_empty() {
                let series = self.series[idx].clone();
                self.panes[k] = Pane::new(shared, &series, idx);
            }
        }
        let series_id = self.series.get(self.panes[k].series_idx).map(|s| s.id);
        let p = &mut self.panes[k];
        if let Some(sop) = cell.get("instance").and_then(Value::as_str) {
            if let Some(i) = p.images.iter().position(|i| i.sop_instance_uid == sop) {
                p.image_idx = i;
            }
        }
        if let Some(f) = cell.get("frame").and_then(Value::as_u64) {
            p.frame_idx = f as u32;
        }
        if let (Some(wc), Some(ww)) = (number(cell, "wc"), number(cell, "ww")) {
            p.view.window_center = wc;
            p.view.window_width = ww;
            // Whatever the series stored, the shared window stands.
            p.window_initialised_for = series_id;
        }
        p.clut = match cell.get("lut").and_then(Value::as_str) {
            Some(name) => name.to_string(),
            None => String::new(),
        };
        if let Some(r) = number(cell, "rotate") {
            p.view.rotation_deg = r;
        }
        if let Some(f) = cell.get("flip").and_then(Value::as_str) {
            p.view.flip_horizontal = f.contains('h');
            p.view.flip_vertical = f.contains('v');
        }
        if let Some(c) = cell.get("cine") {
            if k == self.active {
                self.playing = c.get("playing").and_then(Value::as_bool).unwrap_or(false);
                if let Some(fps) = c.get("fps").and_then(Value::as_f64) {
                    self.fps = fps as f32;
                }
            }
        }
        // The viewport needs the frame's size and aspect, so it waits for
        // the decode; it is applied every frame, as the window can resize.
        let (Some(centre), Some(extent)) = (pair(cell, "centre"), pair(cell, "extent")) else {
            return;
        };
        let p = &mut self.panes[k];
        let Some(key) = p.key() else { return };
        let (Some(frame), Some(rect)) = (self.frames.get(&key), self.cell_rects.get(k)) else {
            return;
        };
        fit_viewport(
            &mut p.view,
            *rect,
            frame.width,
            frame.height,
            aspect_of(frame),
            centre,
            extent,
        );
        p.fit_pending = false;
    }

    /// Other people's measurements, drawn over this viewer's own.
    fn overlay_measurements(&mut self, model: &SharedModel, drags: &HashMap<(String, u16), [f32; 2]>) {
        self.session.overlay.clear();
        let own: Vec<String> = self
            .rois
            .values()
            .flatten()
            .filter_map(|r| r.shared_id.clone())
            .collect();
        for (id, m) in &model.measurements {
            if own.contains(id) {
                continue;
            }
            let hidden = m.get("key").and_then(Value::as_bool) == Some(true) && !model.revealed.contains(id);
            if hidden {
                continue;
            }
            let Some(sop) = m.get("instance").and_then(Value::as_str) else {
                continue;
            };
            let frame = m.get("frame").and_then(Value::as_u64).unwrap_or(0) as u32;
            let image = self
                .panes
                .iter()
                .flat_map(|p| p.images.iter())
                .find(|i| i.sop_instance_uid == sop);
            let (Some(image), Some(mut roi)) = (image, roi_of(m)) else {
                continue;
            };
            for ((drag_id, i), p) in drags {
                if drag_id == id {
                    roi.set_handle(*i as usize, Vec2::new(p[0], p[1]));
                }
            }
            let colour = author_colour(m.get("author").and_then(Value::as_str).unwrap_or_default());
            let key = FrameKey {
                image_id: image.id,
                frame,
            };
            self.session.overlay.entry(key).or_default().push((roi, colour));
        }
    }

    /// The controller's pointer and the session note, over a cell.
    pub(super) fn draw_session(&self, painter: &egui::Painter, t: &ViewTransform, k: usize) {
        if k == 0 {
            let line = self
                .session
                .note
                .as_deref()
                .or(self.session.following.then_some("Following the session"));
            if let Some(line) = line {
                crate::widgets::shadow_text(
                    painter,
                    t.rect.center_top() + Vec2::new(0.0, 24.0),
                    Align2::CENTER_TOP,
                    line,
                    FontId::proportional(13.0),
                    Color32::from_rgb(240, 190, 90),
                );
            }
        }
        if let Some((cell, p, name)) = &self.session.pointer {
            if *cell == k {
                let at = t.image_to_screen(Vec2::new(p[0], p[1]));
                let colour = Color32::from_rgb(120, 200, 255);
                painter.circle_filled(at, 4.0, colour);
                painter.text(
                    at + Vec2::new(8.0, 8.0),
                    Align2::LEFT_TOP,
                    name,
                    FontId::proportional(12.0),
                    colour,
                );
            }
        }
    }
}

fn aspect_of(frame: &Frame) -> f32 {
    frame
        .meta
        .pixel_spacing
        .map(|[row, col]| if col > 0.0 { (row / col) as f32 } else { 1.0 })
        .unwrap_or(1.0)
}

/// The keys of `new` that differ from `old`, with a null for each key gone.
fn difference(old: &Map<String, Value>, new: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in new {
        if old.get(k) != Some(v) {
            out.insert(k.clone(), v.clone());
        }
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            out.insert(k.clone(), Value::Null);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;

    fn state(zoom: f32, pan: Vec2, rotation_deg: f32) -> ViewState {
        ViewState {
            zoom,
            pan,
            rotation_deg,
            ..Default::default()
        }
    }

    /// The acceptance check's two screens: a point on the sender's screen
    /// and the same image point on the follower's are the same anatomy.
    #[test]
    fn the_same_region_shows_on_any_screen() {
        let big = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let small = Rect::from_min_size(Pos2::new(10.0, 40.0), Vec2::new(683.0, 384.0));
        for rotation in [0.0, 90.0, 180.0, 270.0] {
            let sender = ViewTransform::new(big, 800, 600, 1.0, state(3.0, Vec2::new(-120.0, 75.0), rotation));
            let (centre, extent) = viewport_of(&sender);
            let mut view = state(1.0, Vec2::ZERO, rotation);
            fit_viewport(&mut view, small, 800, 600, 1.0, centre, extent);
            let follower = ViewTransform::new(small, 800, 600, 1.0, view);
            // The centres agree.
            let c = follower.screen_to_image(small.center());
            assert!(
                (c.x - centre[0]).abs() < 0.01 && (c.y - centre[1]).abs() < 0.01,
                "{rotation}"
            );
            // Every corner of the sender's region stays on the follower's screen.
            for corner in [big.left_top(), big.right_bottom()] {
                let p = sender.screen_to_image(corner);
                let q = follower.image_to_screen(p);
                assert!(small.expand(1.0).contains(q), "{rotation}: {q:?}");
            }
            // A caliper end lands on the same image point.
            let end = sender.screen_to_image(Pos2::new(700.0, 500.0));
            let back = follower.screen_to_image(follower.image_to_screen(end));
            assert!((end - back).length() < 0.01);
        }
    }

    #[test]
    fn differences_carry_only_what_changed_and_null_what_went() {
        let old = json!({"a": 1, "b": 2}).as_object().unwrap().clone();
        let new = json!({"a": 1, "b": 3, "c": 4}).as_object().unwrap().clone();
        assert_eq!(Value::Object(difference(&old, &new)), json!({"b": 3, "c": 4}));
        let gone = json!({"a": 1}).as_object().unwrap().clone();
        assert_eq!(Value::Object(difference(&old, &gone)), json!({"b": null}));
    }

    #[test]
    fn a_shared_measurement_can_be_drawn() {
        let m = json!({"kind": "length", "points": [[1.0, 2.0], [3.0, 4.0]], "label": "LVIDd"});
        let roi = roi_of(m.as_object().unwrap()).unwrap();
        assert_eq!(roi.name.as_deref(), Some("LVIDd"));
    }
}
