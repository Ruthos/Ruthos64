//! The four-corner annotation overlay, laid out as Horos's
//! `AnnotationsDefault.plist` describes.

use egui::{Align2, FontId, Painter, Pos2, Rect, Vec2};

use ruthos_core::{ImageRow, SeriesRow, StudyRow};
use ruthos_dicom::meta::{dicom_date_to_iso, dicom_time_to_hms};
use ruthos_dicom::Frame;
use ruthos_render::ViewTransform;

use crate::theme;
use crate::widgets::shadow_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnnotationLevel {
    #[default]
    Full,
    Partial,
    None,
}

impl AnnotationLevel {
    pub fn next(self) -> Self {
        match self {
            AnnotationLevel::Full => AnnotationLevel::Partial,
            AnnotationLevel::Partial => AnnotationLevel::None,
            AnnotationLevel::None => AnnotationLevel::Full,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AnnotationLevel::Full => "Full",
            AnnotationLevel::Partial => "Partial",
            AnnotationLevel::None => "None",
        }
    }
}

pub struct AnnotationContext<'a> {
    pub study: &'a StudyRow,
    pub series: &'a SeriesRow,
    pub image: &'a ImageRow,
    pub frame: &'a Frame,
    pub image_index: usize,
    pub image_count: usize,
    pub transform: &'a ViewTransform,
    pub mouse_pixel: Option<(i64, i64)>,
    pub level: AnnotationLevel,
}

pub fn draw(painter: &Painter, rect: Rect, ctx: &AnnotationContext<'_>) {
    if ctx.level == AnnotationLevel::None {
        return;
    }
    let font = FontId::proportional(12.5);
    let line = 15.0;
    let margin = 6.0;
    let color = theme::ANNOTATION_COLOR;
    let m = &ctx.frame.meta;
    let s = &ctx.transform.state;
    let full = ctx.level == AnnotationLevel::Full;

    // ---- top left: sizes, WL/WW, mouse
    let mut tl: Vec<String> = Vec::new();
    if full {
        tl.push(format!("Image size: {} x {}", ctx.frame.width, ctx.frame.height));
        tl.push(format!("View size: {} x {}", rect.width().round() as i64, rect.height().round() as i64));
    }
    if !ctx.frame.is_color() {
        tl.push(format!("WL: {}  WW: {}", s.window_center.round() as i64, s.window_width.round() as i64));
    }
    if full {
        if let Some((x, y)) = ctx.mouse_pixel {
            match ctx.frame.value_at(x, y) {
                Some(v) => tl.push(format!("X: {x} px  Y: {y} px  Value: {}", fmt_value(v))),
                None => tl.push(format!("X: {x} px  Y: {y} px")),
            }
            if let Some([row_mm, col_mm]) = m.pixel_spacing {
                tl.push(format!("X: {:.1} mm  Y: {:.1} mm", x as f64 * col_mm, y as f64 * row_mm));
            }
        }
    }
    for (i, t) in tl.iter().enumerate() {
        shadow_text(
            painter,
            Pos2::new(rect.left() + margin, rect.top() + margin + i as f32 * line),
            Align2::LEFT_TOP,
            t,
            font.clone(),
            color,
        );
    }

    // ---- top right: patient, study, series
    let mut tr: Vec<String> = Vec::new();
    tr.push(ctx.study.patient_name.clone());
    {
        let mut idline = Vec::new();
        if !ctx.study.patient_id.is_empty() {
            idline.push(ctx.study.patient_id.clone());
        }
        if !ctx.study.patient_sex.is_empty() {
            idline.push(ctx.study.patient_sex.clone());
        }
        if let Some(age) = ctx.study.age_at_study() {
            idline.push(age);
        }
        if let Some(species) = &ctx.study.patient_species {
            idline.push(species.clone());
        }
        if !idline.is_empty() {
            tr.push(idline.join("  "));
        }
    }
    if full {
        if let Some(d) = &ctx.study.study_date {
            let mut dt = d.clone();
            if let Some(t) = &ctx.study.study_time {
                dt.push(' ');
                dt.push_str(t);
            }
            tr.push(dt);
        }
        if !ctx.study.study_description.is_empty() {
            tr.push(ctx.study.study_description.clone());
        }
        let sd = ctx.series.display_name();
        if !sd.is_empty() {
            tr.push(sd);
        }
        if let Some(n) = ctx.series.series_number {
            tr.push(format!("Series: {n}"));
        }
    }
    for (i, t) in tr.iter().enumerate() {
        shadow_text(
            painter,
            Pos2::new(rect.right() - margin, rect.top() + margin + i as f32 * line),
            Align2::RIGHT_TOP,
            t,
            font.clone(),
            color,
        );
    }

    // ---- bottom left: zoom, image number, compression, thickness
    let mut bl: Vec<String> = Vec::new();
    bl.push(format!("Im: {}/{}", ctx.image_index + 1, ctx.image_count));
    if m.number_of_frames > 1 {
        bl.push(format!("Frame: {}/{}", ctx.frame.frame_index + 1, m.number_of_frames));
    }
    if full {
        bl.push(format!("Zoom: {}%  Angle: {}°", (s.zoom * 100.0).round() as i64, s.rotation_deg.round() as i64));
        if let Some(p) = m.image_position {
            bl.push(format!("Pos: {:.1}  {:.1}  {:.1}", p[0], p[1], p[2]));
        }
        bl.push(format!("{}", compression_name(&ctx.image.transfer_syntax)));
        match (m.slice_thickness, m.slice_location) {
            (Some(t), Some(l)) => bl.push(format!("Thickness: {t:.2} mm  Location: {l:.2} mm")),
            (Some(t), None) => bl.push(format!("Thickness: {t:.2} mm")),
            (None, Some(l)) => bl.push(format!("Location: {l:.2} mm")),
            _ => {}
        }
    }
    let n = bl.len();
    for (i, t) in bl.iter().enumerate() {
        shadow_text(
            painter,
            Pos2::new(rect.left() + margin, rect.bottom() - margin - (n - 1 - i) as f32 * line),
            Align2::LEFT_BOTTOM,
            t,
            font.clone(),
            color,
        );
    }

    // ---- bottom right: acquisition parameters and date
    if full {
        let mut br: Vec<String> = Vec::new();
        if m.modality == "MR" {
            if let (Some(te), Some(tr_)) = (m.echo_time, m.repetition_time) {
                br.push(format!("TE: {te:.1}  TR: {tr_:.1}"));
            }
            if let Some(fs) = m.magnetic_field_strength {
                br.push(format!("FS: {fs:.1} T"));
            }
        }
        if matches!(m.modality.as_str(), "CR" | "DX" | "RG" | "MG" | "IO") {
            let mut acq = Vec::new();
            if let Some(kvp) = m.kvp {
                acq.push(format!("{kvp:.0} kVp"));
            }
            if let Some(mas) = m.exposure_mas {
                acq.push(format!("{mas:.1} mAs"));
            }
            if !acq.is_empty() {
                br.push(acq.join("  "));
            }
            let mut view = Vec::new();
            if !m.body_part_examined.is_empty() {
                view.push(m.body_part_examined.clone());
            }
            if !m.view_position.is_empty() {
                view.push(m.view_position.clone());
            }
            if !m.laterality.is_empty() {
                view.push(m.laterality.clone());
            }
            if !view.is_empty() {
                br.push(view.join(" "));
            }
        }
        let date = m
            .acquisition_date
            .as_deref()
            .or(m.content_date.as_deref())
            .and_then(dicom_date_to_iso);
        let time = m
            .acquisition_time
            .as_deref()
            .or(m.content_time.as_deref())
            .and_then(dicom_time_to_hms);
        match (date, time) {
            (Some(d), Some(t)) => br.push(format!("{d} {t}")),
            (Some(d), None) => br.push(d),
            _ => {}
        }
        let n = br.len();
        for (i, t) in br.iter().enumerate() {
            shadow_text(
                painter,
                Pos2::new(rect.right() - margin, rect.bottom() - margin - (n - 1 - i) as f32 * line),
                Align2::RIGHT_BOTTOM,
                t,
                font.clone(),
                color,
            );
        }
    }

    // ---- orientation letters at the edge midpoints
    if let Some(labels) = orientation_labels(ctx.frame) {
        let t = ctx.transform;
        let w = ctx.frame.width as f32;
        let h = ctx.frame.height as f32;
        // (image-space edge midpoint, label)
        let edges = [
            (Vec2::new(w, h / 2.0), labels.right.as_str()),
            (Vec2::new(0.0, h / 2.0), labels.left.as_str()),
            (Vec2::new(w / 2.0, 0.0), labels.top.as_str()),
            (Vec2::new(w / 2.0, h), labels.bottom.as_str()),
        ];
        let centre = rect.center();
        for (p, label) in edges {
            if label.is_empty() {
                continue;
            }
            let sp = t.image_to_screen(p);
            // Place the letter at the viewport edge in the direction of that side.
            let dir = sp - centre;
            if dir.length() < 1e-3 {
                continue;
            }
            let dir = dir.normalized();
            let half = rect.size() / 2.0 - Vec2::splat(22.0);
            let k = (half.x / dir.x.abs().max(1e-3)).min(half.y / dir.y.abs().max(1e-3));
            let pos = centre + dir * k;
            shadow_text(painter, pos, Align2::CENTER_CENTER, label, FontId::proportional(15.0), color);
        }
    }
}

struct EdgeLabels {
    left: String,
    right: String,
    top: String,
    bottom: String,
}

/// Anatomical direction letters for the image edges, from Patient Orientation
/// (projection radiographs) or Image Orientation (Patient) (cross-sectional).
fn orientation_labels(frame: &Frame) -> Option<EdgeLabels> {
    let m = &frame.meta;
    if let Some([row_dir, col_dir]) = &m.patient_orientation {
        // Row direction is the direction of increasing column index (to the
        // right of the image); column direction is downwards.
        let right = clean_orientation(row_dir);
        let bottom = clean_orientation(col_dir);
        return Some(EdgeLabels {
            left: opposite(&right),
            right,
            top: opposite(&bottom),
            bottom,
        });
    }
    if let Some(iop) = m.image_orientation {
        let right = direction_from_cosines([iop[0], iop[1], iop[2]]);
        let bottom = direction_from_cosines([iop[3], iop[4], iop[5]]);
        return Some(EdgeLabels {
            left: opposite(&right),
            right,
            top: opposite(&bottom),
            bottom,
        });
    }
    None
}

fn clean_orientation(s: &str) -> String {
    s.chars().filter(|c| "LRAPHF".contains(*c)).take(3).collect()
}

fn opposite(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'L' => 'R',
            'R' => 'L',
            'A' => 'P',
            'P' => 'A',
            'H' => 'F',
            'F' => 'H',
            other => other,
        })
        .collect()
}

/// DICOM direction cosines to anatomical letters (major axis plus any
/// secondary axis above a quarter of the magnitude), as Horos does.
fn direction_from_cosines(v: [f64; 3]) -> String {
    let axes = [
        (v[0], if v[0] < 0.0 { 'R' } else { 'L' }),
        (v[1], if v[1] < 0.0 { 'A' } else { 'P' }),
        (v[2], if v[2] < 0.0 { 'F' } else { 'H' }),
    ];
    let mut sorted = axes.to_vec();
    sorted.sort_by(|a, b| b.0.abs().partial_cmp(&a.0.abs()).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = String::new();
    let major = sorted[0].0.abs();
    for (mag, letter) in sorted {
        if mag.abs() > 1e-4 && mag.abs() >= major * 0.25 {
            out.push(letter);
        }
    }
    out
}

fn fmt_value(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

/// Friendly transfer syntax name for the compression annotation.
pub fn compression_name(ts: &str) -> &'static str {
    match ts.trim_end_matches('\0') {
        "1.2.840.10008.1.2" => "Implicit VR Little Endian",
        "1.2.840.10008.1.2.1" => "Uncompressed",
        "1.2.840.10008.1.2.1.99" => "Deflated",
        "1.2.840.10008.1.2.2" => "Big Endian",
        "1.2.840.10008.1.2.4.50" => "JPEG Baseline",
        "1.2.840.10008.1.2.4.51" => "JPEG Extended",
        "1.2.840.10008.1.2.4.57" => "JPEG Lossless",
        "1.2.840.10008.1.2.4.70" => "JPEG Lossless SV1",
        "1.2.840.10008.1.2.4.80" => "JPEG-LS Lossless",
        "1.2.840.10008.1.2.4.81" => "JPEG-LS Near-lossless",
        "1.2.840.10008.1.2.4.90" => "JPEG 2000 Lossless",
        "1.2.840.10008.1.2.4.91" => "JPEG 2000",
        "1.2.840.10008.1.2.4.201" | "1.2.840.10008.1.2.4.202" | "1.2.840.10008.1.2.4.203" => "HTJ2K",
        "1.2.840.10008.1.2.5" => "RLE Lossless",
        _ => "Unknown transfer syntax",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosines_axial() {
        assert_eq!(direction_from_cosines([1.0, 0.0, 0.0]), "L");
        assert_eq!(direction_from_cosines([0.0, 1.0, 0.0]), "P");
        assert_eq!(direction_from_cosines([0.0, 0.0, -1.0]), "F");
        assert_eq!(direction_from_cosines([0.7, 0.7, 0.0]).len(), 2);
    }

    #[test]
    fn opposites() {
        assert_eq!(opposite("L"), "R");
        assert_eq!(opposite("HF"), "FH");
        assert_eq!(clean_orientation("L "), "L");
    }
}
