//! Background frame decoding.
//!
//! Viewers ask for `(image id, frame index)`; worker threads decode and the
//! viewer picks results up on its next paint. Decoded frames are cached by
//! the viewer, not here, so memory is released when a viewer closes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;

use ruthos_dicom::{decode_frame, Frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub image_id: i64,
    pub frame: u32,
}

struct Request {
    key: FrameKey,
    path: PathBuf,
    /// Decode every frame of the file at once (cine loops).
    all_frames: Option<u32>,
}

pub struct LoadedFrame {
    pub key: FrameKey,
    pub result: Result<Arc<Frame>, String>,
}

pub struct FrameLoader {
    tx: Sender<Request>,
    rx: Receiver<LoadedFrame>,
    pending: Mutex<HashSet<FrameKey>>,
}

impl Default for FrameLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameLoader {
    pub fn new() -> Self {
        let (tx, work_rx) = unbounded::<Request>();
        let (done_tx, rx) = unbounded::<LoadedFrame>();
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        for i in 0..workers {
            let work_rx = work_rx.clone();
            let done_tx = done_tx.clone();
            std::thread::Builder::new()
                .name(format!("ruthos-decode-{i}"))
                .spawn(move || {
                    while let Ok(req) = work_rx.recv() {
                        match req.all_frames {
                            Some(n) => match {
                                let started = std::time::Instant::now();
                                let r = ruthos_dicom::decode_all_frames(&req.path);
                                if let Ok(frames) = &r {
                                    tracing::info!(
                                        "decoded loop of {} frames (image {}) in {:.0} ms",
                                        frames.len(),
                                        req.key.image_id,
                                        started.elapsed().as_secs_f64() * 1000.0
                                    );
                                }
                                r
                            } {
                                Ok(frames) => {
                                    let got = frames.len() as u32;
                                    for frame in frames {
                                        let key = FrameKey { image_id: req.key.image_id, frame: frame.frame_index };
                                        let _ = done_tx.send(LoadedFrame { key, result: Ok(Arc::new(frame)) });
                                    }
                                    for k in got..n {
                                        let key = FrameKey { image_id: req.key.image_id, frame: k };
                                        let _ = done_tx.send(LoadedFrame { key, result: Err("frame missing".into()) });
                                    }
                                }
                                Err(e) => {
                                    for k in 0..n {
                                        let key = FrameKey { image_id: req.key.image_id, frame: k };
                                        let _ = done_tx.send(LoadedFrame { key, result: Err(e.to_string()) });
                                    }
                                }
                            },
                            None => {
                                let result = decode_frame(&req.path, req.key.frame)
                                    .map(Arc::new)
                                    .map_err(|e| e.to_string());
                                let _ = done_tx.send(LoadedFrame { key: req.key, result });
                            }
                        }
                    }
                })
                .expect("spawn decode worker");
        }
        Self {
            tx,
            rx,
            pending: Mutex::new(HashSet::new()),
        }
    }

    /// Queue a decode unless one is already in flight for this key.
    pub fn request(&self, key: FrameKey, path: PathBuf) -> bool {
        let mut pending = self.pending.lock();
        if pending.insert(key) {
            let _ = self.tx.send(Request { key, path, all_frames: None });
            true
        } else {
            false
        }
    }

    /// Queue a decode of every frame of a multi-frame image, unless one is
    /// already in flight for it.
    pub fn request_all(&self, image_id: i64, path: PathBuf, frames: u32) -> bool {
        let mut pending = self.pending.lock();
        if (0..frames).any(|k| pending.contains(&FrameKey { image_id, frame: k })) {
            return false;
        }
        for k in 0..frames {
            pending.insert(FrameKey { image_id, frame: k });
        }
        let _ = self.tx.send(Request { key: FrameKey { image_id, frame: 0 }, path, all_frames: Some(frames) });
        true
    }

    pub fn is_pending(&self, key: &FrameKey) -> bool {
        self.pending.lock().contains(key)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    /// Drain finished decodes.
    pub fn poll(&self) -> Vec<LoadedFrame> {
        let mut out = Vec::new();
        while let Ok(f) = self.rx.try_recv() {
            self.pending.lock().remove(&f.key);
            out.push(f);
        }
        out
    }
}
