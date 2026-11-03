//! Histogram and plot (Horos: ROI > Histogram, Plot).
//!
//! For an area (rectangle, ellipse, polygon, pencil, brush) the window
//! shows the histogram of the modality values inside it; for a caliper line
//! or an open polygon, the values along it. With nothing selected it shows
//! the histogram of the whole image. The numbers are the image's own
//! modality values (Hounsfield units on CT), not the displayed grey levels.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use ruthos_dicom::Frame;

use crate::roi::{Profile, Roi};

/// What the window shows, computed once per change of the measurement.
#[derive(Debug, Clone, PartialEq)]
pub enum PlotData {
    Histogram {
        counts: Vec<u32>,
        lo: f32,
        hi: f32,
        n: usize,
        mean: f64,
        sd: f64,
        median: f32,
    },
    Profile(Profile),
    /// Nothing to plot, and why.
    Empty(&'static str),
}

/// Bins for a histogram of `values`: about one per value for small integer
/// ranges (an 8-bit ultrasound frame), otherwise `max_bins`.
fn bin_count(lo: f32, hi: f32, max_bins: usize) -> usize {
    let span = (hi - lo).max(1e-6);
    if span <= max_bins as f32 && (hi.fract() == 0.0 && lo.fract() == 0.0) {
        (span as usize + 1).max(1)
    } else {
        max_bins
    }
}

pub fn histogram_of(mut values: Vec<f32>, max_bins: usize) -> PlotData {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return PlotData::Empty("No pixels inside");
    }
    let (lo, hi) = values.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
        (lo.min(*v), hi.max(*v))
    });
    let bins = bin_count(lo, hi, max_bins);
    let width = ((hi - lo) / bins as f32).max(1e-6);
    let mut counts = vec![0u32; bins];
    let (mut sum, mut sum_sq) = (0.0f64, 0.0f64);
    for v in &values {
        let i = (((v - lo) / width) as usize).min(bins - 1);
        counts[i] += 1;
        sum += *v as f64;
        sum_sq += (*v as f64) * (*v as f64);
    }
    let n = values.len();
    let mean = sum / n as f64;
    let sd = (sum_sq / n as f64 - mean * mean).max(0.0).sqrt();
    let mid = n / 2;
    let median = *values.select_nth_unstable_by(mid, f32::total_cmp).1;
    PlotData::Histogram {
        counts,
        lo,
        hi,
        n,
        mean,
        sd,
        median,
    }
}

/// The data for a measurement, or for the whole frame when `roi` is `None`.
pub fn plot_data(frame: &Frame, roi: Option<&Roi>) -> PlotData {
    if frame.is_color() {
        return PlotData::Empty("Colour images have no single value per pixel to plot");
    }
    match roi {
        None => {
            let mut v = Vec::with_capacity(frame.width as usize * frame.height as usize);
            for y in 0..frame.height as i64 {
                for x in 0..frame.width as i64 {
                    if let Some(p) = frame.value_at(x, y) {
                        v.push(p);
                    }
                }
            }
            histogram_of(v, 256)
        }
        Some(r) => {
            if let Some(p) = r.profile(frame) {
                return PlotData::Profile(p);
            }
            match r.pixel_values(frame) {
                Some(v) => histogram_of(v, 128),
                None => PlotData::Empty("Select an area, a caliper line or an open polygon"),
            }
        }
    }
}

/// The window's state: what it is showing and the cached data.
pub struct HistogramWindow {
    /// What the data was computed from: the frame and the measurement's
    /// storage form, so an edit recomputes it.
    signature: Option<String>,
    data: PlotData,
}

impl Default for HistogramWindow {
    fn default() -> Self {
        Self {
            signature: None,
            data: PlotData::Empty(""),
        }
    }
}

impl HistogramWindow {
    /// Recompute when the frame or the measurement changed.
    pub fn update(&mut self, frame_key: impl std::fmt::Debug, frame: &Frame, roi: Option<&Roi>) {
        let sig = format!(
            "{frame_key:?}|{:?}",
            roi.map(|r| (r.points(), crate::roi::kind_key(&r.kind)))
        );
        if self.signature.as_deref() != Some(sig.as_str()) {
            self.data = plot_data(frame, roi);
            self.signature = Some(sig);
        }
    }

    pub fn data(&self) -> &PlotData {
        &self.data
    }

    /// The values as comma-separated text, for the clipboard.
    pub fn csv(&self) -> String {
        match &self.data {
            PlotData::Histogram { counts, lo, hi, .. } => {
                let w = (hi - lo) / counts.len().max(1) as f32;
                let mut s = String::from("from,to,count\n");
                for (i, c) in counts.iter().enumerate() {
                    s.push_str(&format!("{},{},{c}\n", lo + w * i as f32, lo + w * (i + 1) as f32));
                }
                s
            }
            PlotData::Profile(p) => {
                let mut s = format!("distance_{},value\n", p.unit);
                for (d, v) in p.distance.iter().zip(&p.values) {
                    s.push_str(&format!("{d:.3},{v}\n"));
                }
                s
            }
            PlotData::Empty(_) => String::new(),
        }
    }

    /// Draw the chart and its numbers.
    pub fn ui(&self, ui: &mut Ui) {
        let size = Vec2::new(460.0, 220.0);
        match &self.data {
            PlotData::Empty(why) => {
                ui.label(*why);
            }
            PlotData::Histogram {
                counts,
                lo,
                hi,
                n,
                mean,
                sd,
                median,
            } => {
                let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
                let plot = chart_frame(ui, rect);
                let max = *counts.iter().max().unwrap_or(&1) as f32;
                let bw = plot.width() / counts.len() as f32;
                let bar = Color32::from_rgb(70, 130, 200);
                for (i, c) in counts.iter().enumerate() {
                    if *c == 0 {
                        continue;
                    }
                    let h = plot.height() * (*c as f32 / max);
                    let x0 = plot.left() + bw * i as f32;
                    ui.painter().rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x0, plot.bottom() - h),
                            Pos2::new(x0 + bw.max(1.0), plot.bottom()),
                        ),
                        0.0,
                        bar,
                    );
                }
                // The mean, and one SD either side.
                let x_of = |v: f64| plot.left() + plot.width() * ((v as f32 - lo) / (hi - lo).max(1e-6));
                let mark = Stroke::new(1.0, Color32::from_rgb(200, 60, 40));
                ui.painter().line_segment(
                    [
                        Pos2::new(x_of(*mean), plot.top()),
                        Pos2::new(x_of(*mean), plot.bottom()),
                    ],
                    mark,
                );
                for v in [mean - sd, mean + sd] {
                    dashed_vertical(ui, x_of(v), plot, mark);
                }
                axis_labels(ui, plot, &format!("{lo:.0}"), &format!("{hi:.0}"), &format!("{max:.0}"));
                if let Some(h) = response.hover_pos().filter(|h| plot.contains(*h)) {
                    let i = (((h.x - plot.left()) / bw) as usize).min(counts.len() - 1);
                    let w = (hi - lo) / counts.len() as f32;
                    hover_text(
                        ui,
                        h,
                        &format!(
                            "{:.1} to {:.1}: {}",
                            lo + w * i as f32,
                            lo + w * (i + 1) as f32,
                            counts[i]
                        ),
                    );
                }
                ui.label(format!(
                    "{n} pixels   mean {mean:.2}   SD {sd:.2}   median {median:.1}   min {lo:.0}   max {hi:.0}"
                ));
            }
            PlotData::Profile(p) => {
                let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
                let plot = chart_frame(ui, rect);
                let (vlo, vhi) = p.values.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(*v), hi.max(*v))
                });
                let span = (vhi - vlo).max(1e-6);
                let dmax = p.distance.last().copied().unwrap_or(1.0).max(1e-9);
                let to_screen = |d: f64, v: f32| {
                    Pos2::new(
                        plot.left() + plot.width() * (d / dmax) as f32,
                        plot.bottom() - plot.height() * ((v - vlo) / span),
                    )
                };
                let line: Vec<Pos2> = p
                    .distance
                    .iter()
                    .zip(&p.values)
                    .map(|(d, v)| to_screen(*d, *v))
                    .collect();
                ui.painter()
                    .line(line, Stroke::new(1.5, Color32::from_rgb(70, 130, 200)));
                axis_labels(ui, plot, "0", &format!("{dmax:.1} {}", p.unit), &format!("{vhi:.0}"));
                ui.painter().text(
                    Pos2::new(plot.left() - 4.0, plot.bottom()),
                    Align2::RIGHT_BOTTOM,
                    format!("{vlo:.0}"),
                    FontId::proportional(11.0),
                    Color32::from_gray(90),
                );
                if let Some(h) = response.hover_pos().filter(|h| plot.contains(*h)) {
                    let d = (h.x - plot.left()) as f64 / plot.width() as f64 * dmax;
                    let i = p.distance.iter().position(|x| *x >= d).unwrap_or(p.values.len() - 1);
                    let at = to_screen(p.distance[i], p.values[i]);
                    ui.painter().circle_filled(at, 3.0, Color32::from_rgb(200, 60, 40));
                    hover_text(ui, h, &format!("{:.2} {}: {}", p.distance[i], p.unit, p.values[i]));
                }
                let n = p.values.len() as f64;
                let mean = p.values.iter().map(|v| *v as f64).sum::<f64>() / n;
                ui.label(format!(
                    "{} samples over {dmax:.2} {}   mean {mean:.2}   min {vlo:.0}   max {vhi:.0}",
                    p.values.len(),
                    p.unit
                ));
            }
        }
    }
}

/// The chart background and the plotting area inside its margins.
fn chart_frame(ui: &Ui, rect: Rect) -> Rect {
    ui.painter().rect_filled(rect, 3.0, Color32::from_gray(250));
    ui.painter().rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_gray(200)),
        egui::StrokeKind::Inside,
    );
    Rect::from_min_max(rect.min + Vec2::new(44.0, 10.0), rect.max - Vec2::new(10.0, 22.0))
}

fn axis_labels(ui: &Ui, plot: Rect, x_lo: &str, x_hi: &str, y_hi: &str) {
    let font = FontId::proportional(11.0);
    let c = Color32::from_gray(90);
    let p = ui.painter();
    p.line_segment(
        [plot.left_bottom(), plot.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(150)),
    );
    p.line_segment(
        [plot.left_bottom(), plot.left_top()],
        Stroke::new(1.0, Color32::from_gray(150)),
    );
    p.text(
        plot.left_bottom() + Vec2::new(0.0, 4.0),
        Align2::LEFT_TOP,
        x_lo,
        font.clone(),
        c,
    );
    p.text(
        plot.right_bottom() + Vec2::new(0.0, 4.0),
        Align2::RIGHT_TOP,
        x_hi,
        font.clone(),
        c,
    );
    p.text(plot.left_top() - Vec2::new(4.0, 0.0), Align2::RIGHT_TOP, y_hi, font, c);
}

fn dashed_vertical(ui: &Ui, x: f32, plot: Rect, stroke: Stroke) {
    if x < plot.left() || x > plot.right() {
        return;
    }
    let mut y = plot.top();
    while y < plot.bottom() {
        ui.painter()
            .line_segment([Pos2::new(x, y), Pos2::new(x, (y + 4.0).min(plot.bottom()))], stroke);
        y += 8.0;
    }
}

fn hover_text(ui: &Ui, at: Pos2, text: &str) {
    let font = FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::BLACK);
    let r = Rect::from_min_size(at + Vec2::new(10.0, -24.0), galley.size() + Vec2::new(8.0, 4.0));
    ui.painter()
        .rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(255, 255, 230, 240));
    ui.painter().galley(r.min + Vec2::new(4.0, 2.0), galley, Color32::BLACK);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_integer_ranges_get_one_bin_per_value() {
        let PlotData::Histogram {
            counts,
            n,
            mean,
            median,
            ..
        } = histogram_of(vec![0.0, 1.0, 1.0, 2.0, 2.0, 2.0], 128)
        else {
            panic!()
        };
        assert_eq!(counts, vec![1, 2, 3]);
        assert_eq!(n, 6);
        assert!((mean - 8.0 / 6.0).abs() < 1e-9);
        assert_eq!(median, 2.0);
    }

    #[test]
    fn wide_ranges_use_the_bin_limit() {
        let v: Vec<f32> = (0..10_000).map(|i| i as f32 * 0.37 - 1000.0).collect();
        let PlotData::Histogram { counts, lo, hi, .. } = histogram_of(v, 128) else {
            panic!()
        };
        assert_eq!(counts.len(), 128);
        assert_eq!(counts.iter().sum::<u32>(), 10_000);
        assert!(lo < -999.0 && hi > 2698.0);
    }

    #[test]
    fn nothing_inside_is_said_so() {
        assert!(matches!(histogram_of(vec![], 64), PlotData::Empty(_)));
    }
}
