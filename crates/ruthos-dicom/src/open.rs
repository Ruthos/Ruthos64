//! Opening files, including ones without a DICOM file meta header.
//!
//! Some older systems and exports write a bare data set with no preamble and
//! no group 0002. dicom-rs rejects those by default; Horos accepts them, so
//! Ruthos does too: the data set is read as Explicit VR Little Endian, then
//! Implicit VR Little Endian, and a meta header is synthesised.

use std::io::Cursor;
use std::path::Path;

use dicom_core::Tag;
use dicom_dictionary_std::tags;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject, OpenFileOptions};

use crate::{DicomError, Result};

/// Open a DICOM file, reading up to `read_until` when given.
pub fn open_any(path: &Path, read_until: Option<Tag>) -> Result<DefaultDicomObject> {
    // No element may claim more bytes than the file holds: dicom-rs
    // allocates a value's declared length before reading it.
    crate::structure::check_file(path)?;
    let mut options = OpenFileOptions::new();
    if let Some(tag) = read_until {
        options = options.read_until(tag);
    }
    match options.open_file(path) {
        Ok(obj) => Ok(obj),
        Err(e) => {
            let msg = e.to_string();
            if looks_like_missing_meta(&msg) {
                open_raw_dataset(path)
            } else {
                Err(classify_open_error(msg))
            }
        }
    }
}

fn looks_like_missing_meta(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("meta group")
        || m.contains("meta") && m.contains("parse")
        || m.contains("dicm")
        || m.contains("preamble")
}

pub(crate) fn classify_open_error(msg: String) -> DicomError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("preamble") || lower.contains("dicm") || lower.contains("magic code") {
        DicomError::NotDicom(msg)
    } else {
        DicomError::Read(msg)
    }
}

/// Read a bare data set and give it a synthetic meta header.
fn open_raw_dataset(path: &Path) -> Result<DefaultDicomObject> {
    let bytes = std::fs::read(path)?;
    open_raw_bytes(&bytes)
}

/// Open a DICOM object held in memory: a file with its 128-byte preamble,
/// one starting at "DICM", or a bare data set. For the fuzz targets and
/// anything else that has the bytes already.
pub fn open_from_bytes(bytes: &[u8]) -> Result<DefaultDicomObject> {
    use dicom_object::file::ReadPreamble;
    crate::structure::check_bytes(bytes)?;
    let preamble = if bytes.len() >= 132 && &bytes[128..132] == b"DICM" {
        Some(ReadPreamble::Always)
    } else if bytes.starts_with(b"DICM") {
        Some(ReadPreamble::Never)
    } else {
        None
    };
    match preamble {
        Some(p) => OpenFileOptions::new()
            .read_preamble(p)
            .from_reader(Cursor::new(bytes))
            .map_err(|e| classify_open_error(e.to_string())),
        None => open_raw_bytes(bytes),
    }
}

fn open_raw_bytes(bytes: &[u8]) -> Result<DefaultDicomObject> {
    if bytes.len() < 16 {
        return Err(DicomError::NotDicom("file too small".into()));
    }
    // Quick plausibility check: a data set starts with a group 0008 tag in
    // either VR encoding, or occasionally group 0002/0004/0010.
    let group = u16::from_le_bytes([bytes[0], bytes[1]]);
    if !matches!(group, 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0018 | 0x0020 | 0x0028) {
        return Err(DicomError::NotDicom(
            "no DICOM preamble and no recognisable first tag".into(),
        ));
    }

    let candidates = [
        (
            true,
            dicom_transfer_syntax_registry::entries::EXPLICIT_VR_LITTLE_ENDIAN.erased(),
        ),
        (
            false,
            dicom_transfer_syntax_registry::entries::IMPLICIT_VR_LITTLE_ENDIAN.erased(),
        ),
    ];
    let mut last_err = None;
    for (explicit, ts) in candidates {
        let mut source = bytes;
        if !crate::structure::bare_holds(&mut source, explicit) {
            continue;
        }
        match InMemDicomObject::read_dataset_with_ts(Cursor::new(bytes), &ts) {
            Ok(obj) => {
                let plausible = obj.get(tags::SOP_INSTANCE_UID).is_some()
                    || (obj.get(tags::ROWS).is_some() && obj.get(tags::COLUMNS).is_some());
                if !plausible {
                    last_err = Some("data set has neither SOP Instance UID nor image dimensions".to_string());
                    continue;
                }
                let sop_class = string_or(&obj, tags::SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.7");
                let sop_instance = obj
                    .get(tags::SOP_INSTANCE_UID)
                    .and_then(|e| e.to_str().ok().map(|s| s.trim_end_matches('\0').trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| synthetic_uid(bytes));
                let meta = FileMetaTableBuilder::new()
                    .transfer_syntax(ts.uid())
                    .media_storage_sop_class_uid(sop_class)
                    .media_storage_sop_instance_uid(sop_instance)
                    .build()
                    .map_err(|e| DicomError::Read(e.to_string()))?;
                return Ok(obj.with_exact_meta(meta));
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(DicomError::NotDicom(format!(
        "no file meta header and the data set could not be read: {}",
        last_err.unwrap_or_default()
    )))
}

fn string_or(obj: &InMemDicomObject, tag: Tag, default: &str) -> String {
    obj.get(tag)
        .and_then(|e| e.to_str().ok().map(|s| s.trim_end_matches('\0').trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// A stable UID derived from the file contents (2.25.<decimal of a hash>).
fn synthetic_uid(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes.iter().step_by(7) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let mut h2: u64 = 0x84222325cbf29ce4;
    for b in bytes.iter().skip(3).step_by(11) {
        h2 ^= *b as u64;
        h2 = h2.wrapping_mul(0x100000001b3);
    }
    format!("2.25.{}{}", h, h2 % 1_000_000_007)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_uid_is_valid_shape() {
        let uid = synthetic_uid(b"hello world, this is a test");
        assert!(uid.starts_with("2.25."));
        assert!(uid.len() <= 64);
        assert!(uid.chars().all(|c| c.is_ascii_digit() || c == '.'));
    }
}
