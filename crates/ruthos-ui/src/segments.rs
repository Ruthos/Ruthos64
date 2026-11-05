//! The Segments panel of the MPR window. Segmentation happens here, where a
//! user can see a boundary on a slice and correct it, and the 3D pane shows
//! the result. Every tool is an assistant: nothing is segmented unless the
//! user asks, and each result is an ordinary segment that can be edited.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, RichText, Ui};
use rayon::prelude::*;

use ruthos_core::volume::{Plane, Volume};
use ruthos_segment::mesh::{passes, surface_nets, Smoothing};
use ruthos_segment::tools::{self, Grow};
use ruthos_segment::{Field, Mask, Provenance, Segmentation, Statistics};

/// Told apart at a glance, on black and on bone.
const COLOURS: [[u8; 3]; 8] = [
    [240, 200, 120],
    [90, 170, 240],
    [240, 110, 100],
    [120, 210, 130],
    [200, 130, 230],
    [250, 160, 60],
    [90, 210, 210],
    [230, 120, 180],
];

/// At most this many pieces become segments in one split; the rest stay in
/// the segment being split.
const MOST_PIECES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Morphology {
    Close,
    Open,
    Dilate,
    Erode,
    FillHoles,
    SliceGaps,
}

/// Build a mesh and write it, reporting what came out. A long job on a large
/// segment, so it runs apart from the window.
fn export_stl(
    mask: Mask,
    strength: Smoothing,
    volume: &Volume,
    note: &str,
    path: &std::path::Path,
) -> Result<String, String> {
    let mut mesh = surface_nets(&mask);
    if mesh.triangles.is_empty() {
        return Err("That segment is empty".to_string());
    }
    let sound = mesh.soundness();
    mesh.taubin(passes(strength));
    let size = mesh.extent();
    mesh.to_patient(volume.origin, volume.axes);
    std::fs::write(path, mesh.to_stl(note)).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let printable = if sound.closed_and_manifold() {
        "closed".to_string()
    } else {
        format!(
            "not closed: {} open and {} shared edges to repair before printing",
            sound.open_edges, sound.nonmanifold_edges
        )
    };
    Ok(format!(
        "{name}: {:.0} x {:.0} x {:.0} mm, {} triangles, {printable}",
        size[0],
        size[1],
        size[2],
        mesh.triangles.len()
    ))
}

fn smoothing_label(s: Smoothing) -> &'static str {
    match s {
        Smoothing::None => "None",
        Smoothing::Light => "Light",
        Smoothing::Medium => "Medium",
        Smoothing::Strong => "Strong",
    }
}

enum ExportTo {
    File(std::path::PathBuf),
    Folder(std::path::PathBuf),
}

/// A file name from a segment's name.
fn file_stem(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

pub struct SegmentPanel {
    pub open: bool,
    pub seg: Option<Segmentation>,
    pub active: Option<u8>,
    /// Region grow is waiting for a click on a slice.
    pub picking: bool,
    /// Bumped on every change, which redraws the overlays and the 3D pane.
    pub generation: u64,
    /// In the 3D pane, show nothing but segments.
    pub only_segments: bool,
    /// The series and date, written into what is exported.
    source: String,
    low: f32,
    high: f32,
    gradient: f32,
    radius_mm: f64,
    stats: HashMap<u8, Statistics>,
    undo: Option<Segmentation>,
    made: u32,
    pub note: Option<String>,
    smoothing: Smoothing,
    /// Exports running, each reporting when it is done.
    exports: Vec<crossbeam_channel::Receiver<Result<String, String>>>,
    #[cfg(feature = "ai")]
    pub ai: crate::segments_ai::AiPanel,
    #[cfg(feature = "ai")]
    dev_followed: bool,
}

impl Default for SegmentPanel {
    fn default() -> Self {
        Self {
            open: false,
            seg: None,
            active: None,
            picking: false,
            generation: 0,
            only_segments: false,
            source: String::new(),
            low: 200.0,
            high: 3000.0,
            gradient: 300.0,
            radius_mm: 1.0,
            stats: HashMap::new(),
            undo: None,
            made: 0,
            note: None,
            smoothing: Smoothing::Medium,
            exports: Vec::new(),
            #[cfg(feature = "ai")]
            ai: Default::default(),
            #[cfg(feature = "ai")]
            dev_followed: false,
        }
    }
}

impl SegmentPanel {
    fn changed(&mut self, volume: &Volume) {
        self.generation += 1;
        self.stats.clear();
        if let Some(seg) = &self.seg {
            for s in &seg.segments {
                if let Some(st) = seg.statistics(s.label, volume) {
                    self.stats.insert(s.label, st);
                }
            }
        }
    }

    fn remember(&mut self) {
        self.undo = self.seg.clone();
    }

    /// Add a mask as a new segment and make it the active one.
    pub fn add(&mut self, volume: &Volume, name: &str, tool: &str, mask: &Mask) {
        if mask.count() == 0 {
            self.note = Some("That found nothing".to_string());
            return;
        }
        self.remember();
        let grid = mask.grid;
        let colour = COLOURS[self.made as usize % COLOURS.len()];
        self.made += 1;
        let seg = self.seg.get_or_insert_with(|| Segmentation::new(grid));
        let provenance = Provenance::SemiAutomatic { tool: tool.to_string() };
        match seg.add(&format!("{name} {}", self.made), colour, provenance, mask) {
            Some(label) => self.active = Some(label),
            None => self.note = Some("Every segment label is in use".to_string()),
        }
        self.changed(volume);
    }

    /// Grow a new segment from a voxel chosen on a slice.
    pub fn grow_from(&mut self, volume: &Volume, seed: usize) {
        self.picking = false;
        let g = Grow {
            low: self.low,
            high: self.high,
            gradient: self.gradient,
        };
        let v = volume.at(seed);
        if v > g.high || v < g.low - g.gradient {
            self.note = Some(format!("That point is {v:.0}, outside the range"));
            return;
        }
        let mask = tools::region_grow(volume, &[seed], g);
        self.add(volume, "Region", "Region grow", &mask);
    }

    fn replace_active(&mut self, volume: &Volume, mask: Mask) {
        let (Some(label), Some(seg)) = (self.active, self.seg.as_mut()) else {
            return;
        };
        let everything = seg.mask(label);
        seg.erase(label, &everything);
        seg.paint(label, &mask);
        self.changed(volume);
    }

    fn morph(&mut self, volume: &Volume, how: Morphology) {
        let (Some(label), Some(seg)) = (self.active, self.seg.as_ref()) else {
            return;
        };
        let mask = seg.mask(label);
        let r = self.radius_mm;
        let result = match how {
            Morphology::Close => tools::close(&mask, r),
            Morphology::Open => tools::open(&mask, r),
            Morphology::Dilate => tools::dilate(&mask, r),
            Morphology::Erode => tools::erode(&mask, r),
            Morphology::FillHoles => tools::fill_holes(&mask),
            Morphology::SliceGaps => tools::fill_slice_gaps(&mask),
        };
        self.remember();
        self.replace_active(volume, result);
    }

    /// For the developer option.
    pub fn split_now(&mut self, volume: &Volume) {
        self.split_active(volume);
    }

    /// Write segments as STL, in patient coordinates, each file naming its
    /// source and the tool that made it.
    fn start_export(&mut self, volume: &Arc<Volume>, labels: Vec<u8>, to: ExportTo) {
        let Some(seg) = self.seg.as_ref() else { return };
        for label in labels {
            let Some(s) = seg.segments.iter().find(|s| s.label == label) else {
                continue;
            };
            let tool = match &s.provenance {
                Provenance::Manual => "manual".to_string(),
                Provenance::SemiAutomatic { tool } => tool.clone(),
                Provenance::Automatic { model, version } => format!("AI-assisted, {model} {version}"),
            };
            let note = format!("{}, {}, {tool}. Planning and education only", self.source, s.name);
            let path = match &to {
                ExportTo::File(p) => p.clone(),
                ExportTo::Folder(d) => d.join(format!("{}.stl", file_stem(&s.name))),
            };
            let (mask, strength, v) = (seg.mask(label), self.smoothing, volume.clone());
            let (tx, rx) = crossbeam_channel::bounded(1);
            self.exports.push(rx);
            std::thread::spawn(move || {
                let _ = tx.send(export_stl(mask, strength, &v, &note, &path));
            });
        }
        self.note = Some("Exporting\u{2026}".to_string());
    }

    /// Each fragment becomes its own segment, with its own colour and volume.
    fn split_active(&mut self, volume: &Volume) {
        let (Some(label), Some(seg)) = (self.active, self.seg.as_ref()) else {
            return;
        };
        let name = seg
            .segments
            .iter()
            .find(|s| s.label == label)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let pieces = tools::split(&seg.mask(label));
        if pieces.len() < 2 {
            self.note = Some("This segment is already one piece".to_string());
            return;
        }
        self.remember();
        let grid = pieces[0].grid;
        let found = pieces.len();
        for piece in pieces.iter().skip(1).take(MOST_PIECES) {
            let colour = COLOURS[self.made as usize % COLOURS.len()];
            self.made += 1;
            let seg = self.seg.get_or_insert_with(|| Segmentation::new(grid));
            let provenance = Provenance::SemiAutomatic {
                tool: "Islands".to_string(),
            };
            if seg
                .add(&format!("{name}, piece {}", self.made), colour, provenance, piece)
                .is_none()
            {
                break;
            }
        }
        self.note = Some(format!("{found} pieces found"));
        self.changed(volume);
    }

    /// True when an AI-assisted segment is present, which the 3D pane says.
    pub fn has_ai(&self) -> bool {
        self.seg
            .iter()
            .flat_map(|s| s.segments.iter())
            .any(|s| matches!(s.provenance, Provenance::Automatic { .. }))
    }

    /// True while the model is writing, when the 3D pane waits for the end
    /// rather than taking the whole label map after every slice.
    pub fn ai_busy(&self) -> bool {
        #[cfg(feature = "ai")]
        return self.ai.busy;
        #[cfg(not(feature = "ai"))]
        false
    }

    /// Developer option: `RUTHOS_MPR_AI=x,y,z` clicks that voxel and follows
    /// through the stack. True until it has finished.
    #[cfg(feature = "ai")]
    pub fn dev_option(&mut self, volume: &Arc<Volume>, root: &std::path::Path) -> bool {
        let Ok(text) = std::env::var("RUTHOS_MPR_AI") else {
            return false;
        };
        let v: Vec<usize> = text.split(',').filter_map(|n| n.parse().ok()).collect();
        let [x, y, z] = v[..] else { return false };
        self.open = true;
        if !self.ai.started() {
            self.ai.start_worker(volume, root);
            return true;
        }
        if !self.ai.ready {
            return true;
        }
        if self.ai.clicks.is_empty() {
            self.ai.include = true;
            self.ai.click(x, y, z);
            return true;
        }
        if self.ai.busy {
            return true;
        }
        if self.dev_followed {
            return false;
        }
        self.dev_followed = true;
        self.ai.follow();
        true
    }

    /// Whether a click on a slice belongs to the model.
    #[cfg(feature = "ai")]
    pub fn ai_picking(&self) -> bool {
        self.ai.picking && self.ai.ready
    }

    /// Write what the model has sent. Overlays follow as it goes; the
    /// statistics wait for the end, since they read the whole volume.
    #[cfg(feature = "ai")]
    pub fn pump_ai(&mut self, volume: &Volume) {
        use crate::segments_ai::Reply;
        if std::mem::take(&mut self.ai.restart) {
            if let (Some(label), Some(seg)) = (self.ai.label, self.seg.as_mut()) {
                seg.labels.iter_mut().filter(|l| **l == label).for_each(|l| *l = 0);
            }
        }
        for reply in self.ai.take_replies() {
            match reply {
                Reply::Mask { z, mask, .. } => self.write_slice(volume, z, &mask),
                Reply::Finished(_) => self.changed(volume),
                Reply::Failed(e) => self.note = Some(e),
                _ => {}
            }
        }
    }

    /// One slice of the model's object, in its segment.
    #[cfg(feature = "ai")]
    fn write_slice(&mut self, volume: &Volume, z: usize, mask: &[bool]) {
        let [w, h, _] = volume.dims;
        let grid = ruthos_segment::Grid {
            dims: volume.dims,
            spacing: volume.spacing,
        };
        if self.ai.label.is_none() {
            if !mask.iter().any(|v| *v) {
                return;
            }
            self.remember();
            let colour = COLOURS[self.made as usize % COLOURS.len()];
            self.made += 1;
            let manifest = ruthos_segment::ai::SAM2_TINY;
            let provenance = Provenance::Automatic {
                model: manifest.name.to_string(),
                version: manifest.version.to_string(),
            };
            let name = format!("{} {}", manifest.name, self.made);
            let seg = self.seg.get_or_insert_with(|| Segmentation::new(grid));
            self.ai.label = seg.add(&name, colour, provenance, &Mask::empty(grid));
            self.active = self.ai.label;
        }
        let (Some(label), Some(seg)) = (self.ai.label, self.seg.as_mut()) else {
            return;
        };
        let start = w * h * z;
        for (out, on) in seg.labels[start..start + w * h].iter_mut().zip(mask) {
            if *on {
                *out = label;
            } else if *out == label {
                *out = 0;
            }
        }
        self.generation += 1;
    }

    /// The panel, which scrolls as the list grows. A change bumps `generation`.
    pub fn ui(&mut self, ui: &mut Ui, volume: &Arc<Volume>, root: &std::path::Path, source: &str) {
        self.source = source.to_string();
        self.exports.retain(|job| match job.try_recv() {
            Ok(result) => {
                self.note = Some(match result {
                    Ok(done) => done,
                    Err(e) => format!("Not exported: {e}"),
                });
                false
            }
            Err(crossbeam_channel::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        if !self.exports.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            #[cfg(feature = "ai")]
            {
                self.ai.ui(ui, root, volume);
                ui.separator();
            }
            #[cfg(not(feature = "ai"))]
            let _ = root;
            self.contents(ui, volume)
        });
    }

    fn contents(&mut self, ui: &mut Ui, volume: &Arc<Volume>) {
        ui.label(RichText::new("Segments").strong());
        ui.label(
            RichText::new("Tools assist: check every result against the slices.")
                .small()
                .color(Color32::from_gray(150)),
        );
        if let Some(n) = self.note.clone() {
            ui.label(RichText::new(n).small().color(Color32::from_rgb(240, 190, 90)));
        }
        ui.separator();
        ui.label(RichText::new("Value range").strong());
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut self.low).speed(5.0).prefix("from "));
            ui.add(egui::DragValue::new(&mut self.high).speed(5.0).prefix("to "));
        });
        ui.horizontal(|ui| {
            if ui.small_button("Bone").clicked() {
                (self.low, self.high) = (200.0, 3000.0);
            }
            if ui.small_button("Contrast").clicked() {
                (self.low, self.high) = (130.0, 600.0);
            }
            if ui.small_button("Implant").clicked() {
                (self.low, self.high) = (3000.0, volume.max.max(3001.0));
            }
        });
        if ui
            .button("Threshold")
            .on_hover_text("Everything in the range")
            .clicked()
        {
            let mask = tools::threshold(volume.as_ref(), self.low, self.high);
            self.add(volume, "Threshold", "Threshold", &mask);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Gradient limit");
            ui.add(egui::DragValue::new(&mut self.gradient).speed(5.0).range(1.0..=10000.0));
        });
        let label = if self.picking {
            "Click a slice\u{2026}"
        } else {
            "Region grow"
        };
        if ui
            .selectable_label(self.picking, label)
            .on_hover_text(
                "Click one point on a slice. Connected voxels join across steps up to the gradient limit, \
                 so thin bone below the range is followed and a strong edge stops it",
            )
            .clicked()
        {
            self.picking = !self.picking;
        }
        ui.separator();

        let Some(seg) = self.seg.as_ref() else {
            ui.label(RichText::new("No segments yet").small());
            return;
        };
        let mut remove = None;
        let mut touched = false;
        let rows: Vec<(u8, String, [u8; 3], bool, bool)> = seg
            .segments
            .iter()
            .map(|s| {
                let assisted = matches!(s.provenance, Provenance::Automatic { .. });
                (s.label, s.name.clone(), s.colour, s.visible, assisted)
            })
            .collect();
        {
            for (label, name, colour, visible, assisted) in rows {
                ui.horizontal(|ui| {
                    let mut shown = visible;
                    if ui.checkbox(&mut shown, "").on_hover_text("Show this segment").changed() {
                        if let Some(s) = self
                            .seg
                            .as_mut()
                            .and_then(|g| g.segments.iter_mut().find(|s| s.label == label))
                        {
                            s.visible = shown;
                        }
                        touched = true;
                    }
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 2.0, Color32::from_rgb(colour[0], colour[1], colour[2]));
                    if ui.selectable_label(self.active == Some(label), name).clicked() {
                        self.active = Some(label);
                    }
                    if assisted {
                        ui.label(
                            RichText::new("AI")
                                .small()
                                .strong()
                                .color(Color32::from_rgb(240, 190, 90)),
                        )
                        .on_hover_text("Made by a model: check it against the slices");
                    }
                    if ui.small_button("Delete").clicked() {
                        remove = Some(label);
                    }
                });
                if let Some(s) = self.stats.get(&label) {
                    ui.label(
                        RichText::new(format!(
                            "    {:.2} ml, {} voxels, mean {:.0} ({:.0} to {:.0})",
                            s.millilitres(),
                            s.voxels,
                            s.mean,
                            s.min,
                            s.max
                        ))
                        .small()
                        .color(Color32::from_gray(160)),
                    );
                }
            }
        }
        if let Some(label) = remove {
            self.remember();
            if let Some(seg) = self.seg.as_mut() {
                seg.remove(label);
            }
            if self.active == Some(label) {
                self.active = None;
            }
            touched = true;
        }
        if touched {
            self.changed(volume);
        }
        ui.separator();
        ui.add_enabled_ui(self.active.is_some(), |ui| {
            ui.label(RichText::new("Selected segment").strong());
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Keep largest")
                    .on_hover_text("Drop table, collar and noise")
                    .clicked()
                {
                    if let Some(mask) = self.active_mask() {
                        self.remember();
                        self.replace_active(volume, tools::keep_largest(&mask));
                    }
                }
                if ui
                    .button("Split pieces")
                    .on_hover_text("Each fragment becomes its own segment")
                    .clicked()
                {
                    self.split_active(volume);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Radius");
                ui.add(
                    egui::DragValue::new(&mut self.radius_mm)
                        .speed(0.1)
                        .range(0.1..=20.0)
                        .suffix(" mm"),
                );
            });
            ui.horizontal_wrapped(|ui| {
                for (how, label) in [
                    (Morphology::Close, "Close"),
                    (Morphology::Open, "Open"),
                    (Morphology::Dilate, "Grow"),
                    (Morphology::Erode, "Shrink"),
                    (Morphology::FillHoles, "Fill holes"),
                    (Morphology::SliceGaps, "Fill slice gaps"),
                ] {
                    if ui.button(label).clicked() {
                        self.morph(volume, how);
                    }
                }
            });
        });
        ui.separator();
        ui.label(RichText::new("Export").strong());
        ui.horizontal(|ui| {
            ui.label("Smoothing");
            egui::ComboBox::from_id_salt("segment-smoothing")
                .selected_text(smoothing_label(self.smoothing))
                .show_ui(ui, |ui| {
                    for s in [Smoothing::None, Smoothing::Light, Smoothing::Medium, Smoothing::Strong] {
                        ui.selectable_value(&mut self.smoothing, s, smoothing_label(s));
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
            let selected = self.active;
            if ui
                .add_enabled(selected.is_some(), egui::Button::new("Export STL\u{2026}"))
                .on_hover_text("The selected segment, as a mesh in patient coordinates")
                .clicked()
            {
                let name = self
                    .seg
                    .iter()
                    .flat_map(|s| s.segments.iter())
                    .find(|s| Some(s.label) == selected)
                    .map(|s| file_stem(&s.name))
                    .unwrap_or_default();
                if let (Some(label), Some(path)) = (
                    selected,
                    rfd::FileDialog::new()
                        .set_file_name(format!("{name}.stl"))
                        .add_filter("STL", &["stl"])
                        .save_file(),
                ) {
                    self.start_export(volume, vec![label], ExportTo::File(path));
                }
            }
            if ui
                .button("Export all shown\u{2026}")
                .on_hover_text("Each shown segment as its own file, such as the fragments of a fracture")
                .clicked()
            {
                let labels: Vec<u8> = self
                    .seg
                    .iter()
                    .flat_map(|s| s.segments.iter())
                    .filter(|s| s.visible)
                    .map(|s| s.label)
                    .collect();
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.start_export(volume, labels, ExportTo::Folder(folder));
                }
            }
        });
        ui.label(
            RichText::new("For planning and education. Smoothing can hide a fine fracture: read the slices.")
                .small()
                .color(Color32::from_gray(150)),
        );
        ui.separator();
        ui.checkbox(&mut self.only_segments, "3D shows only segments");
        if ui.add_enabled(self.undo.is_some(), egui::Button::new("Undo")).clicked() {
            if let Some(previous) = self.undo.take() {
                self.seg = Some(previous);
                self.changed(volume);
            }
        }
    }

    fn active_mask(&self) -> Option<Mask> {
        Some(self.seg.as_ref()?.mask(self.active?))
    }

    /// The segment table the ray marcher reads: label 255 is what cuts hide.
    pub fn table(&self) -> Vec<u8> {
        let mut t = vec![0; 256 * 4];
        if !self.only_segments {
            t[3] = 255;
        }
        if let Some(seg) = &self.seg {
            for s in seg.segments.iter().filter(|s| s.visible) {
                let at = s.label as usize * 4;
                t[at..at + 3].copy_from_slice(&s.colour);
                t[at + 3] = 255;
            }
        }
        t
    }

    /// Segments over a resliced plane, for the overlay on a slice.
    pub fn overlay(&self, volume: &Volume, plane: &Plane) -> Option<Vec<u8>> {
        let seg = self.seg.as_ref().filter(|s| !s.segments.is_empty())?;
        let mut colours = [[0u8; 4]; 256];
        for s in seg.segments.iter().filter(|s| s.visible) {
            colours[s.label as usize] = [s.colour[0], s.colour[1], s.colour[2], 120];
        }
        let (w, h) = (plane.width as usize, plane.height as usize);
        let mut out = vec![0u8; w * h * 4];
        out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                if let Some(i) = voxel_at(volume, plane.point(x as f64, y as f64)) {
                    px.copy_from_slice(&colours[seg.labels[i] as usize]);
                }
            }
        });
        Some(out)
    }
}

/// The voxel holding a patient point, if it is in the volume.
pub fn voxel_at(volume: &Volume, p: [f64; 3]) -> Option<usize> {
    let index = volume.to_index(p);
    let [w, h, d] = volume.dims;
    let round = |v: f64, n: usize| {
        let r = v.round();
        (r >= 0.0 && r < n as f64).then_some(r as usize)
    };
    let (x, y, z) = (round(index[0], w)?, round(index[1], h)?, round(index[2], d)?);
    Some(x + w * (y + h * z))
}
