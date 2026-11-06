//! STOW-RS: sending images to a node, in batches, read from disk as they go.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::client::WebClient;
use super::qido::text;
use crate::scu::SendOutcome;
use crate::NetError;

pub const MOST_IN_BATCH: usize = 50;
pub const MOST_BATCH_BYTES: u64 = 200 * 1024 * 1024;
const MOST_REPLY_BYTES: usize = 64 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

const FAILED_SOP_SEQUENCE: &str = "00081198";
const REFERENCED_SOP_SEQUENCE: &str = "00081199";
const SOP_INSTANCE: &str = "00081155";
const FAILURE_REASON: &str = "00081197";
const WARNING_REASON: &str = "00081196";

/// Files split into batches of at most 50 images or 200 MB. A single file
/// larger than that goes alone.
pub fn batches(files: &[(PathBuf, u64)]) -> Vec<Vec<(PathBuf, u64)>> {
    let mut out: Vec<Vec<(PathBuf, u64)>> = Vec::new();
    let mut bytes = 0;
    for (path, size) in files {
        let full = out
            .last()
            .is_none_or(|b| b.len() >= MOST_IN_BATCH || bytes + size > MOST_BATCH_BYTES);
        if full {
            out.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        if let Some(batch) = out.last_mut() {
            batch.push((path.clone(), *size));
        }
    }
    out
}

/// What a server said about a batch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verdict {
    /// SOP Instance UID and reason.
    pub failed: Vec<(String, String)>,
    pub warned: Vec<(String, String)>,
    pub stored: usize,
}

fn sequence<'a>(reply: &'a Value, tag: &str) -> &'a [Value] {
    reply
        .get(tag)
        .and_then(|e| e.get("Value"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn reason_words(code: &str) -> String {
    match code {
        "" => "no reason given".to_string(),
        "272" | "0110" => "the server failed to process it".to_string(),
        "290" | "0122" => "the server does not take this kind of object".to_string(),
        "42752" | "A700" => "the server is out of space".to_string(),
        "43264" | "A900" => "it does not match what the server expected".to_string(),
        "49152" | "C000" => "the server cannot understand it".to_string(),
        other => format!("code {other}"),
    }
}

/// Read a STOW-RS response dataset.
pub fn read_verdict(reply: &Value) -> Verdict {
    let mut verdict = Verdict::default();
    for item in sequence(reply, FAILED_SOP_SEQUENCE) {
        verdict
            .failed
            .push((text(item, SOP_INSTANCE), reason_words(&text(item, FAILURE_REASON))));
    }
    for item in sequence(reply, REFERENCED_SOP_SEQUENCE) {
        verdict.stored += 1;
        let warning = text(item, WARNING_REASON);
        if !warning.is_empty() {
            verdict.warned.push((text(item, SOP_INSTANCE), warning));
        }
    }
    verdict
}

fn part_head(boundary: &str) -> String {
    format!("--{boundary}\r\nContent-Type: application/dicom\r\n\r\n")
}

fn closing(boundary: &str) -> String {
    format!("--{boundary}--\r\n")
}

/// A batch as a stream, reading each file only as it is needed.
fn body(batch: Vec<(PathBuf, u64)>, boundary: String) -> (reqwest::Body, u64) {
    let length = batch
        .iter()
        .map(|(_, size)| part_head(&boundary).len() as u64 + size + 2)
        .sum::<u64>()
        + closing(&boundary).len() as u64;

    enum At {
        Next,
        Reading(std::fs::File),
        Finished,
    }
    let queue = batch.into_iter().map(|(path, _)| path);
    let stream = futures_util::stream::unfold((queue, At::Next, boundary), |(mut queue, at, boundary)| async move {
        match at {
            At::Finished => None,
            At::Next => match queue.next() {
                Some(path) => match std::fs::File::open(&path) {
                    Ok(file) => {
                        let head = bytes::Bytes::from(part_head(&boundary));
                        Some((Ok(head), (queue, At::Reading(file), boundary)))
                    }
                    Err(e) => Some((Err(e), (queue, At::Finished, boundary))),
                },
                None => {
                    let end = bytes::Bytes::from(closing(&boundary));
                    Some((Ok(end), (queue, At::Finished, boundary)))
                }
            },
            At::Reading(mut file) => {
                let mut buffer = vec![0u8; CHUNK];
                match file.read(&mut buffer) {
                    Ok(0) => {
                        let end_of_part = bytes::Bytes::from_static(b"\r\n");
                        Some((Ok(end_of_part), (queue, At::Next, boundary)))
                    }
                    Ok(n) => {
                        buffer.truncate(n);
                        let piece = bytes::Bytes::from(buffer);
                        Some((Ok(piece), (queue, At::Reading(file), boundary)))
                    }
                    Err(e) => Some((Err(e), (queue, At::Finished, boundary))),
                }
            }
        }
    });
    (reqwest::Body::wrap_stream(stream), length)
}

impl WebClient {
    async fn send_batch(&self, batch: &[(PathBuf, u64)], cancel: &AtomicBool) -> Result<Verdict, NetError> {
        let url = format!("{}/studies", self.node.stow);
        let boundary = format!("ruthos{}", uuid::Uuid::new_v4().simple());
        let content_type = format!("multipart/related; type=\"application/dicom\"; boundary={boundary}");
        let response = self
            .send(|http| {
                let (stream, length) = body(batch.to_vec(), boundary.clone());
                http.post(&url)
                    .header("Accept", "application/dicom+json")
                    .header("Content-Type", &content_type)
                    .header("Content-Length", length)
                    .body(stream)
            })
            .await?;
        let status = response.status();
        // A 409 or 202 carries the same dataset saying which were refused.
        let readable = status.is_success() || status == reqwest::StatusCode::CONFLICT;
        if !readable {
            return Err(NetError::NotDicomWeb(format!("the server answered {status}")));
        }
        let body = self.read_limited(response, MOST_REPLY_BYTES, cancel).await?;
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Verdict {
                stored: batch.len(),
                ..Verdict::default()
            });
        }
        let reply: Value = serde_json::from_slice(&body)
            .map_err(|_| NetError::NotDicomWeb("the reply to a send is not DICOM JSON".into()))?;
        Ok(read_verdict(&reply))
    }

    /// Send files, reporting which were refused and why.
    pub fn send_files(
        &self,
        files: &[PathBuf],
        progress: &dyn Fn(usize, usize),
        cancel: &AtomicBool,
    ) -> Result<SendOutcome, NetError> {
        let mut sized = Vec::with_capacity(files.len());
        let mut outcome = SendOutcome::default();
        for path in files {
            match std::fs::metadata(path) {
                Ok(m) => sized.push((path.clone(), m.len())),
                Err(e) => outcome.failed.push((path.clone(), e.to_string())),
            }
        }
        let total = files.len();
        let mut done = 0;
        for batch in batches(&sized) {
            if cancel.load(Ordering::Relaxed) {
                return Err(NetError::Cancelled);
            }
            let verdict = self.run(self.send_batch(&batch, cancel))?;
            done += batch.len();
            progress(done, total);
            let path_of = |uid: &str| sop_path(&batch, uid);
            outcome.sent += batch.len() - verdict.failed.len().min(batch.len());
            for (uid, reason) in verdict.failed {
                outcome.failed.push((path_of(&uid), reason));
            }
            for (uid, reason) in verdict.warned {
                outcome.warned.push((path_of(&uid), reason));
            }
        }
        Ok(outcome)
    }
}

/// The file in a batch with this SOP Instance UID, for saying which failed.
fn sop_path(batch: &[(PathBuf, u64)], uid: &str) -> PathBuf {
    let found = batch
        .iter()
        .find(|(path, _)| instance_uid(path).as_deref() == Some(uid));
    found.map_or_else(|| PathBuf::from(uid), |(path, _)| path.clone())
}

fn instance_uid(path: &Path) -> Option<String> {
    ruthos_dicom::read_meta(path).ok().map(|m| m.sop_instance_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files(n: usize, size: u64) -> Vec<(PathBuf, u64)> {
        (0..n).map(|i| (PathBuf::from(format!("{i}")), size)).collect()
    }

    #[test]
    fn batches_keep_to_both_limits() {
        let split = batches(&files(120, 10));
        assert_eq!(split.iter().map(Vec::len).collect::<Vec<_>>(), [50, 50, 20]);
        let split = batches(&files(5, 90 * 1024 * 1024));
        assert_eq!(split.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
        let split = batches(&files(2, 300 * 1024 * 1024));
        assert_eq!(split.len(), 2, "one too large still goes, alone");
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn a_verdict_says_what_was_refused_and_warned() {
        let reply = json!({
            "00081198": {"vr": "SQ", "Value": [
                {"00081155": {"vr": "UI", "Value": ["1.1"]}, "00081197": {"vr": "US", "Value": [290]}}
            ]},
            "00081199": {"vr": "SQ", "Value": [
                {"00081155": {"vr": "UI", "Value": ["1.2"]}},
                {"00081155": {"vr": "UI", "Value": ["1.3"]}, "00081196": {"vr": "US", "Value": [45063]}}
            ]},
        });
        let verdict = read_verdict(&reply);
        assert_eq!(verdict.stored, 2);
        assert_eq!(
            verdict.failed,
            [(
                "1.1".to_string(),
                "the server does not take this kind of object".to_string()
            )]
        );
        assert_eq!(verdict.warned, [("1.3".to_string(), "45063".to_string())]);
    }
}
