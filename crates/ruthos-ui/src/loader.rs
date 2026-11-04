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
    pending: Arc<Mutex<HashSet<FrameKey>>>,
    /// Loops too large to hold whole in the memory allowed: they are
    /// decoded a frame at a time.
    one_at_a_time: Arc<Mutex<HashSet<i64>>>,
    /// Messages for the viewer (a loop opened a frame at a time, say).
    notices: Arc<Mutex<Vec<String>>>,
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
        let pending: Arc<Mutex<HashSet<FrameKey>>> = Arc::new(Mutex::new(HashSet::new()));
        let one_at_a_time: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
        let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 4);
        for i in 0..workers {
            let work_rx = work_rx.clone();
            let done_tx = done_tx.clone();
            let (pending, one_at_a_time, notices) = (pending.clone(), one_at_a_time.clone(), notices.clone());
            std::thread::Builder::new()
                .name(format!("ruthos-decode-{i}"))
                .spawn(move || {
                    while let Ok(req) = work_rx.recv() {
                        match req.all_frames {
                            Some(n) => {
                                let started = std::time::Instant::now();
                                let decoded = ruthos_dicom::decode_all_frames(&req.path);
                                if let Ok(frames) = &decoded {
                                    tracing::info!(
                                        "decoded loop of {} frames (image {}) in {:.0} ms",
                                        frames.len(),
                                        req.key.image_id,
                                        started.elapsed().as_secs_f64() * 1000.0
                                    );
                                }
                                match decoded {
                                    Ok(frames) => {
                                        let got = frames.len() as u32;
                                        for frame in frames {
                                            let key = FrameKey {
                                                image_id: req.key.image_id,
                                                frame: frame.frame_index,
                                            };
                                            let _ = done_tx.send(LoadedFrame {
                                                key,
                                                result: Ok(Arc::new(frame)),
                                            });
                                        }
                                        for k in got..n {
                                            let key = FrameKey {
                                                image_id: req.key.image_id,
                                                frame: k,
                                            };
                                            let _ = done_tx.send(LoadedFrame {
                                                key,
                                                result: Err("frame missing".into()),
                                            });
                                        }
                                    }
                                    // Too large to hold whole: from now on this
                                    // loop is decoded a frame at a time, starting
                                    // with the one asked for.
                                    Err(e) if e.to_string().contains("memory") => {
                                        tracing::warn!("image {}: {e}; decoding frame by frame", req.key.image_id);
                                        one_at_a_time.lock().insert(req.key.image_id);
                                        notices.lock().push(format!(
                                            "This loop is too large to hold in memory whole ({e}); it is shown a frame at a time"
                                        ));
                                        {
                                            let mut p = pending.lock();
                                            for k in (0..n).filter(|k| *k != req.key.frame) {
                                                p.remove(&FrameKey {
                                                    image_id: req.key.image_id,
                                                    frame: k,
                                                });
                                            }
                                        }
                                        let result = decode_frame(&req.path, req.key.frame)
                                            .map(Arc::new)
                                            .map_err(|e| e.to_string());
                                        let _ = done_tx.send(LoadedFrame { key: req.key, result });
                                    }
                                    Err(e) => {
                                        for k in 0..n {
                                            let key = FrameKey {
                                                image_id: req.key.image_id,
                                                frame: k,
                                            };
                                            let _ = done_tx.send(LoadedFrame {
                                                key,
                                                result: Err(e.to_string()),
                                            });
                                        }
                                    }
                                }
                            }
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
            pending,
            one_at_a_time,
            notices,
        }
    }

    /// Queue a decode unless one is already in flight for this key.
    pub fn request(&self, key: FrameKey, path: PathBuf) -> bool {
        let mut pending = self.pending.lock();
        if pending.insert(key) {
            let _ = self.tx.send(Request {
                key,
                path,
                all_frames: None,
            });
            true
        } else {
            false
        }
    }

    /// Messages for the viewer since the last call.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock())
    }

    /// Queue a decode of every frame of a multi-frame image, unless one is
    /// already in flight for it. A loop too large to hold whole is decoded
    /// a frame at a time instead, starting with `wanted`.
    pub fn request_all(&self, image_id: i64, path: PathBuf, frames: u32, wanted: u32) -> bool {
        if self.one_at_a_time.lock().contains(&image_id) {
            return self.request(
                FrameKey {
                    image_id,
                    frame: wanted,
                },
                path,
            );
        }
        let mut pending = self.pending.lock();
        if (0..frames).any(|k| pending.contains(&FrameKey { image_id, frame: k })) {
            return false;
        }
        for k in 0..frames {
            pending.insert(FrameKey { image_id, frame: k });
        }
        let _ = self.tx.send(Request {
            key: FrameKey {
                image_id,
                frame: wanted,
            },
            path,
            all_frames: Some(frames),
        });
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
