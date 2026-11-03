//! The anonymiser dialog: Case, Pixels, Verify and write.
//!
//! Nothing is written until the last step, so the user can go back at any
//! point. The work runs on a thread with a progress bar; a cancelled run
//! removes what it wrote to its temporary folder.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Receiver};
use egui::{Color32, Context, Pos2, Rect as ERect, RichText, Sense, Stroke, TextureHandle, Ui, Vec2};

use ruthos_core::teaching::{
    self, CaseChoice, Destination, ExportRequest, ExportResult, MaskDecision, MeasurementTreatment, NewCase, Prepared,
    SeriesClassification,
};
use ruthos_core::{SeriesRow, StudyRow, TeachingCase};
use ruthos_dicom::deid::{collect_context, conflicts_with_tokens_text, MaskTemplate, Rect, VerifyContext};
use ruthos_dicom::open::open_any;

use crate::shared::Shared;
use crate::widgets::menu_heading;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Case,
    Pixels,
    Verify,
}

struct SeriesState {
    series: SeriesRow,
    class: SeriesClassification,
    decision: Option<MaskDecision>,
    approved: bool,
    thumb: Option<(TextureHandle, u32, u32)>,
    manufacturer: String,
    model: String,
    columns: u32,
    rows: u32,
}

struct Review {
    idx: usize,
    frames: Vec<(String, TextureHandle, u32, u32)>,
    rects: Vec<Rect>,
    drag: Option<(Pos2, Pos2)>,
    save_template: bool,
}

// One job message at a time; the prepared export is large by nature.
#[allow(clippy::large_enum_variant)]
enum Job {
    Progress(usize, usize, String),
    Prepared(Result<Prepared, String>),
    Committed(Result<ExportResult, String>),
}

pub struct AnonymizeState {
    study: StudyRow,
    step: Step,
    ctx: VerifyContext,
    cases: Vec<TeachingCase>,
    existing: Option<i64>,
    reference: String,
    name: String,
    title: String,
    to_archive: bool,
    to_folder: bool,
    folder: Option<PathBuf>,
    measurements: MeasurementTreatment,
    include_report: bool,
    findings_text: String,
    conclusion_text: String,
    preserve_season: bool,
    series: Vec<SeriesState>,
    review: Option<Review>,
    rx: Option<Receiver<Job>>,
    progress: (usize, usize, String),
    prepared: Option<Prepared>,
    /// Set by Cancel; the preparing thread stops between images.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The final write is running (it cannot be cancelled).
    writing: bool,
    /// Cancel was pressed while busy: close once the thread has stopped.
    close_when_stopped: bool,
    overrides: HashMap<String, String>,
    blank: Vec<String>,
    result: Option<ExportResult>,
    error: Option<String>,
    busy: bool,
}

const RED: Color32 = Color32::from_rgba_premultiplied(160, 20, 20, 110);

impl AnonymizeState {
    pub fn open(shared: &Shared, study: StudyRow) -> Self {
        let series_rows = shared.db.series_for_study(study.id).unwrap_or_default();
        // Tokens from every series' representative image, for live checks.
        let mut ctx = VerifyContext::default();
        let mut series = Vec::new();
        for s in series_rows {
            let mut st = SeriesState {
                class: teaching::classify_series(&shared.db, &shared.paths, &s),
                series: s,
                decision: None,
                approved: false,
                thumb: None,
                manufacturer: String::new(),
                model: String::new(),
                columns: 0,
                rows: 0,
            };
            if let Ok(Some(img)) = shared.db.representative_image(st.series.id) {
                let path = shared.resolve(&img.path);
                if let Ok(obj) = open_any(&path, None) {
                    ctx.absorb(collect_context(&obj));
                }
                if let Ok(meta) = ruthos_dicom::read_meta(&path) {
                    st.manufacturer = meta.manufacturer.clone();
                    st.model = meta.model_name.clone();
                    st.columns = meta.columns;
                    st.rows = meta.rows;
                }
            }
            match &st.class {
                SeriesClassification::Regions(r) => {
                    st.decision = Some(MaskDecision::Regions(r.clone()));
                    st.approved = true;
                }
                SeriesClassification::RegionsWithTemplate { keep, blank, .. } => {
                    st.decision = Some(MaskDecision::RegionsReviewed {
                        keep: keep.clone(),
                        blank: blank.clone(),
                    });
                    st.approved = false; // checked once per new study
                }
                SeriesClassification::Trusted => {
                    st.decision = Some(MaskDecision::Trusted);
                    st.approved = true;
                }
                SeriesClassification::Template { name, rects } => {
                    st.decision = Some(MaskDecision::Template {
                        name: name.clone(),
                        rects: rects.clone(),
                    });
                    st.approved = false; // looked at once per new study
                }
                SeriesClassification::NeedsReview => {}
            }
            series.push(st);
        }
        let cases = shared.db.teaching_cases().unwrap_or_default();
        let taken: Vec<String> = cases.iter().map(|c| c.name.clone()).collect();
        let prefix = shared.settings.lock().teaching_reference_prefix.clone();
        let report = shared.db.report(study.id).ok().flatten();
        Self {
            name: teaching::suggest_name(study.patient_species.as_deref(), &taken, &ctx),
            reference: teaching::suggest_reference(&shared.db, &prefix),
            title: String::new(),
            study,
            step: Step::Case,
            ctx,
            cases,
            existing: None,
            to_archive: true,
            to_folder: false,
            folder: None,
            measurements: MeasurementTreatment::Include,
            include_report: false,
            findings_text: report.as_ref().map(|r| r.findings.clone()).unwrap_or_default(),
            conclusion_text: report.map(|r| r.conclusion).unwrap_or_default(),
            preserve_season: false,
            series,
            review: None,
            rx: None,
            progress: (0, 0, String::new()),
            prepared: None,
            cancel: Default::default(),
            writing: false,
            close_when_stopped: false,
            overrides: HashMap::new(),
            blank: Vec::new(),
            result: None,
            error: None,
            busy: false,
        }
    }

    /// Returns true when the dialog is finished and should be dropped.
    pub fn show(&mut self, ctx: &Context, shared: &Shared) -> bool {
        self.pump();
        // Cancelled while working: close once the thread has stopped and
        // cleaned up after itself.
        let mut close = self.close_when_stopped && !self.busy;
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        egui::Modal::new(egui::Id::unique("anonymize-modal")).show(ctx, |ui| {
            ui.set_width(880.0);
            ui.set_max_height(620.0);
            ui.horizontal(|ui| {
                ui.heading("Anonymise for teaching");
                ui.label(
                    RichText::new(format!("  {}", self.study.patient_name))
                        .size(14.0)
                        .color(Color32::from_gray(110)),
                );
            });
            self.steps_bar(ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(470.0)
                .auto_shrink([false, false])
                .show(ui, |ui| match self.step {
                    Step::Case => self.case_step(ui, shared),
                    Step::Pixels => self.pixels_step(ui, shared),
                    Step::Verify => self.verify_step(ui, shared),
                });
            ui.separator();
            ui.horizontal(|ui| {
                if self.result.is_some() {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    return;
                }
                // While a copy is being prepared Cancel stops it; it cannot
                // interrupt the final write, which is quick and atomic.
                let can_cancel = !self.busy || !self.writing;
                if ui.add_enabled(can_cancel, egui::Button::new("Cancel")).clicked() {
                    if self.busy {
                        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        self.close_when_stopped = true;
                    } else {
                        if let Some(p) = self.prepared.take() {
                            teaching::abandon(p);
                        }
                        close = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match self.step {
                    Step::Case => {
                        let ok = self.case_valid().is_none();
                        if ui.add_enabled(ok, egui::Button::new("Next: Pixels")).clicked() {
                            self.step = Step::Pixels;
                        }
                    }
                    Step::Pixels => {
                        let ok =
                            self.series.iter().all(|s| s.approved && s.decision.is_some()) && self.review.is_none();
                        if ui.add_enabled(ok, egui::Button::new("Next: Verify")).clicked() {
                            self.step = Step::Verify;
                        }
                        if ui
                            .add_enabled(!self.busy && self.review.is_none(), egui::Button::new("Back"))
                            .clicked()
                        {
                            self.step = Step::Case;
                        }
                    }
                    Step::Verify => {
                        let clean = self.prepared.as_ref().is_some_and(|p| p.is_clean());
                        if ui
                            .add_enabled(clean && !self.busy, egui::Button::new("Write"))
                            .clicked()
                        {
                            self.commit(shared);
                        }
                        if ui
                            .add_enabled(
                                !self.busy,
                                egui::Button::new(if self.prepared.is_some() {
                                    "Verify again"
                                } else {
                                    "Verify"
                                }),
                            )
                            .clicked()
                        {
                            self.prepare(shared);
                        }
                        if ui.add_enabled(!self.busy, egui::Button::new("Back")).clicked() {
                            if let Some(p) = self.prepared.take() {
                                teaching::abandon(p);
                            }
                            self.step = Step::Pixels;
                        }
                    }
                });
            });
        });
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        close
    }

    fn steps_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (i, (step, label)) in [
                (Step::Case, "1  Case"),
                (Step::Pixels, "2  Pixels"),
                (Step::Verify, "3  Verify and write"),
            ]
            .iter()
            .enumerate()
            {
                let active = *step == self.step;
                let text = RichText::new(*label).size(13.0).color(if active {
                    Color32::from_gray(20)
                } else {
                    Color32::from_gray(140)
                });
                ui.label(if active { text.strong() } else { text });
                if i < 2 {
                    ui.label(RichText::new("›").color(Color32::from_gray(160)));
                }
            }
        });
    }

    // ------------------------------------------------------------- step 1

    fn case_valid(&self) -> Option<String> {
        if self.existing.is_none() {
            if !teaching::valid_label(&self.reference, false) {
                return Some("Reference: letters, digits, spaces, hyphens and apostrophes, up to 64 characters".into());
            }
            if !teaching::valid_label(&self.name, true) {
                return Some("Name: letters, digits, spaces, hyphens, apostrophes and ^, up to 64 characters".into());
            }
            if self
                .cases
                .iter()
                .any(|c| c.reference.eq_ignore_ascii_case(self.reference.trim()))
            {
                return Some("That reference exists; choose it from the existing cases to add this study to it".into());
            }
            if self.cases.iter().any(|c| c.name.eq_ignore_ascii_case(self.name.trim())) {
                return Some("That teaching name is already used by another case".into());
            }
            if let Some(t) = conflicts_with_tokens_text(&self.name, &self.ctx) {
                return Some(format!("The name contains \"{t}\", which identifies the source"));
            }
            if let Some(t) = conflicts_with_tokens_text(&self.reference, &self.ctx) {
                return Some(format!("The reference contains \"{t}\", which identifies the source"));
            }
        }
        if let Some(t) = conflicts_with_tokens_text(&self.title, &self.ctx) {
            return Some(format!("The title contains \"{t}\", which identifies the source"));
        }
        if !self.to_archive && !self.to_folder {
            return Some("Choose a destination".into());
        }
        if self.to_folder && self.folder.is_none() {
            return Some("Choose the folder to write to".into());
        }
        None
    }

    fn case_step(&mut self, ui: &mut Ui, shared: &Shared) {
        let hint = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).size(12.0).color(Color32::from_gray(110)));
        };
        menu_heading(ui, "Teaching case");
        ui.horizontal(|ui| {
            if ui.radio(self.existing.is_none(), "New case").clicked() {
                self.existing = None;
            }
            let label = self
                .existing
                .and_then(|id| self.cases.iter().find(|c| c.id == id))
                .map(|c| format!("{}  ({})", c.name.replace('^', " "), c.reference));
            ui.add_enabled_ui(!self.cases.is_empty(), |ui| {
                if ui.radio(self.existing.is_some(), "Add to existing case").clicked() && self.existing.is_none() {
                    self.existing = self.cases.first().map(|c| c.id);
                }
                egui::ComboBox::from_id_salt("anon-existing")
                    .selected_text(label.unwrap_or_else(|| "Choose a case".into()))
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for c in &self.cases {
                            ui.selectable_value(
                                &mut self.existing,
                                Some(c.id),
                                format!("{}  ({})", c.name.replace('^', " "), c.reference),
                            );
                        }
                    });
            });
        });
        if self.existing.is_some() {
            hint(ui, "The study joins that case: same teaching name, same reference, same date offset, so follow-ups keep their intervals. Only do this for the same animal.");
        } else {
            ui.horizontal(|ui| {
                ui.add_sized([110.0, 22.0], egui::Label::new("Teaching name"));
                ui.add_sized([280.0, 24.0], egui::TextEdit::singleline(&mut self.name));
                if ui.button("Suggest another").clicked() {
                    let taken: Vec<String> = self.cases.iter().map(|c| c.name.clone()).collect();
                    self.name = teaching::suggest_name(self.study.patient_species.as_deref(), &taken, &self.ctx);
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized([110.0, 22.0], egui::Label::new("Reference"));
                ui.add_sized([280.0, 24.0], egui::TextEdit::singleline(&mut self.reference));
                ui.label(
                    RichText::new("groups studies into a case; set the prefix in Preferences")
                        .size(11.5)
                        .color(Color32::from_gray(120)),
                );
            });
            hint(ui, "The name replaces the patient name (owner surname ^ animal name); the reference replaces the patient ID.");
        }
        ui.horizontal(|ui| {
            ui.add_sized([110.0, 22.0], egui::Label::new("Teaching title"));
            ui.add_sized(
                [480.0, 24.0],
                egui::TextEdit::singleline(&mut self.title)
                    .hint_text("for example: Aortic regurgitation, 14-year-old Warmblood"),
            );
        });
        ui.add_space(8.0);

        menu_heading(ui, "Destination");
        ui.checkbox(&mut self.to_archive, "This archive, in the Teaching album");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.to_folder, "A folder");
            if self.to_folder {
                if ui.button("Choose…").clicked() {
                    if let Some(d) = rfd::FileDialog::new()
                        .set_title("Folder for the teaching copy")
                        .pick_folder()
                    {
                        self.folder = Some(d);
                    }
                }
                if let Some(f) = &self.folder {
                    ui.label(RichText::new(f.display().to_string()).size(12.0));
                }
            }
        });
        ui.add_space(8.0);

        menu_heading(ui, "Options");
        ui.horizontal(|ui| {
            ui.label("Measurements");
            ui.radio_value(&mut self.measurements, MeasurementTreatment::Include, "Include");
            ui.radio_value(&mut self.measurements, MeasurementTreatment::AnswerKey, "Answer key");
            ui.radio_value(&mut self.measurements, MeasurementTreatment::LeaveOut, "Leave out");
        });
        hint(
            ui,
            match self.measurements {
                MeasurementTreatment::Include => "Measurements appear as they do here.",
                MeasurementTreatment::AnswerKey => "Carried but hidden until revealed in the viewer. Not a protection: the values are in the file. For assessment, leave them out.",
                MeasurementTreatment::LeaveOut => "No measurements travel; only calibrations do.",
            },
        );
        ui.checkbox(&mut self.include_report, "Include the report's findings and conclusion");
        if self.include_report {
            hint(ui, "Read them: free text is where owners, practices and places hide. Whatever you include is still checked for known tokens.");
            ui.add(
                egui::TextEdit::multiline(&mut self.findings_text)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("Findings"),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.conclusion_text)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("Conclusion"),
            );
        }
        ui.add_enabled_ui(self.existing.is_none(), |ui| {
            ui.checkbox(
                &mut self.preserve_season,
                "Preserve season (shift dates by whole years)",
            );
        });
        if self.preserve_season {
            ui.label(RichText::new("Day and month stay real. Combined with breed, age and region this narrows identification; use only when the season matters to the case.").size(12.0).color(Color32::from_rgb(170, 90, 0)));
        }
        let _ = shared;
        if let Some(problem) = self.case_valid() {
            ui.add_space(6.0);
            ui.label(RichText::new(problem).color(Color32::from_rgb(190, 40, 40)));
        }
    }

    // ------------------------------------------------------------- step 2

    fn ensure_thumb(&mut self, shared: &Shared, ctx: &Context, idx: usize) {
        if self.series[idx].thumb.is_some() {
            return;
        }
        let series_id = self.series[idx].series.id;
        if let Some((tex, w, h)) = load_frame_texture(shared, ctx, series_id, 0.0, &format!("anon-thumb-{series_id}")) {
            self.series[idx].thumb = Some((tex, w, h));
        }
    }

    fn pixels_step(&mut self, ui: &mut Ui, shared: &Shared) {
        if self.review.is_some() {
            self.review_view(ui, shared);
            return;
        }
        ui.label(RichText::new("Each series leaves only when its pixels are masked, trusted or reviewed. Anything the rules cannot classify stops here.").size(12.5).color(Color32::from_gray(110)));
        ui.add_space(6.0);
        let ctx = ui.ctx().clone();
        let n = self.series.len();
        let mut open_review: Option<usize> = None;
        for i in 0..n {
            self.ensure_thumb(shared, &ctx, i);
            let (status, color) = {
                let s = &self.series[i];
                match (&s.class, s.approved) {
                    (SeriesClassification::Regions(_), _) => (
                        "Masked by ultrasound regions (check the header band)",
                        Color32::from_rgb(30, 130, 60),
                    ),
                    (SeriesClassification::RegionsWithTemplate { .. }, true) => {
                        ("Regions and device template, checked", Color32::from_rgb(30, 130, 60))
                    }
                    (SeriesClassification::RegionsWithTemplate { .. }, false) => (
                        "Regions and device template: check the frames",
                        Color32::from_rgb(190, 120, 0),
                    ),
                    (SeriesClassification::Trusted, _) => {
                        ("Trusted: Burned In Annotation is NO", Color32::from_rgb(30, 130, 60))
                    }
                    (SeriesClassification::Template { .. }, true) => {
                        ("Device template, checked", Color32::from_rgb(30, 130, 60))
                    }
                    (SeriesClassification::Template { .. }, false) => {
                        ("Device template: check the frames", Color32::from_rgb(190, 120, 0))
                    }
                    (SeriesClassification::NeedsReview, true) => ("Reviewed", Color32::from_rgb(30, 130, 60)),
                    (SeriesClassification::NeedsReview, false) => ("Needs review", Color32::from_rgb(190, 40, 40)),
                }
            };
            ui.horizontal(|ui| {
                let s = &self.series[i];
                let (rect, _) = ui.allocate_exact_size(Vec2::new(150.0, 100.0), Sense::hover());
                ui.painter().rect_filled(rect, 3.0, Color32::BLACK);
                if let Some((tex, w, h)) = &s.thumb {
                    let fit = fit_rect(rect.shrink(2.0), *w, *h);
                    ui.painter().image(
                        tex.id(),
                        fit,
                        ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if let Some(d) = &s.decision {
                        draw_mask_overlay(ui.painter(), fit, *w, *h, d);
                    }
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(s.series.display_name()).strong());
                    ui.label(
                        RichText::new(format!("{} · {}", s.series.modality, s.series.count_label()))
                            .size(12.0)
                            .color(Color32::from_gray(110)),
                    );
                    ui.label(RichText::new(status).color(color));
                    if ui.button("Review…").clicked() {
                        open_review = Some(i);
                    }
                });
            });
            ui.separator();
        }
        if let Some(i) = open_review {
            self.open_review(shared, &ctx, i);
        }
    }

    fn open_review(&mut self, shared: &Shared, ctx: &Context, idx: usize) {
        let series_id = self.series[idx].series.id;
        let mut frames = Vec::new();
        for (label, pos) in [("First", 0.0), ("Middle", 0.5), ("Last", 1.0)] {
            if let Some((tex, w, h)) =
                load_frame_texture(shared, ctx, series_id, pos, &format!("anon-review-{series_id}-{label}"))
            {
                frames.push((label.to_string(), tex, w, h));
            }
        }
        let rects = match &self.series[idx].decision {
            Some(MaskDecision::Template { rects, .. }) | Some(MaskDecision::Reviewed(rects)) => rects.clone(),
            Some(MaskDecision::RegionsReviewed { blank, .. }) => blank.clone(),
            _ => Vec::new(),
        };
        self.review = Some(Review {
            idx,
            frames,
            rects,
            drag: None,
            save_template: false,
        });
    }

    fn review_view(&mut self, ui: &mut Ui, shared: &Shared) {
        let Some(review) = self.review.as_mut() else { return };
        let s = &self.series[review.idx];
        ui.label(RichText::new(format!("Review: {}", s.series.display_name())).strong());
        ui.label(RichText::new("Drag on the first frame to add a rectangle over anything that identifies the owner or the practice. Rectangles are blanked on every frame.").size(12.5).color(Color32::from_gray(110)));
        let keep_regions = matches!(
            s.class,
            SeriesClassification::Regions(_) | SeriesClassification::RegionsWithTemplate { .. }
        );
        let region_keep: Vec<Rect> = match &s.class {
            SeriesClassification::Regions(k) | SeriesClassification::RegionsWithTemplate { keep: k, .. } => k.clone(),
            _ => Vec::new(),
        };
        ui.horizontal(|ui| {
            for (k, (label, tex, w, h)) in review.frames.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label.as_str()).size(12.0));
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(270.0, 200.0),
                        if k == 0 {
                            Sense::click_and_drag()
                        } else {
                            Sense::hover()
                        },
                    );
                    ui.painter().rect_filled(rect, 3.0, Color32::BLACK);
                    let fit = fit_rect(rect.shrink(2.0), *w, *h);
                    ui.painter().image(
                        tex.id(),
                        fit,
                        ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    let plan = if keep_regions {
                        MaskDecision::RegionsReviewed {
                            keep: region_keep.clone(),
                            blank: review.rects.clone(),
                        }
                    } else {
                        MaskDecision::Reviewed(review.rects.clone())
                    };
                    draw_mask_overlay(ui.painter(), fit, *w, *h, &plan);
                    if k == 0 {
                        let scale = fit.width() / *w as f32;
                        if response.drag_started() {
                            if let Some(p) = response.interact_pointer_pos() {
                                review.drag = Some((p, p));
                            }
                        }
                        if response.dragged() {
                            if let (Some((a, _)), Some(p)) = (review.drag, response.interact_pointer_pos()) {
                                review.drag = Some((a, p));
                            }
                        }
                        if let Some((a, b)) = review.drag {
                            ui.painter().rect_stroke(
                                ERect::from_two_pos(a, b),
                                0.0,
                                Stroke::new(1.5, Color32::from_rgb(255, 80, 80)),
                                egui::StrokeKind::Middle,
                            );
                        }
                        if response.drag_stopped() {
                            if let Some((a, b)) = review.drag.take() {
                                let to_px = |p: Pos2| {
                                    let x = ((p.x - fit.left()) / scale)
                                        .round()
                                        .clamp(0.0, (*w as f32 - 1.0).max(0.0))
                                        as u32;
                                    let y = ((p.y - fit.top()) / scale)
                                        .round()
                                        .clamp(0.0, (*h as f32 - 1.0).max(0.0))
                                        as u32;
                                    (x, y)
                                };
                                let (x0, y0) = to_px(a);
                                let (x1, y1) = to_px(b);
                                let r = Rect { x0, y0, x1, y1 }.normalised();
                                if r.x1 > r.x0 + 1 && r.y1 > r.y0 + 1 {
                                    review.rects.push(r);
                                }
                            }
                        }
                    }
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            let mut remove = None;
            for (i, r) in review.rects.iter().enumerate() {
                if ui
                    .small_button(format!(
                        "✕ {}×{} at {},{}",
                        r.x1 - r.x0 + 1,
                        r.y1 - r.y0 + 1,
                        r.x0,
                        r.y0
                    ))
                    .clicked()
                {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                review.rects.remove(i);
            }
            if review.rects.is_empty() {
                ui.label(
                    RichText::new(if keep_regions {
                        "No extra rectangles: only the regions are kept"
                    } else {
                        "No rectangles yet"
                    })
                    .size(12.0)
                    .color(Color32::from_gray(120)),
                );
            }
        });
        if keep_regions {
            ui.label(RichText::new("Some machines put the patient band inside their region. If a name shows at the top, drag a rectangle over it and save the template so the next study from this machine is masked without review.").size(12.0).color(Color32::from_rgb(170, 90, 0)));
        }
        if !s.manufacturer.is_empty() {
            ui.checkbox(
                &mut review.save_template,
                format!(
                    "Save as the template for {} {} ({}×{})",
                    s.manufacturer, s.model, s.columns, s.rows
                ),
            );
        }
        let mut approve: Option<(usize, Vec<Rect>, bool)> = None;
        let mut back = false;
        ui.horizontal(|ui| {
            if ui.button("Approve").clicked() {
                approve = Some((review.idx, review.rects.clone(), review.save_template));
            }
            if ui.button("Back to list").clicked() {
                back = true;
            }
        });
        if let Some((idx, rects, save)) = approve {
            let st = &mut self.series[idx];
            if keep_regions {
                st.decision = Some(if rects.is_empty() {
                    MaskDecision::Regions(region_keep.clone())
                } else {
                    MaskDecision::RegionsReviewed {
                        keep: region_keep.clone(),
                        blank: rects.clone(),
                    }
                });
            } else {
                st.decision = Some(MaskDecision::Reviewed(rects.clone()));
            }
            if save && !rects.is_empty() {
                let t = MaskTemplate {
                    manufacturer: st.manufacturer.clone(),
                    model: st.model.clone(),
                    columns: st.columns,
                    rows: st.rows,
                    rects,
                };
                let _ = teaching::save_template(&shared.paths, &t);
            }
            st.approved = true;
            self.review = None;
        } else if back {
            self.review = None;
        }
    }

    /// Developer aid: jump to a step at startup (`pixels`, `verify` runs the pass).
    pub fn dev_jump(&mut self, shared: &Shared, step: &str) {
        match step {
            "pixels" => self.step = Step::Pixels,
            "verify" => {
                self.step = Step::Verify;
                self.prepare(shared);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- step 3

    fn request(&self) -> ExportRequest {
        let case = match self.existing {
            Some(id) => CaseChoice::Existing(id),
            None => CaseChoice::New(NewCase {
                reference: self.reference.trim().to_string(),
                name: self.name.trim().to_string(),
                title: self.title.trim().to_string(),
                preserve_season: self.preserve_season,
            }),
        };
        let destination = match (self.to_archive, self.to_folder, &self.folder) {
            (true, true, Some(f)) => Destination::Both(f.clone()),
            (false, true, Some(f)) => Destination::Folder(f.clone()),
            _ => Destination::Archive,
        };
        ExportRequest {
            study_id: self.study.id,
            case,
            title: if self.title.trim().is_empty() {
                None
            } else {
                Some(self.title.trim().to_string())
            },
            destination,
            measurements: self.measurements,
            report_text: if self.include_report {
                Some((self.findings_text.clone(), self.conclusion_text.clone()))
            } else {
                None
            },
            masks: self
                .series
                .iter()
                .filter_map(|s| s.decision.clone().map(|d| (s.series.id, d)))
                .collect(),
            overrides: self.overrides.iter().map(|(p, r)| (p.clone(), r.clone())).collect(),
            blank_attributes: self.blank.clone(),
        }
    }

    fn prepare(&mut self, shared: &Shared) {
        if let Some(p) = self.prepared.take() {
            teaching::abandon(p);
        }
        self.error = None;
        let (tx, rx) = unbounded();
        let db = shared.db.clone();
        let paths = shared.paths.clone();
        let request = self.request();
        self.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.busy = true;
        self.writing = false;
        self.rx = Some(rx);
        std::thread::Builder::new()
            .name("ruthos-anonymise".into())
            .spawn(move || {
                let ptx = tx.clone();
                // A fault on this thread must reach the dialog, or it would
                // wait for ever.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    teaching::prepare_cancellable(
                        &db,
                        &paths,
                        request,
                        &|done, total, what| {
                            let _ = ptx.send(Job::Progress(done, total, what.to_string()));
                        },
                        &cancel,
                    )
                    .map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| Err("an internal error stopped the copy; see crash.log".into()));
                let _ = tx.send(Job::Prepared(result));
            })
            .ok();
    }

    fn commit(&mut self, shared: &Shared) {
        let Some(prepared) = self.prepared.take() else { return };
        let (tx, rx) = unbounded();
        let db = shared.db.clone();
        let paths = shared.paths.clone();
        self.busy = true;
        self.writing = true;
        self.rx = Some(rx);
        std::thread::Builder::new()
            .name("ruthos-anonymise-write".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    teaching::commit(&db, &paths, prepared).map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| Err("an internal error stopped the write; see crash.log".into()));
                let _ = tx.send(Job::Committed(result));
            })
            .ok();
    }

    fn pump(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Job::Progress(a, b, what) => self.progress = (a, b, what),
                Job::Prepared(Ok(p)) => {
                    self.prepared = Some(p);
                    self.busy = false;
                    done = true;
                }
                Job::Prepared(Err(e)) => {
                    if e != "cancelled" {
                        self.error = Some(e);
                    }
                    self.busy = false;
                    done = true;
                }
                Job::Committed(Ok(r)) => {
                    self.result = Some(r);
                    self.busy = false;
                    done = true;
                }
                Job::Committed(Err(e)) => {
                    self.error = Some(e);
                    self.busy = false;
                    done = true;
                }
            }
        }
        if done {
            self.rx = None;
        }
    }

    fn verify_step(&mut self, ui: &mut Ui, _shared: &Shared) {
        if let Some(r) = &self.result {
            ui.label(
                RichText::new("Written")
                    .size(16.0)
                    .strong()
                    .color(Color32::from_rgb(30, 130, 60)),
            );
            egui::Grid::new("anon-result")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Reference");
                    ui.label(&r.reference);
                    ui.end_row();
                    ui.label("Series / images");
                    ui.label(format!("{} / {}", r.series, r.images));
                    ui.end_row();
                    ui.label("Destination");
                    ui.label(match (&r.folder, r.archive_study_id) {
                        (Some(f), Some(_)) => format!("this archive and {}", f.display()),
                        (Some(f), None) => f.display().to_string(),
                        (None, _) => "this archive (Teaching album)".to_string(),
                    });
                    ui.end_row();
                    ui.label("Export log");
                    ui.label(r.log_path.display().to_string());
                    ui.end_row();
                });
            return;
        }
        if self.busy {
            let (a, b, what) = &self.progress;
            let frac = if *b > 0 { *a as f32 / *b as f32 } else { 0.0 };
            ui.add(egui::ProgressBar::new(frac).text(if what.is_empty() {
                "Working…".to_string()
            } else {
                what.clone()
            }));
            return;
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(format!("Could not prepare the copy: {e}")).color(Color32::from_rgb(190, 40, 40)));
            ui.add_space(6.0);
        }
        match &self.prepared {
            None => {
                ui.label("Verify builds the whole copy in a temporary folder, reads every file back and checks it. Nothing reaches the destination until the pass is clean and you press Write.");
            }
            Some(p) if p.is_clean() => {
                ui.label(
                    RichText::new(format!(
                        "Clean: {} series, {} images verified.",
                        p.series_count, p.image_count
                    ))
                    .color(Color32::from_rgb(30, 130, 60)),
                );
                if !p.overridden.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "{} descriptor hit(s) accepted with a reason, recorded in the log.",
                            p.overridden.len()
                        ))
                        .size(12.0)
                        .color(Color32::from_gray(110)),
                    );
                }
                ui.label("Press Write to move the copy into place.");
            }
            Some(p) => {
                ui.label(
                    RichText::new(format!("{} problem(s). The copy was not written.", p.findings.len()))
                        .color(Color32::from_rgb(190, 40, 40)),
                );
                let findings = p.findings.clone();
                let mut rerun = false;
                for f in findings {
                    ui.horizontal_wrapped(|ui| {
                        let file = std::path::Path::new(&f.file)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!(
                                "{} {}",
                                f.path,
                                if file.is_empty() {
                                    String::new()
                                } else {
                                    format!("in {file}")
                                }
                            ))
                            .strong(),
                        );
                        ui.label(format!("{}: \"{}\"", f.what, f.matched));
                        if f.overridable {
                            if ui.small_button("Blank this attribute").clicked() {
                                if !self.blank.contains(&f.path) {
                                    self.blank.push(f.path.clone());
                                }
                                rerun = true;
                            }
                            let reason = self.overrides.entry(f.path.clone()).or_default();
                            ui.add(
                                egui::TextEdit::singleline(reason)
                                    .hint_text("accept: give the reason")
                                    .desired_width(220.0),
                            );
                        } else {
                            ui.label(
                                RichText::new("cannot be overridden")
                                    .size(12.0)
                                    .color(Color32::from_gray(110)),
                            );
                        }
                    });
                }
                self.overrides.retain(|_, r| !r.trim().is_empty());
                ui.label(
                    RichText::new("Accepted hits with a reason, and blanked attributes, apply when you verify again.")
                        .size(12.0)
                        .color(Color32::from_gray(110)),
                );
                if rerun {
                    // Handled by the Verify again button; nothing else to do here.
                }
            }
        }
    }
}

/// Decode one frame of a series at a relative position (0 first, 1 last)
/// and upload it as a texture.
fn load_frame_texture(
    shared: &Shared,
    ctx: &Context,
    series_id: i64,
    position: f32,
    name: &str,
) -> Option<(TextureHandle, u32, u32)> {
    let images = shared.db.images_for_series(series_id).ok()?;
    if images.is_empty() {
        return None;
    }
    let (image, frame) = if images.len() == 1 {
        let n = images[0].number_of_frames.max(1) as u32;
        (&images[0], ((n - 1) as f32 * position).round() as u32)
    } else {
        let i = ((images.len() - 1) as f32 * position).round() as usize;
        (&images[i], 0u32)
    };
    let frame = ruthos_dicom::decode_frame(&shared.resolve(&image.path), frame).ok()?;
    let (c, w) = frame.default_window();
    let rgb = ruthos_dicom::pixels::frame_to_rgb8(&frame, c, w, &ruthos_dicom::Clut::grayscale());
    let img = egui::ColorImage::from_rgb([frame.width as usize, frame.height as usize], &rgb);
    let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
    Some((tex, frame.width, frame.height))
}

fn fit_rect(area: ERect, w: u32, h: u32) -> ERect {
    if w == 0 || h == 0 {
        return area;
    }
    let scale = (area.width() / w as f32).min(area.height() / h as f32);
    let size = Vec2::new(w as f32 * scale, h as f32 * scale);
    ERect::from_center_size(area.center(), size)
}

/// Translucent red over what will be blanked.
fn draw_mask_overlay(painter: &egui::Painter, fit: ERect, w: u32, h: u32, decision: &MaskDecision) {
    let scale = fit.width() / w.max(1) as f32;
    let to_screen = |r: &Rect| {
        ERect::from_min_max(
            Pos2::new(fit.left() + r.x0 as f32 * scale, fit.top() + r.y0 as f32 * scale),
            Pos2::new(
                fit.left() + (r.x1 + 1) as f32 * scale,
                fit.top() + (r.y1 + 1) as f32 * scale,
            ),
        )
    };
    match decision {
        MaskDecision::Regions(keep) => {
            // Red everywhere, then the kept regions drawn clear by outlining.
            painter.rect_filled(fit, 0.0, RED);
            for r in keep {
                let sr = to_screen(r).intersect(fit);
                painter.rect_filled(sr, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 0));
                painter.rect_stroke(
                    sr,
                    0.0,
                    Stroke::new(1.5, Color32::from_rgb(80, 220, 120)),
                    egui::StrokeKind::Inside,
                );
            }
            let _ = h;
        }
        MaskDecision::RegionsReviewed { keep, blank } => {
            painter.rect_filled(fit, 0.0, RED);
            for r in keep {
                let sr = to_screen(r).intersect(fit);
                painter.rect_filled(sr, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 0));
                painter.rect_stroke(
                    sr,
                    0.0,
                    Stroke::new(1.5, Color32::from_rgb(80, 220, 120)),
                    egui::StrokeKind::Inside,
                );
            }
            for r in blank {
                painter.rect_filled(to_screen(r).intersect(fit), 0.0, RED);
            }
        }
        MaskDecision::Template { rects, .. } | MaskDecision::Reviewed(rects) => {
            for r in rects {
                painter.rect_filled(to_screen(r).intersect(fit), 0.0, RED);
            }
        }
        MaskDecision::Trusted => {}
    }
}
