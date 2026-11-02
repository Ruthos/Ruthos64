//! The Report window (Horos: ReportController, kept simple).
//!
//! One window per study: the patient header, the named measurements taken
//! in the viewer, a Findings box and a Conclusion box. Text is saved in the
//! archive; Export writes a PDF with the key images.

use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, RichText, Ui, ViewportId};
use parking_lot::Mutex;

use ruthos_core::report::{format_value, write_pdf, ReportDocument, ReportImage, ReportMeasurement};
use ruthos_core::{ImageRow, MeasurementRow, StudyRow};

use crate::icons::Bitmap;
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{toolbar_separator, ToolbarButton};

pub struct ReportState {
    pub id: u64,
    pub viewport_id: ViewportId,
    pub study: StudyRow,
    measurements: Vec<MeasurementRow>,
    key_images: Vec<ImageRow>,
    findings: String,
    conclusion: String,
    saved: (String, String),
    status: Option<(String, Instant)>,
    pub close_requested: bool,
    last_refresh: Instant,
}

impl ReportState {
    pub fn open(shared: &Shared, study: StudyRow) -> Self {
        let id = shared.next_id();
        let report = shared.db.report(study.id).ok().flatten();
        let (findings, conclusion) = report.map(|r| (r.findings, r.conclusion)).unwrap_or_default();
        let mut s = Self {
            id,
            viewport_id: ViewportId::from_hash_of(("ruthos-report", id)),
            study,
            measurements: Vec::new(),
            key_images: Vec::new(),
            saved: (findings.clone(), conclusion.clone()),
            findings,
            conclusion,
            status: None,
            close_requested: false,
            last_refresh: Instant::now(),
        };
        s.refresh(shared);
        s
    }

    pub fn title(&self) -> String {
        format!("Report: {}  {} \u{2014} Ruthos", self.study.patient_name, self.study.study_date.clone().unwrap_or_default())
    }

    fn refresh(&mut self, shared: &Shared) {
        let mut measurements = shared.db.named_measurements_for_study(self.study.id).unwrap_or_default();
        // Values are recomputed from the image so a calibration applied after
        // the measurement was drawn is reflected.
        let mut frames: std::collections::HashMap<(i64, i64), Option<std::sync::Arc<ruthos_dicom::Frame>>> =
            std::collections::HashMap::new();
        for m in &mut measurements {
            let key = (m.roi.image_id, m.roi.frame);
            let frame = frames
                .entry(key)
                .or_insert_with(|| {
                    let path = shared.db.image(m.roi.image_id).ok().flatten().map(|i| shared.resolve(&i.path))?;
                    let mut frame = ruthos_dicom::decode_frame(&path, m.roi.frame as u32).ok()?;
                    if let Ok(Some(spacing)) = shared.db.calibration(m.roi.image_id) {
                        frame.meta.pixel_spacing = Some(spacing);
                        frame.meta.pixel_spacing_is_calibrated = true;
                        frame.meta.user_calibrated = true;
                    }
                    Some(std::sync::Arc::new(frame))
                })
                .clone();
            if let (Some(frame), Some(roi)) = (frame, crate::roi::Roi::from_record(&m.roi)) {
                let (value, unit) = roi.measurement(&frame);
                m.roi.value = value;
                m.roi.unit = unit.to_string();
            }
        }
        self.measurements = measurements;
        self.key_images = shared.db.key_images_for_study(self.study.id).unwrap_or_default();
        self.last_refresh = Instant::now();
    }

    fn is_dirty(&self) -> bool {
        self.saved.0 != self.findings || self.saved.1 != self.conclusion
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    fn save(&mut self, shared: &Shared) {
        match shared.db.set_report(self.study.id, &self.findings, &self.conclusion) {
            Ok(()) => {
                self.saved = (self.findings.clone(), self.conclusion.clone());
                self.set_status("Report saved");
            }
            Err(e) => self.set_status(format!("Could not save: {e}")),
        }
    }

    pub fn show(&mut self, ui: &mut Ui, shared: &Shared) {
        ui.style_mut().visuals = theme::browser_visuals();
        if ui.input(|i| i.viewport().close_requested()) {
            tracing::info!("report window for study {} closing on request", self.study.id);
            if self.is_dirty() {
                self.save(shared);
            }
            self.close_requested = true;
        }
        // Measurements made in the viewer appear here without a reopen.
        if self.last_refresh.elapsed().as_secs_f32() > 5.0 {
            self.refresh(shared);
        }

        egui::Panel::top(("report-toolbar", self.id))
            .exact_size(theme::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .stroke(theme::toolbar_stroke(false))
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| {
                theme::paint_toolbar_background(ui, false, egui::vec2(6.0, 3.0));
                ui.horizontal(|ui| {
                    if ToolbarButton::new(Bitmap::Save, "Save").enabled(self.is_dirty()).tooltip("Save the report text (Ctrl+S)").show(ui).clicked() {
                        self.save(shared);
                    }
                    if ToolbarButton::new(Bitmap::Export, "Export PDF").tooltip("Write the report, measurements and key images as a PDF").show(ui).clicked() {
                        self.export_pdf(shared);
                    }
                    toolbar_separator(ui);
                    if ToolbarButton::new(Bitmap::Viewer, "2D Viewer").tooltip("Open this study in the viewer").show(ui).clicked() {
                        shared.open_requests.lock().push(self.study.id);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some((text, when)) = &self.status {
                            if when.elapsed().as_secs_f32() < 4.0 {
                                ui.label(RichText::new(text).size(12.0).color(Color32::from_gray(70)));
                            }
                        }
                    });
                });
            });

        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save(shared);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(246, 246, 246)).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.body(ui));
            });
    }

    fn body(&mut self, ui: &mut Ui) {
        let s = &self.study;
        ui.label(RichText::new(&s.patient_name).size(20.0).strong());
        ui.add_space(4.0);
        egui::Grid::new(("report-header", self.id)).num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
            for (label, value) in self.header_rows() {
                ui.label(RichText::new(label).color(Color32::from_gray(110)));
                ui.label(value);
                ui.end_row();
            }
        });
        ui.add_space(14.0);

        ui.label(RichText::new("Measurements").size(15.0).strong());
        ui.add_space(4.0);
        if self.measurements.is_empty() {
            ui.label(
                RichText::new("None yet. In the viewer, click a measurement's number to give it a name and it appears here.")
                    .size(12.5)
                    .color(Color32::from_gray(110)),
            );
        } else {
            egui::Grid::new(("report-measurements", self.id))
                .num_columns(3)
                .striped(true)
                .spacing([18.0, 4.0])
                .min_col_width(90.0)
                .show(ui, |ui| {
                    ui.label(RichText::new("Measurement").strong());
                    ui.label(RichText::new("Value").strong());
                    ui.label(RichText::new("Image").strong());
                    ui.end_row();
                    for m in &self.measurements {
                        ui.label(m.roi.name.clone().unwrap_or_default());
                        ui.label(format_value(m.roi.value, &m.roi.unit));
                        ui.label(RichText::new(m.location()).color(Color32::from_gray(90)));
                        ui.end_row();
                    }
                });
        }
        ui.add_space(14.0);

        ui.label(RichText::new("Findings").size(15.0).strong());
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.findings)
                .desired_width(f32::INFINITY)
                .desired_rows(10)
                .hint_text("What was seen."),
        );
        ui.add_space(12.0);
        ui.label(RichText::new("Conclusion").size(15.0).strong());
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.conclusion)
                .desired_width(f32::INFINITY)
                .desired_rows(4)
                .hint_text("What it means."),
        );
        ui.add_space(12.0);
        let n = self.key_images.len();
        ui.label(
            RichText::new(match n {
                0 => "No key images. Mark images with the Key Image button in the viewer to include them in the PDF.".to_string(),
                1 => "1 key image will be included in the PDF.".to_string(),
                n => format!("{n} key images will be included in the PDF."),
            })
            .size(12.5)
            .color(Color32::from_gray(110)),
        );
    }

    fn header_rows(&self) -> Vec<(String, String)> {
        let s = &self.study;
        let mut rows = vec![("Patient ID".to_string(), s.patient_id.clone())];
        let species = [s.patient_species.clone(), s.patient_breed.clone()]
            .into_iter()
            .flatten()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if !species.is_empty() {
            rows.push(("Species".into(), species));
        }
        let mut sex_age = s.patient_sex.clone();
        if let Some(age) = s.age_at_study() {
            if !sex_age.is_empty() {
                sex_age.push_str(", ");
            }
            sex_age.push_str(&age);
        }
        if !sex_age.is_empty() {
            rows.push(("Sex, age".into(), sex_age));
        }
        if let Some(owner) = s.responsible_person.clone().filter(|v| !v.is_empty()) {
            rows.push(("Owner".into(), owner));
        }
        let mut when = s.study_date.clone().unwrap_or_default();
        if let Some(t) = &s.study_time {
            when.push(' ');
            when.push_str(t);
        }
        rows.push(("Study date".into(), when));
        if !s.study_description.is_empty() {
            rows.push(("Description".into(), s.study_description.clone()));
        }
        rows.push(("Modality".into(), s.modalities.replace('\\', ", ")));
        if !s.institution_name.is_empty() {
            rows.push(("Institution".into(), s.institution_name.clone()));
        }
        if !s.referring_physician.is_empty() {
            rows.push(("Referring".into(), s.referring_physician.clone()));
        }
        rows
    }

    fn export_pdf(&mut self, shared: &Shared) {
        if self.is_dirty() {
            self.save(shared);
        }
        let default_name = format!(
            "{}_{}_report.pdf",
            self.study.patient_name.replace([',', ' '], "_"),
            self.study.study_date.clone().unwrap_or_default()
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export report")
            .add_filter("PDF", &["pdf"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        let images = self.render_key_images(shared);
        let doc = ReportDocument {
            title: format!("Report: {}", self.study.patient_name),
            header: self.header_rows(),
            measurements: self
                .measurements
                .iter()
                .map(|m| ReportMeasurement {
                    name: m.roi.name.clone().unwrap_or_default(),
                    value: format_value(m.roi.value, &m.roi.unit),
                    location: m.location(),
                })
                .collect(),
            findings: self.findings.clone(),
            conclusion: self.conclusion.clone(),
            images,
            footer: format!("Ruthos \u{2014} {}", chrono::Local::now().format("%Y-%m-%d %H:%M")),
        };
        match write_pdf(&doc, &path) {
            Ok(()) => {
                self.set_status(format!("Exported {}", path.display()));
                // Show it: Windows opens PDFs with the user's reader.
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &path.to_string_lossy()])
                    .spawn();
            }
            Err(e) => self.set_status(format!("Export failed: {e}")),
        }
    }

    /// Decode and window each key image for the PDF, at most 1000 px wide.
    fn render_key_images(&self, shared: &Shared) -> Vec<ReportImage> {
        let mut out = Vec::new();
        for img in &self.key_images {
            let path = shared.resolve(&img.path);
            let Ok(frame) = ruthos_dicom::decode_frame(&path, 0) else { continue };
            let series = shared.db.series(img.series_id).ok().flatten();
            let (c, w) = match series.as_ref().and_then(|s| s.window_center.zip(s.window_width)) {
                Some((c, w)) if w > 0.0 => (c as f32, w as f32),
                _ => frame.default_window(),
            };
            let clut = ruthos_dicom::Clut::grayscale();
            let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, c, w, &clut);
            let Some(full) = image::RgbImage::from_raw(frame.width, frame.height, rgb) else { continue };
            let max_w = 1000u32;
            let small = if frame.width > max_w {
                let h = (frame.height as f32 * max_w as f32 / frame.width as f32).round().max(1.0) as u32;
                image::imageops::resize(&full, max_w, h, image::imageops::FilterType::Triangle)
            } else {
                full
            };
            let caption = format!(
                "{} \u{2014} image {}",
                series.map(|s| s.display_name()).unwrap_or_default(),
                img.instance_number.unwrap_or(0)
            );
            out.push(ReportImage { caption, width: small.width(), height: small.height(), rgb: small.into_raw() });
        }
        out
    }
}

/// Shared handle type the app keeps per open report window.
pub type ReportHandle = Arc<Mutex<ReportState>>;
