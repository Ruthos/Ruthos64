//! Teaching copies: packaging, upload, download and verification, so every
//! participant holds identical pixels under identical UIDs. Only studies
//! flagged as teaching copies are offered for upload, and that flag exists
//! only after the anonymiser's verification pass: the relay cannot inspect
//! a package, so the control on what leaves the machine is on this side.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::http::{new_key, Http};
use crate::SessionError;

const MAX_ATTEMPTS: usize = 3;

/// Letters, digits, hyphens, underscores and full stops, at most 80.
pub fn valid_reference(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 80
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Zip a teaching-copy folder, sidecar included.
pub fn package(folder: &Path) -> Result<Vec<u8>, SessionError> {
    let fail = |e: String| SessionError::Refused(format!("The case could not be packaged: {e}"));
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in walkdir::WalkDir::new(folder).sort_by_file_name() {
        let entry = entry.map_err(|e| fail(e.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(folder).map_err(|e| fail(e.to_string()))?;
        let name = rel.to_string_lossy().replace('\\', "/");
        zip.start_file(name, options).map_err(|e| fail(e.to_string()))?;
        let bytes = std::fs::read(entry.path()).map_err(|e| fail(e.to_string()))?;
        zip.write_all(&bytes).map_err(|e| fail(e.to_string()))?;
    }
    let cursor = zip.finish().map_err(|e| fail(e.to_string()))?;
    Ok(cursor.into_inner())
}

/// Unpack into `to`, refusing paths that climb out of it and links, which a
/// teaching copy never holds.
pub fn unpack(bytes: &[u8], to: &Path) -> Result<PathBuf, SessionError> {
    let fail = |e: String| SessionError::Refused(format!("The case could not be unpacked: {e}"));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| fail(e.to_string()))?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| fail(e.to_string()))?;
        if file.is_symlink() {
            return Err(fail("it holds a link".into()));
        }
        let rel = file
            .enclosed_name()
            .ok_or_else(|| fail("a path leaves the case".into()))?;
        let dest = to.join(rel);
        if file.is_dir() {
            continue;
        }
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| fail(e.to_string()))?;
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| fail(e.to_string()))?;
        std::io::copy(&mut file, &mut out).map_err(|e| fail(e.to_string()))?;
    }
    Ok(to.to_path_buf())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaseEntry {
    #[serde(rename = "ref")]
    pub reference: String,
    pub status: String,
    pub size: u64,
    pub sha256: String,
    pub download_url: Option<String>,
}

/// Upload a package. A 409 at the end means the store holds something else
/// than was declared, so the upload starts again from the declaration.
pub async fn upload(http: &Http, token: &str, reference: &str, bytes: Vec<u8>) -> Result<(), SessionError> {
    if !valid_reference(reference) {
        return Err(SessionError::Refused(
            "A teaching reference may hold only letters, digits, hyphens, underscores and full stops".to_string(),
        ));
    }
    let hex = sha256_hex(&bytes);
    let mut last = None;
    for _ in 0..MAX_ATTEMPTS {
        let body = json!({"ref": reference, "size": bytes.len(), "sha256": hex});
        let slot = http.post_json(token, "/v1/cases", &body, &new_key()).await?;
        let url = slot
            .get("upload_url")
            .and_then(Value::as_str)
            .ok_or_else(|| SessionError::Refused("The server gave no upload address".to_string()))?;
        let headers: Vec<(String, String)> = slot
            .get("headers")
            .and_then(Value::as_object)
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        http.put_bytes(url, &headers, bytes.clone()).await?;
        let path = format!("/v1/cases/{reference}/complete");
        match http.post_json(token, &path, &json!({}), &new_key()).await {
            Ok(_) => return Ok(()),
            Err(e @ SessionError::Api { status: 409, .. }) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| SessionError::Network("upload failed".to_string())))
}

pub async fn list(http: &Http, token: &str, sid: &str) -> Result<Vec<CaseEntry>, SessionError> {
    let v = http.get_json(token, &format!("/v1/sessions/{sid}/cases")).await?;
    let list = v.get("cases").cloned().unwrap_or(v);
    serde_json::from_value(list)
        .map_err(|_| SessionError::Refused("The server sent an unreadable case list".to_string()))
}

/// Check a download against what the list declared.
pub fn verify(bytes: &[u8], entry: &CaseEntry) -> Result<(), SessionError> {
    if !valid_reference(&entry.reference) {
        return Err(SessionError::Refused("The case has an invalid reference".to_string()));
    }
    if sha256_hex(bytes) != entry.sha256.to_ascii_lowercase() {
        return Err(SessionError::Refused(format!(
            "{} failed its checksum and was not imported",
            entry.reference
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_follow_the_relay() {
        assert!(valid_reference("EQ-CARD-2026_014.a"));
        for bad in ["", "has space", "a/b", "a\\b", "é", &"x".repeat(81)] {
            assert!(!valid_reference(bad), "{bad}");
        }
    }

    #[test]
    fn a_corrupted_byte_is_rejected() {
        let good = b"case bytes".to_vec();
        let entry = CaseEntry {
            reference: "A".into(),
            sha256: sha256_hex(&good),
            ..Default::default()
        };
        assert!(verify(&good, &entry).is_ok());
        let mut bad = good.clone();
        bad[0] ^= 1;
        assert!(verify(&bad, &entry).is_err());
    }

    #[test]
    fn a_folder_survives_packaging() {
        let dir = std::env::temp_dir().join(format!("ruthos-case-{}", uuid::Uuid::new_v4()));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("ruthos-case.json"), b"{}").unwrap();
        std::fs::write(src.join("sub").join("a.dcm"), b"pixels").unwrap();
        let bytes = package(&src).unwrap();
        let out = dir.join("out");
        unpack(&bytes, &out).unwrap();
        assert_eq!(std::fs::read(out.join("sub").join("a.dcm")).unwrap(), b"pixels");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
