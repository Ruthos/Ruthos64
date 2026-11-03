//! Pixel decoding.
//!
//! A [`Frame`] is one decoded image frame in a form the renderer can upload
//! directly: 32-bit float modality values for grayscale images (rescale slope
//! and intercept already applied, MONOCHROME1 left as stored and flagged), or
//! 8-bit RGBA for colour images.
//!
//! JPEG transfer syntaxes go through libjpeg-turbo (see [`crate::jpeg`]);
//! everything else through dicom-rs.

use std::path::Path;

use dicom_dictionary_std::tags;
use dicom_pixeldata::{ConvertOptions, PixelDecoder, VoiLutOption};

use crate::clut::Clut;
use crate::jpeg::{self, JpegSamples};
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

    /// Default window from the header, else the full dynamic range. Colour
    /// images default to the identity window over 0..255, as in Horos.
    pub fn default_window(&self) -> (f32, f32) {
        if self.is_color() {
            return (128.0, 256.0);
        }
        match (self.meta.window_center, self.meta.window_width) {
            (Some(c), Some(w)) if w > 0.0 => (c as f32, w as f32),
            _ => self.full_dynamic_window(),
        }
    }

    /// A window spanning the minimum to maximum value in the frame.
    pub fn full_dynamic_window(&self) -> (f32, f32) {
        if self.is_color() {
            return (128.0, 256.0);
        }
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
    let obj = crate::open::open_any(path, None)?;
    let meta = {
        let mut m = meta_from_object(&obj);
        m.transfer_syntax = obj.meta().transfer_syntax().trim_end_matches('\0').to_string();
        m
    };

    if jpeg::is_jpeg_transfer_syntax(&meta.transfer_syntax) {
        return decode_jpeg_frame(&obj, meta, frame_index);
    }

    let decoded = obj
        .decode_pixel_data_frame(frame_index)
        .map_err(|e| DicomError::Pixels(error_chain(&e)))?;

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
            .map_err(|e| DicomError::Pixels(error_chain(&e)))?;
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
        .map_err(|e| DicomError::Pixels(error_chain(&e)))?;

    gray_frame(values, width, height, frame_index, meta)
}

/// Decode every frame of a file in one pass. For a cine loop this reads and
/// parses the file once instead of once per frame.
pub fn decode_all_frames(path: &Path) -> Result<Vec<Frame>> {
    let obj = crate::open::open_any(path, None)?;
    let meta = {
        let mut m = meta_from_object(&obj);
        m.transfer_syntax = obj.meta().transfer_syntax().trim_end_matches('\0').to_string();
        m
    };
    let frames = meta.number_of_frames.max(1);

    if jpeg::is_jpeg_transfer_syntax(&meta.transfer_syntax) {
        return (0..frames).map(|k| decode_jpeg_frame(&obj, meta.clone(), k)).collect();
    }

    let decoded = obj
        .decode_pixel_data()
        .map_err(|e| DicomError::Pixels(error_chain(&e)))?;
    let width = decoded.columns();
    let height = decoded.rows();
    if width == 0 || height == 0 {
        return Err(DicomError::Unsupported("zero-sized image".into()));
    }
    let is_color = decoded.samples_per_pixel() > 1
        || *decoded.photometric_interpretation() == dicom_pixeldata::PhotometricInterpretation::PaletteColor;
    let available = decoded.number_of_frames().max(1).min(frames);
    let mut out = Vec::with_capacity(available as usize);
    for k in 0..available {
        if is_color {
            let options = ConvertOptions::new().with_voi_lut(VoiLutOption::Default);
            let img = decoded
                .to_dynamic_image_with_options(k, &options)
                .map_err(|e| DicomError::Pixels(error_chain(&e)))?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            out.push(Frame {
                width: w,
                height: h,
                frame_index: k,
                pixels: FramePixels::Rgba(rgba.into_raw()),
                min: 0.0,
                max: 255.0,
                inverted: false,
                meta: meta.clone(),
            });
        } else {
            let values: Vec<f32> = decoded
                .to_vec_frame::<f32>(k)
                .map_err(|e| DicomError::Pixels(error_chain(&e)))?;
            out.push(gray_frame(values, width, height, k, meta.clone())?);
        }
    }
    Ok(out)
}

fn gray_frame(values: Vec<f32>, width: u32, height: u32, frame_index: u32, meta: InstanceMeta) -> Result<Frame> {
    let expected = (width as usize) * (height as usize);
    if values.len() < expected {
        return Err(DicomError::Pixels(format!(
            "decoded {} samples, expected {expected}",
            values.len()
        )));
    }
    let values = if values.len() > expected {
        values[..expected].to_vec()
    } else {
        values
    };
    let (min, max) = min_max(&values);
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

/// JPEG family: pull the encapsulated codestream for the frame and decode it
/// with libjpeg-turbo, then apply the modality rescale ourselves.
fn decode_jpeg_frame(obj: &dicom_object::DefaultDicomObject, meta: InstanceMeta, frame_index: u32) -> Result<Frame> {
    let element = obj
        .element(tags::PIXEL_DATA)
        .map_err(|_| DicomError::Missing("Pixel Data"))?;
    let fragments = element
        .fragments()
        .ok_or_else(|| DicomError::Pixels("JPEG transfer syntax but pixel data is not encapsulated".into()))?;
    if fragments.is_empty() {
        return Err(DicomError::Pixels("no pixel data fragments".into()));
    }
    let frames = meta.number_of_frames.max(1) as usize;
    let codestream: Vec<u8> = if fragments.len() == frames {
        fragments
            .get(frame_index as usize)
            .ok_or_else(|| DicomError::Pixels(format!("frame {frame_index} out of range")))?
            .to_vec()
    } else if frames == 1 {
        fragments.iter().flat_map(|f| f.iter().copied()).collect()
    } else {
        return Err(DicomError::Unsupported(format!(
            "{} fragments for {frames} frames; multi-fragment frames are not supported yet",
            fragments.len()
        )));
    };

    let img = jpeg::decode(&codestream)?;
    let (width, height) = (img.width, img.height);
    if meta.rows != 0 && meta.columns != 0 && (meta.rows != height || meta.columns != width) {
        tracing::warn!(
            "JPEG size {width}x{height} differs from header {}x{}; using the JPEG",
            meta.columns,
            meta.rows
        );
    }

    if img.components == 3 {
        let rgb = match img.samples {
            JpegSamples::U8(v) => v,
            JpegSamples::U16(v) => {
                // High-precision colour is rare; scale to 8 bits.
                let shift = img.precision.saturating_sub(8);
                v.into_iter().map(|s| (s >> shift) as u8).collect()
            }
        };
        let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
        for p in rgb.chunks_exact(3) {
            rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        return Ok(Frame {
            width,
            height,
            frame_index,
            pixels: FramePixels::Rgba(rgba),
            min: 0.0,
            max: 255.0,
            inverted: false,
            meta,
        });
    }

    let slope = meta.rescale_slope as f32;
    let intercept = meta.rescale_intercept as f32;
    let signed = meta.pixel_representation == 1;
    let values: Vec<f32> = match img.samples {
        JpegSamples::U8(v) => v.into_iter().map(|s| s as f32 * slope + intercept).collect(),
        JpegSamples::U16(v) => {
            if signed && img.lossless {
                v.into_iter().map(|s| (s as i16) as f32 * slope + intercept).collect()
            } else {
                v.into_iter().map(|s| s as f32 * slope + intercept).collect()
            }
        }
    };
    gray_frame(values, width, height, frame_index, meta)
}

/// Join an error and all of its sources into one line, so a codec failure
/// deep inside dicom-rs is reported with its actual cause.
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(s) = cur {
        let text = s.to_string();
        if parts.last() != Some(&text) {
            parts.push(text);
        }
        cur = s.source();
    }
    parts.join(": ")
}

/// Min and max of the frame. Non-finite values are skipped.
fn min_max(values: &[f32]) -> (f32, f32) {
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

/// The window function: modality value to 0..1.
#[inline]
pub fn window_value(v: f32, center: f32, width: f32) -> f32 {
    let w = width.max(1.0);
    ((v - (center - 0.5)) / (w - 1.0) + 0.5).clamp(0.0, 1.0)
}

/// Build an 8-bit grayscale preview of a frame with the given window applied.
/// Used for thumbnails.
pub fn frame_to_gray8(frame: &Frame, center: f32, width: f32) -> Vec<u8> {
    match &frame.pixels {
        FramePixels::Gray(values) => values
            .iter()
            .map(|&v| {
                let t = window_value(v, center, width);
                let t = if frame.inverted { 1.0 - t } else { t };
                (t * 255.0 + 0.5) as u8
            })
            .collect(),
        FramePixels::Rgba(rgba) => rgba
            .chunks_exact(4)
            .map(|p| {
                let luma = (p[0] as f32 * 0.299 + p[1] as f32 * 0.587 + p[2] as f32 * 0.114).round();
                (window_value(luma, center, width) * 255.0 + 0.5) as u8
            })
            .collect(),
    }
}

/// Render a frame to RGB8 exactly as the viewer shows it: window applied
/// (per channel for colour), then the CLUT for grayscale.
pub fn frame_to_rgb8(frame: &Frame, center: f32, width: f32, clut: &Clut) -> Vec<u8> {
    match &frame.pixels {
        FramePixels::Gray(_) => {
            let gray = frame_to_gray8(frame, center, width);
            gray.iter().flat_map(|&g| clut.table[g as usize]).collect()
        }
        FramePixels::Rgba(rgba) => rgba
            .chunks_exact(4)
            .flat_map(|p| {
                [
                    (window_value(p[0] as f32, center, width) * 255.0 + 0.5) as u8,
                    (window_value(p[1] as f32, center, width) * 255.0 + 0.5) as u8,
                    (window_value(p[2] as f32, center, width) * 255.0 + 0.5) as u8,
                ]
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_frame(values: Vec<f32>, w: u32, h: u32) -> Frame {
        let (min, max) = min_max(&values);
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

    #[test]
    fn colour_identity_window() {
        let f = Frame {
            width: 1,
            height: 1,
            frame_index: 0,
            pixels: FramePixels::Rgba(vec![10, 128, 250, 255]),
            min: 0.0,
            max: 255.0,
            inverted: false,
            meta: InstanceMeta::default(),
        };
        let (c, w) = f.default_window();
        let rgb = frame_to_rgb8(&f, c, w, &Clut::grayscale());
        assert_eq!(rgb, vec![10, 128, 250]);
        // A narrow window stretches contrast.
        let rgb = frame_to_rgb8(&f, 128.0, 64.0, &Clut::grayscale());
        assert_eq!(rgb[0], 0);
        assert_eq!(rgb[2], 255);
    }
}
