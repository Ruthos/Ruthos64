//! Series thumbnails.
//!
//! A worker pool decodes the representative image of a series, applies the
//! default window, shrinks it to fit a square and caches it as PNG under
//! `THUMBS/`. Colour images keep their colour. Results come back as RGBA
//! ready for a GPU texture, with whether the image shows colour flow.

use std::collections::HashSet;
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;

use ruthos_dicom::pixels::{decode_frame, frame_to_gray8, frame_to_rgb8};
use ruthos_dicom::Clut;

use crate::paths::DataPaths;

#[derive(Debug, Clone)]
pub struct ThumbnailUpdate {
    pub series_id: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Colour Doppler: flow painted over the image.
    pub colour_flow: bool,
}

struct Request {
    series_id: i64,
    file: PathBuf,
    frame: u32,
}

pub struct ThumbnailService {
    tx: Sender<Request>,
    rx: Receiver<ThumbnailUpdate>,
    pending: Mutex<HashSet<i64>>,
    paths: DataPaths,
}

impl ThumbnailService {
    pub fn new(paths: DataPaths, size: u32) -> Self {
        let (tx, work_rx) = unbounded::<Request>();
        let (done_tx, rx) = unbounded::<ThumbnailUpdate>();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 3);
        for i in 0..workers {
            let work_rx = work_rx.clone();
            let done_tx = done_tx.clone();
            let paths = paths.clone();
            std::thread::Builder::new()
                .name(format!("ruthos-thumbs-{i}"))
                .spawn(move || {
                    while let Ok(req) = work_rx.recv() {
                        match make_thumbnail(&paths, &req, size) {
                            Ok(update) => {
                                let _ = done_tx.send(update);
                            }
                            Err(e) => {
                                tracing::debug!("thumbnail for series {} failed: {e}", req.series_id);
                                // Send a placeholder so the UI stops asking.
                                let _ = done_tx.send(placeholder(req.series_id, size));
                            }
                        }
                    }
                })
                .expect("spawn thumbnail worker");
        }
        Self {
            tx,
            rx,
            pending: Mutex::new(HashSet::new()),
            paths,
        }
    }

    /// Where a thumbnail is kept: `<id>.c.png`, or `<id>.d.png` when it
    /// shows colour flow. (`<id>.png` is the old greyscale kind, remade.)
    fn cached(paths: &DataPaths, series_id: i64, colour_flow: bool) -> PathBuf {
        let kind = if colour_flow { "d" } else { "c" };
        paths.thumbs_dir().join(format!("{series_id}.{kind}.png"))
    }

    /// Ask for a thumbnail. Repeated requests for the same series are ignored
    /// until the first completes.
    pub fn request(&self, series_id: i64, file: PathBuf, frame: u32) {
        let mut pending = self.pending.lock();
        if pending.insert(series_id) {
            let _ = self.tx.send(Request { series_id, file, frame });
        }
    }

    /// Drain completed thumbnails.
    pub fn poll(&self) -> Vec<ThumbnailUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = self.rx.try_recv() {
            self.pending.lock().remove(&u.series_id);
            out.push(u);
        }
        out
    }

    /// Forget a cached thumbnail (after a delete).
    pub fn invalidate(&self, series_id: i64) {
        for name in [
            format!("{series_id}.png"),
            format!("{series_id}.c.png"),
            format!("{series_id}.d.png"),
        ] {
            let _ = std::fs::remove_file(self.paths.thumbs_dir().join(name));
        }
        self.pending.lock().remove(&series_id);
    }
}

fn make_thumbnail(paths: &DataPaths, req: &Request, size: u32) -> anyhow::Result<ThumbnailUpdate> {
    for colour_flow in [false, true] {
        if let Ok(img) = image::open(ThumbnailService::cached(paths, req.series_id, colour_flow)) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            return Ok(ThumbnailUpdate {
                series_id: req.series_id,
                width: w,
                height: h,
                rgba: rgba.into_raw(),
                colour_flow,
            });
        }
    }

    let frame = decode_frame(&req.file, req.frame)?;
    let (center, width) = frame.default_window();
    let (tw, th) = (
        fit_w(frame.width, frame.height, size),
        fit_h(frame.width, frame.height, size),
    );
    let (thumb, colour_flow) = if frame.is_color() {
        let rgb = frame_to_rgb8(&frame, center, width, &Clut::grayscale());
        let src = image::RgbImage::from_raw(frame.width, frame.height, rgb)
            .ok_or_else(|| anyhow::anyhow!("thumbnail buffer size mismatch"))?;
        // Judged on the whole image, where a thin jet is still many pixels;
        // the region says so outright when the scanner wrote it.
        let flow = frame.meta.us_regions.iter().any(|r| r.data_type == 2) || shows_colour_flow(src.as_raw());
        (
            image::DynamicImage::ImageRgb8(image::imageops::thumbnail(&src, tw, th)),
            flow,
        )
    } else {
        let gray = frame_to_gray8(&frame, center, width);
        let src = image::GrayImage::from_raw(frame.width, frame.height, gray)
            .ok_or_else(|| anyhow::anyhow!("thumbnail buffer size mismatch"))?;
        (
            image::DynamicImage::ImageLuma8(image::imageops::thumbnail(&src, tw, th)),
            false,
        )
    };
    let _ = std::fs::create_dir_all(paths.thumbs_dir());
    let _ = thumb.save(ThumbnailService::cached(paths, req.series_id, colour_flow));
    let rgba = thumb.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(ThumbnailUpdate {
        series_id: req.series_id,
        width: w,
        height: h,
        rgba: rgba.into_raw(),
        colour_flow,
    })
}

/// Whether an RGB picture shows colour Doppler: enough strongly red or
/// blue pixels. Green is left out (the ECG trace), as are the faint tints
/// some scanners give their greyscale, and a few coloured words.
pub fn shows_colour_flow(rgb: &[u8]) -> bool {
    let mut total = 0usize;
    let mut flow = 0usize;
    for p in rgb.as_chunks::<3>().0 {
        total += 1;
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let hi = r.max(g).max(b);
        if hi - r.min(g).min(b) < 60 {
            continue;
        }
        if (r == hi && r - b >= 60) || (b == hi && b - r >= 60) {
            flow += 1;
        }
    }
    // One pixel in two hundred.
    total > 0 && flow * 200 >= total
}

fn fit_w(w: u32, h: u32, size: u32) -> u32 {
    if w >= h {
        size
    } else {
        ((w as f32 / h as f32) * size as f32).round().max(1.0) as u32
    }
}

fn fit_h(w: u32, h: u32, size: u32) -> u32 {
    if h >= w {
        size
    } else {
        ((h as f32 / w as f32) * size as f32).round().max(1.0) as u32
    }
}

fn placeholder(series_id: i64, size: u32) -> ThumbnailUpdate {
    let n = (size * size) as usize;
    let mut rgba = Vec::with_capacity(n * 4);
    for _ in 0..n {
        rgba.extend_from_slice(&[40, 40, 40, 255]);
    }
    ThumbnailUpdate {
        series_id,
        width: size,
        height: size,
        rgba,
        colour_flow: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect() {
        assert_eq!((fit_w(200, 100, 100), fit_h(200, 100, 100)), (100, 50));
        assert_eq!((fit_w(100, 400, 100), fit_h(100, 400, 100)), (25, 100));
        assert_eq!((fit_w(50, 50, 100), fit_h(50, 50, 100)), (100, 100));
    }

    #[test]
    fn colour_flow_is_red_and_blue_not_green_or_a_tint() {
        let picture = |n: usize, px: [u8; 3]| {
            let mut v = vec![60u8; 1000 * 3];
            for i in 0..n {
                v[i * 3..i * 3 + 3].copy_from_slice(&px);
            }
            v
        };
        assert!(shows_colour_flow(&picture(20, [220, 40, 30])), "a red jet");
        assert!(shows_colour_flow(&picture(20, [30, 80, 230])), "a blue jet");
        assert!(!shows_colour_flow(&picture(200, [40, 220, 60])), "an ECG trace");
        assert!(!shows_colour_flow(&picture(1000, [120, 100, 80])), "a sepia tint");
        assert!(!shows_colour_flow(&picture(2, [220, 40, 30])), "a red word");
    }
}
