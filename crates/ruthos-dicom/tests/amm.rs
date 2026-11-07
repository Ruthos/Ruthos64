//! Anatomical M-mode against a wall whose motion is known exactly.

use std::path::PathBuf;
use std::sync::Arc;

use ruthos_dicom::amm::{calibration, read_strip, smooth, write_strip, Calibration, COLUMNS_PER_FRAME};
use ruthos_dicom::{decode_frame, read_meta, Frame, FramePixels, UsRegion};

const SIDE: usize = 200;
const CENTRE: f64 = 100.0;
const SWING: f64 = 30.0;
const FRAMES: usize = 40;

/// A 0.2 mm a pixel 2D region over the whole image.
fn region() -> UsRegion {
    UsRegion {
        x0: 0,
        y0: 0,
        x1: SIDE as i64 - 1,
        y1: SIDE as i64 - 1,
        spatial_format: 1,
        data_type: 1,
        units_x: 3,
        units_y: 3,
        delta_x: 0.02,
        delta_y: 0.02,
        ..UsRegion::default()
    }
}

fn wall_at(frame: usize) -> f64 {
    CENTRE + SWING * (frame as f64 / FRAMES as f64 * std::f64::consts::TAU).sin()
}

/// A bright wall moving up and down through the image.
fn frame(n: usize, ms: Option<f64>) -> Frame {
    let mut values = vec![10.0f32; SIDE * SIDE];
    let wall = wall_at(n);
    for y in 0..SIDE {
        if (y as f64 - wall).abs() < 3.0 {
            values[y * SIDE..(y + 1) * SIDE].fill(200.0);
        }
    }
    let mut meta = read_meta(&sample()).unwrap();
    meta.frame_time_ms = ms;
    meta.us_regions = vec![region()];
    Frame {
        width: SIDE as u32,
        height: SIDE as u32,
        frame_index: n as u32,
        pixels: FramePixels::Gray(values),
        min: 10.0,
        max: 200.0,
        inverted: false,
        meta,
    }
}

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom/CT_small.dcm")
}

fn loop_frames() -> Vec<Arc<Frame>> {
    (0..FRAMES).map(|n| Arc::new(frame(n, Some(20.0)))).collect()
}

const LINE: [[f64; 2]; 2] = [[100.0, 20.0], [100.0, 180.0]];
const SCALE: Calibration = Calibration {
    mm: [0.2, 0.2],
    ms_per_frame: 20.0,
};

/// Where the wall is in a column of a strip, by the middle of its bright part.
fn wall_in(strip: &ruthos_dicom::amm::Strip, frame: usize) -> f64 {
    let bright: Vec<usize> = (0..strip.samples).filter(|s| strip.at(frame, *s) > 100.0).collect();
    bright.iter().sum::<usize>() as f64 / bright.len() as f64
}

#[test]
fn a_strip_follows_a_wall_to_within_a_sample() {
    let strip = read_strip(&loop_frames(), &LINE, SCALE);
    assert_eq!(strip.frames, FRAMES);
    assert!((strip.length_mm() - 32.0).abs() < 1e-6, "{}", strip.length_mm());
    for n in 0..FRAMES {
        let found = 20.0 + wall_in(&strip, n) * strip.mm_per_sample / 0.2;
        assert!(
            (found - wall_at(n)).abs() <= 1.0,
            "frame {n}: {found} against {}",
            wall_at(n)
        );
    }
}

#[test]
fn a_slanted_line_is_measured_in_millimetres() {
    // Pixels twice as tall as they are wide.
    let scale = Calibration {
        mm: [0.2, 0.4],
        ms_per_frame: 20.0,
    };
    let strip = read_strip(&loop_frames(), &[[0.0, 0.0], [30.0, 40.0]], scale);
    // 6 mm across and 16 mm down.
    let want = 6.0f64.hypot(16.0);
    assert!((strip.length_mm() - want).abs() < 1e-6, "{}", strip.length_mm());
}

#[test]
fn a_curve_goes_through_every_point_clicked() {
    let clicked = [[20.0, 100.0], [60.0, 40.0], [120.0, 90.0], [170.0, 30.0]];
    let curve = smooth(&clicked);
    assert!(curve.len() > clicked.len() * 8);
    for p in clicked {
        assert!(curve.iter().any(|c| (c[0] - p[0]).hypot(c[1] - p[1]) < 1e-9), "{p:?}");
    }
    let strip = read_strip(&loop_frames(), &clicked, SCALE);
    assert!(strip.curved);
    let chord: f64 = clicked
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]) * 0.2)
        .sum();
    assert!(strip.length_mm() >= chord, "a curve is no shorter than its chords");
}

#[test]
fn two_points_stay_a_straight_line() {
    assert_eq!(smooth(&LINE), LINE.to_vec());
    assert!(!read_strip(&loop_frames(), &LINE, SCALE).curved);
}

#[test]
fn no_scale_means_no_strip() {
    let good = frame(0, Some(20.0));
    assert_eq!(calibration(&good, &LINE).unwrap().mm, [0.2, 0.2]);

    let no_time = frame(0, None);
    assert!(calibration(&no_time, &LINE).unwrap_err().contains("how long a frame"));

    let outside = [[100.0, 20.0], [100.0, 900.0]];
    assert!(calibration(&good, &outside).unwrap_err().contains("leaves"));

    let mut none = frame(0, Some(20.0));
    none.meta.us_regions.clear();
    assert!(calibration(&none, &LINE).unwrap_err().contains("region"));

    // An M-mode region is time across: no line belongs in it.
    let mut strip_region = frame(0, Some(20.0));
    strip_region.meta.us_regions[0].spatial_format = 2;
    strip_region.meta.us_regions[0].units_x = 4;
    assert!(calibration(&strip_region, &LINE).is_err());
}

#[test]
fn a_slow_loop_says_so() {
    let strip = read_strip(&loop_frames(), &LINE, SCALE);
    assert!(strip.warning().is_none(), "50 frames a second");
    assert_eq!(strip.label(), "Anatomical M-mode, derived at 50 fps");
    let slow = Calibration {
        mm: [0.2, 0.2],
        ms_per_frame: 50.0,
    };
    let warning = read_strip(&loop_frames(), &LINE, slow).warning().unwrap();
    assert!(warning.contains("20 frames"), "{warning}");
}

#[test]
fn a_saved_strip_is_an_m_mode_image_with_its_scale() {
    let strip = read_strip(&loop_frames(), &LINE, SCALE);
    let folder = std::env::temp_dir().join(format!("ruthos-amm-{}", std::process::id()));
    let file = write_strip(&sample(), &strip, (105.0, 190.0), false, &folder).unwrap();

    let meta = read_meta(&file).unwrap();
    assert_eq!(meta.modality, "US");
    assert_eq!(
        meta.study_instance_uid,
        read_meta(&sample()).unwrap().study_instance_uid
    );
    let region = &meta.us_regions[0];
    assert_eq!(region.spatial_format, 2);
    assert!(region.is_time_across(), "the M-mode tools read it as a strip");
    let ms = region.delta_x * 1000.0 * COLUMNS_PER_FRAME as f64;
    assert!((ms - 20.0).abs() < 1e-9, "{ms}");
    assert!((region.delta_y * 10.0 - strip.mm_per_sample).abs() < 1e-9);

    // Time is never made up: every frame is a block of identical columns.
    let saved = decode_frame(&file, 0).unwrap();
    assert_eq!(saved.width as usize, FRAMES * COLUMNS_PER_FRAME);
    assert_eq!(saved.height as usize, strip.samples);
    let FramePixels::Gray(pixels) = &saved.pixels else {
        panic!("a strip is greyscale");
    };
    let width = saved.width as usize;
    for row in pixels.chunks(width) {
        for block in row.chunks(COLUMNS_PER_FRAME) {
            assert!(block.iter().all(|v| v == &block[0]));
        }
    }

    // For `dciodvfy` in CI.
    if let Some(interop) = std::env::var_os("RUTHOS_SEG_INTEROP") {
        std::fs::copy(&file, PathBuf::from(interop).join("ruthos-amm.dcm")).unwrap();
    }
    std::fs::remove_dir_all(folder).ok();
}

#[test]
fn a_smoothed_strip_blends_between_frames_and_keeps_its_scale() {
    let strip = read_strip(&loop_frames(), &LINE, SCALE);
    let folder = std::env::temp_dir().join(format!("ruthos-amm-smooth-{}", std::process::id()));
    let file_of = |smooth| write_strip(&sample(), &strip, (105.0, 190.0), smooth, &folder).unwrap();
    let (blocks, blended) = (file_of(false), file_of(true));

    let (a, b) = (read_meta(&blocks).unwrap(), read_meta(&blended).unwrap());
    assert_eq!(a.us_regions[0].delta_x, b.us_regions[0].delta_x);
    assert_eq!(a.us_regions[0].delta_y, b.us_regions[0].delta_y);

    let pixels = |file: &PathBuf| match decode_frame(file, 0).unwrap().pixels {
        FramePixels::Gray(v) => v,
        _ => panic!("a strip is greyscale"),
    };
    let (blocks, blended) = (pixels(&blocks), pixels(&blended));
    assert_eq!(blocks.len(), blended.len());
    let width = FRAMES * COLUMNS_PER_FRAME;
    let mut softened = false;
    for (hard, soft) in blocks.chunks(width).zip(blended.chunks(width)) {
        let tallest = hard.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        let widest = soft.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        // A step between frames is shared out over the columns of a frame.
        assert!(
            widest <= tallest / COLUMNS_PER_FRAME as f32 + 1.0,
            "{widest} against {tallest}"
        );
        softened |= tallest > 16.0 && widest < tallest / 2.0;
    }
    assert!(softened, "the wall's edge should have softened somewhere");
    std::fs::remove_dir_all(folder).ok();
}
