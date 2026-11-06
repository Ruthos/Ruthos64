//! WADO-RS: bringing images into the archive.
//!
//! Each part goes straight to a file in the incoming folder, where the importer
//! files it as it does a C-STORE. Large studies never sit in memory. Names are
//! generated here: nothing a server says about a part reaches the disk.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use parking_lot::Mutex;

use super::client::{next_chunk, Transfer, WebClient};
use super::multipart::{framing, ReadError, Reader, Sink};
use crate::scu::{RetrieveMethod, RetrieveOutcome, RetrieveProgress};
use crate::NetError;

/// A single part larger than this is not an image.
const MOST_PART_BYTES: u64 = 32 * 1024 * 1024 * 1024;

fn accept(transfer: &str) -> String {
    format!("multipart/related; type=\"application/dicom\"; transfer-syntax={transfer}")
}

/// Writes parts into a folder under generated names. Until a part is complete
/// it has a `.part` name, which the incoming scanner leaves alone.
struct Folder<'a> {
    dir: &'a Path,
    open: Option<(std::fs::File, PathBuf, u64)>,
    written: Vec<PathBuf>,
    on_part: &'a mut dyn FnMut(),
}

impl Sink for Folder<'_> {
    fn begin(&mut self, _content_type: Option<&str>) -> std::io::Result<()> {
        let path = self.dir.join(format!("web-{}.part", uuid::Uuid::new_v4().simple()));
        self.open = Some((std::fs::File::create(&path)?, path, 0));
        Ok(())
    }

    fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some((file, _, total)) = self.open.as_mut() {
            *total += bytes.len() as u64;
            if *total > MOST_PART_BYTES {
                return Err(std::io::Error::other("a part is too large to be an image"));
            }
            file.write_all(bytes)?;
        }
        Ok(())
    }

    fn end(&mut self) -> std::io::Result<()> {
        if let Some((file, path, _)) = self.open.take() {
            drop(file);
            let done = path.with_extension("dcm");
            std::fs::rename(&path, &done)?;
            self.written.push(done);
            (self.on_part)();
        }
        Ok(())
    }
}

impl Folder<'_> {
    /// A part left unfinished is not an image.
    fn abandon(&mut self) {
        if let Some((file, path, _)) = self.open.take() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// What came of one request, and whether more could follow.
struct Fetched {
    files: Vec<PathBuf>,
    /// Why the response ended early, with what arrived kept.
    stopped: Option<NetError>,
}

impl WebClient {
    /// One WADO-RS GET, writing its parts into `out`.
    async fn fetch(
        &self,
        url: &str,
        out: &Path,
        on_part: &mut dyn FnMut(),
        cancel: &AtomicBool,
    ) -> Result<Fetched, NetError> {
        let preferred = self.node.transfer.parameter();
        let mut response = self
            .send(|http| http.get(url).header("Accept", accept(preferred)))
            .await?;
        // Fall back to what the server has as stored.
        if response.status() == reqwest::StatusCode::NOT_ACCEPTABLE && self.node.transfer != Transfer::Original {
            response = self
                .send(|http| http.get(url).header("Accept", accept(Transfer::Original.parameter())))
                .await?;
        }
        let status = response.status();
        if !status.is_success() {
            return Err(NetError::NotDicomWeb(format!("the server answered {status}")));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|c| c.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Some(frame) = framing(&content_type) else {
            return Err(self.not_dicom(&response, "multipart/related"));
        };
        for what in &frame.deviations {
            self.report_deviation(what);
        }
        let mut reader = Reader::new(&frame.boundary).with_most_parts(self.node.most_parts);
        let mut sink = Folder {
            dir: out,
            open: None,
            written: Vec::new(),
            on_part,
        };
        let stopped = loop {
            match next_chunk(&mut response, cancel).await {
                Ok(Some(chunk)) => match reader.feed(&chunk, &mut sink) {
                    Ok(()) => {
                        if reader.finished() {
                            break None;
                        }
                    }
                    Err(ReadError::TooManyParts) => {
                        break Some(NetError::TooLarge(format!("more than {} images", reader.parts())));
                    }
                    Err(e) => break Some(NetError::Other(e.to_string())),
                },
                Ok(None) => {
                    break (!reader.finished()).then(|| NetError::NotDicomWeb("the reply ended early".into()));
                }
                Err(e) => break Some(e),
            }
        };
        sink.abandon();
        Ok(Fetched {
            files: sink.written,
            stopped,
        })
    }

    /// Fetch, trying once more after a stall. Parts already written stay, and
    /// the importer knows a duplicate when it meets one.
    async fn fetch_with_retry(
        &self,
        url: &str,
        out: &Path,
        on_part: &mut dyn FnMut(),
        cancel: &AtomicBool,
    ) -> Result<Fetched, NetError> {
        let mut first = self.fetch(url, out, on_part, cancel).await?;
        if first.stopped == Some(NetError::Timeout) {
            tracing::warn!(node = %self.node.name, "a retrieve stalled; trying once more");
            let second = self.fetch(url, out, on_part, cancel).await?;
            first.files.extend(second.files);
            first.stopped = second.stopped;
        }
        Ok(first)
    }

    /// Fetch what `have` lacks of one series. A series with nothing local comes
    /// in one request. WADO-RS has no resume inside a multipart response, so a
    /// series begun earlier is listed and its missing instances asked for singly.
    #[allow(clippy::too_many_arguments)]
    async fn fetch_series(
        &self,
        study: &str,
        series: &str,
        out: &Path,
        have: &(dyn Fn(&str) -> bool + Sync),
        started: bool,
        progress: &Mutex<(RetrieveProgress, Vec<PathBuf>)>,
        report: &(dyn Fn(RetrieveProgress) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Option<NetError>, NetError> {
        let base = format!("{}/studies/{study}/series/{series}", self.node.wado);
        let mut count = || {
            let mut state = progress.lock();
            state.0.completed += 1;
            state.0.received += 1;
            state.0.remaining = state.0.remaining.map(|r| r.saturating_sub(1));
            report(state.0);
        };
        if !started {
            let fetched = self.fetch_with_retry(&base, out, &mut count, cancel).await?;
            progress.lock().1.extend(fetched.files);
            return Ok(fetched.stopped);
        }
        let instances = self.instances_of(study, series, cancel).await?;
        let total = instances.len() as u32;
        let missing: Vec<String> = instances
            .into_iter()
            .map(|i| i.sop_instance_uid)
            .filter(|uid| !have(uid))
            .collect();
        {
            let mut state = progress.lock();
            let skipped = total - missing.len() as u32;
            state.0.remaining = state.0.remaining.map(|r| r.saturating_sub(skipped));
            report(state.0);
        }
        for uid in missing {
            let url = format!("{base}/instances/{uid}");
            let fetched = self.fetch_with_retry(&url, out, &mut count, cancel).await?;
            progress.lock().1.extend(fetched.files);
            if fetched.stopped.is_some() {
                return Ok(fetched.stopped);
            }
        }
        Ok(None)
    }

    /// Bring a study, or one series of it, into `out`. `have` says whether an
    /// instance is already in the archive, and `started` whether a series has
    /// any there. Images already written stay when this stops, for any reason.
    #[allow(clippy::too_many_arguments)]
    pub fn retrieve(
        &self,
        study: &str,
        series: Option<&str>,
        out: &Path,
        have: &(dyn Fn(&str) -> bool + Sync),
        started: &(dyn Fn(&str) -> bool + Sync),
        report: &(dyn Fn(RetrieveProgress) + Sync),
        cancel: &AtomicBool,
    ) -> Result<RetrieveOutcome, NetError> {
        std::fs::create_dir_all(out).map_err(|e| NetError::Other(e.to_string()))?;
        self.run(async {
            let listed = self.series_of(study, cancel).await?;
            let chosen: Vec<_> = listed
                .iter()
                .filter(|s| series.is_none_or(|wanted| s.series_uid == wanted))
                .collect();
            let known: Option<u32> = chosen.iter().map(|s| s.instances).sum();
            let progress = Mutex::new((
                RetrieveProgress {
                    remaining: known,
                    ..RetrieveProgress::default()
                },
                Vec::new(),
            ));
            let parallel = self.node.parallel.clamp(1, 8);
            let results: Vec<_> = futures_util::stream::iter(chosen)
                .map(|s| {
                    let begun = started(&s.series_uid);
                    let progress = &progress;
                    async move {
                        self.fetch_series(study, &s.series_uid, out, have, begun, progress, report, cancel)
                            .await
                    }
                })
                .buffer_unordered(parallel)
                .collect()
                .await;
            let (state, files) = progress.into_inner();
            // Any failure is reported, and the files stay in the incoming folder.
            for result in results {
                let problem = match result {
                    Ok(stopped) => stopped,
                    Err(e) => Some(e),
                };
                if let Some(problem) = problem {
                    return Err(problem);
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(NetError::Cancelled);
            }
            Ok(RetrieveOutcome {
                method: RetrieveMethod::Web,
                progress: state,
                files,
            })
        })
    }
}
