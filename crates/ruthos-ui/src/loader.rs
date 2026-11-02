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
                        let result = decode_frame(&req.path, req.key.frame)
                            .map(Arc::new)
                            .map_err(|e| e.to_string());
                        let _ = done_tx.send(LoadedFrame { key: req.key, result });
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
            let _ = self.tx.send(Request { key, path });
            true
        } else {
            false
        }
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
