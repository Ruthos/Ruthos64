//! Study reports: the text the clinician writes plus the named measurements,
//! laid out as a PDF.
//!
//! The layout is deliberately plain: a header with the patient, a table of
//! measurements, findings, conclusion, then the key images two to a row.
//! Everything is A4 with built-in Helvetica so the file needs no fonts.

use std::io::BufWriter;
use std::path::Path;

use printpdf::{
    BuiltinFont, Color, ColorBits, ColorSpace, Image, ImageTransform, ImageXObject, IndirectFontRef, Line, Mm,
    PdfDocument, PdfDocumentReference, PdfLayerReference, Point, Px, Rgb,
};

/// One measurement line in the report.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportMeasurement {
    pub name: String,
    pub value: String,
    /// Where it was taken: series and image.
    pub location: String,
    /// The scale basis in words; empty when not applicable.
    pub basis: String,
    /// False for detector-plane and uncalibrated values, which get a footnote.
    pub patient_scale: bool,
}

/// A rendered key image, 8-bit RGB.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportImage {
    pub caption: String,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReportDocument {
    pub title: String,
    /// Label and value pairs for the header block.
    pub header: Vec<(String, String)>,
    pub measurements: Vec<ReportMeasurement>,
    /// The LV M-mode block: paired diastolic and systolic values and FS.
    pub mmode: Vec<ReportMeasurement>,
    pub findings: String,
    pub conclusion: String,
    pub images: Vec<ReportImage>,
    pub footer: String,
}

/// Format a stored measurement value the way the viewer shows it.
pub fn format_value(value: Option<f64>, unit: &str) -> String {
    match (value, unit) {
        (Some(mm), "mm") if mm >= 10.0 => format!("{:.2} cm", mm / 10.0),
        (Some(mm), "mm") => format!("{mm:.2} mm"),
        (Some(deg), "deg") => format!("{deg:.1}\u{00b0}"),
        (Some(a), "mm2") if a >= 100.0 => format!("{:.2} cm\u{00b2}", a / 100.0),
        (Some(a), "mm2") => format!("{a:.1} mm\u{00b2}"),
        (Some(px), "px") => format!("{px:.1} px (not calibrated)"),
        (Some(v), "") => format!("{v:.0}"),
        (Some(v), u) => format!("{v:.2} {u}"),
        (None, _) => String::from("\u{2014}"),
    }
}

const PAGE_W: f32 = 210.0;
const PAGE_H: f32 = 297.0;
const MARGIN: f32 = 18.0;
const TEXT_W: f32 = PAGE_W - 2.0 * MARGIN;

struct Writer {
    doc: PdfDocumentReference,
    layer: PdfLayerReference,
    regular: IndirectFontRef,
    bold: IndirectFontRef,
    y: f32,
    page_no: usize,
    footer: String,
}

impl Writer {
    fn new(title: &str, footer: &str) -> Self {
        let (doc, page, layer) = PdfDocument::new(title, Mm(PAGE_W), Mm(PAGE_H), "Page 1");
        let regular = doc.add_builtin_font(BuiltinFont::Helvetica).expect("builtin font");
        let bold = doc.add_builtin_font(BuiltinFont::HelveticaBold).expect("builtin font");
        let layer = doc.get_page(page).get_layer(layer);
        let mut w = Self { doc, layer, regular, bold, y: PAGE_H - MARGIN, page_no: 1, footer: footer.to_string() };
        w.footer();
        w
    }

    fn footer(&mut self) {
        self.layer.set_fill_color(Color::Rgb(Rgb::new(0.45, 0.45, 0.45, None)));
        self.layer.use_text(&self.footer, 8.0, Mm(MARGIN), Mm(10.0), &self.regular);
        let page = format!("Page {}", self.page_no);
        self.layer.use_text(&page, 8.0, Mm(PAGE_W - MARGIN - 14.0), Mm(10.0), &self.regular);
        self.layer.set_fill_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
    }

    fn new_page(&mut self) {
        self.page_no += 1;
        let (page, layer) = self.doc.add_page(Mm(PAGE_W), Mm(PAGE_H), format!("Page {}", self.page_no));
        self.layer = self.doc.get_page(page).get_layer(layer);
        self.y = PAGE_H - MARGIN;
        self.footer();
    }

    fn need(&mut self, height: f32) {
        if self.y - height < MARGIN + 6.0 {
            self.new_page();
        }
    }

    fn text(&mut self, text: &str, size: f32, bold: bool) {
        let line_h = size * 0.42;
        for line in wrap(text, TEXT_W, size) {
            self.need(line_h);
            let font = if bold { &self.bold } else { &self.regular };
            self.layer.use_text(line, size, Mm(MARGIN), Mm(self.y - line_h * 0.8), font);
            self.y -= line_h;
        }
    }

    fn gap(&mut self, mm: f32) {
        self.y -= mm;
    }

    fn rule(&mut self) {
        self.need(2.0);
        self.layer.set_outline_color(Color::Rgb(Rgb::new(0.6, 0.6, 0.6, None)));
        self.layer.set_outline_thickness(0.4);
        self.layer.add_line(Line {
            points: vec![
                (Point::new(Mm(MARGIN), Mm(self.y)), false),
                (Point::new(Mm(PAGE_W - MARGIN), Mm(self.y)), false),
            ],
            is_closed: false,
        });
        self.y -= 2.0;
    }

    fn heading(&mut self, text: &str) {
        self.gap(3.0);
        self.text(text, 12.0, true);
        self.gap(1.0);
    }

    fn columns(&mut self, cells: &[(&str, f32)], size: f32, bold: bool) {
        let line_h = size * 0.42;
        self.need(line_h);
        let font = if bold { &self.bold } else { &self.regular };
        let mut x = MARGIN;
        for (text, width) in cells {
            let shown = truncate_to(text, *width - 2.0, size);
            self.layer.use_text(shown, size, Mm(x), Mm(self.y - line_h * 0.8), font);
            x += width;
        }
        self.y -= line_h;
    }

    fn image(&mut self, img: &ReportImage, x: f32, width_mm: f32) -> f32 {
        if img.width == 0 || img.height == 0 || img.rgb.len() != (img.width * img.height * 3) as usize {
            return 0.0;
        }
        let height_mm = width_mm * img.height as f32 / img.width as f32;
        // At 300 dpi one pixel is 25.4/300 mm; scale to the wanted width.
        let dpi = 300.0;
        let natural_w_mm = img.width as f32 * 25.4 / dpi;
        let scale = width_mm / natural_w_mm;
        let xobject = ImageXObject {
            width: Px(img.width as usize),
            height: Px(img.height as usize),
            color_space: ColorSpace::Rgb,
            bits_per_component: ColorBits::Bit8,
            interpolate: true,
            image_data: img.rgb.clone(),
            image_filter: None,
            smask: None,
            clipping_bbox: None,
        };
        Image { image: xobject }.add_to_layer(
            self.layer.clone(),
            ImageTransform {
                translate_x: Some(Mm(x)),
                translate_y: Some(Mm(self.y - height_mm)),
                scale_x: Some(scale),
                scale_y: Some(scale),
                dpi: Some(dpi),
                ..Default::default()
            },
        );
        height_mm
    }
}

/// Approximate Helvetica width of a string in millimetres.
fn text_width_mm(text: &str, size: f32) -> f32 {
    let pt: f32 = text
        .chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | 't' | 'f' | 'I' | '.' | ',' | ':' | ';' | '\'' | '|' | ' ' => 0.28,
            'm' | 'w' | 'M' | 'W' => 0.83,
            c if c.is_ascii_uppercase() => 0.68,
            c if c.is_ascii_digit() => 0.56,
            _ => 0.54,
        })
        .sum::<f32>()
        * size;
    pt * 25.4 / 72.0
}

fn wrap(text: &str, width_mm: f32, size: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if text_width_mm(&candidate, size) > width_mm && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

fn truncate_to(text: &str, width_mm: f32, size: f32) -> String {
    if text_width_mm(text, size) <= width_mm {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        out.push(c);
        if text_width_mm(&format!("{out}\u{2026}"), size) > width_mm {
            out.pop();
            out.push('\u{2026}');
            return out;
        }
    }
    out
}

/// Write the report as a PDF file.
pub fn write_pdf(report: &ReportDocument, path: &Path) -> std::io::Result<()> {
    let mut w = Writer::new(&report.title, &report.footer);

    w.text(&report.title, 18.0, true);
    w.gap(2.0);
    for (label, value) in &report.header {
        w.columns(&[(label.as_str(), 42.0), (value.as_str(), TEXT_W - 42.0)], 10.0, false);
    }
    w.gap(2.0);
    w.rule();

    if !report.measurements.is_empty() {
        w.heading("Measurements");
        w.columns(&[("Measurement", 56.0), ("Value", 36.0), ("Basis", 44.0), ("Image", TEXT_W - 136.0)], 9.5, true);
        w.gap(0.5);
        let mut flagged = false;
        for m in &report.measurements {
            let value = if m.patient_scale { m.value.clone() } else { format!("{} *", m.value) };
            flagged |= !m.patient_scale;
            w.columns(&[(m.name.as_str(), 56.0), (value.as_str(), 36.0), (m.basis.as_str(), 44.0), (m.location.as_str(), TEXT_W - 136.0)], 10.0, false);
        }
        if flagged {
            w.gap(1.0);
            w.text("* Measured at the detector plane or uncalibrated: the value is not the size of the structure in the patient. Detector-plane values overstate it by the radiographic magnification. Ratios of such values are unaffected.", 8.5, false);
        }
    }

    if !report.mmode.is_empty() {
        w.heading("LV M-mode");
        w.columns(&[("Measurement", 56.0), ("Value", 36.0), ("Basis", 44.0), ("Image", TEXT_W - 136.0)], 9.5, true);
        w.gap(0.5);
        for m in &report.mmode {
            w.columns(&[(m.name.as_str(), 56.0), (m.value.as_str(), 36.0), (m.basis.as_str(), 44.0), (m.location.as_str(), TEXT_W - 136.0)], 10.0, false);
        }
    }

    if !report.findings.trim().is_empty() {
        w.heading("Findings");
        w.text(report.findings.trim(), 10.5, false);
    }
    if !report.conclusion.trim().is_empty() {
        w.heading("Conclusion");
        w.text(report.conclusion.trim(), 10.5, false);
    }

    if !report.images.is_empty() {
        w.heading("Key images");
        let gap = 6.0;
        let col_w = (TEXT_W - gap) / 2.0;
        let mut i = 0;
        while i < report.images.len() {
            let row: Vec<&ReportImage> = report.images[i..(i + 2).min(report.images.len())].iter().collect();
            let row_h = row
                .iter()
                .map(|im| if im.width > 0 { col_w * im.height as f32 / im.width as f32 } else { 0.0 })
                .fold(0.0_f32, f32::max);
            w.need(row_h + 8.0);
            let top = w.y;
            for (k, im) in row.iter().enumerate() {
                let x = MARGIN + k as f32 * (col_w + gap);
                w.y = top;
                let h = w.image(im, x, col_w);
                w.layer.use_text(&im.caption, 8.5, Mm(x), Mm(top - h - 3.5), &w.regular);
            }
            w.y = top - row_h - 7.0;
            i += 2;
        }
    }

    let file = std::fs::File::create(path)?;
    w.doc.save(&mut BufWriter::new(file)).map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_format_like_the_viewer() {
        assert_eq!(format_value(Some(42.0), "mm"), "4.20 cm");
        assert_eq!(format_value(Some(4.2), "mm"), "4.20 mm");
        assert_eq!(format_value(Some(31.26), "deg"), "31.3\u{00b0}");
        assert_eq!(format_value(Some(250.0), "mm2"), "2.50 cm\u{00b2}");
        assert_eq!(format_value(None, "mm"), "\u{2014}");
    }

    #[test]
    fn wraps_long_text() {
        let lines = wrap("one two three four five six seven eight nine ten", 20.0, 10.0);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| text_width_mm(l, 10.0) <= 20.0 || !l.contains(' ')));
    }

    #[test]
    fn writes_a_pdf() {
        let dir = std::env::temp_dir().join(format!("ruthos-report-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");
        let doc = ReportDocument {
            title: "Report for REX".into(),
            header: vec![("Patient".into(), "REX".into()), ("Study".into(), "Thorax".into())],
            measurements: vec![ReportMeasurement {
                name: "VHS long axis".into(),
                value: "7.80 cm".into(),
                location: "DX 1, image 2".into(),
                basis: "Detector plane".into(),
                patient_scale: false,
            }],
            mmode: vec![ReportMeasurement {
                name: "FS".into(),
                value: "38.0 %".into(),
                location: "US 1".into(),
                basis: String::new(),
                patient_scale: true,
            }],
            findings: "Normal thorax. ".repeat(60),
            conclusion: "No abnormality detected.".into(),
            images: vec![ReportImage { caption: "Image 1".into(), width: 4, height: 3, rgb: vec![128; 36] }],
            footer: "Ruthos".into(),
        };
        write_pdf(&doc, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
