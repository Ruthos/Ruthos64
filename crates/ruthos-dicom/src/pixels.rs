//! Pixel decoding.
//!
//! A [`Frame`] is one decoded image frame in a form the renderer can upload
//! directly: 32-bit float modality values for grayscale images (rescale slope
//! and intercept already applied, MONOCHROME1 left as stored and flagged), or
//! 8-bit RGBA for colour images.

use std::path::Path;

use dicom_object::open_file;
use dicom_pixeldata::{ConvertOptions, PixelDecoder, VoiLutOption};

use crate::meta::{meta_from_object, InstanceMeta};
use crate::{DicomError, Result};

/// Pixel payload of one frame.
#[derive(Debug, Clone)]
pub enum FramePixels {
    /// Grayscale modality values, row-major, `width * height` entries.
    Gray(Vec<f32>),
    /// Colour, row-major RGBA8, `width * height * 4` bytes.
    Rgba(Vec<u8>),
}

/// One decoded frame together with the header it came from.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub frame_index: u32,
    pub pixels: FramePixels,
    /// Minimum and maximum modality value in the frame (grayscale only).
    pub min: f32,
    pub max: f32,
    /// True for MONOCHROME1, where low values are displayed bright.
    pub inverted: bool,
    pub meta: InstanceMeta,
}

impl Frame {
    pub fn is_color(&self) -> bool {
        matches!(self.pixels, FramePixels::Rgba(_))
    }

    /// Default window from the header, else the full dynamic range.
    pub fn default_window(&self) -> (f32, f32) {
        match (self.meta.window_center, self.meta.window_width) {
            (Some(c), Some(w)) if w > 0.0 => (c as f32, w as f32),
            _ => self.full_dynamic_window(),
        }
    }

    /// A window spanning the minimum to maximum value in the frame.
    pub fn full_dynamic_window(&self) -> (f32, f32) {
        let width = (self.max - self.min).max(1.0);
        let center = self.min + width / 2.0;
        (center, width)
    }

    /// Modality value at pixel `(x, y)`, if grayscale and in bounds.
    pub fn value_at(&self, x: i64, y: i64) -> Option<f32> {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return None;
        }
        match &self.pixels {
            FramePixels::Gray(v) => v.get((y as usize) * self.width as usize + x as usize).copied(),
            FramePixels::Rgba(_) => None,
        }
    }

    /// Byte size of the pixel payload.
    pub fn byte_len(&self) -> usize {
        match &self.pixels {
            FramePixels::Gray(v) => v.len() * 4,
            FramePixels::Rgba(v) => v.len(),
        }
    }
}

/// Decode one frame of the file at `path`.
pub fn decode_frame(path: &Path, frame_index: u32) -> Result<Frame> {
    let obj = open_file(path).map_err(|e| DicomError::Read(e.to_string()))?;
    let meta = {
        let mut m = meta_from_object(&obj);
        m.transfer_syntax = obj.meta().transfer_syntax().trim_end_matches('\0').to_string();
        m
    };

    let decoded = obj
        .decode_pixel_data_frame(frame_index)
        .map_err(|e| DicomError::Pixels(e.to_string()))?;

    let width = decoded.columns();
    let height = decoded.rows();
    if width == 0 || height == 0 {
        return Err(DicomError::Unsupported("zero-sized image".into()));
    }

    let is_color = decoded.samples_per_pixel() > 1
        || *decoded.photometric_interpretation() == dicom_pixeldata::PhotometricInterpretation::PaletteColor;

    if is_color {
        let options = ConvertOptions::new().with_voi_lut(VoiLutOption::Default);
        let img = decoded
            .to_dynamic_image_with_options(0, &options)
            .map_err(|e| DicomError::Pixels(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Ok(Frame {
            width: w,
            height: h,
            frame_index,
            pixels: FramePixels::Rgba(rgba.into_raw()),
            min: 0.0,
            max: 255.0,
            inverted: false,
            meta,
        });
    }

    // Grayscale: modality LUT applied (rescale), no VOI, no inversion.
    let values: Vec<f32> = decoded
        .to_vec_frame::<f32>(0)
        .map_err(|e| DicomError::Pixels(e.to_string()))?;

    let expected = (width as usize) * (height as usize);
    if values.len() < expected {
        return Err(DicomError::Pixels(format!(
            "decoded {} samples, expected {expected}",
            values.len()
        )));
    }
    let values = if values.len() > expected { values[..expected].to_vec() } else { values };

    let (min, max) = min_max_ignoring_padding(&values, &meta);
    let inverted = meta.is_monochrome1();

    Ok(Frame {
        width,
        height,
        frame_index,
        pixels: FramePixels::Gray(values),
        min,
        max,
        inverted,
        meta,
    })
}

/// Min and max of the frame. Non-finite values are skipped.
fn min_max_ignoring_padding(values: &[f32], _meta: &InstanceMeta) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in values {
        if v.is_finite() {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
    }
    if !min.is_finite() || !max.is_finite() {
        (0.0, 1.0)
    } else {
        (min, max)
    }
}

/// Build an 8-bit grayscale preview of a frame with the given window applied.
/// Used for thumbnails and export.
pub fn frame_to_gray8(frame: &Frame, center: f32, width: f32) -> Vec<u8> {
    match &frame.pixels {
        FramePixels::Gray(values) => {
            let w = width.max(1.0);
            let lo = center - 0.5 - (w - 1.0) / 2.0;
            let hi = center - 0.5 + (w - 1.0) / 2.0;
            values
                .iter()
                .map(|&v| {
                    let t = if v <= lo {
                        0.0
                    } else if v > hi {
                        1.0
                    } else {
                        ((v - (center - 0.5)) / (w - 1.0) + 0.5).clamp(0.0, 1.0)
                    };
                    let t = if frame.inverted { 1.0 - t } else { t };
                    (t * 255.0 + 0.5) as u8
                })
                .collect()
        }
        FramePixels::Rgba(rgba) => rgba
            .chunks_exact(4)
            .map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_frame(values: Vec<f32>, w: u32, h: u32) -> Frame {
        let (min, max) = min_max_ignoring_padding(&values, &InstanceMeta::default());
        Frame {
            width: w,
            height: h,
            frame_index: 0,
            pixels: FramePixels::Gray(values),
            min,
            max,
            inverted: false,
            meta: InstanceMeta::default(),
        }
    }

    #[test]
    fn window_maps_extremes() {
        let f = synthetic_frame(vec![0.0, 100.0, 200.0, 300.0], 2, 2);
        let g = frame_to_gray8(&f, 150.0, 300.0);
        assert_eq!(g[0], 0);
        assert_eq!(g[3], 255);
        assert!(g[1] < g[2]);
    }

    #[test]
    fn full_dynamic_window() {
        let f = synthetic_frame(vec![10.0, 20.0, 30.0, 40.0], 2, 2);
        let (c, w) = f.full_dynamic_window();
        assert_eq!(w, 30.0);
        assert_eq!(c, 25.0);
    }
}
