//! The scale a measurement is computed from, and where it came from.
//!
//! A millimetre at the detector and a millimetre in the animal are
//! different quantities, so every measurement records the basis of its
//! scale and shows it. The resolution order is fixed; the first rule that
//! applies decides. See the measurements specification, section 1.

use crate::meta::{InstanceMeta, UsRegion};

/// Where a pixel spacing came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpacingBasis {
    /// A hand calibration from a known length.
    Hand,
    /// The ultrasound region under the point.
    UltrasoundRegion,
    /// Pixel Spacing with Calibration Type GEOMETRY or FIDUCIAL.
    Calibrated,
    /// A cross-sectional modality's Pixel Spacing.
    PatientPlane,
    /// Imager Pixel Spacing divided by the estimated magnification factor.
    MagnificationEstimated,
    /// Pixel Spacing differs from Imager Pixel Spacing, no type stated.
    CalibratedUnstated,
    /// Imager Pixel Spacing: measured at the detector.
    DetectorPlane,
    /// No spacing at all: pixels.
    Uncalibrated,
}

impl SpacingBasis {
    pub fn label(self) -> &'static str {
        match self {
            SpacingBasis::Hand => "Hand calibration",
            SpacingBasis::UltrasoundRegion => "Ultrasound region",
            SpacingBasis::Calibrated => "Calibrated",
            SpacingBasis::PatientPlane => "Patient plane",
            SpacingBasis::MagnificationEstimated => "Corrected for magnification (estimated)",
            SpacingBasis::CalibratedUnstated => "Calibrated, method not stated",
            SpacingBasis::DetectorPlane => "Detector plane",
            SpacingBasis::Uncalibrated => "Uncalibrated (pixels)",
        }
    }

    /// Stable text for storage.
    pub fn key(self) -> &'static str {
        match self {
            SpacingBasis::Hand => "hand",
            SpacingBasis::UltrasoundRegion => "us-region",
            SpacingBasis::Calibrated => "calibrated",
            SpacingBasis::PatientPlane => "patient-plane",
            SpacingBasis::MagnificationEstimated => "magnification-estimated",
            SpacingBasis::CalibratedUnstated => "calibrated-unstated",
            SpacingBasis::DetectorPlane => "detector-plane",
            SpacingBasis::Uncalibrated => "uncalibrated",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "hand" => SpacingBasis::Hand,
            "us-region" => SpacingBasis::UltrasoundRegion,
            "calibrated" => SpacingBasis::Calibrated,
            "patient-plane" => SpacingBasis::PatientPlane,
            "magnification-estimated" => SpacingBasis::MagnificationEstimated,
            "calibrated-unstated" => SpacingBasis::CalibratedUnstated,
            "detector-plane" => SpacingBasis::DetectorPlane,
            "uncalibrated" => SpacingBasis::Uncalibrated,
            _ => return None,
        })
    }

    /// A short mark shown beside values that need a second look.
    pub fn marker(self) -> &'static str {
        match self {
            SpacingBasis::DetectorPlane => " \u{2020}",
            SpacingBasis::MagnificationEstimated => " \u{2248}",
            SpacingBasis::CalibratedUnstated => " ?",
            SpacingBasis::Uncalibrated => " px",
            _ => "",
        }
    }

    /// The one-line explanation offered on hover.
    pub fn explanation(self) -> &'static str {
        match self {
            SpacingBasis::Hand => "Scale set by hand from a known length on this image.",
            SpacingBasis::UltrasoundRegion => "Scale from the ultrasound region's physical delta under the measurement.",
            SpacingBasis::Calibrated => "Pixel Spacing declared calibrated by geometry or a fiducial.",
            SpacingBasis::PatientPlane => "Cross-sectional image: Pixel Spacing is in the patient.",
            SpacingBasis::MagnificationEstimated => "Detector spacing divided by the modality's estimated magnification factor; an estimate, not a measurement.",
            SpacingBasis::CalibratedUnstated => "Pixel Spacing differs from the detector spacing, but the modality did not say how it was calibrated.",
            SpacingBasis::DetectorPlane => "Measured at the detector: the value overstates the size of the structure by the radiographic magnification, typically five to fifteen per cent.",
            SpacingBasis::Uncalibrated => "No scale on this image; the value is in pixels.",
        }
    }

    /// Whether the value is a physical length in the patient.
    pub fn is_patient_scale(self) -> bool {
        !matches!(self, SpacingBasis::DetectorPlane | SpacingBasis::Uncalibrated)
    }
}

/// A hand calibration: `[row, column]` mm per pixel, scoped to one
/// ultrasound region when set from a length inside one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandCalibration {
    pub spacing: [f64; 2],
    /// Index into `us_regions` when scoped; `None` applies everywhere.
    pub region: Option<usize>,
}

/// A spacing with its basis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    /// `[row, column]` millimetres per pixel.
    pub spacing: [f64; 2],
    pub basis: SpacingBasis,
}

/// Why a measurement has no millimetre value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasureIssue {
    /// The endpoints lie in different ultrasound regions, or one outside.
    CrossesRegions,
    /// Inside a region whose horizontal axis is time.
    TimeAxis,
    /// No scale on the image.
    Uncalibrated,
    /// A time was asked for where no axis is time.
    NoTime,
    /// A velocity, slope or half-time was asked for outside a spectral
    /// Doppler strip.
    NoVelocity,
    /// A distance was asked for where the vertical axis is not distance.
    NoDistance,
}

impl MeasureIssue {
    pub fn label(self) -> &'static str {
        match self {
            MeasureIssue::CrossesRegions => "crosses regions",
            MeasureIssue::TimeAxis => "n/a on a time axis",
            MeasureIssue::Uncalibrated => "not calibrated",
            MeasureIssue::NoTime => "no time axis here",
            MeasureIssue::NoVelocity => "no velocity scale here",
            MeasureIssue::NoDistance => "no distance scale here",
        }
    }
}

/// A length in millimetres with its basis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub mm: f64,
    pub basis: SpacingBasis,
}

const CROSS_SECTIONAL: &[&str] = &["CT", "MR", "PT", "NM", "OPT"];
const PROJECTION: &[&str] = &["CR", "DX", "RF", "XA", "MG", "RG", "DF", "PX", "IO", "XC"];

impl InstanceMeta {
    fn modality_upper(&self) -> String {
        self.modality.trim().to_ascii_uppercase()
    }

    pub fn is_cross_sectional(&self) -> bool {
        CROSS_SECTIONAL.contains(&self.modality_upper().as_str())
    }

    pub fn is_projection(&self) -> bool {
        PROJECTION.contains(&self.modality_upper().as_str())
    }

    /// Index of the ultrasound region under a point.
    pub fn us_region_index_at(&self, x: f64, y: f64) -> Option<usize> {
        self.us_regions.iter().position(|r| r.contains(x, y))
    }

    /// The ultrasound region under an image point, if any.
    pub fn us_region_at(&self, x: f64, y: f64) -> Option<&UsRegion> {
        self.us_regions.iter().find(|r| r.contains(x, y))
    }

    /// Whether angles, areas and statistics make sense at a point: not
    /// inside a region whose horizontal axis is time.
    pub fn allows_geometry_at(&self, x: f64, y: f64) -> bool {
        !self.us_region_at(x, y).is_some_and(|r| r.is_time_across())
    }

    /// The image-wide basis stated in the corner annotation, resolved at
    /// the image centre.
    pub fn image_scale(&self) -> Scale {
        let x = self.columns as f64 / 2.0;
        let y = self.rows as f64 / 2.0;
        self.scale_at(x, y).unwrap_or(Scale {
            spacing: [1.0, 1.0],
            basis: SpacingBasis::Uncalibrated,
        })
    }

    /// Resolve the spacing at a point by the eight-row table. `None` only
    /// inside a time-axis region, where there is no two-dimensional scale;
    /// an image with no scale resolves to `Uncalibrated` with unit spacing.
    pub fn scale_at(&self, x: f64, y: f64) -> Option<Scale> {
        let region_index = self.us_region_index_at(x, y);
        // 1. Hand calibration, scoped to its region when it has one.
        if let Some(h) = &self.hand_calibration {
            let applies = match h.region {
                Some(i) => region_index == Some(i),
                None => true,
            };
            if applies {
                return Some(Scale {
                    spacing: h.spacing,
                    basis: SpacingBasis::Hand,
                });
            }
        }
        // 2. Ultrasound region with distance units.
        if let Some(r) = region_index.map(|i| &self.us_regions[i]) {
            if r.is_time_across() {
                return None;
            }
            if let Some(s) = r.mm_per_pixel() {
                return Some(Scale {
                    spacing: s,
                    basis: SpacingBasis::UltrasoundRegion,
                });
            }
        }
        let ps = self.pixel_spacing_tag;
        let ips = self.imager_pixel_spacing;
        let cal_type = self.pixel_spacing_calibration_type.trim().to_ascii_uppercase();
        // 3. Declared calibration.
        if let Some(s) = ps {
            if cal_type == "GEOMETRY" || cal_type == "FIDUCIAL" {
                return Some(Scale {
                    spacing: s,
                    basis: SpacingBasis::Calibrated,
                });
            }
        }
        // 4. Cross-sectional.
        if self.is_cross_sectional() {
            if let Some(s) = ps {
                return Some(Scale {
                    spacing: s,
                    basis: SpacingBasis::PatientPlane,
                });
            }
        }
        if self.is_projection() {
            // 5. Detector spacing corrected by the estimated magnification.
            if let (Some(i), Some(f)) = (ips, self.magnification_factor) {
                if f > 0.0 {
                    return Some(Scale {
                        spacing: [i[0] / f, i[1] / f],
                        basis: SpacingBasis::MagnificationEstimated,
                    });
                }
            }
            // 6. Pixel Spacing that differs from the detector spacing.
            if let Some(s) = ps {
                let differs = match ips {
                    Some(i) => (s[0] - i[0]).abs() > 1e-6 || (s[1] - i[1]).abs() > 1e-6,
                    None => true,
                };
                if differs {
                    return Some(Scale {
                        spacing: s,
                        basis: SpacingBasis::CalibratedUnstated,
                    });
                }
            }
            // 7. The detector itself.
            if let Some(i) = ips.or(ps) {
                return Some(Scale {
                    spacing: i,
                    basis: SpacingBasis::DetectorPlane,
                });
            }
        } else if let Some(s) = ps.or(ips) {
            // Ultrasound without regions, secondary capture and the rest.
            return Some(Scale {
                spacing: s,
                basis: SpacingBasis::CalibratedUnstated,
            });
        }
        // 8. Nothing.
        Some(Scale {
            spacing: [1.0, 1.0],
            basis: SpacingBasis::Uncalibrated,
        })
    }

    /// Length between two image points, with its basis or the reason there
    /// is none. Inside a time-axis strip only the vertical distance counts.
    pub fn length(&self, a: [f64; 2], b: [f64; 2]) -> Result<Length, MeasureIssue> {
        let ra = self.us_region_index_at(a[0], a[1]);
        let rb = self.us_region_index_at(b[0], b[1]);
        if !self.us_regions.is_empty() && ra != rb {
            return Err(MeasureIssue::CrossesRegions);
        }
        if let Some(r) = ra.map(|i| &self.us_regions[i]) {
            if r.is_time_across() {
                let hand = self.hand_calibration.filter(|h| h.region == ra || h.region.is_none());
                let (row_mm, basis) = match hand {
                    Some(h) => (h.spacing[0], SpacingBasis::Hand),
                    None => (
                        r.row_mm().ok_or(MeasureIssue::Uncalibrated)?,
                        SpacingBasis::UltrasoundRegion,
                    ),
                };
                return Ok(Length {
                    mm: (b[1] - a[1]).abs() * row_mm,
                    basis,
                });
            }
        }
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let scale = self.scale_at(mid[0], mid[1]).ok_or(MeasureIssue::TimeAxis)?;
        if scale.basis == SpacingBasis::Uncalibrated {
            return Err(MeasureIssue::Uncalibrated);
        }
        let dx = (b[0] - a[0]) * scale.spacing[1];
        let dy = (b[1] - a[1]) * scale.spacing[0];
        Ok(Length {
            mm: (dx * dx + dy * dy).sqrt(),
            basis: scale.basis,
        })
    }

    /// Blood velocity in m/s at an image point inside a spectral Doppler
    /// region, signed (towards the transducer positive, as the scanner
    /// records it).
    pub fn velocity_at(&self, x: f64, y: f64) -> Option<f64> {
        let r = self.us_region_at(x, y)?;
        r.is_velocity_y().then(|| r.value_y(y) / 100.0)
    }

    /// The time between two points in seconds, when both lie in the same
    /// region whose horizontal axis is time (M-mode or spectral Doppler).
    pub fn time_between(&self, a: [f64; 2], b: [f64; 2]) -> Option<f64> {
        let ra = self.us_region_index_at(a[0], a[1])?;
        if self.us_region_index_at(b[0], b[1]) != Some(ra) {
            return None;
        }
        let s = self.us_regions[ra].seconds_per_pixel()?;
        Some((b[0] - a[0]).abs() * s)
    }

    /// Spacing at a point, without the basis, for callers that only scale.
    pub fn spacing_at(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        let s = self.scale_at(x, y)?;
        if s.basis == SpacingBasis::Uncalibrated {
            return None;
        }
        Some(s.spacing)
    }

    /// Length in millimetres, without the basis.
    pub fn length_mm(&self, a: [f64; 2], b: [f64; 2]) -> Option<f64> {
        self.length(a, b).ok().map(|l| l.mm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(modality: &str) -> InstanceMeta {
        InstanceMeta {
            modality: modality.into(),
            rows: 100,
            columns: 100,
            ..Default::default()
        }
    }

    fn region(x0: i64, y0: i64, x1: i64, y1: i64, ux: u16, dx: f64, dy: f64) -> UsRegion {
        UsRegion {
            ref_x0: 0,
            ref_y0: 0,
            ref_value_x: 0.0,
            ref_value_y: 0.0,
            x0,
            y0,
            x1,
            y1,
            spatial_format: if ux == 4 { 2 } else { 1 },
            data_type: 1,
            units_x: ux,
            units_y: 3,
            delta_x: dx,
            delta_y: dy,
        }
    }

    #[test]
    fn doppler_velocity_and_time() {
        let mut m = meta("US");
        // A spectral strip: 0.005 s per pixel across, -2 cm/s per pixel down,
        // baseline 40 rows into the region.
        let mut r = region(0, 100, 199, 199, 4, 0.005, -2.0);
        r.units_y = 7;
        r.ref_y0 = 40;
        m.us_regions = vec![r];
        assert_eq!(m.velocity_at(10.0, 140.0), Some(0.0));
        assert!((m.velocity_at(10.0, 90.0 + 100.0).unwrap() - (-1.0)).abs() < 1e-9);
        assert!((m.velocity_at(10.0, 115.0).unwrap() - 0.5).abs() < 1e-9);
        assert!((m.time_between([10.0, 120.0], [50.0, 150.0]).unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(m.velocity_at(10.0, 50.0), None, "outside the strip");
    }

    #[test]
    fn row_1_hand_calibration_scoped_to_its_region() {
        let mut m = meta("US");
        m.us_regions = vec![
            region(0, 0, 49, 99, 3, 0.05, 0.05),
            region(50, 0, 99, 99, 4, 0.001, 0.02),
        ];
        m.hand_calibration = Some(HandCalibration {
            spacing: [0.7, 0.7],
            region: Some(0),
        });
        assert_eq!(
            m.scale_at(10.0, 10.0).unwrap(),
            Scale {
                spacing: [0.7, 0.7],
                basis: SpacingBasis::Hand
            }
        );
        // The M-mode region keeps its own scale.
        let l = m.length([70.0, 10.0], [70.0, 20.0]).unwrap();
        assert!((l.mm - 2.0).abs() < 1e-9);
        assert_eq!(l.basis, SpacingBasis::UltrasoundRegion);
    }

    #[test]
    fn row_2_ultrasound_region() {
        let mut m = meta("US");
        m.us_regions = vec![region(0, 0, 99, 99, 3, 0.05, 0.05)];
        let s = m.scale_at(5.0, 5.0).unwrap();
        assert_eq!(s.basis, SpacingBasis::UltrasoundRegion);
        assert!((s.spacing[0] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn row_3_declared_calibration() {
        let mut m = meta("DX");
        m.pixel_spacing_tag = Some([0.1, 0.1]);
        m.imager_pixel_spacing = Some([0.12, 0.12]);
        m.pixel_spacing_calibration_type = "GEOMETRY".into();
        assert_eq!(
            m.scale_at(5.0, 5.0).unwrap(),
            Scale {
                spacing: [0.1, 0.1],
                basis: SpacingBasis::Calibrated
            }
        );
    }

    #[test]
    fn row_4_cross_sectional() {
        let mut m = meta("CT");
        m.pixel_spacing_tag = Some([0.5, 0.5]);
        assert_eq!(m.scale_at(5.0, 5.0).unwrap().basis, SpacingBasis::PatientPlane);
    }

    #[test]
    fn row_5_magnification_estimate() {
        let mut m = meta("DX");
        m.imager_pixel_spacing = Some([0.1, 0.1]);
        m.magnification_factor = Some(1.1);
        let l = m.length([0.0, 0.0], [100.0, 0.0]).unwrap();
        assert!((l.mm - 9.0909).abs() < 1e-3, "{}", l.mm);
        assert_eq!(l.basis, SpacingBasis::MagnificationEstimated);
    }

    #[test]
    fn row_6_pixel_spacing_differs_without_type() {
        let mut m = meta("CR");
        m.pixel_spacing_tag = Some([0.09, 0.09]);
        m.imager_pixel_spacing = Some([0.1, 0.1]);
        let s = m.scale_at(5.0, 5.0).unwrap();
        assert_eq!(s.basis, SpacingBasis::CalibratedUnstated);
        assert_eq!(s.spacing, [0.09, 0.09]);
    }

    #[test]
    fn row_7_detector_plane_including_equal_spacings() {
        let mut only_imager = meta("DX");
        only_imager.imager_pixel_spacing = Some([0.1, 0.1]);
        assert_eq!(
            only_imager.scale_at(5.0, 5.0).unwrap().basis,
            SpacingBasis::DetectorPlane
        );
        let mut equal = meta("DX");
        equal.imager_pixel_spacing = Some([0.1, 0.1]);
        equal.pixel_spacing_tag = Some([0.1, 0.1]);
        assert_eq!(equal.scale_at(5.0, 5.0).unwrap().basis, SpacingBasis::DetectorPlane);
    }

    #[test]
    fn row_8_uncalibrated() {
        let m = meta("OT");
        assert_eq!(m.scale_at(5.0, 5.0).unwrap().basis, SpacingBasis::Uncalibrated);
        assert_eq!(m.length([0.0, 0.0], [3.0, 4.0]), Err(MeasureIssue::Uncalibrated));
        assert!(m.spacing_at(5.0, 5.0).is_none());
    }

    #[test]
    fn crossing_regions_and_time_axis() {
        let mut m = meta("US");
        m.us_regions = vec![
            region(0, 0, 49, 99, 3, 0.05, 0.05),
            region(50, 0, 99, 99, 4, 0.001, 0.02),
        ];
        assert_eq!(m.length([10.0, 10.0], [70.0, 10.0]), Err(MeasureIssue::CrossesRegions));
        assert_eq!(m.length([10.0, 10.0], [10.0, 120.0]), Err(MeasureIssue::CrossesRegions));
        assert!(m.allows_geometry_at(10.0, 10.0));
        assert!(!m.allows_geometry_at(70.0, 10.0));
        assert!(m.scale_at(70.0, 10.0).is_none());
    }
}
