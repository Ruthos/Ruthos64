//! Series thumbnails.
//!
//! A worker pool decodes the representative image of a series, applies the
//! default window, shrinks it to fit a square and caches it as PNG under
//! `THUMBS/`. Results come back as RGBA ready for a GPU texture.

use std::collections::HashSet;
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;

use ruthos_dicom::pixels::{decode_frame, frame_to_gray8};

use crate::paths::DataPaths;

#[derive(Debug, Clone)]
pub struct ThumbnailUpdate {
    pub series_id: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 3);
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

    pub fn cached_path(&self, series_id: i64) -> PathBuf {
        self.paths.thumbs_dir().join(format!("{series_id}.png"))
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
        let _ = std::fs::remove_file(self.cached_path(series_id));
        self.pending.lock().remove(&series_id);
    }
}

fn make_thumbnail(paths: &DataPaths, req: &Request, size: u32) -> anyhow::Result<ThumbnailUpdate> {
    let cached = paths.thumbs_dir().join(format!("{}.png", req.series_id));
    if let Ok(img) = image::open(&cached) {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Ok(ThumbnailUpdate {
            series_id: req.series_id,
            width: w,
            height: h,
            rgba: rgba.into_raw(),
        });
    }

    let frame = decode_frame(&req.file, req.frame)?;
    let (center, width) = frame.default_window();
    let gray = frame_to_gray8(&frame, center, width);
    let src = image::GrayImage::from_raw(frame.width, frame.height, gray)
        .ok_or_else(|| anyhow::anyhow!("thumbnail buffer size mismatch"))?;
    let thumb = image::imageops::thumbnail(&src, fit_w(frame.width, frame.height, size), fit_h(frame.width, frame.height, size));
    let _ = std::fs::create_dir_all(paths.thumbs_dir());
    let _ = thumb.save(&cached);
    let rgba = image::DynamicImage::ImageLuma8(thumb).to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(ThumbnailUpdate {
        series_id: req.series_id,
        width: w,
        height: h,
        rgba: rgba.into_raw(),
    })
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
}
