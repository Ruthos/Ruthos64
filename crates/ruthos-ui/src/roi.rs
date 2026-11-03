//! Regions of interest and measurements.
//!
//! Points are in image pixel coordinates so ROIs survive zoom, pan, rotation
//! and flips. Statistics are computed from the frame's modality values.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use ruthos_core::measure::Library;
use ruthos_core::RoiRecord;
use ruthos_dicom::{Frame, MeasureIssue, SpacingBasis};
use ruthos_render::ViewTransform;

use crate::theme;

#[derive(Debug, Clone, PartialEq)]
pub enum RoiKind {
    /// A closed outline through any number of points (three or more).
    Polygon {
        points: Vec<Vec2>,
    },
    Length {
        a: Vec2,
        b: Vec2,
    },
    /// Left-ventricular dimensions: four points along one line, in order
    /// septum top, septum bottom, free wall top, free wall bottom. Drawn in
    /// an M-mode strip (`mmode`) or on a 2D image. The ends move freely, so
    /// the line can be rotated, shortened or extended; the two inner
    /// points stay on the line. In an M-mode strip each thickness is taken
    /// vertically, at one instant, whatever the line's slant.
    Lv {
        phase: MmodePhase,
        mmode: bool,
        p: [Vec2; 4],
    },
    /// Vertex is `b`.
    Angle {
        a: Vec2,
        b: Vec2,
        c: Vec2,
    },
    Rect {
        a: Vec2,
        b: Vec2,
    },
    Oval {
        a: Vec2,
        b: Vec2,
    },
    Arrow {
        a: Vec2,
        b: Vec2,
    },
    Point {
        p: Vec2,
    },
}

/// Diastole or systole, for an LV M-mode line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmodePhase {
    Diastole,
    Systole,
}

impl MmodePhase {
    pub fn label(self) -> &'static str {
        match self {
            MmodePhase::Diastole => "Diastole",
            MmodePhase::Systole => "Systole",
        }
    }

    /// Definition keys of this phase's three values.
    pub fn keys(self) -> [&'static str; 3] {
        match self {
            MmodePhase::Diastole => ruthos_core::mmode::KEYS_D,
            MmodePhase::Systole => ruthos_core::mmode::KEYS_S,
        }
    }

    pub fn labels(self) -> [&'static str; 3] {
        match self {
            MmodePhase::Diastole => ruthos_core::mmode::LABELS_D,
            MmodePhase::Systole => ruthos_core::mmode::LABELS_S,
        }
    }

    pub fn kind_key(self, mmode: bool) -> &'static str {
        use ruthos_core::mmode::{KIND_2D_D, KIND_2D_S, KIND_D, KIND_S};
        match (self, mmode) {
            (MmodePhase::Diastole, true) => KIND_D,
            (MmodePhase::Systole, true) => KIND_S,
            (MmodePhase::Diastole, false) => KIND_2D_D,
            (MmodePhase::Systole, false) => KIND_2D_S,
        }
    }
}

static ECHO_CM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Show echo lengths in centimetres (true) or millimetres (false).
pub fn set_echo_cm(cm: bool) {
    ECHO_CM.store(cm, std::sync::atomic::Ordering::Relaxed);
}

pub fn echo_cm() -> bool {
    ECHO_CM.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Roi {
    pub kind: RoiKind,
    /// The named measurement this ROI stands for, e.g. `LVIDd`.
    pub name: Option<String>,
    /// Archive row id once stored; edits keep it.
    pub id: Option<i64>,
    /// The definition this measurement was assigned to (`lv.lvidd`).
    pub def_key: Option<String>,
    /// What the definition records (`Roi::resolve`); not stored, since it
    /// follows the definition.
    pub quantity: Option<Quantity>,
}

/// What a named measurement records. One caliper line can give a
/// distance, a time, velocities, a slope and a pressure half-time; the
/// definition it is named after decides which is stored and reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    Length,
    Time,
    Velocity,
    Slope,
    PressureHalfTime,
    Angle,
    Area,
    Value,
}

impl Quantity {
    pub fn parse(s: &str) -> Option<Quantity> {
        Some(match s {
            "length" => Quantity::Length,
            "time" => Quantity::Time,
            "velocity" => Quantity::Velocity,
            "slope" => Quantity::Slope,
            "pht" => Quantity::PressureHalfTime,
            "angle" => Quantity::Angle,
            "area" => Quantity::Area,
            "value" => Quantity::Value,
            _ => return None,
        })
    }
}

/// Everything a caliper line can read where it lies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaliperReadings {
    /// Distance: in 2D along the line, in M-mode vertically.
    pub length: Result<ruthos_dicom::Length, MeasureIssue>,
    /// Time between the ends, in a region whose horizontal axis is time.
    pub time_ms: Option<f64>,
    /// Velocities at the two ends, in a spectral Doppler strip (m/s).
    pub v1: Option<f64>,
    pub v2: Option<f64>,
}

impl CaliperReadings {
    pub fn read(frame: &Frame, a: Vec2, b: Vec2) -> Self {
        let m = &frame.meta;
        let (pa, pb) = ([a.x as f64, a.y as f64], [b.x as f64, b.y as f64]);
        let v1 = m.velocity_at(pa[0], pa[1]);
        let v2 = m.velocity_at(pb[0], pb[1]);
        // The vertical axis of a Doppler strip is velocity, not distance.
        let length = if v1.is_some() || v2.is_some() {
            Err(MeasureIssue::NoDistance)
        } else {
            m.length(pa, pb)
        };
        CaliperReadings {
            length,
            time_ms: m.time_between(pa, pb).map(|s| s * 1000.0),
            v1,
            v2,
        }
    }

    /// The larger of the two end velocities, as a magnitude.
    pub fn peak(&self) -> Option<f64> {
        match (self.v1, self.v2) {
            (Some(a), Some(b)) => Some(a.abs().max(b.abs())),
            (Some(a), None) | (None, Some(a)) => Some(a.abs()),
            (None, None) => None,
        }
    }

    /// Rate of change of velocity between the ends, m/s\u{b2}.
    pub fn slope(&self) -> Option<f64> {
        let (a, b, t) = (self.v1?, self.v2?, self.time_ms?);
        (t > 0.5).then(|| (a.abs() - b.abs()).abs() / (t / 1000.0))
    }

    /// Pressure half-time from a deceleration line: the time for the
    /// peak velocity to fall to peak / \u{221a}2 at the line's slope.
    pub fn pressure_half_time_ms(&self) -> Option<f64> {
        let (peak, slope) = (self.peak()?, self.slope()?);
        (slope > 0.0).then(|| peak * (1.0 - std::f64::consts::FRAC_1_SQRT_2) / slope * 1000.0)
    }
}

/// The outcome of measuring a ROI on a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub value: Option<f64>,
    pub unit: &'static str,
    pub basis: Option<SpacingBasis>,
    pub issue: Option<MeasureIssue>,
}

/// Pixel statistics inside a closed ROI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoiStats {
    pub count: usize,
    pub mean: f64,
    pub sd: f64,
    pub min: f32,
    pub max: f32,
    pub area_mm2: Option<f64>,
}

impl Roi {
    pub fn new(kind: RoiKind) -> Self {
        Self {
            kind,
            name: None,
            id: None,
            def_key: None,
            quantity: None,
        }
    }

    /// Look up what this measurement's definition records.
    pub fn resolve(&mut self, library: &Library) {
        self.quantity = self
            .def_key
            .as_deref()
            .and_then(|k| library.definition(k))
            .and_then(|d| Quantity::parse(&d.quantity));
    }

    /// The storage form, with the value as measured on `frame`.
    pub fn to_record(&self, image_id: i64, frame_index: u32, frame: Option<&Frame>) -> RoiRecord {
        let kind = match &self.kind {
            RoiKind::Length { .. } => "length",
            RoiKind::Polygon { .. } => "polygon",
            RoiKind::Lv { phase, mmode, .. } => phase.kind_key(*mmode),
            RoiKind::Angle { .. } => "angle",
            RoiKind::Rect { .. } => "rect",
            RoiKind::Oval { .. } => "oval",
            RoiKind::Arrow { .. } => "arrow",
            RoiKind::Point { .. } => "point",
        };
        let m = frame.map(|f| self.measure(f));
        RoiRecord {
            id: self.id.unwrap_or(0),
            image_id,
            frame: frame_index as i64,
            kind: kind.to_string(),
            points: self.points().iter().map(|p| [p.x, p.y]).collect(),
            name: self.name.clone(),
            value: m.as_ref().and_then(|m| m.value),
            unit: m.as_ref().map(|m| m.unit.to_string()).unwrap_or_default(),
            basis: m.as_ref().and_then(|m| m.basis).map(|b| b.key().to_string()),
            def_key: self.def_key.clone(),
        }
    }

    pub fn from_record(r: &RoiRecord) -> Option<Self> {
        let p = |i: usize| r.points.get(i).map(|q| Vec2::new(q[0], q[1]));
        let kind = match r.kind.as_str() {
            "length" => RoiKind::Length { a: p(0)?, b: p(1)? },
            "angle" => RoiKind::Angle {
                a: p(0)?,
                b: p(1)?,
                c: p(2)?,
            },
            "rect" => RoiKind::Rect { a: p(0)?, b: p(1)? },
            "oval" => RoiKind::Oval { a: p(0)?, b: p(1)? },
            "arrow" => RoiKind::Arrow { a: p(0)?, b: p(1)? },
            "point" => RoiKind::Point { p: p(0)? },
            "polygon" if r.points.len() >= 3 => RoiKind::Polygon {
                points: r.points.iter().map(|q| Vec2::new(q[0], q[1])).collect(),
            },
            "lvmmode-d" | "lvmmode-s" | "lv2d-d" | "lv2d-s" => {
                let phase = if r.kind.ends_with("-s") {
                    MmodePhase::Systole
                } else {
                    MmodePhase::Diastole
                };
                RoiKind::Lv {
                    phase,
                    mmode: r.kind.starts_with("lvmmode"),
                    p: [p(0)?, p(1)?, p(2)?, p(3)?],
                }
            }
            _ => return None,
        };
        Some(Self {
            kind,
            name: r.name.clone(),
            id: if r.id > 0 { Some(r.id) } else { None },
            def_key: r.def_key.clone(),
            quantity: None,
        })
    }

    /// The headline number, unit and scale basis, for storage and reports.
    pub fn measure(&self, frame: &Frame) -> Measurement {
        if let Some(m) = self.measure_defined(frame) {
            return m;
        }
        // In a spectral Doppler strip a line measures time, and a point
        // measures velocity.
        if let RoiKind::Length { a, b } = &self.kind {
            if let Some(d) = doppler_line(frame, *a, *b) {
                return Measurement {
                    value: Some(d.dt_ms),
                    unit: "ms",
                    basis: Some(SpacingBasis::UltrasoundRegion),
                    issue: None,
                };
            }
        }
        if let RoiKind::Point { p } = &self.kind {
            if let Some(v) = frame.meta.velocity_at(p.x as f64, p.y as f64) {
                return Measurement {
                    value: Some(v.abs()),
                    unit: "m/s",
                    basis: Some(SpacingBasis::UltrasoundRegion),
                    issue: None,
                };
            }
        }
        let none = |issue: Option<MeasureIssue>| Measurement {
            value: None,
            unit: "",
            basis: None,
            issue,
        };
        match &self.kind {
            RoiKind::Length { a, b } => match frame.meta.length([a.x as f64, a.y as f64], [b.x as f64, b.y as f64]) {
                Ok(l) => Measurement {
                    value: Some(l.mm),
                    unit: "mm",
                    basis: Some(l.basis),
                    issue: None,
                },
                Err(MeasureIssue::Uncalibrated) => Measurement {
                    value: Some((*b - *a).length() as f64),
                    unit: "px",
                    basis: Some(SpacingBasis::Uncalibrated),
                    issue: Some(MeasureIssue::Uncalibrated),
                },
                Err(e) => none(Some(e)),
            },
            RoiKind::Angle { a, b, c } => {
                if !frame.meta.allows_geometry_at(b.x as f64, b.y as f64) {
                    return none(Some(MeasureIssue::TimeAxis));
                }
                let scale = frame.meta.scale_at(b.x as f64, b.y as f64);
                let spacing = scale
                    .filter(|s| s.basis != SpacingBasis::Uncalibrated)
                    .map(|s| s.spacing);
                Measurement {
                    value: Some(angle_deg(*a, *b, *c, spacing) as f64),
                    unit: "deg",
                    basis: scale.map(|s| s.basis),
                    issue: None,
                }
            }
            RoiKind::Rect { .. } | RoiKind::Oval { .. } | RoiKind::Polygon { .. } => {
                let Some((area_px, centre)) = self.area_px() else {
                    return none(None);
                };
                if !frame.meta.allows_geometry_at(centre.x as f64, centre.y as f64) {
                    return none(Some(MeasureIssue::TimeAxis));
                }
                match frame
                    .meta
                    .scale_at(centre.x as f64, centre.y as f64)
                    .filter(|s| s.basis != SpacingBasis::Uncalibrated)
                {
                    Some(sc) => Measurement {
                        value: Some(area_px * sc.spacing[0] * sc.spacing[1]),
                        unit: "mm2",
                        basis: Some(sc.basis),
                        issue: None,
                    },
                    None => Measurement {
                        value: Some(area_px),
                        unit: "px",
                        basis: Some(SpacingBasis::Uncalibrated),
                        issue: Some(MeasureIssue::Uncalibrated),
                    },
                }
            }
            RoiKind::Point { p } => Measurement {
                value: frame.value_at(p.x.floor() as i64, p.y.floor() as i64).map(|v| v as f64),
                unit: "",
                basis: None,
                issue: None,
            },
            RoiKind::Arrow { .. } => none(None),
            RoiKind::Lv { .. } => match self.mmode_values(frame) {
                Ok((v, basis)) => Measurement {
                    value: Some(v[1]),
                    unit: "mm",
                    basis: Some(basis),
                    issue: None,
                },
                Err(e) => none(Some(e)),
            },
        }
    }

    /// What a caliper or point named after a definition records: the
    /// definition's quantity. `None` when it has no quantity or the kind
    /// has nothing to choose (angles, areas, LV lines).
    fn measure_defined(&self, frame: &Frame) -> Option<Measurement> {
        let q = self.quantity?;
        let ok = |value: f64, unit: &'static str| Measurement {
            value: Some(value),
            unit,
            basis: Some(SpacingBasis::UltrasoundRegion),
            issue: None,
        };
        let issue = |i: MeasureIssue| Measurement {
            value: None,
            unit: "",
            basis: None,
            issue: Some(i),
        };
        match (&self.kind, q) {
            (RoiKind::Length { a, b }, q) => {
                let r = CaliperReadings::read(frame, *a, *b);
                Some(match q {
                    Quantity::Length => match r.length {
                        Ok(l) => Measurement {
                            value: Some(l.mm),
                            unit: "mm",
                            basis: Some(l.basis),
                            issue: None,
                        },
                        Err(e) => issue(e),
                    },
                    Quantity::Time => r
                        .time_ms
                        .map(|t| ok(t, "ms"))
                        .unwrap_or_else(|| issue(MeasureIssue::NoTime)),
                    Quantity::Velocity => r
                        .peak()
                        .map(|v| ok(v, "m/s"))
                        .unwrap_or_else(|| issue(MeasureIssue::NoVelocity)),
                    Quantity::Slope => r
                        .slope()
                        .map(|v| ok(v, "m/s2"))
                        .unwrap_or_else(|| issue(MeasureIssue::NoVelocity)),
                    Quantity::PressureHalfTime => r
                        .pressure_half_time_ms()
                        .map(|t| ok(t, "ms"))
                        .unwrap_or_else(|| issue(MeasureIssue::NoVelocity)),
                    _ => return None,
                })
            }
            (RoiKind::Point { p }, Quantity::Velocity) => Some(
                frame
                    .meta
                    .velocity_at(p.x as f64, p.y as f64)
                    .map(|v| ok(v.abs(), "m/s"))
                    .unwrap_or_else(|| issue(MeasureIssue::NoVelocity)),
            ),
            _ => None,
        }
    }

    /// The label text for a caliper or point named after a definition: the
    /// recorded value first, then what supports it.
    fn defined_text(&self, frame: &Frame) -> Option<String> {
        let q = self.quantity?;
        let m = self.measure_defined(frame)?;
        let Some(v) = m.value else {
            return Some(m.issue.map(|i| i.label().to_string()).unwrap_or_default());
        };
        let readings = match &self.kind {
            RoiKind::Length { a, b } => Some(CaliperReadings::read(frame, *a, *b)),
            _ => None,
        };
        Some(match q {
            Quantity::Length if v >= 10.0 => format!("{:.2} cm{}", v / 10.0, m.basis.map(|b| b.marker()).unwrap_or("")),
            Quantity::Length => format!("{v:.2} mm{}", m.basis.map(|b| b.marker()).unwrap_or("")),
            Quantity::Time => {
                let mut t = format!("{v:.0} ms");
                // An RR interval also gives the heart rate.
                if self.def_key.as_deref().is_some_and(|k| k.ends_with(".rr")) && v > 0.0 {
                    t.push_str(&format!("  {:.0} bpm", 60000.0 / v));
                }
                t
            }
            Quantity::Velocity => format!("{v:.2} m/s  {:.1} mmHg", 4.0 * v * v),
            Quantity::Slope => format!("{v:.2} m/s\u{b2}"),
            Quantity::PressureHalfTime => {
                let slope = readings.and_then(|r| r.slope()).unwrap_or(0.0);
                format!("{v:.0} ms  slope {slope:.2} m/s\u{b2}")
            }
            _ => return None,
        })
    }

    /// The three wall and cavity thicknesses of an LV line in millimetres
    /// (septum, cavity, free wall) and their scale basis. In an M-mode
    /// strip each is the vertical distance between its points.
    pub fn mmode_values(&self, frame: &Frame) -> Result<([f64; 3], SpacingBasis), MeasureIssue> {
        let RoiKind::Lv { p, .. } = &self.kind else {
            return Err(MeasureIssue::Uncalibrated);
        };
        let mut out = [0.0; 3];
        let mut basis = SpacingBasis::Uncalibrated;
        for i in 0..3 {
            let l = frame
                .meta
                .length([p[i].x as f64, p[i].y as f64], [p[i + 1].x as f64, p[i + 1].y as f64])?;
            out[i] = l.mm;
            basis = l.basis;
        }
        Ok((out, basis))
    }

    /// The enclosed area in square pixels, from the geometry, and the
    /// centre whose scale applies.
    pub fn area_px(&self) -> Option<(f64, Vec2)> {
        match &self.kind {
            RoiKind::Rect { a, b } => Some((((b.x - a.x) * (b.y - a.y)).abs() as f64, (*a + *b) / 2.0)),
            RoiKind::Oval { a, b } => Some((
                std::f64::consts::PI * ((b.x - a.x) / 2.0).abs() as f64 * ((b.y - a.y) / 2.0).abs() as f64,
                (*a + *b) / 2.0,
            )),
            RoiKind::Polygon { points } if points.len() >= 3 => {
                let n = points.len();
                let mut twice = 0.0f64;
                for i in 0..n {
                    let (p, q) = (points[i], points[(i + 1) % n]);
                    twice += p.x as f64 * q.y as f64 - q.x as f64 * p.y as f64;
                }
                let centre = points.iter().fold(Vec2::ZERO, |acc, p| acc + *p) / n as f32;
                Some((twice.abs() / 2.0, centre))
            }
            _ => None,
        }
    }

    /// The phase of an LV line.
    pub fn lv_phase(&self) -> Option<MmodePhase> {
        match &self.kind {
            RoiKind::Lv { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// Value and unit only, for callers that do not show the basis.
    pub fn measurement(&self, frame: &Frame) -> (Option<f64>, &'static str) {
        let m = self.measure(frame);
        (m.value, m.unit)
    }

    pub fn points(&self) -> Vec<Vec2> {
        match &self.kind {
            RoiKind::Length { a, b } | RoiKind::Rect { a, b } | RoiKind::Oval { a, b } | RoiKind::Arrow { a, b } => {
                vec![*a, *b]
            }
            RoiKind::Angle { a, b, c } => vec![*a, *b, *c],
            RoiKind::Point { p } => vec![*p],
            RoiKind::Lv { p, .. } => p.to_vec(),
            RoiKind::Polygon { points } => points.clone(),
        }
    }

    /// The draggable handles: the defining points, or the four corners of a
    /// rectangle's or oval's bounding box.
    pub fn handles(&self) -> Vec<Vec2> {
        match &self.kind {
            RoiKind::Rect { a, b } | RoiKind::Oval { a, b } => {
                vec![
                    Vec2::new(a.x, a.y),
                    Vec2::new(b.x, a.y),
                    Vec2::new(b.x, b.y),
                    Vec2::new(a.x, b.y),
                ]
            }
            _ => self.points(),
        }
    }

    /// Move one handle. A corner of a rectangle or oval moves the two edges
    /// that meet there while the opposite corner stays fixed.
    pub fn set_handle(&mut self, i: usize, p: Vec2) {
        match &mut self.kind {
            RoiKind::Rect { a, b } | RoiKind::Oval { a, b } => match i {
                0 => *a = p,
                1 => {
                    b.x = p.x;
                    a.y = p.y;
                }
                2 => *b = p,
                3 => {
                    a.x = p.x;
                    b.y = p.y;
                }
                _ => {}
            },
            RoiKind::Length { a, b } | RoiKind::Arrow { a, b } => match i {
                0 => *a = p,
                1 => *b = p,
                _ => {}
            },
            RoiKind::Angle { a, b, c } => match i {
                0 => *a = p,
                1 => *b = p,
                2 => *c = p,
                _ => {}
            },
            RoiKind::Point { p: q } => *q = p,
            RoiKind::Polygon { points } => {
                if let Some(q) = points.get_mut(i) {
                    *q = p;
                }
            }
            RoiKind::Lv { p: pts, .. } => {
                let (a, b) = (pts[0], pts[3]);
                let dir = b - a;
                let len2 = dir.length_sq();
                let t_of = |q: Vec2| if len2 > 1e-6 { (q - a).dot(dir) / len2 } else { 0.0 };
                match i {
                    // An end moves anywhere: the line rotates, shortens or
                    // lengthens, and the inner points keep their places
                    // along it.
                    0 | 3 => {
                        let (t1, t2) = (t_of(pts[1]), t_of(pts[2]));
                        pts[i] = p;
                        let (na, nb) = (pts[0], pts[3]);
                        pts[1] = na + (nb - na) * t1;
                        pts[2] = na + (nb - na) * t2;
                    }
                    // An inner point slides along the line, between its
                    // neighbours.
                    1 | 2 => {
                        let (lo, hi) = (t_of(pts[i - 1]), t_of(pts[i + 1]));
                        let t = t_of(p).clamp(lo.min(hi), hi.max(lo));
                        pts[i] = a + dir * t;
                    }
                    _ => {}
                }
            }
        }
    }

    /// The handle within 6 screen pixels of a point, nearest first.
    pub fn handle_hit(&self, t: &ViewTransform, screen: Pos2) -> Option<usize> {
        self.handles()
            .iter()
            .enumerate()
            .map(|(i, h)| (i, (t.image_to_screen(*h) - screen).length()))
            .filter(|(_, d)| *d <= 6.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Draw the handles of a selected measurement: 7 screen pixels, the
    /// active one filled.
    pub fn draw_handles(&self, painter: &Painter, t: &ViewTransform, active: Option<usize>, color: Color32) {
        for (i, h) in self.handles().iter().enumerate() {
            let r = Rect::from_center_size(t.image_to_screen(*h), Vec2::splat(7.0));
            if active == Some(i) {
                painter.rect_filled(r, 0.0, color);
            } else {
                painter.rect_filled(r, 0.0, Color32::from_black_alpha(120));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Middle);
            }
        }
    }

    pub fn translate(&mut self, d: Vec2) {
        match &mut self.kind {
            RoiKind::Length { a, b } | RoiKind::Rect { a, b } | RoiKind::Oval { a, b } | RoiKind::Arrow { a, b } => {
                *a += d;
                *b += d;
            }
            RoiKind::Angle { a, b, c } => {
                *a += d;
                *b += d;
                *c += d;
            }
            RoiKind::Point { p } => *p += d,
            RoiKind::Lv { p, .. } => p.iter_mut().for_each(|q| *q += d),
            RoiKind::Polygon { points } => points.iter_mut().for_each(|q| *q += d),
        }
    }

    /// True when the ROI is too small to be meaningful (a click, not a drag).
    pub fn is_degenerate(&self) -> bool {
        match &self.kind {
            RoiKind::Length { a, b } | RoiKind::Rect { a, b } | RoiKind::Oval { a, b } | RoiKind::Arrow { a, b } => {
                (*b - *a).length() < 1.0
            }
            RoiKind::Angle { a, b, c } => (*b - *a).length() < 1.0 || (*c - *b).length() < 1.0,
            RoiKind::Point { .. } => false,
            RoiKind::Lv { p, .. } => p.windows(2).any(|w| (w[1] - w[0]).length() < 1.0),
            RoiKind::Polygon { points } => points.len() < 3 || self.area_px().is_none_or(|(a, _)| a < 1.0),
        }
    }

    /// Distance in screen points from `screen` to the ROI outline.
    pub fn screen_distance(&self, t: &ViewTransform, screen: Pos2) -> f32 {
        let pts: Vec<Pos2> = self.points().into_iter().map(|p| t.image_to_screen(p)).collect();
        match &self.kind {
            RoiKind::Length { .. } | RoiKind::Arrow { .. } => segment_distance(pts[0], pts[1], screen),
            RoiKind::Angle { .. } => {
                segment_distance(pts[0], pts[1], screen).min(segment_distance(pts[1], pts[2], screen))
            }
            RoiKind::Rect { .. } => {
                let (p0, p1) = (pts[0], pts[1]);
                let corners = rect_corners(t, self);
                let mut d = f32::INFINITY;
                for i in 0..4 {
                    d = d.min(segment_distance(corners[i], corners[(i + 1) % 4], screen));
                }
                let _ = (p0, p1);
                d
            }
            RoiKind::Oval { .. } => {
                let poly = oval_polygon(t, self, 48);
                let mut d = f32::INFINITY;
                for i in 0..poly.len() {
                    d = d.min(segment_distance(poly[i], poly[(i + 1) % poly.len()], screen));
                }
                d
            }
            RoiKind::Point { .. } => (pts[0] - screen).length(),
            RoiKind::Lv { .. } => segment_distance(pts[0], pts[3], screen),
            RoiKind::Polygon { .. } => (0..pts.len())
                .map(|i| segment_distance(pts[i], pts[(i + 1) % pts.len()], screen))
                .fold(f32::INFINITY, f32::min),
        }
    }

    /// The text shown next to the ROI: the assigned name, then the value.
    pub fn label(&self, frame: &Frame) -> String {
        let value = self.value_text(frame);
        match &self.name {
            Some(n) if value.is_empty() => n.clone(),
            Some(n) => format!("{n}: {value}"),
            None => value,
        }
    }

    /// The measured value as text.
    pub fn value_text(&self, frame: &Frame) -> String {
        if let Some(text) = self.defined_text(frame) {
            return text;
        }
        match &self.kind {
            RoiKind::Length { .. } | RoiKind::Arrow { .. } => {
                if matches!(self.kind, RoiKind::Arrow { .. }) {
                    return String::new();
                }
                let m = self.measure(frame);
                if let RoiKind::Length { a, b } = &self.kind {
                    if let Some(d) = doppler_line(frame, *a, *b) {
                        let mut text = format!(
                            "\u{394}t {:.0} ms  {:.2} \u{2192} {:.2} m/s",
                            d.dt_ms,
                            d.v1.abs(),
                            d.v2.abs()
                        );
                        if d.dt_ms > 0.5 {
                            let slope = (d.v2.abs() - d.v1.abs()).abs() / (d.dt_ms / 1000.0);
                            text.push_str(&format!("  {slope:.1} m/s\u{b2}"));
                        }
                        return text;
                    }
                }
                // In an M-mode strip a line also spans time.
                let dt = match &self.kind {
                    RoiKind::Length { a, b } => frame
                        .meta
                        .time_between([a.x as f64, a.y as f64], [b.x as f64, b.y as f64])
                        .filter(|s| *s >= 0.001)
                        .map(|s| format!("  \u{394}t {:.0} ms", s * 1000.0))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                match (m.value, m.unit, m.issue) {
                    (Some(mm), "mm", _) if mm >= 10.0 => {
                        format!("{:.2} cm{}{dt}", mm / 10.0, m.basis.map(|b| b.marker()).unwrap_or(""))
                    }
                    (Some(mm), "mm", _) => format!("{mm:.2} mm{}{dt}", m.basis.map(|b| b.marker()).unwrap_or("")),
                    (Some(px), "px", _) => format!("{px:.1} px  (not calibrated)"),
                    (_, _, Some(issue)) => issue.label().to_string(),
                    _ => String::new(),
                }
            }
            RoiKind::Angle { a, b, c } => {
                if !frame.meta.allows_geometry_at(b.x as f64, b.y as f64) {
                    return MeasureIssue::TimeAxis.label().to_string();
                }
                let deg = angle_deg(*a, *b, *c, spacing_for(frame, *b));
                format!("{deg:.1}°")
            }
            RoiKind::Rect { .. } | RoiKind::Oval { .. } | RoiKind::Polygon { .. } => {
                let m = self.measure(frame);
                let area = match (m.value, m.unit, m.issue) {
                    (Some(a), "mm2", _) if a >= 100.0 => {
                        format!(
                            "Area: {:.2} cm\u{b2}{}",
                            a / 100.0,
                            m.basis.map(|b| b.marker()).unwrap_or("")
                        )
                    }
                    (Some(a), "mm2", _) => {
                        format!("Area: {a:.1} mm\u{b2}{}", m.basis.map(|b| b.marker()).unwrap_or(""))
                    }
                    (Some(a), "px", _) => format!("Area: {a:.0} px  (not calibrated)"),
                    (_, _, Some(issue)) => return issue.label().to_string(),
                    _ => return String::new(),
                };
                // Pixel statistics where the image has a single channel.
                match self.stats(frame) {
                    Some(s) => format!(
                        "Mean: {:.1}  SD: {:.1}\nMin: {:.0}  Max: {:.0}\n{area}",
                        s.mean, s.sd, s.min, s.max
                    ),
                    None => area,
                }
            }
            RoiKind::Point { p } => {
                if let Some(v) = frame.meta.velocity_at(p.x as f64, p.y as f64) {
                    // The simplified Bernoulli gradient, 4v\u{b2}.
                    return format!("{:.2} m/s  {:.1} mmHg", v.abs(), 4.0 * v * v);
                }
                match frame.value_at(p.x.floor() as i64, p.y.floor() as i64) {
                    Some(v) => format!("{v:.0}"),
                    None => String::new(),
                }
            }
            RoiKind::Lv { phase, .. } => match self.mmode_values(frame) {
                Ok((v, basis)) => {
                    let labels = phase.labels();
                    let cm = echo_cm();
                    let parts: Vec<String> = (0..3)
                        .map(|i| format!("{} {}", labels[i], mmode_number(v[i], cm)))
                        .collect();
                    format!(
                        "{} {}{}",
                        parts.join("  "),
                        if cm { "cm" } else { "mm" },
                        basis.marker()
                    )
                }
                Err(e) => e.label().to_string(),
            },
        }
    }

    /// Statistics for closed ROIs on grayscale frames.
    pub fn stats(&self, frame: &Frame) -> Option<RoiStats> {
        let polygon: Option<&Vec<Vec2>> = match &self.kind {
            RoiKind::Polygon { points } => Some(points),
            _ => None,
        };
        let (a, b, oval) = match &self.kind {
            RoiKind::Rect { a, b } => (*a, *b, false),
            RoiKind::Oval { a, b } => (*a, *b, true),
            RoiKind::Polygon { points } => {
                let (lo, hi) = points.iter().fold(
                    (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
                    |(lo, hi), p| (lo.min(*p), hi.max(*p)),
                );
                (lo, hi, false)
            }
            _ => return None,
        };
        if frame.is_color() {
            return None;
        }
        let min_x = a.x.min(b.x).floor().max(0.0) as i64;
        let max_x = a.x.max(b.x).ceil().min(frame.width as f32) as i64;
        let min_y = a.y.min(b.y).floor().max(0.0) as i64;
        let max_y = a.y.max(b.y).ceil().min(frame.height as f32) as i64;
        if max_x <= min_x || max_y <= min_y {
            return None;
        }
        let cx = (a.x + b.x) / 2.0;
        let cy = (a.y + b.y) / 2.0;
        let rx = ((b.x - a.x) / 2.0).abs().max(0.5);
        let ry = ((b.y - a.y) / 2.0).abs().max(0.5);

        let mut n = 0usize;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for y in min_y..max_y {
            for x in min_x..max_x {
                if oval {
                    let dx = (x as f32 + 0.5 - cx) / rx;
                    let dy = (y as f32 + 0.5 - cy) / ry;
                    if dx * dx + dy * dy > 1.0 {
                        continue;
                    }
                }
                if let Some(poly) = polygon {
                    if !point_in_polygon(Vec2::new(x as f32 + 0.5, y as f32 + 0.5), poly) {
                        continue;
                    }
                }
                if let Some(v) = frame.value_at(x, y) {
                    n += 1;
                    sum += v as f64;
                    sum_sq += (v as f64) * (v as f64);
                    min = min.min(v);
                    max = max.max(v);
                }
            }
        }
        if n == 0 {
            return None;
        }
        let mean = sum / n as f64;
        let var = (sum_sq / n as f64 - mean * mean).max(0.0);
        let centre = self.area_px().map(|(_, c)| c).unwrap_or((a + b) / 2.0);
        let area_mm2 = spacing_for(frame, centre).map(|[r, c]| n as f64 * r * c);
        Some(RoiStats {
            count: n,
            mean,
            sd: var.sqrt(),
            min,
            max,
            area_mm2,
        })
    }

    /// Draw the ROI and its label. Returns the label's screen rectangle, the
    /// click target for assigning the measurement a name.
    pub fn draw(
        &self,
        painter: &Painter,
        t: &ViewTransform,
        frame: &Frame,
        color: Color32,
        selected: bool,
    ) -> Option<Rect> {
        self.draw_opts(painter, t, frame, color, selected, true)
    }

    /// Draw with or without the value label. Without it, the returned label
    /// rectangle is empty so nothing can be clicked.
    pub fn draw_opts(
        &self,
        painter: &Painter,
        t: &ViewTransform,
        frame: &Frame,
        color: Color32,
        selected: bool,
        labels: bool,
    ) -> Option<Rect> {
        let shadow_text =
            |painter: &Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32| -> Rect {
                if labels {
                    crate::widgets::shadow_text(painter, pos, anchor, text, font, color)
                } else {
                    Rect::NOTHING
                }
            };
        let stroke = Stroke::new(1.5, color);
        let pts: Vec<Pos2> = self.points().into_iter().map(|p| t.image_to_screen(p)).collect();
        let font = FontId::proportional(12.0);
        let label_font = if self.name.is_some() {
            FontId::proportional(12.5)
        } else {
            font.clone()
        };
        match &self.kind {
            RoiKind::Length { .. } => {
                painter.line_segment([pts[0], pts[1]], stroke);
                // Origin and end are marked with short perpendicular ticks so
                // both are visible while the length is being drawn.
                let d = pts[1] - pts[0];
                let n = if d.length() > 1e-3 {
                    Vec2::new(-d.y, d.x) / d.length()
                } else {
                    Vec2::new(0.0, 1.0)
                };
                for p in &pts {
                    painter.line_segment([*p - n * 6.0, *p + n * 6.0], stroke);
                }
                let _ = selected;
                let mid = Pos2::new((pts[0].x + pts[1].x) / 2.0, (pts[0].y + pts[1].y) / 2.0);
                let r = shadow_text(
                    painter,
                    mid + Vec2::new(8.0, -8.0),
                    Align2::LEFT_BOTTOM,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Arrow { .. } => {
                painter.arrow(pts[0], pts[1] - pts[0], Stroke::new(2.0, color));
            }
            RoiKind::Angle { .. } => {
                painter.line_segment([pts[0], pts[1]], stroke);
                painter.line_segment([pts[1], pts[2]], stroke);
                for p in &pts {
                    painter.circle_filled(*p, 2.5, color);
                }
                let r = shadow_text(
                    painter,
                    pts[1] + Vec2::new(10.0, 10.0),
                    Align2::LEFT_TOP,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Rect { .. } => {
                let corners = rect_corners(t, self);
                for i in 0..4 {
                    painter.line_segment([corners[i], corners[(i + 1) % 4]], stroke);
                }
                let anchor = corners
                    .iter()
                    .fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |m, p| {
                        Pos2::new(m.x.max(p.x), m.y.max(p.y))
                    });
                let r = shadow_text(
                    painter,
                    anchor + Vec2::new(6.0, 0.0),
                    Align2::LEFT_BOTTOM,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Polygon { .. } => {
                let mut closed = pts.clone();
                closed.push(pts[0]);
                painter.line(closed, stroke);
                for p in &pts {
                    painter.circle_filled(*p, 2.0, color);
                }
                let anchor = pts
                    .iter()
                    .fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |m, p| {
                        Pos2::new(m.x.max(p.x), m.y.max(p.y))
                    });
                let r = shadow_text(
                    painter,
                    anchor + Vec2::new(6.0, 0.0),
                    Align2::LEFT_BOTTOM,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Oval { .. } => {
                let poly = oval_polygon(t, self, 64);
                let mut closed = poly.clone();
                closed.push(poly[0]);
                painter.line(closed, stroke);
                let anchor = poly
                    .iter()
                    .fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |m, p| {
                        Pos2::new(m.x.max(p.x), m.y.max(p.y))
                    });
                let r = shadow_text(
                    painter,
                    anchor + Vec2::new(6.0, 0.0),
                    Align2::LEFT_BOTTOM,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Point { .. } => {
                let p = pts[0];
                painter.line_segment([p + Vec2::new(-6.0, 0.0), p + Vec2::new(6.0, 0.0)], stroke);
                painter.line_segment([p + Vec2::new(0.0, -6.0), p + Vec2::new(0.0, 6.0)], stroke);
                let r = shadow_text(
                    painter,
                    p + Vec2::new(8.0, -4.0),
                    Align2::LEFT_BOTTOM,
                    &self.label(frame),
                    label_font,
                    color,
                );
                return Some(r);
            }
            RoiKind::Lv { phase, .. } => {
                // One line, a tick across it at each point, each thickness
                // named beside its segment, and the phase past the end.
                painter.line_segment([pts[0], pts[3]], stroke);
                let along = pts[3] - pts[0];
                let unit = if along.length() > 1e-3 {
                    along / along.length()
                } else {
                    Vec2::Y
                };
                let mut normal = Vec2::new(-unit.y, unit.x);
                if normal.x < 0.0 || (normal.x == 0.0 && normal.y < 0.0) {
                    normal = -normal;
                }
                for p in &pts {
                    painter.line_segment([*p - normal * 7.0, *p + normal * 7.0], stroke);
                }
                let values = self.mmode_values(frame).ok();
                let labels = phase.labels();
                let cm = echo_cm();
                for i in 0..3 {
                    let mid = pts[i] + (pts[i + 1] - pts[i]) / 2.0;
                    let text = match &values {
                        Some((v, _)) => format!("{} {}", labels[i], mmode_number(v[i], cm)),
                        None => labels[i].to_string(),
                    };
                    shadow_text(
                        painter,
                        mid + normal * 10.0,
                        if normal.x >= 0.0 {
                            Align2::LEFT_CENTER
                        } else {
                            Align2::RIGHT_CENTER
                        },
                        &text,
                        font.clone(),
                        color,
                    );
                }
                let title = match (&self.name, &values) {
                    (Some(n), _) => n.clone(),
                    (None, Some((_, basis))) => format!("{}{}", phase.label(), basis.marker()),
                    (None, None) => format!("{}: {}", phase.label(), self.value_text(frame)),
                };
                let r = shadow_text(
                    painter,
                    pts[3] + unit * 12.0 + normal * 6.0,
                    Align2::LEFT_TOP,
                    &title,
                    label_font,
                    color,
                );
                return Some(r);
            }
        }
        None
    }
}

/// Even-odd test: is `p` inside the closed outline `poly`?
pub fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// A line inside a spectral Doppler strip: the time it spans and the
/// velocities at its ends.
struct DopplerLine {
    dt_ms: f64,
    v1: f64,
    v2: f64,
}

fn doppler_line(frame: &Frame, a: Vec2, b: Vec2) -> Option<DopplerLine> {
    let m = &frame.meta;
    let v1 = m.velocity_at(a.x as f64, a.y as f64)?;
    let v2 = m.velocity_at(b.x as f64, b.y as f64)?;
    let dt = m.time_between([a.x as f64, a.y as f64], [b.x as f64, b.y as f64])?;
    Some(DopplerLine {
        dt_ms: dt * 1000.0,
        v1,
        v2,
    })
}

fn rect_corners(t: &ViewTransform, roi: &Roi) -> [Pos2; 4] {
    let RoiKind::Rect { a, b } = &roi.kind else {
        return [Pos2::ZERO; 4];
    };
    [
        t.image_to_screen(Vec2::new(a.x, a.y)),
        t.image_to_screen(Vec2::new(b.x, a.y)),
        t.image_to_screen(Vec2::new(b.x, b.y)),
        t.image_to_screen(Vec2::new(a.x, b.y)),
    ]
}

fn oval_polygon(t: &ViewTransform, roi: &Roi, n: usize) -> Vec<Pos2> {
    let RoiKind::Oval { a, b } = &roi.kind else {
        return Vec::new();
    };
    let c = Vec2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let rx = (b.x - a.x) / 2.0;
    let ry = (b.y - a.y) / 2.0;
    (0..n)
        .map(|i| {
            let th = i as f32 / n as f32 * std::f32::consts::TAU;
            t.image_to_screen(c + Vec2::new(rx * th.cos(), ry * th.sin()))
        })
        .collect()
}

fn segment_distance(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq < 1e-6 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Pixel spacing that applies at an image point: hand calibration, then the
/// ultrasound region under the point, then the image's own spacing.
pub fn spacing_for(frame: &Frame, p: Vec2) -> Option<[f64; 2]> {
    frame.meta.spacing_at(p.x as f64, p.y as f64)
}

/// Length between two image points in mm, region-aware for ultrasound.
pub fn length_mm(frame: &Frame, a: Vec2, b: Vec2) -> Option<f64> {
    frame.meta.length_mm([a.x as f64, a.y as f64], [b.x as f64, b.y as f64])
}

/// Angle at vertex `b` in degrees, corrected for anisotropic pixel spacing.
pub fn angle_deg(a: Vec2, b: Vec2, c: Vec2, spacing: Option<[f64; 2]>) -> f32 {
    let [row, col] = spacing.unwrap_or([1.0, 1.0]);
    let to_mm = |v: Vec2| Vec2::new(v.x * col as f32, v.y * row as f32);
    let u = to_mm(a - b);
    let v = to_mm(c - b);
    let denom = u.length() * v.length();
    if denom < 1e-6 {
        return 0.0;
    }
    (u.dot(v) / denom).clamp(-1.0, 1.0).acos().to_degrees()
}

pub fn roi_color(selected: bool) -> Color32 {
    if selected {
        theme::ROI_SELECTED
    } else {
        theme::ROI_COLOR
    }
}

/// An echo length without its unit, in the chosen unit.
pub fn mmode_number(mm: f64, cm: bool) -> String {
    if cm {
        format!("{:.2}", mm / 10.0)
    } else {
        format!("{mm:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruthos_dicom::meta::InstanceMeta;
    use ruthos_dicom::pixels::FramePixels;

    fn frame() -> Frame {
        let mut values = vec![0.0f32; 100];
        for (i, v) in values.iter_mut().enumerate() {
            *v = i as f32;
        }
        let meta = InstanceMeta {
            pixel_spacing: Some([0.5, 0.5]),
            pixel_spacing_tag: Some([0.5, 0.5]),
            ..Default::default()
        };
        Frame {
            width: 10,
            height: 10,
            frame_index: 0,
            pixels: FramePixels::Gray(values),
            min: 0.0,
            max: 99.0,
            inverted: false,
            meta,
        }
    }

    #[test]
    fn rect_stats() {
        let f = frame();
        let roi = Roi::new(RoiKind::Rect {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(2.0, 2.0),
        });
        let s = roi.stats(&f).unwrap();
        assert_eq!(s.count, 4);
        assert!((s.mean - 5.5).abs() < 1e-9); // values 0,1,10,11
        assert_eq!(s.min, 0.0);
        assert_eq!(s.max, 11.0);
        assert!((s.area_mm2.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn angle_right() {
        let d = angle_deg(Vec2::new(1.0, 0.0), Vec2::ZERO, Vec2::new(0.0, 1.0), None);
        assert!((d - 90.0).abs() < 1e-4);
    }

    /// A frame with an M-mode strip: time across, 0.1 cm per row down.
    fn mmode_frame() -> Frame {
        let mut f = frame();
        f.width = 200;
        f.height = 200;
        f.pixels = FramePixels::Gray(vec![0.0; 40_000]);
        f.meta.rows = 200;
        f.meta.columns = 200;
        f.meta.pixel_spacing = None;
        f.meta.pixel_spacing_tag = None;
        f.meta.modality = "US".into();
        f.meta.us_regions = vec![ruthos_dicom::UsRegion {
            x0: 0,
            y0: 100,
            x1: 199,
            y1: 199,
            spatial_format: 2,
            data_type: 1,
            units_x: 4,
            units_y: 3,
            delta_x: 0.005,
            delta_y: 0.1,
            ..Default::default()
        }];
        f
    }

    fn lv(phase: MmodePhase, mmode: bool, pts: [(f32, f32); 4]) -> Roi {
        Roi::new(RoiKind::Lv {
            phase,
            mmode,
            p: pts.map(|(x, y)| Vec2::new(x, y)),
        })
    }

    #[test]
    fn mmode_values_come_from_the_strip_scale() {
        let f = mmode_frame();
        let roi = lv(
            MmodePhase::Diastole,
            true,
            [(50.0, 110.0), (50.0, 120.0), (50.0, 160.0), (50.0, 170.0)],
        );
        let (v, basis) = roi.mmode_values(&f).unwrap();
        assert_eq!(v, [10.0, 40.0, 10.0]);
        assert_eq!(basis, SpacingBasis::UltrasoundRegion);
        set_echo_cm(false);
        assert_eq!(roi.value_text(&f), "IVSd 10.0  LVIDd 40.0  LVFWd 10.0 mm");
        set_echo_cm(true);
        assert_eq!(roi.value_text(&f), "IVSd 1.00  LVIDd 4.00  LVFWd 1.00 cm");
        set_echo_cm(false);
        let rec = roi.to_record(1, 0, Some(&f));
        assert_eq!(
            (rec.kind.as_str(), rec.value, rec.unit.as_str()),
            ("lvmmode-d", Some(40.0), "mm")
        );
        assert_eq!(rec.points.len(), 4);
        assert_eq!(Roi::from_record(&rec).unwrap(), roi);
        // A slanted line in the strip still measures vertically.
        let slanted = lv(
            MmodePhase::Diastole,
            true,
            [(40.0, 110.0), (44.0, 120.0), (60.0, 160.0), (64.0, 170.0)],
        );
        assert_eq!(slanted.mmode_values(&f).unwrap().0, [10.0, 40.0, 10.0]);
    }

    #[test]
    fn lv_in_2d_measures_along_the_line_and_stores_as_2d() {
        let f = frame(); // 0.5 mm per pixel both ways
        let roi = lv(
            MmodePhase::Systole,
            false,
            [(0.0, 0.0), (3.0, 4.0), (6.0, 8.0), (9.0, 12.0)],
        );
        let (v, _) = roi.mmode_values(&f).unwrap();
        assert_eq!(v, [2.5, 2.5, 2.5], "5 pixels along the line, 0.5 mm each");
        let rec = roi.to_record(1, 7, Some(&f));
        assert_eq!(rec.kind, "lv2d-s");
        assert_eq!(Roi::from_record(&rec).unwrap(), roi);
    }

    #[test]
    fn lv_ends_move_anywhere_and_inner_points_follow() {
        let mut roi = lv(
            MmodePhase::Systole,
            true,
            [(50.0, 10.0), (50.0, 20.0), (50.0, 60.0), (50.0, 70.0)],
        );
        // Rotate and lengthen by moving the bottom end sideways and down.
        roi.set_handle(3, Vec2::new(110.0, 130.0));
        let pts = roi.points();
        assert_eq!(pts[3], Vec2::new(110.0, 130.0));
        assert!(
            (pts[1] - Vec2::new(60.0, 30.0)).length() < 1e-4,
            "inner points keep their fraction"
        );
        assert!((pts[2] - Vec2::new(100.0, 110.0)).length() < 1e-4);
        // An inner point slides along the line and stops at its neighbour.
        roi.set_handle(1, Vec2::new(500.0, 0.0));
        let pts = roi.points();
        assert!((pts[1] - pts[2]).length() < 1e-3);
        roi.translate(Vec2::new(5.0, 7.0));
        assert_eq!(roi.points()[0], Vec2::new(55.0, 17.0), "the whole line moves freely");
    }

    #[test]
    fn a_caliper_records_what_its_definition_asks_for() {
        let mut f = mmode_frame();
        f.meta.us_regions[0].units_y = 7;
        f.meta.us_regions[0].delta_y = -2.0;
        f.meta.us_regions[0].ref_y0 = 40;
        // Peak 0.70 m/s falling to 0 over 200 ms: slope 3.5 m/s\u{b2}.
        let line = |q: Quantity, key: &str| {
            let mut r = Roi::new(RoiKind::Length {
                a: Vec2::new(20.0, 105.0),
                b: Vec2::new(60.0, 140.0),
            });
            r.def_key = Some(key.into());
            r.quantity = Some(q);
            r
        };
        let pht = line(Quantity::PressureHalfTime, "echo.ar_pht");
        let m = pht.measure(&f);
        assert_eq!(m.unit, "ms");
        let want = 0.7 * (1.0 - std::f64::consts::FRAC_1_SQRT_2) / 3.5 * 1000.0;
        assert!((m.value.unwrap() - want).abs() < 1e-6);
        assert_eq!(pht.value_text(&f), format!("{want:.0} ms  slope 3.50 m/s\u{b2}"));
        let rr = line(Quantity::Time, "ecg.rr");
        assert_eq!((rr.measure(&f).value, rr.measure(&f).unit), (Some(200.0), "ms"));
        assert_eq!(rr.value_text(&f), "200 ms  300 bpm");
        let slope = line(Quantity::Slope, "echo.mv_dec_slope");
        assert!((slope.measure(&f).value.unwrap() - 3.5).abs() < 1e-9);
        let vel = line(Quantity::Velocity, "echo.mr_vmax");
        assert!((vel.measure(&f).value.unwrap() - 0.7).abs() < 1e-9);
        // A length asked for in a Doppler strip is refused, not faked.
        let len = line(Quantity::Length, "echo.ao");
        assert_eq!(len.measure(&f).issue, Some(MeasureIssue::NoDistance));
        let rec = rr.to_record(1, 0, Some(&f));
        assert_eq!((rec.value, rec.unit.as_str()), (Some(200.0), "ms"));
    }

    #[test]
    fn polygon_area_from_its_outline() {
        let f = frame(); // 0.5 mm per pixel, 10 x 10 pixels
                         // A right triangle with legs of 8 pixels: 32 square pixels, 8 mm\u{b2}.
        let roi = Roi::new(RoiKind::Polygon {
            points: vec![Vec2::new(0.0, 0.0), Vec2::new(8.0, 0.0), Vec2::new(0.0, 8.0)],
        });
        let m = roi.measure(&f);
        assert_eq!(m.unit, "mm2");
        assert!((m.value.unwrap() - 8.0).abs() < 1e-9);
        let rec = roi.to_record(1, 0, Some(&f));
        assert_eq!(rec.kind, "polygon");
        assert_eq!(Roi::from_record(&rec).unwrap(), roi);
        assert!(point_in_polygon(
            Vec2::new(1.0, 1.0),
            &[Vec2::new(0.0, 0.0), Vec2::new(8.0, 0.0), Vec2::new(0.0, 8.0)]
        ));
        assert!(!point_in_polygon(
            Vec2::new(7.0, 7.0),
            &[Vec2::new(0.0, 0.0), Vec2::new(8.0, 0.0), Vec2::new(0.0, 8.0)]
        ));
        let s = roi.stats(&f).unwrap();
        assert!(s.count > 20 && s.count < 40, "about half of the 8 x 8 box");
    }

    #[test]
    fn ellipse_area_needs_no_pixel_statistics() {
        let mut f = frame();
        f.pixels = FramePixels::Rgba(vec![0; 400]);
        let roi = Roi::new(RoiKind::Oval {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(4.0, 2.0),
        });
        let m = roi.measure(&f);
        // Semi-axes 2 and 1 pixel: 2\u{3c0} px\u{b2}, 0.25 mm\u{b2} each.
        assert!((m.value.unwrap() - 2.0 * std::f64::consts::PI * 0.25).abs() < 1e-9);
        assert!(
            roi.value_text(&f).starts_with("Area: 1.6 mm"),
            "colour images still get an area"
        );
    }

    #[test]
    fn a_time_asked_for_on_a_2d_image_is_refused() {
        let f = frame();
        let mut r = Roi::new(RoiKind::Length {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(0.0, 4.0),
        });
        r.def_key = Some("ecg.rr".into());
        r.quantity = Some(Quantity::Time);
        assert_eq!(r.measure(&f).issue, Some(MeasureIssue::NoTime));
        assert_eq!(r.value_text(&f), "no time axis here");
        r.quantity = Some(Quantity::Length);
        assert_eq!(r.measure(&f).unit, "mm");
    }

    #[test]
    fn doppler_points_and_lines() {
        let mut f = mmode_frame();
        // Spectral strip: -2 cm/s per pixel, baseline 40 rows in.
        f.meta.us_regions[0].units_y = 7;
        f.meta.us_regions[0].delta_y = -2.0;
        f.meta.us_regions[0].ref_y0 = 40;
        let point = Roi::new(RoiKind::Point {
            p: Vec2::new(20.0, 90.0 + 15.0),
        });
        let m = point.measure(&f);
        assert_eq!(m.unit, "m/s");
        assert!((m.value.unwrap() - 0.7).abs() < 1e-6);
        assert_eq!(point.value_text(&f), "0.70 m/s  2.0 mmHg");
        let line = Roi::new(RoiKind::Length {
            a: Vec2::new(20.0, 105.0),
            b: Vec2::new(60.0, 140.0),
        });
        let m = line.measure(&f);
        assert_eq!(m.unit, "ms");
        assert!((m.value.unwrap() - 200.0).abs() < 1e-6);
        assert!(line
            .value_text(&f)
            .starts_with("\u{394}t 200 ms  0.70 \u{2192} 0.00 m/s"));
    }

    #[test]
    fn corner_handles_move_two_edges() {
        let mut roi = Roi::new(RoiKind::Rect {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(10.0, 10.0),
        });
        assert_eq!(roi.handles().len(), 4);
        roi.set_handle(1, Vec2::new(12.0, -2.0));
        assert_eq!(
            roi.kind,
            RoiKind::Rect {
                a: Vec2::new(0.0, -2.0),
                b: Vec2::new(12.0, 10.0)
            }
        );
        roi.set_handle(3, Vec2::new(-1.0, 11.0));
        assert_eq!(
            roi.kind,
            RoiKind::Rect {
                a: Vec2::new(-1.0, -2.0),
                b: Vec2::new(12.0, 11.0)
            }
        );
        let mut angle = Roi::new(RoiKind::Angle {
            a: Vec2::ZERO,
            b: Vec2::new(1.0, 0.0),
            c: Vec2::new(1.0, 1.0),
        });
        angle.set_handle(1, Vec2::new(5.0, 5.0));
        assert_eq!(angle.handles()[1], Vec2::new(5.0, 5.0));
    }

    #[test]
    fn length_label() {
        let f = frame();
        let mut roi = Roi::new(RoiKind::Length {
            a: Vec2::ZERO,
            b: Vec2::new(0.0, 4.0),
        });
        assert_eq!(roi.label(&f), "2.00 mm ?");
        roi.name = Some("LVIDd".into());
        assert_eq!(roi.label(&f), "LVIDd: 2.00 mm ?");
        let rec = roi.to_record(7, 0, Some(&f));
        assert_eq!((rec.kind.as_str(), rec.unit.as_str()), ("length", "mm"));
        assert!((rec.value.unwrap() - 2.0).abs() < 1e-6);
        assert_eq!(rec.basis.as_deref(), Some("calibrated-unstated"));
        assert_eq!(Roi::from_record(&rec).unwrap(), roi);
    }
}
