//! Regions of interest and measurements.
//!
//! Points are in image pixel coordinates so ROIs survive zoom, pan, rotation
//! and flips. Statistics are computed from the frame's modality values.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use ruthos_core::RoiRecord;
use ruthos_dicom::Frame;
use ruthos_render::ViewTransform;

use crate::theme;
use crate::widgets::shadow_text;

#[derive(Debug, Clone, PartialEq)]
pub enum RoiKind {
    Length { a: Vec2, b: Vec2 },
    /// Vertex is `b`.
    Angle { a: Vec2, b: Vec2, c: Vec2 },
    Rect { a: Vec2, b: Vec2 },
    Oval { a: Vec2, b: Vec2 },
    Arrow { a: Vec2, b: Vec2 },
    Point { p: Vec2 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Roi {
    pub kind: RoiKind,
    /// The named measurement this ROI stands for, e.g. `LVIDd`.
    pub name: Option<String>,
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
        Self { kind, name: None }
    }

    /// The storage form, with the value as measured on `frame`.
    pub fn to_record(&self, image_id: i64, frame_index: u32, frame: Option<&Frame>) -> RoiRecord {
        let kind = match &self.kind {
            RoiKind::Length { .. } => "length",
            RoiKind::Angle { .. } => "angle",
            RoiKind::Rect { .. } => "rect",
            RoiKind::Oval { .. } => "oval",
            RoiKind::Arrow { .. } => "arrow",
            RoiKind::Point { .. } => "point",
        };
        let (value, unit) = frame.map(|f| self.measurement(f)).unwrap_or((None, ""));
        RoiRecord {
            id: 0,
            image_id,
            frame: frame_index as i64,
            kind: kind.to_string(),
            points: self.points().iter().map(|p| [p.x, p.y]).collect(),
            name: self.name.clone(),
            value,
            unit: unit.to_string(),
        }
    }

    pub fn from_record(r: &RoiRecord) -> Option<Self> {
        let p = |i: usize| r.points.get(i).map(|q| Vec2::new(q[0], q[1]));
        let kind = match r.kind.as_str() {
            "length" => RoiKind::Length { a: p(0)?, b: p(1)? },
            "angle" => RoiKind::Angle { a: p(0)?, b: p(1)?, c: p(2)? },
            "rect" => RoiKind::Rect { a: p(0)?, b: p(1)? },
            "oval" => RoiKind::Oval { a: p(0)?, b: p(1)? },
            "arrow" => RoiKind::Arrow { a: p(0)?, b: p(1)? },
            "point" => RoiKind::Point { p: p(0)? },
            _ => return None,
        };
        Some(Self { kind, name: r.name.clone() })
    }

    /// The headline number and its unit, for storage and reports.
    pub fn measurement(&self, frame: &Frame) -> (Option<f64>, &'static str) {
        let spacing = frame.meta.pixel_spacing;
        match &self.kind {
            RoiKind::Length { a, b } => match ViewTransform::length_mm(*a, *b, spacing) {
                Some(mm) => (Some(mm as f64), "mm"),
                None => (Some((*b - *a).length() as f64), "px"),
            },
            RoiKind::Angle { a, b, c } => (Some(angle_deg(*a, *b, *c, spacing) as f64), "deg"),
            RoiKind::Rect { .. } | RoiKind::Oval { .. } => match self.stats(frame) {
                Some(s) => match s.area_mm2 {
                    Some(a) => (Some(a), "mm2"),
                    None => (Some(s.count as f64), "px"),
                },
                None => (None, ""),
            },
            RoiKind::Point { p } => (frame.value_at(p.x.floor() as i64, p.y.floor() as i64).map(|v| v as f64), ""),
            RoiKind::Arrow { .. } => (None, ""),
        }
    }

    pub fn points(&self) -> Vec<Vec2> {
        match &self.kind {
            RoiKind::Length { a, b } | RoiKind::Rect { a, b } | RoiKind::Oval { a, b } | RoiKind::Arrow { a, b } => {
                vec![*a, *b]
            }
            RoiKind::Angle { a, b, c } => vec![*a, *b, *c],
            RoiKind::Point { p } => vec![*p],
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
        }
    }

    /// Distance in screen points from `screen` to the ROI outline.
    pub fn screen_distance(&self, t: &ViewTransform, screen: Pos2) -> f32 {
        let pts: Vec<Pos2> = self.points().into_iter().map(|p| t.image_to_screen(p)).collect();
        match &self.kind {
            RoiKind::Length { .. } | RoiKind::Arrow { .. } => segment_distance(pts[0], pts[1], screen),
            RoiKind::Angle { .. } => segment_distance(pts[0], pts[1], screen).min(segment_distance(pts[1], pts[2], screen)),
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
        let spacing = frame.meta.pixel_spacing;
        match &self.kind {
            RoiKind::Length { a, b } | RoiKind::Arrow { a, b } => {
                if matches!(self.kind, RoiKind::Arrow { .. }) {
                    return String::new();
                }
                match ViewTransform::length_mm(*a, *b, spacing) {
                    Some(mm) if mm >= 10.0 => format!("{:.2} cm", mm / 10.0),
                    Some(mm) => format!("{mm:.2} mm"),
                    None => format!("{:.1} px  (not calibrated)", (*b - *a).length()),
                }
            }
            RoiKind::Angle { a, b, c } => {
                let deg = angle_deg(*a, *b, *c, spacing);
                format!("{deg:.1}°")
            }
            RoiKind::Rect { .. } | RoiKind::Oval { .. } => match self.stats(frame) {
                Some(s) => {
                    let mut lines = vec![format!("Mean: {:.1}  SD: {:.1}", s.mean, s.sd)];
                    lines.push(format!("Min: {:.0}  Max: {:.0}", s.min, s.max));
                    match s.area_mm2 {
                        Some(a) if a >= 100.0 => lines.push(format!("Area: {:.2} cm²", a / 100.0)),
                        Some(a) => lines.push(format!("Area: {a:.1} mm²")),
                        None => lines.push(format!("Area: {} px  (not calibrated)", s.count)),
                    }
                    lines.join("\n")
                }
                None => String::new(),
            },
            RoiKind::Point { p } => match frame.value_at(p.x.floor() as i64, p.y.floor() as i64) {
                Some(v) => format!("{v:.0}"),
                None => String::new(),
            },
        }
    }

    /// Statistics for closed ROIs on grayscale frames.
    pub fn stats(&self, frame: &Frame) -> Option<RoiStats> {
        let (a, b, oval) = match &self.kind {
            RoiKind::Rect { a, b } => (*a, *b, false),
            RoiKind::Oval { a, b } => (*a, *b, true),
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
        let area_mm2 = frame.meta.pixel_spacing.map(|[r, c]| n as f64 * r * c);
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
    pub fn draw(&self, painter: &Painter, t: &ViewTransform, frame: &Frame, color: Color32, selected: bool) -> Option<Rect> {
        let stroke = Stroke::new(1.5, color);
        let pts: Vec<Pos2> = self.points().into_iter().map(|p| t.image_to_screen(p)).collect();
        let font = FontId::proportional(12.0);
        let label_font = if self.name.is_some() { FontId::proportional(12.5) } else { font.clone() };
        match &self.kind {
            RoiKind::Length { .. } => {
                painter.line_segment([pts[0], pts[1]], stroke);
                // Origin and end are marked with short perpendicular ticks so
                // both are visible while the length is being drawn.
                let d = pts[1] - pts[0];
                let n = if d.length() > 1e-3 { Vec2::new(-d.y, d.x) / d.length() } else { Vec2::new(0.0, 1.0) };
                for p in &pts {
                    painter.line_segment([*p - n * 6.0, *p + n * 6.0], stroke);
                }
                if selected {
                    for p in &pts {
                        painter.rect_stroke(Rect::from_center_size(*p, Vec2::splat(7.0)), 0.0, Stroke::new(1.0, color), egui::StrokeKind::Middle);
                    }
                }
                let mid = Pos2::new((pts[0].x + pts[1].x) / 2.0, (pts[0].y + pts[1].y) / 2.0);
                let r = shadow_text(painter, mid + Vec2::new(8.0, -8.0), Align2::LEFT_BOTTOM, &self.label(frame), label_font, color);
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
                if selected {
                    for p in &pts {
                        painter.rect_stroke(Rect::from_center_size(*p, Vec2::splat(7.0)), 0.0, Stroke::new(1.0, color), egui::StrokeKind::Middle);
                    }
                }
                let r = shadow_text(painter, pts[1] + Vec2::new(10.0, 10.0), Align2::LEFT_TOP, &self.label(frame), label_font, color);
                return Some(r);
            }
            RoiKind::Rect { .. } => {
                let corners = rect_corners(t, self);
                for i in 0..4 {
                    painter.line_segment([corners[i], corners[(i + 1) % 4]], stroke);
                }
                let anchor = corners.iter().fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |m, p| {
                    Pos2::new(m.x.max(p.x), m.y.max(p.y))
                });
                let r = shadow_text(painter, anchor + Vec2::new(6.0, 0.0), Align2::LEFT_BOTTOM, &self.label(frame), label_font, color);
                return Some(r);
            }
            RoiKind::Oval { .. } => {
                let poly = oval_polygon(t, self, 64);
                let mut closed = poly.clone();
                closed.push(poly[0]);
                painter.line(closed, stroke);
                let anchor = poly.iter().fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |m, p| {
                    Pos2::new(m.x.max(p.x), m.y.max(p.y))
                });
                let r = shadow_text(painter, anchor + Vec2::new(6.0, 0.0), Align2::LEFT_BOTTOM, &self.label(frame), label_font, color);
                return Some(r);
            }
            RoiKind::Point { .. } => {
                let p = pts[0];
                painter.line_segment([p + Vec2::new(-6.0, 0.0), p + Vec2::new(6.0, 0.0)], stroke);
                painter.line_segment([p + Vec2::new(0.0, -6.0), p + Vec2::new(0.0, 6.0)], stroke);
                let r = shadow_text(painter, p + Vec2::new(8.0, -4.0), Align2::LEFT_BOTTOM, &self.label(frame), label_font, color);
                return Some(r);
            }
        }
        None
    }
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
        let mut meta = InstanceMeta::default();
        meta.pixel_spacing = Some([0.5, 0.5]);
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
        let roi = Roi::new(RoiKind::Rect { a: Vec2::new(0.0, 0.0), b: Vec2::new(2.0, 2.0) });
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

    #[test]
    fn length_label() {
        let f = frame();
        let mut roi = Roi::new(RoiKind::Length { a: Vec2::ZERO, b: Vec2::new(0.0, 4.0) });
        assert_eq!(roi.label(&f), "2.00 mm");
        roi.name = Some("LVIDd".into());
        assert_eq!(roi.label(&f), "LVIDd: 2.00 mm");
        let rec = roi.to_record(7, 0, Some(&f));
        assert_eq!((rec.kind.as_str(), rec.unit.as_str()), ("length", "mm"));
        assert!((rec.value.unwrap() - 2.0).abs() < 1e-6);
        assert_eq!(Roi::from_record(&rec).unwrap(), roi);
    }
}
