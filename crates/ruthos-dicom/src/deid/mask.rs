//! Burned-in annotation masking.
//!
//! A series leaves the anonymiser only when every frame has been masked,
//! trusted or reviewed. Masking decodes the pixel data, blanks the pixels
//! the plan says to blank, and hands back uncompressed samples for the
//! caller to write as Explicit VR Little Endian. Series that need no
//! masking are copied byte for byte and never pass through here.

use serde::{Deserialize, Serialize};

use dicom_pixeldata::{PhotometricInterpretation, PixelDecoder};

use crate::meta::UsRegion;
use crate::{DicomError, Result};

/// A rectangle in pixel coordinates, inclusive of both corners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn normalised(self) -> Self {
        Self {
            x0: self.x0.min(self.x1),
            y0: self.y0.min(self.y1),
            x1: self.x0.max(self.x1),
            y1: self.y0.max(self.y1),
        }
    }
}

/// How a series' pixels are handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskPlan {
    /// Keep only the pixels inside these rectangles (ultrasound regions).
    KeepOnly(Vec<Rect>),
    /// Blank the pixels inside these rectangles (a device template or a review).
    Blank(Vec<Rect>),
    /// Keep only the regions, and blank these rectangles as well: for
    /// machines whose region includes the header band.
    Combined { keep: Vec<Rect>, blank: Vec<Rect> },
}

impl MaskPlan {
    fn keep(&self, x: u32, y: u32) -> bool {
        match self {
            MaskPlan::KeepOnly(rects) => rects.iter().any(|r| r.contains(x, y)),
            MaskPlan::Blank(rects) => !rects.iter().any(|r| r.contains(x, y)),
            MaskPlan::Combined { keep, blank } => {
                keep.iter().any(|r| r.contains(x, y)) && !blank.iter().any(|r| r.contains(x, y))
            }
        }
    }
}

/// A device mask template: rectangles to blank for one machine and matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskTemplate {
    pub manufacturer: String,
    pub model: String,
    pub columns: u32,
    pub rows: u32,
    pub rects: Vec<Rect>,
}

impl MaskTemplate {
    /// File stem the template is saved under.
    pub fn key(manufacturer: &str, model: &str, columns: u32, rows: u32) -> String {
        let clean = |s: &str| {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches('-')
                .to_string()
        };
        format!("{}__{}__{columns}x{rows}", clean(manufacturer), clean(model))
    }
}

/// The rectangles of the ultrasound regions, the area to keep.
pub fn regions_union(regions: &[UsRegion]) -> Vec<Rect> {
    regions
        .iter()
        .filter(|r| r.x1 >= r.x0 && r.y1 >= r.y0 && r.x0 >= 0 && r.y0 >= 0)
        .map(|r| Rect {
            x0: r.x0 as u32,
            y0: r.y0 as u32,
            x1: r.x1 as u32,
            y1: r.y1 as u32,
        })
        .collect()
}

/// Uncompressed masked samples and the attributes that describe them.
#[derive(Debug, Clone)]
pub struct MaskedPixels {
    pub data: Vec<u8>,
    pub rows: u32,
    pub columns: u32,
    pub frames: u32,
    pub samples_per_pixel: u16,
    pub bits_allocated: u16,
    pub bits_stored: u16,
    pub high_bit: u16,
    pub pixel_representation: u16,
    /// As the decoder delivered it (JPEG colour comes back as RGB).
    pub photometric: String,
}

/// Decode a dataset's pixel data and apply the plan to every frame.
pub fn apply_mask(obj: &dicom_object::DefaultDicomObject, plan: &MaskPlan) -> Result<MaskedPixels> {
    let decoded = obj
        .decode_pixel_data()
        .map_err(|e| DicomError::Pixels(crate::pixels::error_chain(&e)))?;
    let rows = decoded.rows();
    let columns = decoded.columns();
    let frames = decoded.number_of_frames().max(1);
    let spp = decoded.samples_per_pixel().max(1);
    let bits_allocated = decoded.bits_allocated();
    let bps = (bits_allocated as usize).div_ceil(8);
    if bps == 0 || bps > 2 {
        return Err(DicomError::Unsupported(format!(
            "{bits_allocated}-bit samples cannot be masked"
        )));
    }
    let photometric = decoded.photometric_interpretation().clone();
    let mut data = decoded.data().to_vec();
    let frame_len = rows as usize * columns as usize * spp as usize * bps;
    if data.len() < frame_len * frames as usize {
        return Err(DicomError::Pixels(format!(
            "pixel data holds {} bytes, expected {} for {frames} frames",
            data.len(),
            frame_len * frames as usize
        )));
    }
    // Black is the minimum sample, except MONOCHROME1 where it is the maximum.
    let black: u16 = if photometric == PhotometricInterpretation::Monochrome1 {
        ((1u32 << decoded.bits_stored().clamp(1, 16)) - 1) as u16
    } else {
        0
    };
    let black_bytes = black.to_le_bytes();

    for f in 0..frames as usize {
        let base = f * frame_len;
        for y in 0..rows {
            for x in 0..columns {
                if plan.keep(x, y) {
                    continue;
                }
                let px = base + ((y as usize * columns as usize + x as usize) * spp as usize) * bps;
                for s in 0..spp as usize {
                    let at = px + s * bps;
                    data[at] = black_bytes[0];
                    if bps == 2 {
                        data[at + 1] = black_bytes[1];
                    }
                }
            }
        }
    }
    if data.len() % 2 == 1 {
        data.push(0);
    }
    Ok(MaskedPixels {
        data,
        rows,
        columns,
        frames,
        samples_per_pixel: spp,
        bits_allocated,
        bits_stored: decoded.bits_stored(),
        high_bit: decoded.high_bit(),
        pixel_representation: match decoded.pixel_representation() {
            dicom_pixeldata::PixelRepresentation::Signed => 1,
            _ => 0,
        },
        photometric: photometric.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_only_and_blank_are_complements() {
        let r = Rect {
            x0: 2,
            y0: 2,
            x1: 4,
            y1: 4,
        };
        let keep = MaskPlan::KeepOnly(vec![r]);
        let blank = MaskPlan::Blank(vec![r]);
        assert!(keep.keep(3, 3));
        assert!(!keep.keep(0, 0));
        assert!(!blank.keep(3, 3));
        assert!(blank.keep(0, 0));
    }

    #[test]
    fn combined_blanks_inside_a_kept_region() {
        let keep = Rect {
            x0: 0,
            y0: 0,
            x1: 9,
            y1: 9,
        };
        let band = Rect {
            x0: 0,
            y0: 0,
            x1: 9,
            y1: 1,
        };
        let plan = MaskPlan::Combined {
            keep: vec![keep],
            blank: vec![band],
        };
        assert!(plan.keep(5, 5));
        assert!(!plan.keep(5, 1), "the band inside the region is blanked");
        assert!(!plan.keep(12, 5), "outside the region is blanked");
    }

    #[test]
    fn template_key_is_filesystem_safe() {
        assert_eq!(
            MaskTemplate::key("GEMS Ultrasound", "Vivid q", 636, 422),
            "gems-ultrasound__vivid-q__636x422"
        );
    }
}
