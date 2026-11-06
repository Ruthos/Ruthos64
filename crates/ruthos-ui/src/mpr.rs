//! Multiplanar reconstruction: linked planes through a volume, in their own
//! window (Horos: 2D Viewer > 3D > 2D Orthogonal MPR and 3D MPR).
//!
//! The three planes start square to the acquisition (axial, coronal and
//! sagittal for an axial CT) and meet at the crosshair. In each pane the
//! other planes show as coloured lines:
//!
//! - with the Crosshair tool, dragging a line moves that plane, dragging
//!   where the lines cross moves both, and dragging a round handle tilts
//!   the planes about the pane's own axis for oblique planes; the pane being
//!   worked in keeps still while the other two turn. Lock planes stops all
//!   of that, so measuring cannot move them by accident;
//! - the wheel steps the pane's own plane, Ctrl and the wheel zoom, the
//!   right button sets window and level and the middle button pans;
//! - the measuring tools (caliper, angle, rectangle, ellipse, arrow, point,
//!   text) draw on a plane in millimetres. A measurement belongs to the
//!   plane it was drawn on and shows whenever that plane is on screen;
//! - Save to study writes the plane, or a stack of planes, as a new series
//!   with its measurements; Save image writes the pane as a picture;
//! - the layouts are three panes in a row, a 2x2 grid whose fourth pane is
//!   the original slice through the crosshair, or one pane.
//!
//! Each plane is resampled on the CPU into an ordinary frame with square
//! pixels of a known size, then drawn by the same GPU path as the 2D viewer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportId};
use parking_lot::Mutex;
use rayon::prelude::*;
use ruthos_render::volume3d::{
    Camera, GpuVolume, Placement, RenderMode, TransferFunction, VolumeCallback, SEGMENT_WIDTH,
};
use ruthos_segment::tools::{flood, Outline};
use ruthos_segment::{Field, Grid};

use ruthos_core::mpr_export::{save_planes, stack, MprSaveResult, SavedPlane};
use ruthos_core::volume::{
    cross, dot, norm, normalize, orientation_label, reslice, rotate, scale, sub, Plane, SlabMode, Volume, V3,
};
use ruthos_core::{SeriesRow, StudyRow};
use ruthos_dicom::{Clut, Frame};
use ruthos_render::{GpuImage, ImageCallback, Interpolation, ViewState, ViewTransform};

use crate::icons::{self, Bitmap};
use crate::pacs::Task;
use crate::roi::{roi_color, Roi, RoiKind};
use crate::segments::SegmentPanel;
use crate::shared::Shared;
use crate::widgets::{
    menu_heading, menu_item, menu_item_enabled, tool_menu_row, toolbar_caption, toolbar_separator, Icon, ToolbarButton,
};

/// The colour of each plane's line, as Horos draws them; the fourth is the
/// original slice.
const PLANE_COLOURS: [Color32; 4] = [
    Color32::from_rgb(235, 80, 70),
    Color32::from_rgb(90, 200, 90),
    Color32::from_rgb(80, 150, 245),
    Color32::from_rgb(200, 200, 200),
];

/// The pane that shows the original slice in the 2x2 layout.
const ORIGINAL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Row,
    Grid,
    Single(usize),
}

/// What the left button does in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MprTool {
    Crosshair,
    Window,
    Pan,
    Zoom,
    Length,
    Angle,
    Rectangle,
    Oval,
    Arrow,
    Point,
    Text,
}

impl MprTool {
    const ALL: [MprTool; 11] = [
        MprTool::Crosshair,
        MprTool::Window,
        MprTool::Pan,
        MprTool::Zoom,
        MprTool::Length,
        MprTool::Angle,
        MprTool::Rectangle,
        MprTool::Oval,
        MprTool::Arrow,
        MprTool::Point,
        MprTool::Text,
    ];

    fn icon(self) -> Icon {
        match self {
            MprTool::Crosshair => Icon::Glyph(icons::TOOL_POINT),
            MprTool::Window => Icon::Bitmap(Bitmap::Wlww),
            MprTool::Pan => Icon::Bitmap(Bitmap::Move),
            MprTool::Zoom => Icon::Bitmap(Bitmap::Zoom),
            MprTool::Length => Icon::Bitmap(Bitmap::Length),
            MprTool::Angle => Icon::Bitmap(Bitmap::Angle),
            MprTool::Rectangle => Icon::Bitmap(Bitmap::Rectangle),
            MprTool::Oval => Icon::Bitmap(Bitmap::Oval),
            MprTool::Arrow => Icon::Bitmap(Bitmap::Arrow),
            MprTool::Point => Icon::Bitmap(Bitmap::Point),
            MprTool::Text => Icon::Bitmap(Bitmap::Text),
        }
    }

    fn label(self) -> &'static str {
        match self {
            MprTool::Crosshair => "Crosshair",
            MprTool::Window => "WL/WW",
            MprTool::Pan => "Move",
            MprTool::Zoom => "Zoom",
            MprTool::Length => "Caliper",
            MprTool::Angle => "Angle",
            MprTool::Rectangle => "Rectangle",
            MprTool::Oval => "Ellipse",
            MprTool::Arrow => "Arrow",
            MprTool::Point => "Point",
            MprTool::Text => "Text",
        }
    }

    fn key(self) -> Key {
        match self {
            MprTool::Crosshair => Key::C,
            MprTool::Window => Key::W,
            MprTool::Pan => Key::M,
            MprTool::Zoom => Key::Z,
            MprTool::Length => Key::L,
            MprTool::Angle => Key::A,
            MprTool::Rectangle => Key::B,
            MprTool::Oval => Key::E,
            MprTool::Arrow => Key::N,
            MprTool::Point => Key::P,
            MprTool::Text => Key::X,
        }
    }

    fn measures(self) -> bool {
        matches!(
            self,
            MprTool::Length
                | MprTool::Angle
                | MprTool::Rectangle
                | MprTool::Oval
                | MprTool::Arrow
                | MprTool::Point
                | MprTool::Text
        )
    }
}

/// One pane: how it is viewed and the last image made for it.
#[derive(Default)]
struct PaneView {
    state: ViewState,
    fitted: bool,
    /// What the image was made from, so it is remade only on a change.
    signature: String,
    frame: Option<Arc<Frame>>,
    plane: Option<Plane>,
    gpu: Option<Arc<GpuImage>>,
    /// Segments over this plane.
    overlay: Option<egui::TextureHandle>,
    overlay_signature: String,
}

/// What a drag in a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    /// Moving another plane (by pane index) along its normal.
    Line(usize),
    /// Moving the crosshair within the pane.
    Centre,
    /// Turning the planes about this pane's normal; the last pointer angle.
    Rotate(f64),
    Window,
    Pan,
    Zoom,
    /// Drawing a measurement from this image point.
    Draw(Vec2),
    /// Moving a measurement's handle.
    Handle(usize, usize),
    /// Moving a whole measurement.
    Move(usize),
}

/// A measurement on a plane: the plane it belongs to, as a key, and the
/// measurement in that plane's pixels.
#[derive(Debug, Clone)]
struct PlaneRoi {
    key: String,
    roi: Roi,
}

/// An angle being drawn: the first arm is dragged, then the end clicked.
#[derive(Debug, Clone, Copy)]
struct AngleDraft {
    pane: usize,
    a: Vec2,
    b: Vec2,
    c: Vec2,
}

/// The text box for a text note.
#[derive(Debug, Clone)]
struct TextPrompt {
    pane: usize,
    at: Vec2,
    screen: Pos2,
    text: String,
    focus: bool,
}

/// Save to study.
struct SaveDialog {
    pane: usize,
    stack: bool,
    spacing_mm: f64,
    description: String,
    running: Option<Task<Result<MprSaveResult, String>>>,
    progress: Arc<Mutex<(usize, usize)>>,
    message: Option<String>,
}

type Loading = (
    Task<Result<Volume, String>>,
    Arc<Mutex<(usize, usize)>>,
    Arc<AtomicBool>,
);

pub struct MprState {
    pub id: u64,
    pub viewport_id: ViewportId,
    pub close_requested: bool,
    pub series_id: i64,
    study: StudyRow,
    series: SeriesRow,
    loading: Option<Loading>,
    volume: Option<Arc<Volume>>,
    error: Option<String>,
    /// The three plane normals' frame: rotated copies of the volume axes.
    axes: [V3; 3],
    /// Where the planes meet, in patient coordinates.
    centre: V3,
    /// Per pane, how far its own image has been turned back so it keeps
    /// still while the planes are tilted in it.
    phi: [f64; 3],
    panes: [PaneView; 4],
    window: (f32, f32),
    slab_mm: f32,
    slab_mode: SlabMode,
    drag: Option<(usize, Drag)>,
    clut: String,
    status: String,
    layout: Layout,
    tool: MprTool,
    /// The planes cannot be moved or tilted.
    locked: bool,
    rois: Vec<PlaneRoi>,
    selected: Option<usize>,
    /// The measurement being dragged out, in the pane's image pixels.
    draft: Option<(usize, Vec2, Vec2)>,
    angle_draft: Option<AngleDraft>,
    text_prompt: Option<TextPrompt>,
    save: Option<SaveDialog>,
    /// The pane last worked in: what Save acts on.
    active: usize,
    /// Frames drawn since the volume arrived, for the screenshot option.
    frames_shown: u32,
    /// The 3D pane, in the fourth place of the 2x2 layout.
    three_d: Option<View3d>,
    segments: SegmentPanel,
    /// A save of the segments under way.
    seg_save: Option<crossbeam_channel::Receiver<Result<ruthos_dicom::seg::SegWritten, String>>>,
    /// The saved segments on offer when the window opened.
    seg_offer: bool,
    /// Asking whether to save before closing.
    close_prompt: bool,
    close_after_save: bool,
    /// The developer option has saved.
    dev_saved: bool,
}

/// What the left button does in the 3D pane. The middle button always moves
/// and the right always zooms, as in the 2D viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool3d {
    Rotate,
    Window,
    Move,
    Zoom,
    /// Build a box, then cut inside or outside it.
    CutBox,
    /// Build an outline from clicks or a drag, then cut through the depth.
    CutPath,
    /// Remove everything connected to the point clicked.
    RemovePiece,
}

impl Tool3d {
    const VIEW: [Tool3d; 4] = [Tool3d::Rotate, Tool3d::Window, Tool3d::Move, Tool3d::Zoom];
    const CUT: [Tool3d; 3] = [Tool3d::CutBox, Tool3d::CutPath, Tool3d::RemovePiece];

    fn icon(self) -> Icon {
        match self {
            Tool3d::Rotate => Icon::Glyph(icons::TOOL_ROTATE),
            Tool3d::Window => Icon::Bitmap(Bitmap::Wlww),
            Tool3d::Move => Icon::Bitmap(Bitmap::Move),
            Tool3d::Zoom => Icon::Bitmap(Bitmap::Zoom),
            Tool3d::CutBox => Icon::Glyph(icons::THREE_D),
            Tool3d::CutPath => Icon::Bitmap(Bitmap::Pencil),
            Tool3d::RemovePiece => Icon::Glyph(icons::TRASH),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool3d::Rotate => "Rotate",
            Tool3d::Window => "Window",
            Tool3d::Move => "Move",
            Tool3d::Zoom => "Zoom",
            Tool3d::CutBox => "Cut box",
            Tool3d::CutPath => "Cut path",
            Tool3d::RemovePiece => "Remove piece",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Tool3d::Rotate => "Drag to turn the volume",
            Tool3d::Window => "Drag across for contrast, up and down for level",
            Tool3d::Move => "Drag to move the view",
            Tool3d::Zoom => "Drag up and down to zoom",
            Tool3d::CutBox => {
                "Drag the handles to size a box, turning the view to check it, then choose Cut inside or Cut outside"
            }
            Tool3d::CutPath => {
                "Click points or drag to draw, drag a point to move it, then choose Cut inside or Cut outside"
            }
            Tool3d::RemovePiece => "Click the table: everything connected to that point goes",
        }
    }
}

/// A toolbar menu: an icon and caret over a caption, as in the viewer.
fn menu_with_caption(ui: &mut Ui, icon: Icon, caption: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.add_space(2.0);
        let caret = icons::rich(icons::CARET_DOWN, 11.0);
        let gap = egui::RichText::new(" ").size(13.0);
        match icon {
            Icon::Glyph(g) => ui.menu_button((icons::rich(g, 18.0), gap, caret), add),
            Icon::Bitmap(b) => ui.menu_button((icons::image(ui.ctx(), b, 18.0), gap, caret), add),
        };
        toolbar_caption(ui, caption);
    });
}

/// A box along the volume's axes, in voxels, built before it cuts anything.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CutBox {
    low: V3,
    high: V3,
}

impl CutBox {
    /// The whole volume: the user brings the faces in.
    fn around(dims: [usize; 3]) -> Self {
        Self {
            low: [-0.5; 3],
            high: std::array::from_fn(|a| dims[a] as f64 - 0.5),
        }
    }

    fn contains(&self, voxel: [usize; 3]) -> bool {
        (0..3).all(|a| (self.low[a]..=self.high[a]).contains(&(voxel[a] as f64)))
    }

    /// This box as what the AI model is given.
    #[cfg(feature = "ai")]
    fn limits(&self, dims: [usize; 3]) -> Option<crate::segments_ai::Limits> {
        let (low, high) = ([self.low[0], self.low[1]], [self.high[0], self.high[1]]);
        let region = ruthos_segment::ai::Region::between(low, high, dims[0], dims[1])?;
        let first = self.low[2].ceil().max(0.0) as usize;
        let last = (self.high[2].floor().max(0.0) as usize).min(dims[2].saturating_sub(1));
        (first <= last).then_some(crate::segments_ai::Limits {
            region,
            slices: (first, last),
        })
    }

    fn corner(&self, n: usize) -> V3 {
        std::array::from_fn(|a| if n >> a & 1 == 1 { self.high[a] } else { self.low[a] })
    }

    /// The middle of a face.
    fn face(&self, axis: usize, high: bool) -> V3 {
        let mut c: V3 = std::array::from_fn(|a| (self.low[a] + self.high[a]) / 2.0);
        c[axis] = if high { self.high[axis] } else { self.low[axis] };
        c
    }

    /// Move a face by some voxels, keeping the box in the volume and at least
    /// one voxel thick.
    fn move_face(&mut self, axis: usize, high: bool, by: f64, dims: [usize; 3]) {
        if high {
            self.high[axis] = (self.high[axis] + by).clamp(self.low[axis] + 1.0, dims[axis] as f64 - 0.5);
        } else {
            self.low[axis] = (self.low[axis] + by).clamp(-0.5, self.high[axis] - 1.0);
        }
    }
}

/// The twelve edges of a box, by corner.
const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

/// How far along an axis a drag on screen goes, given the screen distance one
/// unit of that axis covers. Nothing when the axis points at the viewer.
fn along_axis(drag: Vec2, unit: Vec2) -> f64 {
    let square = unit.length_sq();
    if square < 1e-3 {
        return 0.0;
    }
    (drag.dot(unit) / square) as f64
}

/// An outline built on the view, cut straight through the depth from the
/// direction it was started in, however the view has turned since.
#[derive(Debug, Clone, PartialEq)]
struct CutPath {
    points: Vec<V3>,
    /// The view it extrudes along, drawn back so the points stay in frame.
    camera: Camera,
    aspect: f64,
}

/// What a press took hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grabbed {
    Face(usize, bool),
    Point(usize),
}

/// How close a press must be to a handle, in screen points.
const HANDLE_REACH: f32 = 12.0;

/// The 3D pane. The slices stay in charge: it sits beside them, and every
/// view of it is a derived picture.
struct View3d {
    gpu: Arc<GpuVolume>,
    placement: Placement,
    camera: Camera,
    presets: Vec<TransferFunction>,
    preset: usize,
    mode: RenderMode,
    baked: Arc<Vec<u8>>,
    segments: Arc<Vec<u8>>,
    /// When the user last moved the view: rendering refines after it.
    moved: Option<std::time::Instant>,
    tool: Tool3d,
    /// The window dragged onto the preset: a shift in values and a stretch.
    shift: f32,
    scale: f32,
    /// On the source grid: 0 is shown, and n is what cut n removed, so undoing
    /// a cut takes no copy of the volume.
    labels: Vec<u8>,
    cuts: u8,
    cut_box: Option<CutBox>,
    cut_path: Option<CutPath>,
    grabbed: Option<Grabbed>,
    /// The segments' labels as last sent, and which change that was.
    segment_labels: Option<Arc<Vec<u8>>>,
    segment_generation: u64,
    segment_only: bool,
}

/// While rotating, rays take longer steps; when the user stops, the final
/// image is always computed at full quality.
const REFINE_AFTER: std::time::Duration = std::time::Duration::from_millis(180);

/// The most memory a volume takes on the GPU.
const VOLUME_BYTES: u64 = 1 << 30;

impl View3d {
    fn new(shared: &Shared, volume: &Volume) -> Self {
        let rs = &shared.render_state;
        let placement = Placement {
            dims: volume.dims,
            spacing: volume.spacing,
            origin: volume.origin,
            axes: volume.axes,
        };
        let gpu = GpuVolume::upload(
            &rs.device,
            &rs.queue,
            &placement,
            (volume.min, volume.max),
            VOLUME_BYTES,
            |x, y, z| volume.voxel(x, y, z),
        );
        let presets = TransferFunction::presets();
        let baked = Arc::new(presets[0].bake(volume.min, volume.max));
        Self {
            gpu: Arc::new(gpu),
            camera: Camera::framing(&placement),
            placement,
            presets,
            preset: 0,
            mode: RenderMode::Shaded,
            baked,
            segments: Arc::new(shown_only()),
            moved: None,
            tool: Tool3d::Rotate,
            shift: 0.0,
            scale: 1.0,
            labels: vec![0; volume.dims[0] * volume.dims[1] * volume.dims[2]],
            cuts: 0,
            cut_box: None,
            cut_path: None,
            grabbed: None,
            segment_labels: None,
            segment_generation: 0,
            segment_only: false,
        }
    }

    fn set_preset(&mut self, i: usize) {
        self.preset = i;
        self.shift = 0.0;
        self.scale = 1.0;
        self.rebake();
    }

    fn transfer(&self) -> TransferFunction {
        self.presets[self.preset].windowed(self.shift, self.scale)
    }

    fn rebake(&mut self) {
        self.baked = Arc::new(self.transfer().bake(self.gpu.min, self.gpu.max));
    }

    /// Back to the framing and the preset's own window.
    fn reset_view(&mut self) {
        self.camera = Camera::framing(&self.placement);
        self.shift = 0.0;
        self.scale = 1.0;
        self.rebake();
    }

    /// Cuts hide voxels whatever segment they are in, so what reaches the GPU
    /// is the segment labels with cut voxels as label 255.
    fn send_labels(&self, shared: &Shared) {
        let merged: Vec<u8> = match &self.segment_labels {
            Some(seg) => self
                .labels
                .par_iter()
                .zip(seg.par_iter())
                .map(|(cut, label)| if *cut != 0 { 255 } else { *label })
                .collect(),
            None => self.labels.par_iter().map(|c| if *c != 0 { 255 } else { 0 }).collect(),
        };
        self.gpu
            .write_labels(&shared.render_state.queue, self.placement.dims, &merged);
    }

    /// Take the segments as they now stand.
    fn set_segments(&mut self, shared: &Shared, panel: &SegmentPanel) {
        self.segment_labels = panel.seg.as_ref().map(|s| Arc::new(s.labels.clone()));
        self.segments = Arc::new(panel.table());
        self.segment_generation = panel.generation;
        self.segment_only = panel.only_segments;
        self.send_labels(shared);
    }

    /// Hide voxels as the next cut. The 254th cut joins the 253rd, which only
    /// means they undo together.
    fn cut(&mut self, shared: &Shared, voxels: impl IntoIterator<Item = usize>) {
        self.cuts = (self.cuts + 1).min(254);
        for i in voxels {
            if self.labels[i] == 0 {
                self.labels[i] = self.cuts;
            }
        }
        self.send_labels(shared);
    }

    fn undo_cut(&mut self, shared: &Shared) {
        if self.cuts == 0 {
            return;
        }
        let cut = self.cuts;
        self.labels.par_iter_mut().filter(|l| **l == cut).for_each(|l| *l = 0);
        self.cuts -= 1;
        self.send_labels(shared);
    }

    fn restore_cuts(&mut self, shared: &Shared) {
        self.labels.fill(0);
        self.cuts = 0;
        self.send_labels(shared);
    }

    fn grid(&self) -> Grid {
        Grid {
            dims: self.placement.dims,
            spacing: self.placement.spacing,
        }
    }

    /// A patient point from a place in the voxel grid.
    fn patient(&self, voxel: V3) -> V3 {
        let p = &self.placement;
        std::array::from_fn(|c| p.origin[c] + (0..3).map(|a| p.axes[a][c] * voxel[a] * p.spacing[a]).sum::<f64>())
    }

    fn to_screen(&self, rect: Rect, aspect: f64, point: V3) -> Pos2 {
        let s = self.camera.project(point, aspect);
        rect.center() + Vec2::new(s[0] as f32 * rect.width() / 2.0, -s[1] as f32 * rect.height() / 2.0)
    }

    fn has_shape(&self) -> bool {
        match self.tool {
            Tool3d::CutBox => self.cut_box.is_some(),
            Tool3d::CutPath => self.cut_path.as_ref().is_some_and(|c| c.points.len() >= 3),
            _ => false,
        }
    }

    fn clear_shape(&mut self) {
        match self.tool {
            Tool3d::CutBox => self.cut_box = Some(CutBox::around(self.placement.dims)),
            _ => self.cut_path = None,
        }
        self.grabbed = None;
    }

    /// Cut with the shape that has been built: what is inside it, or what is
    /// outside. Only now does anything leave the view.
    fn cut_shape(&mut self, shared: &Shared, inside: bool) {
        let grid = self.grid();
        let voxels: Vec<usize> = match self.tool {
            Tool3d::CutBox => {
                let Some(shape) = self.cut_box else { return };
                let labels = &self.labels;
                (0..grid.len())
                    .into_par_iter()
                    .filter(|&i| labels[i] == 0 && shape.contains(grid.position(i)) == inside)
                    .collect()
            }
            Tool3d::CutPath => {
                let Some(path) = self.cut_path.clone() else { return };
                let points: Vec<[f32; 2]> = path
                    .points
                    .iter()
                    .map(|q| {
                        let s = path.camera.project(*q, path.aspect);
                        [s[0] as f32, s[1] as f32]
                    })
                    .collect();
                let outline = Outline::new(&points);
                let labels = &self.labels;
                let this = &*self;
                (0..grid.len())
                    .into_par_iter()
                    .filter(|&i| {
                        if labels[i] != 0 {
                            return false;
                        }
                        let at = grid.position(i);
                        let s = path
                            .camera
                            .project(this.patient([at[0] as f64, at[1] as f64, at[2] as f64]), path.aspect);
                        outline.contains([s[0] as f32, s[1] as f32]) == inside
                    })
                    .collect()
            }
            _ => return,
        };
        self.cut(shared, voxels);
        // The box stays where it was built, for the next cut or to undo and
        // adjust; a path has done its job.
        if self.tool == Tool3d::CutPath {
            self.clear_shape();
        }
    }

    /// Add a point to the path, at a place on the view.
    fn add_point(&mut self, ndc: [f64; 2], aspect: f64) {
        let point = self.camera.point_at(ndc, aspect);
        let camera = self.camera;
        self.cut_path
            .get_or_insert_with(|| {
                let mut camera = camera;
                camera.zoom(2.0);
                CutPath {
                    points: Vec::new(),
                    camera,
                    aspect,
                }
            })
            .points
            .push(point);
    }

    /// Take hold of whatever handle is under a press.
    fn grab(&mut self, rect: Rect, aspect: f64, at: Pos2) {
        let mut best: Option<(f32, Grabbed)> = None;
        let mut offer = |p: Pos2, g: Grabbed| {
            let d = p.distance(at);
            if d <= HANDLE_REACH && best.is_none_or(|(b, _)| d < b) {
                best = Some((d, g));
            }
        };
        match self.tool {
            Tool3d::CutBox => {
                if let Some(b) = self.cut_box {
                    for axis in 0..3 {
                        for high in [false, true] {
                            offer(
                                self.to_screen(rect, aspect, self.patient(b.face(axis, high))),
                                Grabbed::Face(axis, high),
                            );
                        }
                    }
                }
            }
            Tool3d::CutPath => {
                if let Some(path) = &self.cut_path {
                    for (i, q) in path.points.iter().enumerate() {
                        offer(self.to_screen(rect, aspect, *q), Grabbed::Point(i));
                    }
                }
            }
            _ => {}
        }
        self.grabbed = best.map(|(_, g)| g);
    }

    /// Follow a drag on whatever has been taken hold of.
    fn drag_grabbed(&mut self, grabbed: Grabbed, rect: Rect, aspect: f64, drag: Vec2, at: Option<Pos2>) {
        match grabbed {
            Grabbed::Face(axis, high) => {
                let Some(mut b) = self.cut_box else { return };
                let from = b.face(axis, high);
                let mut next = from;
                next[axis] += 1.0;
                let unit =
                    self.to_screen(rect, aspect, self.patient(next)) - self.to_screen(rect, aspect, self.patient(from));
                b.move_face(axis, high, along_axis(drag, unit), self.placement.dims);
                self.cut_box = Some(b);
            }
            Grabbed::Point(i) => {
                if let Some(p) = at {
                    let v = to_view(rect, p);
                    let point = self.camera.point_at([v[0] as f64, v[1] as f64], aspect);
                    if let Some(q) = self.cut_path.as_mut().and_then(|c| c.points.get_mut(i)) {
                        *q = point;
                    }
                }
            }
        }
    }

    /// The shape being built, drawn over the rendering.
    fn draw_shape(&self, painter: &egui::Painter, rect: Rect, aspect: f64) {
        const LINE: Color32 = Color32::from_rgb(255, 210, 90);
        let handle = |p: Pos2, held: bool| {
            let fill = if held { Color32::WHITE } else { LINE };
            painter.circle(p, 5.5, fill, Stroke::new(1.0, Color32::BLACK));
        };
        match self.tool {
            Tool3d::CutBox => {
                let Some(b) = self.cut_box else { return };
                let screen = |c: V3| self.to_screen(rect, aspect, self.patient(c));
                for (i, j) in BOX_EDGES {
                    painter.line_segment([screen(b.corner(i)), screen(b.corner(j))], Stroke::new(1.5, LINE));
                }
                for axis in 0..3 {
                    for high in [false, true] {
                        handle(
                            screen(b.face(axis, high)),
                            self.grabbed == Some(Grabbed::Face(axis, high)),
                        );
                    }
                }
            }
            Tool3d::CutPath => {
                let Some(path) = &self.cut_path else { return };
                let mut line: Vec<Pos2> = path.points.iter().map(|q| self.to_screen(rect, aspect, *q)).collect();
                for (i, p) in line.iter().enumerate() {
                    if path.points.len() <= 64 {
                        handle(*p, self.grabbed == Some(Grabbed::Point(i)));
                    }
                }
                if line.len() >= 3 {
                    line.push(line[0]);
                }
                painter.add(egui::Shape::line(line, Stroke::new(1.5, LINE)));
            }
            _ => {}
        }
    }

    /// The first visible voxel along the ray through a point on the view.
    fn pick(&self, volume: &Volume, ndc: [f64; 2], aspect: f64) -> Option<usize> {
        let transfer = self.transfer();
        let (c, p) = (self.camera, &self.placement);
        let grid = Grid {
            dims: p.dims,
            spacing: p.spacing,
        };
        let forward = c.forward();
        let reach = p.diagonal();
        let step = p.spacing.iter().copied().fold(f64::MAX, f64::min).max(0.1) / 2.0;
        let middle = c.point_at(ndc, aspect);
        let mut depth = -reach;
        while depth < reach {
            let at: V3 = std::array::from_fn(|i| middle[i] + forward[i] * depth);
            depth += step;
            let index = volume.to_index(at);
            let voxel = std::array::from_fn::<isize, 3, _>(|a| index[a].round() as isize);
            if (0..3).any(|a| voxel[a] < 0 || voxel[a] >= p.dims[a] as isize) {
                continue;
            }
            let (x, y, z) = (voxel[0] as usize, voxel[1] as usize, voxel[2] as usize);
            let i = grid.index(x, y, z);
            if self.labels[i] == 0 && transfer.at(volume.voxel(x, y, z))[3] > 0.05 {
                return Some(i);
            }
        }
        None
    }

    /// Remove everything shown that is connected to the point clicked.
    fn remove_piece(&mut self, shared: &Shared, volume: &Volume, ndc: [f64; 2], aspect: f64) -> Option<usize> {
        let seed = self.pick(volume, ndc, aspect)?;
        let transfer = self.transfer();
        let grid = Grid {
            dims: self.placement.dims,
            spacing: self.placement.spacing,
        };
        let labels = &self.labels;
        let piece = flood(&grid, seed, |i| labels[i] == 0 && transfer.at(volume.at(i))[3] > 0.02);
        let n = piece.len();
        self.cut(shared, piece);
        Some(n)
    }
}

/// The segment table with only uncut voxels shown: label 0 has no colour of its
/// own, and every other label is hidden.
fn shown_only() -> Vec<u8> {
    let mut table = vec![0; SEGMENT_WIDTH * 4];
    table[3] = 255;
    table
}

/// A point on the pane in -1 to 1 across the view, up being positive.
fn to_view(rect: Rect, p: Pos2) -> [f32; 2] {
    [
        (p.x - rect.left()) / rect.width() * 2.0 - 1.0,
        1.0 - (p.y - rect.top()) / rect.height() * 2.0,
    ]
}

impl MprState {
    /// Open an MPR window on a series and start building its volume.
    pub fn open(shared: &Shared, study: StudyRow, series: SeriesRow, ctx: &egui::Context) -> Self {
        let id = shared.next_id();
        let files: Vec<std::path::PathBuf> = shared
            .db
            .images_for_series(series.id)
            .unwrap_or_default()
            .iter()
            .map(|i| shared.resolve(&i.path))
            .collect();
        let progress = Arc::new(Mutex::new((0usize, files.len())));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (progress.clone(), cancel.clone());
        let repaint = ctx.clone();
        let task = Task::spawn(ctx, "ruthos-mpr-load", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Volume::load(
                    &files,
                    &|done, total| {
                        *p.lock() = (done, total);
                        repaint.request_repaint();
                    },
                    &c,
                )
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("an internal error stopped the volume from loading; see crash.log".into()))
        });
        let layout = match std::env::var("RUTHOS_MPR_LAYOUT").ok().as_deref() {
            // Developer option: `RUTHOS_MPR_LAYOUT=grid|row|single`.
            Some("grid") => Layout::Grid,
            Some("single") => Layout::Single(0),
            _ => Layout::Row,
        };
        Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-mpr", id)),
            close_requested: false,
            series_id: series.id,
            study,
            series,
            loading: Some((task, progress, cancel)),
            volume: None,
            error: None,
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            centre: [0.0; 3],
            phi: [0.0; 3],
            panes: Default::default(),
            window: (40.0, 400.0),
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
            drag: None,
            clut: "No CLUT".into(),
            status: String::new(),
            layout,
            tool: MprTool::Crosshair,
            locked: false,
            rois: Vec::new(),
            selected: None,
            draft: None,
            angle_draft: None,
            text_prompt: None,
            save: None,
            active: 0,
            frames_shown: 0,
            three_d: None,
            segments: SegmentPanel::default(),
            seg_save: None,
            seg_offer: false,
            close_prompt: false,
            close_after_save: false,
            dev_saved: false,
        }
    }

    /// Show or hide the 3D pane, which takes the fourth place of the grid.
    fn set_3d(&mut self, on: bool, shared: &Shared) {
        if !on {
            self.three_d = None;
            return;
        }
        if let Some(volume) = &self.volume {
            let mut view = View3d::new(shared, volume);
            view.set_segments(shared, &self.segments);
            self.three_d = Some(view);
            self.layout = Layout::Grid;
        }
    }

    pub fn title(&self) -> String {
        format!(
            "MPR: {}  {} - Ruthos",
            self.study.patient_name,
            self.series.display_name()
        )
    }

    fn reset(&mut self) {
        let Some(v) = &self.volume else { return };
        self.axes = v.axes;
        self.centre = v.centre();
        self.phi = [0.0; 3];
        for p in &mut self.panes {
            p.fitted = false;
            p.signature.clear();
        }
    }

    /// Normal, right and down of pane `i`; the fourth pane is the original
    /// slice, square to the acquisition whatever the tilt.
    fn basis(&self, i: usize) -> (V3, V3, V3) {
        if i == ORIGINAL {
            if let Some(v) = &self.volume {
                return (v.axes[2], v.axes[0], v.axes[1]);
            }
        }
        pane_basis(&self.axes, self.phi[i.min(2)], i.min(2))
    }

    fn pane_name(&self, i: usize) -> String {
        if i == ORIGINAL {
            return "Original slice".into();
        }
        plane_name(self.basis(i).0)
    }

    /// The plane pane `i` shows.
    fn plane(&self, i: usize, v: &Volume) -> Plane {
        let (n, right, down) = self.basis(i);
        // The image is centred on the volume's centre, moved onto the plane,
        // so it stays put while the crosshair moves.
        let vc = v.centre();
        let centre = sub(vc, scale(n, dot(sub(vc, self.centre), n)));
        let diag = v.diagonal();
        let pixel_mm = v.finest().max(diag / 1024.0);
        let size = (diag / pixel_mm).ceil().clamp(16.0, 1024.0) as u32;
        Plane {
            centre,
            right,
            down,
            width: size,
            height: size,
            pixel_mm,
            slab_mm: self.slab_mm as f64,
            slab_mode: self.slab_mode,
        }
    }

    fn pump(&mut self, shared: &Shared) {
        if let Some((task, _, _)) = &self.loading {
            if let Some(result) = task.take() {
                self.loading = None;
                match result {
                    Ok(v) => {
                        let (c, w) = volume_window(&v);
                        self.window = (c, w);
                        if v.irregular_spacing {
                            self.status =
                                "Some slices are further apart than the rest: distances across the gap are approximate"
                                    .into();
                        }
                        self.volume = Some(Arc::new(v));
                        self.reset();
                        self.find_saved_segments(shared);
                        // Developer option: `RUTHOS_MPR_TILT=<degrees>` tilts the
                        // planes in the first pane, as a handle drag would.
                        if let Some(deg) = std::env::var("RUTHOS_MPR_TILT")
                            .ok()
                            .and_then(|v| v.parse::<f64>().ok())
                        {
                            let (n, _, _) = self.basis(0);
                            let da = deg.to_radians();
                            for a in &mut self.axes {
                                *a = normalize(rotate(*a, n, da));
                            }
                            self.phi[0] += da;
                        }
                        // Developer option: `RUTHOS_MPR_MEASURE=1` puts a caliper
                        // and a text note on the coronal plane.
                        if std::env::var_os("RUTHOS_MPR_MEASURE").is_some() {
                            if let Some(v) = self.volume.clone() {
                                let plane = self.plane(1, &v);
                                let key = plane_key(&plane);
                                let (w, h) = (plane.width as f32, plane.height as f32);
                                let a = Vec2::new(w * 0.3, h * 0.5);
                                let b = Vec2::new(w * 0.7, h * 0.5);
                                self.push_roi(&key, &plane, Roi::new(RoiKind::Length { a, b }));
                                let mut note = Roi::new(RoiKind::Text {
                                    p: Vec2::new(w * 0.3, h * 0.3),
                                });
                                note.name = Some("Lung fields".into());
                                self.push_roi(&key, &plane, note);
                                self.selected = Some(0);
                            }
                        }
                    }
                    Err(e) if e == "cancelled" => self.close_requested = true,
                    Err(e) => self.error = Some(e),
                }
            }
        }
        // A save that finished.
        if let Some(d) = &mut self.save {
            if let Some(r) = d.running.as_ref().and_then(|t| t.take()) {
                d.running = None;
                match r {
                    Ok(res) => {
                        let n = res.image_ids.len();
                        d.message = None;
                        self.status = format!(
                            "Saved {} to the study as \"{}\"",
                            if n == 1 {
                                "1 image".to_string()
                            } else {
                                format!("{n} images")
                            },
                            d.description
                        );
                        let _ = shared.import_tx.send(ruthos_core::ImportEvent::Finished(res.summary));
                        self.save = None;
                    }
                    Err(e) => d.message = Some(format!("Not saved: {e}")),
                }
            }
        }
    }

    /// The panel reads Preferences, and what it decides is kept there.
    #[cfg(feature = "ai")]
    fn sync_ai(&mut self, shared: &Shared) {
        let ai = &mut self.segments.ai;
        let chosen = ai.newly_chosen.take();
        let agreed = ai.newly_agreed.take();
        let mut settings = shared.settings.lock();
        ai.band = settings.refine_band as usize;
        ai.research_mode = settings.research_mode;
        ai.agreed = settings.research_agreed.keys().cloned().collect();
        if let Some(id) = agreed {
            let date = chrono::Local::now().format("%Y-%m-%d").to_string();
            settings.research_agreed.insert(id.to_string(), date);
            ai.agreed.push(id.to_string());
        }
        if let Some(id) = chosen {
            settings.ai_model = id.to_string();
        }
        if chosen.is_some() || agreed.is_some() {
            if let Err(e) = settings.save() {
                self.segments.note = Some(format!("Could not save the settings: {e}"));
            }
        }
        // Developer option: `RUTHOS_MPR_AI_MODEL=<id>` chooses the model.
        let id = std::env::var("RUTHOS_MPR_AI_MODEL").unwrap_or_else(|_| settings.ai_model.clone());
        if !ai.started() && !id.is_empty() {
            ai.choose(&id);
        }
        ai.keep_to_offered();
    }

    /// Offer what is saved for this series, or load it if that is the setting.
    fn find_saved_segments(&mut self, shared: &Shared) {
        self.segments.saved = ruthos_core::seg::saved_for_series(
            &shared.db,
            &shared.paths,
            self.study.id,
            &self.series.series_instance_uid,
        )
        .unwrap_or_default();
        if self.segments.saved.is_empty() {
            return;
        }
        if shared.settings.lock().load_newest_segments {
            self.load_newest_segments();
        } else {
            self.seg_offer = true;
        }
    }

    /// Put the newest saved segments on the panel, or say why they cannot be.
    fn load_newest_segments(&mut self) {
        let (Some(saved), Some(volume)) = (self.segments.saved.first().cloned(), self.volume.clone()) else {
            return;
        };
        let grid = ruthos_segment::Grid {
            dims: volume.dims,
            spacing: volume.spacing,
        };
        let geometry = ruthos_dicom::seg::Geometry {
            dims: volume.dims,
            spacing: volume.spacing,
            origin: volume.origin,
            axes: volume.axes,
        };
        self.segments.open = true;
        let placed = ruthos_dicom::seg::read_seg(&saved.path).and_then(|f| f.place(&geometry));
        match placed {
            Ok(placed) => {
                let (seg, overlapping, left_out) = ruthos_segment::store::from_placed(grid, placed);
                let count = seg.segments.len();
                self.segments.load(&volume, seg);
                let plural = if count == 1 { "" } else { "s" };
                let mut note = format!("Loaded {count} saved segment{plural}");
                if overlapping > 0 {
                    note.push_str(&format!(
                        ". {overlapping} voxels were in two segments and went to the later one"
                    ));
                }
                if left_out > 0 {
                    note.push_str(&format!(". {left_out} segments did not fit"));
                }
                self.segments.note = Some(note);
            }
            Err(e) => self.segments.note = Some(e.to_string()),
        }
    }

    /// Save the panel's segments to the study, apart from the window.
    fn save_segments(&mut self, shared: &Shared) {
        let Some(volume) = self.volume.as_ref() else {
            return;
        };
        let (creator, unrefined) = {
            let s = shared.settings.lock();
            (s.user_name.clone(), s.save_unrefined_masks)
        };
        let data = self.segments.to_save(unrefined);
        if data.is_empty() {
            self.segments.note = Some("Every segment is empty, so there is nothing to save".to_string());
            self.close_after_save = false;
            return;
        }
        self.segments.saving = true;
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.seg_save = Some(rx);
        let (db, paths, dims) = (shared.db.clone(), shared.paths.clone(), volume.dims);
        let (study, series) = (self.study.id, self.series.id);
        std::thread::spawn(move || {
            let saved = ruthos_core::seg::save_segments(&db, &paths, study, series, dims, &data, &creator);
            let _ = tx.send(saved.map_err(|e| e.to_string()));
        });
    }

    fn pump_seg_save(&mut self, shared: &Shared) {
        let Some(rx) = &self.seg_save else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.seg_save = None;
        match result {
            Ok(written) => {
                self.segments.saved_to_study();
                self.segments.note = Some(format!("Saved to the study, in {} frames", written.frames));
                self.segments.saved = ruthos_core::seg::saved_for_series(
                    &shared.db,
                    &shared.paths,
                    self.study.id,
                    &self.series.series_instance_uid,
                )
                .unwrap_or_default();
                if self.close_after_save {
                    self.close_requested = true;
                }
            }
            Err(e) => {
                self.segments.saving = false;
                self.segments.open = true;
                self.segments.note = Some(format!("Not saved: {e}"));
                self.close_after_save = false;
            }
        }
    }

    fn seg_dialogs(&mut self, ctx: &egui::Context, shared: &Shared) {
        if self.seg_offer {
            let newest = self.segments.saved.first().map(|s| s.describe()).unwrap_or_default();
            let mut load = false;
            let mut close = false;
            egui::Window::new("Saved segments")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This series has saved segments.");
                    ui.label(egui::RichText::new(newest).small());
                    ui.horizontal(|ui| {
                        load = ui.button("Load them").clicked();
                        close = ui.button("Not now").clicked();
                    });
                });
            if load {
                self.load_newest_segments();
            }
            if load || close {
                self.seg_offer = false;
            }
        }
        if self.close_prompt {
            let (mut save, mut discard, mut cancel) = (false, false, false);
            egui::Window::new("Unsaved segments")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Save the segments to the study before closing?");
                    ui.horizontal(|ui| {
                        save = ui
                            .add_enabled(!self.segments.saving, egui::Button::new("Save"))
                            .clicked();
                        discard = ui.button("Do not save").clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if save {
                self.close_after_save = true;
                self.close_prompt = false;
                self.save_segments(shared);
            }
            if discard {
                self.close_requested = true;
            }
            if cancel {
                self.close_prompt = false;
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        crate::crash::debug_panic("mpr");
        if ui.input(|i| i.viewport().close_requested()) {
            if self.segments.dirty && self.segments.seg.is_some() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_prompt = true;
            } else {
                if let Some((_, _, cancel)) = &self.loading {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.close_requested = true;
            }
        }
        self.pump(shared);
        self.pump_seg_save(shared);
        if std::mem::take(&mut self.segments.save_requested) {
            self.save_segments(shared);
        }
        if std::mem::take(&mut self.segments.load_requested) {
            self.load_newest_segments();
        }
        #[cfg(feature = "ai")]
        if let Some(dims) = self.volume.as_ref().map(|v| v.dims) {
            let cut_box = self.three_d.as_ref().and_then(|v| v.cut_box);
            self.segments.ai.cut_box = cut_box.and_then(|b| b.limits(dims));
        }
        self.keys(ui);
        ui.style_mut().visuals = crate::theme::viewer_visuals();
        egui::Panel::top(("mpr-toolbar", self.id))
            .exact_size(crate::theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(crate::theme::toolbar_stroke(true))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                crate::theme::paint_toolbar_background(ui, true, egui::vec2(6.0, 3.0));
                self.toolbar(ui, shared)
            });
        egui::Panel::bottom("mpr-status").show(ui, |ui| {
            ui.label(egui::RichText::new(&self.status).size(12.0));
        });
        if self.segments.open {
            if let Some(volume) = self.volume.clone() {
                #[cfg(feature = "ai")]
                {
                    self.sync_ai(shared);
                    self.segments.pump_ai(&volume);
                }
                if let Some(z) = self.segments.jump.take() {
                    let mut at = volume.to_index(self.centre);
                    at[2] = z as f64;
                    self.centre = volume.to_patient(at);
                }
                let root = shared.paths.root.clone();
                let source = format!(
                    "{}, {}",
                    self.series.series_description,
                    self.study.study_date.as_deref().unwrap_or("no date")
                );
                egui::Panel::right("mpr-segments").default_size(270.0).show(ui, |ui| {
                    self.segments.ui(ui, &volume, &root, &source);
                });
            }
        }
        if let Some(v) = self.three_d.as_mut() {
            let changed =
                v.segment_generation != self.segments.generation || v.segment_only != self.segments.only_segments;
            if changed && !self.segments.ai_busy() {
                v.set_segments(shared, &self.segments);
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.panes_ui(ui, shared));
        let ctx = ui.ctx().clone();
        self.text_prompt_ui(&ctx);
        self.save_dialog(&ctx, shared);
        self.seg_dialogs(&ctx, shared);

        // Developer option: `RUTHOS_MPR_SCREENSHOT=<file.png>` saves the
        // window as drawn, once the volume has been on screen a moment.
        if self.volume.is_some() {
            // Developer option: `RUTHOS_MPR_3D=<preset number>` opens the 3D
            // pane on that preset; `RUTHOS_MPR_3D_TURN=<radians>` turns it.
            if self.frames_shown == 0 {
                if let Some(n) = std::env::var("RUTHOS_MPR_3D")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    if let (Some(range), Some(volume)) = (std::env::var("RUTHOS_MPR_SEG").ok(), self.volume.clone()) {
                        if let Some((low, high)) = range.split_once(',') {
                            let (low, high) = (low.parse().unwrap_or(200.0), high.parse().unwrap_or(3000.0));
                            let mask = ruthos_segment::tools::threshold(volume.as_ref(), low, high);
                            self.segments.add(&volume, "Threshold", "Threshold", &mask);
                            self.segments.split_now(&volume);
                            self.segments.open = true;
                        }
                    }
                    self.set_3d(true, shared);
                    if let Some(v) = self.three_d.as_mut() {
                        v.set_preset(n.min(v.presets.len() - 1));
                        if let Some(a) = std::env::var("RUTHOS_MPR_3D_TURN")
                            .ok()
                            .and_then(|t| t.parse::<f64>().ok())
                        {
                            v.camera.orbit(a, a / 3.0);
                        }
                        // `RUTHOS_MPR_SEG=low,high` adds a threshold segment,
                        // opens the panel and splits it into pieces.
                        // `RUTHOS_MPR_3D_BOX=x0,y0,z0,x1,y1,z1` (shares of the
                        // volume) builds a box and `RUTHOS_MPR_3D_CUT=inside`
                        // or `outside` then cuts with it.
                        let share_box: Vec<f64> = std::env::var("RUTHOS_MPR_3D_BOX")
                            .ok()
                            .iter()
                            .flat_map(|c| c.split(','))
                            .filter_map(|n| n.parse().ok())
                            .collect();
                        if let [a, b, c, d, e, f] = share_box[..] {
                            let n = v.placement.dims.map(|d| d as f64);
                            v.tool = Tool3d::CutBox;
                            v.cut_box = Some(CutBox {
                                low: [a * n[0] - 0.5, b * n[1] - 0.5, c * n[2] - 0.5],
                                high: [d * n[0] - 0.5, e * n[1] - 0.5, f * n[2] - 0.5],
                            });
                            if let Some(how) = std::env::var("RUTHOS_MPR_3D_CUT").ok().filter(|h| !h.is_empty()) {
                                v.cut_shape(shared, how == "inside");
                            }
                        }
                    }
                }
            }
            #[cfg(feature = "ai")]
            let waiting = match self.volume.clone() {
                Some(volume) => {
                    self.sync_ai(shared);
                    self.segments.dev_option(&volume, &shared.paths.root)
                }
                None => false,
            };
            #[cfg(not(feature = "ai"))]
            let waiting = false;
            // Developer option: `RUTHOS_MPR_SAVE_SEGMENTS` saves the segments once
            // the model has finished.
            let saving = self.seg_save.is_some()
                || (!waiting && !self.dev_saved && std::env::var_os("RUTHOS_MPR_SAVE_SEGMENTS").is_some());
            if !waiting && saving && !self.dev_saved {
                self.dev_saved = true;
                self.save_segments(shared);
            }
            if waiting || saving {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            } else {
                self.frames_shown += 1;
            }
            if self.frames_shown < 30 {
                ui.ctx().request_repaint();
            }
            if self.frames_shown == 20 {
                if let Some(path) = std::env::var_os("RUTHOS_MPR_SCREENSHOT") {
                    ui.ctx().request_screenshot(move |img| {
                        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                        if let Some(png) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                            let _ = png.save(&path);
                        }
                    });
                }
            }
        }
    }

    /// Tool letters, Delete and Escape, as in the 2D viewer.
    fn keys(&mut self, ui: &Ui) {
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let (pressed, delete, escape) = ui.input(|i| {
            let plain = !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.command;
            let tool = MprTool::ALL.into_iter().find(|t| plain && i.key_pressed(t.key()));
            (
                tool,
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::Escape),
            )
        });
        if let Some(t) = pressed {
            self.tool = t;
            self.angle_draft = None;
        }
        if escape {
            self.draft = None;
            self.angle_draft = None;
            self.selected = None;
        }
        if delete {
            if let Some(i) = self.selected.take() {
                if i < self.rois.len() {
                    self.rois.remove(i);
                }
            }
        }
    }

    /// The toolbar, built as the viewer's is: buttons with an icon over a label,
    /// and menus with a caption saying what is set.
    fn toolbar(&mut self, ui: &mut Ui, shared: &Shared) {
        let ready = self.volume.is_some();
        ui.horizontal(|ui| {
            if ToolbarButton::new(Bitmap::DatabaseWindow, "Database")
                .tooltip("Show the database window")
                .show(ui)
                .clicked()
            {
                shared.focus_browser.store(true, Ordering::Relaxed);
            }
            toolbar_separator(ui);
            if ToolbarButton::new(Bitmap::Reset, "Reset")
                .enabled(ready)
                .tooltip("Back to the original planes")
                .show(ui)
                .clicked()
            {
                self.reset();
            }
            if ToolbarButton::new(icons::LOCKED, "Lock")
                .enabled(ready)
                .selected(self.locked)
                .tooltip("Stop the planes moving or tilting while you measure")
                .show(ui)
                .clicked()
            {
                self.locked = !self.locked;
            }
            toolbar_separator(ui);
            ui.add_enabled_ui(ready, |ui| {
                self.layout_menu(ui);
                self.slab_menu(ui);
            });
            toolbar_separator(ui);
            ui.add_enabled_ui(ready, |ui| {
                self.tool_menu(ui);
                self.window_menu(ui, shared);
                self.clut_menu(ui, shared);
            });
            toolbar_separator(ui);
            if !ruthos_core::portable::active()
                && ToolbarButton::new(icons::SERIES, "Segments")
                    .enabled(ready)
                    .selected(self.segments.open)
                    .tooltip("Threshold, region grow, islands and AI, on the slices and in 3D")
                    .show(ui)
                    .clicked()
            {
                self.segments.open = !self.segments.open;
            }
            if ToolbarButton::new(icons::THREE_D, "3D")
                .enabled(ready)
                .selected(self.three_d.is_some())
                .tooltip("Volume rendering in the fourth pane")
                .show(ui)
                .clicked()
            {
                let on = self.three_d.is_none();
                self.set_3d(on, shared);
            }
            self.three_d_controls(ui, shared);
            toolbar_separator(ui);
            if ToolbarButton::new(Bitmap::Save, "Save to study")
                .width(76.0)
                .enabled(ready)
                .tooltip("Write this plane, or a stack of planes, as a new series in the study, with its measurements")
                .show(ui)
                .clicked()
            {
                self.open_save_dialog();
            }
            if ToolbarButton::new(Bitmap::Export, "Save image")
                .width(68.0)
                .enabled(ready)
                .tooltip("Save the pane last worked in as a PNG or JPEG picture")
                .show(ui)
                .clicked()
            {
                self.save_image();
            }
        });
    }

    fn open_save_dialog(&mut self) {
        let pane = self.active;
        let spacing = self.volume.as_ref().map(|v| v.finest().max(0.5)).unwrap_or(1.0);
        self.save = Some(SaveDialog {
            pane,
            stack: false,
            spacing_mm: (spacing * 10.0).round() / 10.0,
            description: format!("MPR {}", self.pane_name(pane).to_lowercase()),
            running: None,
            progress: Arc::new(Mutex::new((0, 0))),
            message: None,
        });
    }

    fn layout_menu(&mut self, ui: &mut Ui) {
        let caption = match self.layout {
            Layout::Row => "Layout: 1×3".to_string(),
            Layout::Grid => "Layout: 2×2".to_string(),
            Layout::Single(i) => format!("Layout: {}", self.pane_name(i)),
        };
        let names: Vec<String> = (0..4).map(|i| self.pane_name(i)).collect();
        menu_with_caption(ui, Icon::Glyph(icons::TILING), &caption, |ui| {
            if menu_item(ui, "1×3: three planes", self.layout == Layout::Row).clicked() {
                self.layout = Layout::Row;
                ui.close();
            }
            let grid = if self.three_d.is_some() {
                "2×2: three planes and 3D"
            } else {
                "2×2: three planes and the original slice"
            };
            if menu_item(ui, grid, self.layout == Layout::Grid).clicked() {
                self.layout = Layout::Grid;
                ui.close();
            }
            ui.separator();
            for (i, name) in names.iter().enumerate() {
                if menu_item(ui, format!("1×1: {name}"), self.layout == Layout::Single(i)).clicked() {
                    self.layout = Layout::Single(i);
                    ui.close();
                }
            }
        });
    }

    fn slab_menu(&mut self, ui: &mut Ui) {
        let mode = match self.slab_mode {
            SlabMode::Mip => "MIP",
            SlabMode::MinIp => "MinIP",
            SlabMode::Mean => "Mean",
        };
        let caption = if self.slab_mm > 0.0 {
            format!("Slab: {:.1} mm {mode}", self.slab_mm)
        } else {
            "Slab: off".to_string()
        };
        menu_with_caption(ui, Icon::Bitmap(Bitmap::Stack), &caption, |ui| {
            menu_heading(ui, "Thickness");
            ui.add(
                egui::Slider::new(&mut self.slab_mm, 0.0..=60.0)
                    .suffix(" mm")
                    .step_by(0.5),
            )
            .on_hover_text("0 shows a single plane");
            ui.separator();
            for (m, label, hint) in [
                (SlabMode::Mip, "MIP", "Maximum intensity: vessels, bone, contrast"),
                (SlabMode::MinIp, "MinIP", "Minimum intensity: airways, gas"),
                (SlabMode::Mean, "Mean", "Average: a thicker, smoother slice"),
            ] {
                if menu_item(ui, label, self.slab_mode == m).on_hover_text(hint).clicked() {
                    self.slab_mode = m;
                    ui.close();
                }
            }
        });
    }

    fn tool_menu(&mut self, ui: &mut Ui) {
        let caption = format!("Tool: {}", self.tool.label());
        menu_with_caption(ui, self.tool.icon(), &caption, |ui| {
            let mut row = |ui: &mut Ui, t: MprTool| {
                let label = format!("{}  ({:?})", t.label(), t.key());
                if tool_menu_row(ui, t.icon(), &label, self.tool == t, false).clicked() {
                    self.tool = t;
                    self.angle_draft = None;
                    ui.close();
                }
            };
            menu_heading(ui, "Navigate");
            for t in MprTool::ALL
                .into_iter()
                .filter(|t| !t.measures() && *t != MprTool::Text)
            {
                row(ui, t);
            }
            menu_heading(ui, "Measure and annotate");
            for t in MprTool::ALL.into_iter().filter(|t| t.measures() || *t == MprTool::Text) {
                row(ui, t);
            }
        });
    }

    fn window_menu(&mut self, ui: &mut Ui, shared: &Shared) {
        let caption = format!("WL {:.0}  WW {:.0}", self.window.0, self.window.1);
        menu_with_caption(ui, Icon::Bitmap(Bitmap::Wlww), &caption, |ui| {
            if menu_item(ui, "Default WL & WW", false).clicked() {
                if let Some(v) = &self.volume {
                    self.window = volume_window(v);
                }
                ui.close();
            }
            ui.separator();
            for p in &shared.presets.presets {
                if menu_item(ui, format!("{}  ({} / {})", p.name, p.center, p.width), false).clicked() {
                    self.window = (p.center, p.width);
                    ui.close();
                }
            }
            ui.separator();
            menu_heading(ui, "Custom");
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label("WL");
                ui.add(egui::DragValue::new(&mut self.window.0).speed(2.0));
                ui.label("WW");
                ui.add(
                    egui::DragValue::new(&mut self.window.1)
                        .speed(2.0)
                        .range(1.0..=100000.0),
                );
            });
        });
    }

    fn clut_menu(&mut self, ui: &mut Ui, shared: &Shared) {
        let caption = crate::widgets::truncate(&self.clut, 12);
        menu_with_caption(ui, Icon::Glyph(icons::CLUT), &caption, |ui| {
            for name in shared.cluts.names() {
                if menu_item(ui, name, self.clut == name).clicked() {
                    self.clut = name.to_string();
                    ui.close();
                }
            }
        });
    }

    /// The 3D pane's own controls, shown only while it is open.
    fn three_d_controls(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(v) = self.three_d.as_mut() else { return };
        let caption = format!("3D: {}", v.tool.label());
        menu_with_caption(ui, v.tool.icon(), &caption, |ui| {
            let row = |ui: &mut Ui, v: &mut View3d, t: Tool3d| {
                if tool_menu_row(ui, t.icon(), t.label(), v.tool == t, false)
                    .on_hover_text(t.hint())
                    .clicked()
                {
                    v.tool = t;
                    if t == Tool3d::CutBox && v.cut_box.is_none() {
                        v.cut_box = Some(CutBox::around(v.placement.dims));
                    }
                    ui.close();
                }
            };
            menu_heading(ui, "View");
            for t in Tool3d::VIEW {
                row(ui, v, t);
            }
            menu_heading(ui, "Cut");
            for t in Tool3d::CUT {
                row(ui, v, t);
            }
            ui.separator();
            if menu_item_enabled(ui, "Undo cut", false, v.cuts > 0).clicked() {
                v.undo_cut(shared);
                ui.close();
            }
            if menu_item_enabled(ui, "Restore everything cut", false, v.cuts > 0).clicked() {
                v.restore_cuts(shared);
                ui.close();
            }
            if menu_item(ui, "Reset view", false).clicked() {
                v.reset_view();
                ui.close();
            }
        });
        let caption = crate::widgets::truncate(&v.presets[v.preset].name, 13);
        menu_with_caption(ui, Icon::Glyph(icons::CLUT), &caption, |ui| {
            let mut chosen = None;
            for (i, p) in v.presets.iter().enumerate() {
                if menu_item(ui, p.name.as_str(), i == v.preset).clicked() {
                    chosen = Some(i);
                    ui.close();
                }
            }
            if let Some(i) = chosen {
                v.set_preset(i);
            }
            ui.separator();
            for (mode, label) in [
                (RenderMode::Shaded, "Shaded"),
                (RenderMode::Maximum, "Maximum intensity (MIP)"),
                (RenderMode::Minimum, "Minimum intensity (MinIP)"),
            ] {
                if menu_item(ui, label, v.mode == mode).clicked() {
                    v.mode = mode;
                    ui.close();
                }
            }
        });
        if matches!(v.tool, Tool3d::CutBox | Tool3d::CutPath) {
            toolbar_separator(ui);
            let ready = v.has_shape();
            if ToolbarButton::new(icons::TRASH, "Cut inside")
                .width(64.0)
                .enabled(ready)
                .tooltip("Remove what the shape encloses")
                .show(ui)
                .clicked()
            {
                v.cut_shape(shared, true);
            }
            if ToolbarButton::new(icons::THREE_D, "Cut outside")
                .width(68.0)
                .enabled(ready)
                .tooltip("Keep only what the shape encloses")
                .show(ui)
                .clicked()
            {
                v.cut_shape(shared, false);
            }
            if ToolbarButton::new(icons::RESET, "Clear")
                .tooltip("Discard the shape")
                .show(ui)
                .clicked()
            {
                v.clear_shape();
            }
        }
    }

    fn panes_ui(&mut self, ui: &mut Ui, shared: &Shared) {
        let area = ui.available_rect_before_wrap();
        if let Some(e) = &self.error {
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                format!("This series cannot be reconstructed.\n{e}"),
                FontId::proportional(15.0),
                Color32::from_rgb(240, 150, 120),
            );
            return;
        }
        let Some(volume) = self.volume.clone() else {
            let (done, total) = self.loading.as_ref().map(|l| *l.1.lock()).unwrap_or((0, 0));
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                format!("Building the volume: slice {done} of {total}"),
                FontId::proportional(15.0),
                Color32::from_gray(200),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let gap = 2.0;
        let cells: Vec<(usize, Rect)> = match self.layout {
            Layout::Row => {
                let w = (area.width() - 2.0 * gap) / 3.0;
                (0..3)
                    .map(|i| {
                        (
                            i,
                            Rect::from_min_size(
                                Pos2::new(area.left() + i as f32 * (w + gap), area.top()),
                                Vec2::new(w, area.height()),
                            ),
                        )
                    })
                    .collect()
            }
            Layout::Grid => {
                let (w, h) = ((area.width() - gap) / 2.0, (area.height() - gap) / 2.0);
                (0..4)
                    .map(|i| {
                        let (c, r) = ((i % 2) as f32, (i / 2) as f32);
                        (
                            i,
                            Rect::from_min_size(
                                Pos2::new(area.left() + c * (w + gap), area.top() + r * (h + gap)),
                                Vec2::new(w, h),
                            ),
                        )
                    })
                    .collect()
            }
            Layout::Single(i) => vec![(i, area)],
        };
        let clut = Arc::new(shared.cluts.get(&self.clut).cloned().unwrap_or_else(Clut::grayscale));
        for (i, rect) in cells {
            if i == 3 && self.three_d.is_some() {
                self.pane_3d(ui, shared, rect);
            } else {
                self.pane(ui, shared, &volume, i, rect, &clut);
            }
        }
    }

    fn pane_3d(&mut self, ui: &mut Ui, shared: &Shared, rect: Rect) {
        let key = self.id * 64 + 48;
        let assisted = self.segments.has_ai();
        let volume = self.volume.clone();
        let Some(v) = self.three_d.as_mut() else { return };
        let response = ui.interact(rect, egui::Id::unique(("mpr-3d", self.id)), Sense::click_and_drag());
        let aspect = (rect.width() / rect.height().max(1.0)) as f64;
        let d = response.drag_delta();
        let share = |value: f32| (value / rect.height().max(1.0)) as f64 * 2.0;
        let primary = response.dragged_by(egui::PointerButton::Primary);
        let pointer = response.interact_pointer_pos();
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(at) = ui.input(|i| i.pointer.press_origin()) {
                v.grab(rect, aspect, at);
            }
        }
        if response.drag_stopped() {
            v.grabbed = None;
        }
        if response.dragged_by(egui::PointerButton::Middle) || (primary && v.tool == Tool3d::Move) {
            v.camera.slide(share(d.x), -share(d.y));
            v.moved = Some(std::time::Instant::now());
        } else if response.dragged_by(egui::PointerButton::Secondary) || (primary && v.tool == Tool3d::Zoom) {
            v.camera.zoom((d.y as f64 * 0.006).exp());
            v.moved = Some(std::time::Instant::now());
        } else if primary {
            match (v.grabbed, v.tool) {
                (Some(g), _) => v.drag_grabbed(g, rect, aspect, d, pointer),
                // Off every handle the box tool still turns the view, so a box
                // can be checked from all sides as it is built.
                (None, Tool3d::Rotate | Tool3d::CutBox) => {
                    v.camera.orbit(-d.x as f64 * 0.008, -d.y as f64 * 0.008);
                    v.moved = Some(std::time::Instant::now());
                }
                (None, Tool3d::Window) => {
                    v.scale = (v.scale * (d.x * 0.004).exp()).clamp(0.05, 20.0);
                    v.shift += d.y * (v.gpu.max - v.gpu.min) / 800.0;
                    v.rebake();
                }
                (None, Tool3d::CutPath) => {
                    if let Some(p) = pointer {
                        let last = v.cut_path.as_ref().and_then(|c| c.points.last()).copied();
                        let far = last.is_none_or(|q| v.to_screen(rect, aspect, q).distance(p) >= 4.0);
                        if far {
                            let at = to_view(rect, p);
                            v.add_point([at[0] as f64, at[1] as f64], aspect);
                        }
                    }
                }
                _ => {}
            }
        }
        if response.clicked() && v.tool == Tool3d::CutPath {
            if let Some(p) = pointer {
                let at = to_view(rect, p);
                v.add_point([at[0] as f64, at[1] as f64], aspect);
            }
        }
        if response.clicked() && v.tool == Tool3d::RemovePiece {
            if let (Some(p), Some(volume)) = (pointer, volume.as_ref()) {
                let at = to_view(rect, p);
                let removed = v.remove_piece(shared, volume, [at[0] as f64, at[1] as f64], aspect);
                self.status = match removed {
                    Some(n) => format!("Removed {n} voxels"),
                    None => "Nothing visible there".to_string(),
                };
            }
        }
        let Some(v) = self.three_d.as_mut() else { return };
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                v.camera.zoom((-scroll as f64 * 0.002).exp());
                v.moved = Some(std::time::Instant::now());
            }
        }
        let moving = v.moved.is_some_and(|t| t.elapsed() < REFINE_AFTER);
        if moving {
            ui.ctx().request_repaint_after(REFINE_AFTER);
        }
        let finest = v.gpu.spacing.iter().copied().fold(f64::MAX, f64::min) as f32;
        let step_mm = finest * if moving { 2.0 } else { 0.3 };
        let painter = ui.painter().with_clip_rect(rect);
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            VolumeCallback {
                key,
                volume: v.gpu.clone(),
                transfer: v.baked.clone(),
                segments: v.segments.clone(),
                use_segments: v.cuts > 0 || v.segment_labels.is_some(),
                placement: v.placement,
                camera: v.camera,
                aspect: (rect.width() / rect.height().max(1.0)) as f64,
                mode: v.mode,
                step_mm,
            },
        ));
        painter.text(
            rect.left_top() + Vec2::new(6.0, 6.0),
            Align2::LEFT_TOP,
            format!("3D: {}", v.presets[v.preset].name),
            FontId::proportional(13.0),
            Color32::from_gray(210),
        );
        v.draw_shape(&painter, rect, aspect);
        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            if assisted {
                "AI-assisted: fine fractures may not appear. The slices are the reference"
            } else {
                "A derived picture: the slices are the reference"
            },
            FontId::proportional(11.0),
            Color32::from_gray(170),
        );
    }

    fn pane(&mut self, ui: &mut Ui, shared: &Shared, volume: &Volume, i: usize, rect: Rect, clut: &Arc<Clut>) {
        // The image, remade when this pane's plane changed.
        let plane = self.plane(i, volume);
        let n = plane.normal();
        let key = plane_key(&plane);
        let sig = format!("{key}|{:.2}|{:?}", self.slab_mm, self.slab_mode);
        let overlay_sig = format!("{key}|{}", self.segments.generation);
        if self.panes[i].overlay_signature != overlay_sig {
            self.panes[i].overlay = self.segments.overlay(volume, &plane).map(|rgba| {
                ui.ctx().load_texture(
                    format!("mpr-overlay-{}-{i}", self.id),
                    egui::ColorImage::from_rgba_unmultiplied([plane.width as usize, plane.height as usize], &rgba),
                    egui::TextureOptions::NEAREST,
                )
            });
            self.panes[i].overlay_signature = overlay_sig;
        }
        if self.panes[i].signature != sig {
            let frame = Arc::new(reslice(volume, &plane));
            self.panes[i].gpu = Some(Arc::new(GpuImage::upload(
                &shared.render_state.device,
                &shared.render_state.queue,
                &frame,
            )));
            self.panes[i].frame = Some(frame);
            self.panes[i].plane = Some(plane);
            self.panes[i].signature = sig;
        }
        let response = ui.interact(
            rect,
            egui::Id::unique(("mpr-pane", self.id, i)),
            Sense::click_and_drag(),
        );
        let window = self.window;
        let pane = &mut self.panes[i];
        if !pane.fitted {
            pane.state.fit(rect, plane.width, plane.height, 1.0);
            pane.fitted = true;
        }
        pane.state.window_center = window.0;
        pane.state.window_width = window.1;
        let t = ViewTransform::new(rect, plane.width, plane.height, 1.0, pane.state);
        let (gpu, frame) = (pane.gpu.clone(), pane.frame.clone());
        let painter = ui.painter_at(rect);
        if let Some(gpu) = gpu {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ImageCallback {
                    key: self.id * 64 + 40 + i as u64,
                    image: gpu,
                    clut: clut.clone(),
                    transform: t,
                    inverted: false,
                    interpolation: Interpolation::Bilinear,
                },
            ));
        }

        if let Some(overlay) = &self.panes[i].overlay {
            let (a, b) = (
                t.image_to_screen(Vec2::ZERO),
                t.image_to_screen(Vec2::new(plane.width as f32, plane.height as f32)),
            );
            painter.image(
                overlay.id(),
                Rect::from_two_pos(a, b),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // A handle of the range control: its value from a point on a slice.
        #[cfg(feature = "ai")]
        if self.segments.ai.picking_handle() && response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let q = t.screen_to_image(p);
                if let Some(voxel) = crate::segments::voxel_at(volume, plane.point(q.x as f64, q.y as f64)) {
                    self.segments.ai.pick_handle(ruthos_segment::Field::at(volume, voxel));
                }
            }
        }
        // The model's prompt: a click on a slice.
        #[cfg(feature = "ai")]
        if self.segments.ai_picking() && response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let q = t.screen_to_image(p);
                if let Some(voxel) = crate::segments::voxel_at(volume, plane.point(q.x as f64, q.y as f64)) {
                    let [w, h, _] = volume.dims;
                    self.segments.ai.click(voxel % w, (voxel / w) % h, voxel / (w * h));
                }
            }
        }
        // Region grow's seed: a click on a slice.
        if self.segments.picking && response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let q = t.screen_to_image(p);
                match crate::segments::voxel_at(volume, plane.point(q.x as f64, q.y as f64)) {
                    Some(seed) => {
                        if let Some(whole) = self.volume.clone() {
                            self.segments.grow_from(&whole, seed);
                        }
                    }
                    None => self.segments.note = Some("That point is outside the volume".to_string()),
                }
            }
        }

        // The crosshair: where the other planes cut this one.
        let (cx, cy) = plane.pixel_of(self.centre);
        let centre_screen = t.image_to_screen(Vec2::new(cx as f32, cy as f32));
        let reach = rect.width().max(rect.height()) * 2.0;
        let handle_at = rect.width().min(rect.height()) * 0.32;
        let mut lines: Vec<(usize, Vec2)> = Vec::new();
        for j in (0..3).filter(|j| *j != i) {
            let (nj, _, _) = self.basis(j);
            let c = cross(n, nj);
            if norm(c) < 1e-3 {
                continue; // parallel: no line
            }
            let d = normalize(c);
            let tip = t.image_to_screen(Vec2::new(
                (cx + dot(d, plane.right)) as f32,
                (cy + dot(d, plane.down)) as f32,
            ));
            lines.push((j, (tip - centre_screen).normalized()));
        }
        let rotatable = i != ORIGINAL;
        for (j, dir) in &lines {
            let colour = PLANE_COLOURS[*j];
            for s in [-1.0f32, 1.0] {
                painter.line_segment(
                    [centre_screen + *dir * s * 12.0, centre_screen + *dir * s * reach],
                    Stroke::new(1.0, colour),
                );
                if rotatable && !self.locked {
                    painter.circle_stroke(centre_screen + *dir * s * handle_at, 5.0, Stroke::new(1.5, colour));
                }
            }
        }
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, PLANE_COLOURS[i]), egui::StrokeKind::Inside);

        // Measurements on this plane, and the one being drawn.
        if let Some(frame) = &frame {
            for (k, pr) in self.rois.iter().enumerate().filter(|(_, r)| r.key == key) {
                let sel = self.selected == Some(k);
                pr.roi.draw(&painter, &t, frame, roi_color(sel), sel);
                if sel {
                    pr.roi.draw_handles(&painter, &t, None, roi_color(true));
                }
            }
            if let Some((p, a, b)) = self.draft {
                if p == i {
                    if let Some(kind) = draft_kind(self.tool, a, b) {
                        Roi::new(kind).draw(&painter, &t, frame, crate::theme::ROI_DRAFT, false);
                    }
                }
            }
            if let Some(d) = self.angle_draft.filter(|d| d.pane == i) {
                Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: d.c }).draw(
                    &painter,
                    &t,
                    frame,
                    crate::theme::ROI_DRAFT,
                    false,
                );
            }
        }

        // Words: the plane, its position, the slab and the orientation.
        let font = FontId::proportional(13.0);
        let text = Color32::from_gray(230);
        crate::widgets::shadow_text(
            &painter,
            rect.left_top() + Vec2::new(8.0, 6.0),
            Align2::LEFT_TOP,
            &format!(
                "{}\n{:.1} mm{}{}",
                self.pane_name(i),
                dot(self.centre, n),
                if self.slab_mm > 0.0 && i != ORIGINAL {
                    format!(
                        "\nSlab {:.1} mm {}",
                        self.slab_mm,
                        match self.slab_mode {
                            SlabMode::Mip => "MIP",
                            SlabMode::MinIp => "MinIP",
                            SlabMode::Mean => "mean",
                        }
                    )
                } else {
                    String::new()
                },
                if self.locked { "\nPlanes locked" } else { "" }
            ),
            font.clone(),
            text,
        );
        crate::widgets::shadow_text(
            &painter,
            rect.left_bottom() + Vec2::new(8.0, -6.0),
            Align2::LEFT_BOTTOM,
            &format!("WL {:.0}  WW {:.0}", self.window.0, self.window.1),
            font.clone(),
            text,
        );
        let neg = |v: V3| scale(v, -1.0);
        for (pos, anchor, d) in [
            (
                rect.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                neg(plane.right),
            ),
            (
                rect.right_center() - Vec2::new(6.0, 0.0),
                Align2::RIGHT_CENTER,
                plane.right,
            ),
            (
                rect.center_top() + Vec2::new(0.0, 6.0),
                Align2::CENTER_TOP,
                neg(plane.down),
            ),
            (
                rect.center_bottom() - Vec2::new(0.0, 6.0),
                Align2::CENTER_BOTTOM,
                plane.down,
            ),
        ] {
            crate::widgets::shadow_text(&painter, pos, anchor, &orientation_label(d), font.clone(), text);
        }

        // The value and position under the pointer.
        if let Some(h) = response.hover_pos() {
            let ip = t.screen_to_image(h);
            if let Some(frame) = &frame {
                if let Some(v) = frame.value_at(ip.x.floor() as i64, ip.y.floor() as i64) {
                    let p = plane.point(ip.x as f64 - 0.5, ip.y as f64 - 0.5);
                    self.status = format!(
                        "{}: value {v:.0}   x {:.1}  y {:.1}  z {:.1} mm",
                        self.pane_name(i),
                        p[0],
                        p[1],
                        p[2]
                    );
                }
            }
        }

        self.pane_input(ui, &response, i, &t, &plane, &key, centre_screen, &lines, handle_at);
    }

    /// The index of the measurement on this plane under a screen point.
    fn roi_at(&self, key: &str, t: &ViewTransform, p: Pos2) -> Option<usize> {
        self.rois
            .iter()
            .enumerate()
            .filter(|(_, r)| r.key == key)
            .map(|(k, r)| (k, r.roi.screen_distance(t, p)))
            .filter(|(_, d)| *d < 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        i: usize,
        t: &ViewTransform,
        plane: &Plane,
        key: &str,
        centre_screen: Pos2,
        lines: &[(usize, Vec2)],
        handle_at: f32,
    ) {
        let n = plane.normal();
        if response.hovered() && (response.clicked() || response.drag_started()) {
            self.active = i;
        }
        // Wheel: step this pane's plane (unless locked); with Ctrl, zoom.
        if response.hovered() {
            let (scroll, ctrl, zoom) =
                ui.input(|inp| (inp.smooth_scroll_delta.y, inp.modifiers.ctrl, inp.zoom_delta()));
            if (zoom - 1.0).abs() > 1e-3 || (ctrl && scroll.abs() > 0.0) {
                let factor = if (zoom - 1.0).abs() > 1e-3 {
                    zoom
                } else {
                    (scroll * 0.002).exp()
                };
                let anchor = response.hover_pos().unwrap_or(t.rect.center());
                self.zoom_about(i, t, anchor, factor);
            } else if scroll.abs() > 0.0 && !self.locked {
                let step = self.volume.as_ref().map(|v| v.finest()).unwrap_or(1.0);
                let steps = (scroll / 40.0).round().clamp(-10.0, 10.0);
                let steps = if steps == 0.0 { scroll.signum() } else { steps };
                self.centre = sub(self.centre, scale(n, step * steps as f64));
            }
        }

        // Clicks: select a measurement, place a point, a text note, or the
        // end of an angle.
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let ip = t.screen_to_image(p);
                if let Some(d) = self.angle_draft.filter(|d| d.pane == i) {
                    let roi = Roi::new(RoiKind::Angle { a: d.a, b: d.b, c: ip });
                    self.angle_draft = None;
                    if !roi.is_degenerate() {
                        self.push_roi(key, plane, roi);
                    }
                } else if let Some(k) = self.roi_at(key, t, p) {
                    self.selected = Some(k);
                } else {
                    self.selected = None;
                    match self.tool {
                        MprTool::Point => self.push_roi(key, plane, Roi::new(RoiKind::Point { p: ip })),
                        MprTool::Text => {
                            self.text_prompt = Some(TextPrompt {
                                pane: i,
                                at: ip,
                                screen: p,
                                text: String::new(),
                                focus: true,
                            })
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(d) = &mut self.angle_draft {
            if d.pane == i {
                if let Some(h) = response.hover_pos() {
                    d.c = t.screen_to_image(h);
                }
            }
        }

        // A press decides what the drag does.
        if response.drag_started() {
            let origin = ui.input(|inp| inp.pointer.press_origin()).or(response.hover_pos());
            let (primary, secondary, middle) = ui.input(|inp| {
                (
                    inp.pointer.button_down(egui::PointerButton::Primary),
                    inp.pointer.button_down(egui::PointerButton::Secondary),
                    inp.pointer.button_down(egui::PointerButton::Middle),
                )
            });
            let what = if middle {
                Drag::Pan
            } else if secondary {
                Drag::Window
            } else if let (true, Some(p)) = (primary, origin) {
                self.primary_drag(i, t, key, p, centre_screen, lines, handle_at)
            } else {
                Drag::Window
            };
            self.drag = Some((i, what));
        }
        if response.dragged() {
            if let Some((pane, what)) = self.drag.filter(|(p, _)| *p == i) {
                let d = response.drag_delta();
                // Screen points to millimetres in this plane.
                let mm = plane.pixel_mm / self.panes[pane].state.zoom.max(1e-6) as f64;
                let delta =
                    ruthos_core::volume::add(scale(plane.right, d.x as f64 * mm), scale(plane.down, d.y as f64 * mm));
                let pointer = response.hover_pos().or(ui.input(|inp| inp.pointer.latest_pos()));
                match what {
                    Drag::Centre => self.centre = ruthos_core::volume::add(self.centre, delta),
                    Drag::Line(j) => {
                        let (nj, _, _) = self.basis(j);
                        self.centre = ruthos_core::volume::add(self.centre, scale(nj, dot(delta, nj)));
                    }
                    Drag::Rotate(last) => {
                        if let Some(p) = pointer {
                            let rel = p - centre_screen;
                            let angle = (rel.y as f64).atan2(rel.x as f64);
                            let mut da = angle - last;
                            if da > std::f64::consts::PI {
                                da -= std::f64::consts::TAU;
                            } else if da < -std::f64::consts::PI {
                                da += std::f64::consts::TAU;
                            }
                            for a in &mut self.axes {
                                *a = normalize(rotate(*a, n, da));
                            }
                            self.phi[i] += da;
                            self.drag = Some((i, Drag::Rotate(angle)));
                        }
                    }
                    Drag::Window => {
                        let (c, w) = self.window;
                        let s = (w.abs() / 256.0).max(0.5);
                        self.window = (c + d.y * s, (w + d.x * s).max(1.0));
                    }
                    Drag::Pan => self.panes[i].state.pan += d,
                    Drag::Zoom => {
                        let anchor = t.rect.center() + self.panes[i].state.pan;
                        self.zoom_about(i, t, anchor, (-d.y * 0.01).exp());
                    }
                    Drag::Draw(a) => {
                        if let Some(p) = pointer {
                            self.draft = Some((i, a, t.screen_to_image(p)));
                        }
                    }
                    Drag::Handle(k, h) => {
                        if let Some(p) = pointer {
                            if let Some(r) = self.rois.get_mut(k) {
                                r.roi.set_handle(h, t.screen_to_image(p));
                            }
                        }
                    }
                    Drag::Move(k) => {
                        if let Some(r) = self.rois.get_mut(k) {
                            r.roi.translate(d / t.state.zoom.max(1e-6));
                        }
                    }
                }
            }
        }
        if response.drag_stopped() {
            if let Some((p, Drag::Draw(_))) = self.drag {
                if let Some((dp, a, b)) = self.draft.take() {
                    if dp == p && self.tool == MprTool::Angle {
                        if (b - a).length() >= 1.0 {
                            self.angle_draft = Some(AngleDraft { pane: p, a, b, c: b });
                            self.status = "Angle: now click the end of the second arm".into();
                        }
                    } else if let Some(kind) = draft_kind(self.tool, a, b) {
                        let roi = Roi::new(kind);
                        if !roi.is_degenerate() {
                            self.push_roi(key, plane, roi);
                        }
                    }
                }
            }
            self.drag = None;
        }
        if response.double_clicked() && !self.tool.measures() {
            self.panes[i].fitted = false;
        }
        // Keep the crosshair inside the volume.
        if let Some(v) = &self.volume {
            let idx = v.to_index(self.centre);
            let clamped = [
                idx[0].clamp(0.0, (v.dims[0] - 1) as f64),
                idx[1].clamp(0.0, (v.dims[1] - 1) as f64),
                idx[2].clamp(0.0, (v.dims[2] - 1) as f64),
            ];
            if clamped != idx {
                self.centre = v.to_patient(clamped);
            }
        }
    }

    /// What a left-button drag starting at `p` does, by tool.
    #[allow(clippy::too_many_arguments)]
    fn primary_drag(
        &mut self,
        i: usize,
        t: &ViewTransform,
        key: &str,
        p: Pos2,
        centre_screen: Pos2,
        lines: &[(usize, Vec2)],
        handle_at: f32,
    ) -> Drag {
        // A handle or the body of the selected measurement, with any tool.
        if let Some(k) = self
            .selected
            .filter(|k| self.rois.get(*k).is_some_and(|r| r.key == key))
        {
            if let Some(h) = self.rois[k].roi.handle_hit(t, p) {
                return Drag::Handle(k, h);
            }
            if self.rois[k].roi.screen_distance(t, p) < 6.0 {
                return Drag::Move(k);
            }
        }
        match self.tool {
            MprTool::Crosshair if !self.locked => {
                let rel = p - centre_screen;
                let on_handle = i != ORIGINAL
                    && lines.iter().any(|(_, dir)| {
                        [-1.0f32, 1.0]
                            .iter()
                            .any(|s| (p - (centre_screen + *dir * *s * handle_at)).length() < 9.0)
                    });
                if on_handle {
                    Drag::Rotate((rel.y as f64).atan2(rel.x as f64))
                } else if rel.length() < 12.0 {
                    Drag::Centre
                } else {
                    let near = lines
                        .iter()
                        .map(|(j, dir)| (*j, (rel - *dir * rel.dot(*dir)).length()))
                        .filter(|(_, d)| *d < 7.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1));
                    match near {
                        Some((j, _)) => Drag::Line(j),
                        None => Drag::Window,
                    }
                }
            }
            MprTool::Crosshair | MprTool::Window => Drag::Window,
            MprTool::Pan => Drag::Pan,
            MprTool::Zoom => Drag::Zoom,
            MprTool::Length | MprTool::Angle | MprTool::Rectangle | MprTool::Oval | MprTool::Arrow => {
                self.angle_draft = None;
                let a = t.screen_to_image(p);
                self.draft = Some((i, a, a));
                Drag::Draw(a)
            }
            MprTool::Point | MprTool::Text => Drag::Window,
        }
    }

    fn zoom_about(&mut self, i: usize, t: &ViewTransform, anchor: Pos2, factor: f32) {
        let state = &mut self.panes[i].state;
        let before = t.screen_to_image(anchor);
        state.zoom = (state.zoom * factor).clamp(0.05, 40.0);
        let t2 = ViewTransform::new(t.rect, t.image_w, t.image_h, 1.0, *state);
        state.pan += anchor - t2.image_to_screen(before);
    }

    fn push_roi(&mut self, key: &str, _plane: &Plane, roi: Roi) {
        self.rois.push(PlaneRoi {
            key: key.to_string(),
            roi,
        });
        self.selected = Some(self.rois.len() - 1);
    }

    fn text_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(mut tp) = self.text_prompt.take() else { return };
        let mut done: Option<bool> = None;
        egui::Area::new(egui::Id::unique(("ruthos-mpr-text", self.id)))
            .fixed_pos(tp.screen + Vec2::new(0.0, -28.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut tp.text)
                                .desired_width(220.0)
                                .hint_text("Text"),
                        );
                        if tp.focus {
                            edit.request_focus();
                            tp.focus = false;
                        }
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.button("Add").clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                    });
                });
            });
        match done {
            None => self.text_prompt = Some(tp),
            Some(false) => {}
            Some(true) => {
                let text = tp.text.trim().to_string();
                if let (false, Some(plane)) = (text.is_empty(), self.panes[tp.pane].plane) {
                    let mut roi = Roi::new(RoiKind::Text { p: tp.at });
                    roi.name = Some(text);
                    let key = plane_key(&plane);
                    self.push_roi(&key, &plane, roi);
                }
            }
        }
    }

    /// Save image: the pane last worked in, windowed as shown.
    fn save_image(&mut self) {
        let i = self.active;
        let Some(frame) = self.panes[i].frame.clone() else {
            return;
        };
        let name = format!(
            "{}_{}.png",
            self.study.patient_name.replace([',', ' ', '^'], "_"),
            self.pane_name(i).replace(' ', "_")
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save image")
            .add_filter("PNG image", &["png"])
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, self.window.0, self.window.1, &Clut::grayscale());
        match image::RgbImage::from_raw(frame.width, frame.height, rgb).map(|img| img.save(&path)) {
            Some(Ok(())) => self.status = format!("Saved {}", path.display()),
            Some(Err(e)) => self.status = format!("Not saved: {e}"),
            None => self.status = "Not saved: the image size did not match".into(),
        }
    }

    /// Save to study: one plane or a stack, with the measurements drawn on
    /// those planes, as a new series.
    fn save_dialog(&mut self, ctx: &egui::Context, shared: &Shared) {
        let Some(mut d) = self.save.take() else { return };
        let mut close = false;
        let mut start = false;
        let pane_name = self.pane_name(d.pane);
        let on_plane = self.panes[d.pane]
            .plane
            .map(|p| {
                let k = plane_key(&p);
                self.rois.iter().filter(|r| r.key == k).count()
            })
            .unwrap_or(0);
        let total_rois = self.rois.len();
        egui::Window::new("Save to study")
            .id(egui::Id::unique(("ruthos-mpr-save", self.id)))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("From the {} pane.", pane_name.to_lowercase()));
                ui.radio_value(
                    &mut d.stack,
                    false,
                    format!(
                        "This plane, with its {}",
                        if on_plane == 1 {
                            "measurement".to_string()
                        } else {
                            format!("{on_plane} measurements")
                        }
                    ),
                );
                ui.radio_value(&mut d.stack, true, "A stack of parallel planes across the volume");
                if d.stack {
                    ui.horizontal(|ui| {
                        ui.label("Every");
                        ui.add(
                            egui::DragValue::new(&mut d.spacing_mm)
                                .range(0.1..=50.0)
                                .speed(0.1)
                                .suffix(" mm"),
                        );
                        if self.slab_mm > 0.0 {
                            ui.label(format!("(each a {:.1} mm slab)", self.slab_mm));
                        }
                    });
                }
                if total_rois > on_plane {
                    ui.label(
                        egui::RichText::new("Measurements on other planes are saved when their plane is.")
                            .size(12.0)
                            .color(Color32::from_gray(110)),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label("Series description");
                    ui.add(egui::TextEdit::singleline(&mut d.description).desired_width(200.0));
                });
                if let Some(m) = &d.message {
                    ui.label(egui::RichText::new(m).color(Color32::from_rgb(190, 40, 30)));
                }
                ui.horizontal(|ui| {
                    if d.running.is_some() {
                        let (done, total) = *d.progress.lock();
                        ui.spinner();
                        ui.label(format!("Saving image {done} of {total}"));
                    } else {
                        if ui.button("Save").clicked() {
                            start = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                });
            });
        if start {
            self.start_save(&mut d, ctx, shared);
        }
        if d.running.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if !close {
            self.save = Some(d);
        }
    }

    fn start_save(&self, d: &mut SaveDialog, ctx: &egui::Context, shared: &Shared) {
        let (Some(volume), Some(base)) = (self.volume.clone(), self.panes[d.pane].plane) else {
            return;
        };
        let planes = if d.stack {
            stack(&volume, &base, d.spacing_mm, None)
        } else {
            vec![base]
        };
        let rois: Vec<PlaneRoi> = self.rois.clone();
        let Some(template) = shared
            .db
            .images_for_series(self.series.id)
            .ok()
            .and_then(|v| v.first().cloned())
            .map(|img| shared.resolve(&img.path))
        else {
            d.message = Some("Not saved: the series has no images".into());
            return;
        };
        let (db, paths) = (shared.db.clone(), shared.paths.clone());
        let description = d.description.trim().to_string();
        let window = self.window;
        let progress = d.progress.clone();
        let repaint = ctx.clone();
        d.message = None;
        d.running = Some(Task::spawn(ctx, "ruthos-mpr-save", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Each plane takes the measurements drawn on it.
                let saved: Vec<SavedPlane> = planes
                    .iter()
                    .map(|p| {
                        let key = plane_key(p);
                        let on: Vec<&PlaneRoi> = rois.iter().filter(|r| r.key == key).collect();
                        let records = if on.is_empty() {
                            Vec::new()
                        } else {
                            let frame = reslice(&volume, p);
                            on.iter().map(|r| r.roi.to_record(0, 0, Some(&frame))).collect()
                        };
                        SavedPlane {
                            plane: *p,
                            rois: records,
                        }
                    })
                    .collect();
                save_planes(
                    &db,
                    &paths,
                    &template,
                    &volume,
                    &saved,
                    if description.is_empty() { "MPR" } else { &description },
                    window,
                    &|done, total| {
                        *progress.lock() = (done, total);
                        repaint.request_repaint();
                    },
                )
                .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("an internal error stopped the save; see crash.log".into()))
        }));
    }
}

/// A plane's identity for its measurements: where it is and how it lies.
fn plane_key(p: &Plane) -> String {
    let r = |v: f64| (v * 1000.0).round() as i64;
    format!(
        "{},{},{}|{},{},{}|{},{},{}|{}|{}",
        r(p.right[0]),
        r(p.right[1]),
        r(p.right[2]),
        r(p.down[0]),
        r(p.down[1]),
        r(p.down[2]),
        (p.centre[0] * 20.0).round() as i64,
        (p.centre[1] * 20.0).round() as i64,
        (p.centre[2] * 20.0).round() as i64,
        p.width,
        r(p.pixel_mm)
    )
}

fn draft_kind(tool: MprTool, a: Vec2, b: Vec2) -> Option<RoiKind> {
    Some(match tool {
        MprTool::Length | MprTool::Angle => RoiKind::Length { a, b },
        MprTool::Rectangle => RoiKind::Rect { a, b },
        MprTool::Oval => RoiKind::Oval { a, b },
        MprTool::Arrow => RoiKind::Arrow { a, b },
        _ => return None,
    })
}

/// The window the series asks for, else its full range.
fn volume_window(v: &Volume) -> (f32, f32) {
    match (v.meta.window_center, v.meta.window_width) {
        (Some(c), Some(w)) if w > 0.0 => (c as f32, w as f32),
        _ => ((v.min + v.max) / 2.0, (v.max - v.min).max(1.0)),
    }
}

/// Normal, right and down of pane `i` as it is shown, from the planes'
/// frame and how far the pane's own image is turned back.
fn pane_basis(axes: &[V3; 3], phi: f64, i: usize) -> (V3, V3, V3) {
    let [a0, a1, a2] = *axes;
    let neg = |v: V3| scale(v, -1.0);
    let (right, down) = match i {
        0 => (a0, a1),
        1 => (a0, neg(a2)),
        _ => (a1, neg(a2)),
    };
    let n = normalize(cross(right, down));
    (n, rotate(right, n, -phi), rotate(down, n, -phi))
}

/// The anatomical plane a normal is closest to, or "Oblique (near ...)".
fn plane_name(n: V3) -> String {
    let (k, strength) = (0..3)
        .map(|k| (k, n[k].abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((2, 1.0));
    let name = ["Sagittal", "Coronal", "Axial"][k];
    if strength > 0.999 {
        name.to_string()
    } else {
        format!("Oblique (near {})", name.to_lowercase())
    }
}

/// Whether a series can be opened in MPR before its volume is built: a
/// stack of at least three single-frame images.
pub fn series_suits_mpr(images: &[ruthos_core::ImageRow]) -> bool {
    images.len() >= 3 && images.iter().all(|i| i.number_of_frames <= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXES: [V3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    #[test]
    fn a_box_starts_round_the_volume_and_stays_in_it() {
        let dims = [10, 20, 30];
        let mut b = CutBox::around(dims);
        assert!(b.contains([0, 0, 0]) && b.contains([9, 19, 29]));
        b.move_face(0, true, -4.0, dims);
        assert!(b.contains([5, 0, 0]));
        assert!(!b.contains([6, 0, 0]));
        // Faces cannot pass each other or leave the volume.
        b.move_face(0, false, 100.0, dims);
        assert!(b.high[0] - b.low[0] >= 1.0);
        b.move_face(2, true, 100.0, dims);
        assert_eq!(b.high[2], 29.5);
    }

    #[test]
    fn a_box_has_its_corners_and_faces_in_place() {
        let b = CutBox {
            low: [1.0, 2.0, 3.0],
            high: [5.0, 6.0, 7.0],
        };
        assert_eq!(b.corner(0), [1.0, 2.0, 3.0]);
        assert_eq!(b.corner(7), [5.0, 6.0, 7.0]);
        assert_eq!(b.face(1, true), [3.0, 6.0, 5.0]);
        // Every edge joins corners differing along one axis.
        for (i, j) in BOX_EDGES {
            assert_eq!((i ^ j).count_ones(), 1);
        }
    }

    #[test]
    fn a_drag_moves_a_face_along_its_axis_on_screen() {
        // One voxel covers 2 points to the right.
        assert_eq!(along_axis(Vec2::new(10.0, 0.0), Vec2::new(2.0, 0.0)), 5.0);
        // A drag across the axis does nothing.
        assert_eq!(along_axis(Vec2::new(0.0, 10.0), Vec2::new(2.0, 0.0)), 0.0);
        // An axis pointing at the viewer cannot be dragged.
        assert_eq!(along_axis(Vec2::new(10.0, 0.0), Vec2::ZERO), 0.0);
    }

    #[test]
    fn the_three_panes_start_square_and_named() {
        let names: Vec<String> = (0..3).map(|i| plane_name(pane_basis(&AXES, 0.0, i).0)).collect();
        assert_eq!(names, ["Axial", "Coronal", "Sagittal"]);
        for i in 0..3 {
            let (n, r, d) = pane_basis(&AXES, 0.0, i);
            assert!(dot(n, r).abs() < 1e-9 && dot(n, d).abs() < 1e-9 && dot(r, d).abs() < 1e-9);
        }
        // Coronal shows the head at the top: down points to the feet.
        assert_eq!(pane_basis(&AXES, 0.0, 1).2, [0.0, 0.0, -1.0]);
    }

    #[test]
    fn tilting_in_a_pane_turns_the_others_but_not_it() {
        let mut axes = AXES;
        let mut phi = [0.0; 3];
        let before = pane_basis(&axes, phi[0], 0);
        // What a rotate drag in the axial pane does.
        let n = before.0;
        let da = 20f64.to_radians();
        for a in &mut axes {
            *a = normalize(rotate(*a, n, da));
        }
        phi[0] += da;
        let after = pane_basis(&axes, phi[0], 0);
        for k in 0..3 {
            assert!((after.1[k] - before.1[k]).abs() < 1e-9, "the axial pane keeps still");
            assert!((after.2[k] - before.2[k]).abs() < 1e-9);
        }
        assert!(plane_name(pane_basis(&axes, phi[1], 1).0).starts_with("Oblique"));
        assert!(plane_name(pane_basis(&axes, phi[2], 2).0).starts_with("Oblique"));
        let ns: Vec<V3> = (0..3).map(|i| pane_basis(&axes, phi[i], i).0).collect();
        assert!(dot(ns[0], ns[1]).abs() < 1e-9 && dot(ns[1], ns[2]).abs() < 1e-9 && dot(ns[0], ns[2]).abs() < 1e-9);
    }

    #[test]
    fn a_measurement_belongs_to_its_plane() {
        let p = Plane {
            centre: [1.0, 2.0, 3.0],
            right: [1.0, 0.0, 0.0],
            down: [0.0, 1.0, 0.0],
            width: 200,
            height: 200,
            pixel_mm: 0.8,
            slab_mm: 0.0,
            slab_mode: SlabMode::Mip,
        };
        // A slab or a hair of float noise is the same plane; a step is not.
        let same = Plane {
            centre: [1.0 + 1e-6, 2.0, 3.0],
            slab_mm: 10.0,
            ..p
        };
        let moved = Plane {
            centre: [1.0, 2.0, 3.8],
            ..p
        };
        assert_eq!(plane_key(&p), plane_key(&same));
        assert_ne!(plane_key(&p), plane_key(&moved));
        assert!(draft_kind(MprTool::Length, Vec2::ZERO, Vec2::X).is_some());
        assert!(draft_kind(MprTool::Point, Vec2::ZERO, Vec2::X).is_none());
    }
}
