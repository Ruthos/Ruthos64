//! Background frame decoding.
//!
//! Viewers ask for `(image id, frame index)`; worker threads decode and the
//! viewer picks results up on its next paint. Decoded frames are cached by
//! the viewer, not here, so memory is released when a viewer closes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{select, unbounded, Receiver, Sender, TryRecvError};
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

/// The most slices waiting to be decoded before reading ahead holds off, so
/// that a change of direction is not left behind a queue of unwanted slices.
const MOST_PENDING_AHEAD: usize = 16;

/// The most decoded slices kept for a viewer that has not come for them, which
/// may be one that has closed.
const MOST_HELD: usize = 64;

/// The next piece of work: what a viewer is waiting for, before reading ahead.
fn next_request(wanted: &Receiver<Request>, ahead: &Receiver<Request>) -> Option<Request> {
    match wanted.try_recv() {
        Ok(request) => return Some(request),
        Err(TryRecvError::Disconnected) => return None,
        Err(TryRecvError::Empty) => {}
    }
    select! {
        recv(wanted) -> request => request.ok(),
        recv(ahead) -> request => request.ok(),
    }
}

pub struct FrameLoader {
    tx: Sender<Request>,
    ahead_tx: Sender<Request>,
    rx: Receiver<LoadedFrame>,
    /// Decoded, and waiting for the viewer that wants them. Viewers share this
    /// loader, so one must not take what another asked for.
    held: Mutex<std::collections::VecDeque<LoadedFrame>>,
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
        let (ahead_tx, ahead_rx) = unbounded::<Request>();
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
            let ahead_rx = ahead_rx.clone();
            let done_tx = done_tx.clone();
            let (pending, one_at_a_time, notices) = (pending.clone(), one_at_a_time.clone(), notices.clone());
            std::thread::Builder::new()
                .name(format!("ruthos-decode-{i}"))
                .spawn(move || {
                    while let Some(req) = next_request(&work_rx, &ahead_rx) {
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
            ahead_tx,
            rx,
            held: Mutex::new(std::collections::VecDeque::new()),
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

    /// Queue a slice the viewer may want next, behind anything it is waiting for.
    pub fn request_ahead(&self, key: FrameKey, path: PathBuf) -> bool {
        let mut pending = self.pending.lock();
        if pending.len() >= MOST_PENDING_AHEAD || !pending.insert(key) {
            return false;
        }
        let _ = self.ahead_tx.send(Request {
            key,
            path,
            all_frames: None,
        });
        true
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
        self.pending.lock().len() > self.held.lock().len()
    }

    /// Take the finished decodes of the images a viewer holds. The rest stay
    /// for the viewer they belong to.
    pub fn poll_for(&self, mine: &dyn Fn(i64) -> bool) -> Vec<LoadedFrame> {
        let mut held = self.held.lock();
        held.extend(self.rx.try_iter());
        let mut out = Vec::new();
        let mut pending = self.pending.lock();
        for frame in std::mem::take(&mut *held) {
            if mine(frame.key.image_id) {
                pending.remove(&frame.key);
                out.push(frame);
            } else {
                held.push_back(frame);
            }
        }
        while held.len() > MOST_HELD {
            if let Some(old) = held.pop_front() {
                pending.remove(&old.key);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(image_id: i64) -> Request {
        Request {
            key: FrameKey { image_id, frame: 0 },
            path: PathBuf::new(),
            all_frames: None,
        }
    }

    #[test]
    fn what_a_viewer_is_waiting_for_goes_before_reading_ahead() {
        let (wanted_tx, wanted) = unbounded();
        let (ahead_tx, ahead) = unbounded();
        for i in 0..5 {
            ahead_tx.send(request(i)).unwrap();
        }
        wanted_tx.send(request(99)).unwrap();
        assert_eq!(next_request(&wanted, &ahead).unwrap().key.image_id, 99);
        assert_eq!(next_request(&wanted, &ahead).unwrap().key.image_id, 0);
    }

    #[test]
    fn work_stops_only_when_a_loader_is_gone() {
        let (wanted_tx, wanted) = unbounded();
        let (_ahead_tx, ahead) = unbounded::<Request>();
        drop(wanted_tx);
        assert!(next_request(&wanted, &ahead).is_none());
    }

    fn sample() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom/CT_small.dcm")
    }

    fn wait_for(loader: &FrameLoader, mine: &dyn Fn(i64) -> bool) -> Vec<LoadedFrame> {
        for _ in 0..500 {
            let got = loader.poll_for(mine);
            if !got.is_empty() {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Vec::new()
    }

    #[test]
    fn one_viewer_cannot_take_what_another_asked_for() {
        let loader = FrameLoader::new();
        let key = FrameKey { image_id: 7, frame: 0 };
        assert!(loader.request(key, sample()));
        // Another viewer polls until the slice is certainly done.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(loader.poll_for(&|id| id == 8).is_empty());
        assert!(loader.is_pending(&key), "still on its way to the viewer that asked");
        let got = wait_for(&loader, &|id| id == 7);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, key);
        assert!(!loader.is_pending(&key));
    }

    #[test]
    fn a_slice_nobody_comes_for_does_not_keep_the_window_repainting() {
        let loader = FrameLoader::new();
        loader.request(FrameKey { image_id: 7, frame: 0 }, sample());
        for _ in 0..500 {
            loader.poll_for(&|_| false);
            if !loader.has_pending() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the loader still says it is busy");
    }

    #[test]
    fn reading_ahead_holds_off_once_enough_is_waiting() {
        let loader = FrameLoader::new();
        let mut asked = 0;
        for i in 0..100 {
            let key = FrameKey {
                image_id: 1000 + i,
                frame: 0,
            };
            // A path that does not exist still fails only when a worker reaches it.
            if loader.request_ahead(key, PathBuf::from("missing.dcm")) {
                asked += 1;
            }
            if loader.pending.lock().len() >= MOST_PENDING_AHEAD {
                assert!(!loader.request_ahead(FrameKey { image_id: 1, frame: 0 }, PathBuf::new()));
            }
        }
        assert!(asked >= MOST_PENDING_AHEAD);
    }
}
