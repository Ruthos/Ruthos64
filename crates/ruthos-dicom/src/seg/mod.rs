//! DICOM Segmentation objects: segments saved as a series of the study, in the
//! form 3D Slicer, OHIF and the main PACS products read.
//!
//! Binary only, one bit a voxel. A SEG refers to the images it was drawn on, so
//! it travels with them, and only frames holding part of a segment are written,
//! which keeps a small structure in a large CT to a small file.

pub mod codes;
pub mod read;
pub mod write;

pub use codes::{Code, Term, TERMS};
pub use read::{read_seg, Geometry, Placed, SegFile};
pub use write::{source_series, write_seg, SegOptions, SegWritten, SourceSeries, SourceSlice};

/// The Segmentation Storage SOP class.
pub const SOP_CLASS: &str = "1.2.840.10008.5.1.4.1.1.66.4";

/// How a segment was made, as the standard's Segment Algorithm Type puts it. A
/// name is required for anything but manual work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Algorithm {
    Manual,
    SemiAutomatic(String),
    Automatic(String),
}

impl Algorithm {
    pub fn kind(&self) -> &'static str {
        match self {
            Algorithm::Manual => "MANUAL",
            Algorithm::SemiAutomatic(_) => "SEMIAUTOMATIC",
            Algorithm::Automatic(_) => "AUTOMATIC",
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Algorithm::Manual => None,
            Algorithm::SemiAutomatic(n) | Algorithm::Automatic(n) => Some(n),
        }
    }
}

/// One segment's description. Its voxels are apart, in [`SegmentData`] or
/// [`Placed`].
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// From 1, unique in the object.
    pub number: u16,
    /// Free text, always kept.
    pub label: String,
    /// sRGB; stored as CIELab.
    pub colour: [u8; 3],
    pub algorithm: Algorithm,
    pub category: Code,
    pub kind: Code,
}

/// A segment to write, with its voxels as a mask per source slice. A slice left
/// out holds none of it.
#[derive(Debug, Clone)]
pub struct SegmentData {
    pub segment: Segment,
    pub slices: std::collections::BTreeMap<usize, Vec<bool>>,
}

/// sRGB under the D65 white, as a CIELab value under D50, the white the standard
/// uses, in its 16 bit encoding.
pub fn to_cielab([r, g, b]: [u8; 3]) -> [u16; 3] {
    let linear = |v: u8| {
        let c = v as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(r), linear(g), linear(b));
    // sRGB to XYZ, already adapted to D50 by the Bradford transform.
    let x = 0.4360747 * r + 0.3850649 * g + 0.1430804 * b;
    let y = 0.2225045 * r + 0.7168786 * g + 0.0606169 * b;
    let z = 0.0139322 * r + 0.0971045 * g + 0.7141733 * b;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / WHITE[0]), f(y / WHITE[1]), f(z / WHITE[2]));
    let (l, a, b) = (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
    let word = |v: f64| v.clamp(0.0, 65535.0).round() as u16;
    [
        word(l * 65535.0 / 100.0),
        word((a + 128.0) * 65535.0 / 255.0),
        word((b + 128.0) * 65535.0 / 255.0),
    ]
}

const WHITE: [f64; 3] = [0.96422, 1.0, 0.82521];

/// The reverse of [`to_cielab`]. A colour outside sRGB is held to its edge.
pub fn from_cielab([l, a, b]: [u16; 3]) -> [u8; 3] {
    let (l, a, b) = (
        l as f64 * 100.0 / 65535.0,
        a as f64 * 255.0 / 65535.0 - 128.0,
        b as f64 * 255.0 / 65535.0 - 128.0,
    );
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inverse = |t: f64| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            (116.0 * t - 16.0) * 27.0 / 24389.0
        }
    };
    let (x, y, z) = (inverse(fx) * WHITE[0], inverse(fy) * WHITE[1], inverse(fz) * WHITE[2]);
    let r = 3.1338561 * x - 1.6168667 * y - 0.4906146 * z;
    let g = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
    let b = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
    let gamma = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        let v = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round() as u8
    };
    [gamma(r), gamma(g), gamma(b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_is_the_top_of_lightness_and_neutral() {
        let [l, a, b] = to_cielab([255, 255, 255]);
        assert!(l > 65500, "{l}");
        let neutral = (128.0 * 65535.0 / 255.0) as i32;
        assert!(
            (a as i32 - neutral).abs() < 40 && (b as i32 - neutral).abs() < 40,
            "{a} {b}"
        );
    }

    #[test]
    fn colours_survive_the_round_trip() {
        for c in [
            [240, 200, 120],
            [90, 170, 240],
            [240, 110, 100],
            [120, 210, 130],
            [200, 130, 230],
            [0, 0, 0],
            [255, 255, 255],
            [12, 34, 56],
        ] {
            let back = from_cielab(to_cielab(c));
            for k in 0..3 {
                assert!(back[k].abs_diff(c[k]) <= 1, "{c:?} came back {back:?}");
            }
        }
    }
}
