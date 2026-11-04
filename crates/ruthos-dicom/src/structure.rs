//! A structure check run before dicom-rs parses a file.
//!
//! dicom-rs allocates (and zero-fills) an element's declared length before
//! reading its value, so a file of a few bytes that claims a 4 GB element
//! makes it take 4 GB: a denial of service through any import or the
//! listener, found by the fuzzer. This refuses any element or fragment that
//! claims more bytes than the file holds, and limits nesting.
//!
//! The check is only worth anything if it reads a file exactly as the parser
//! will: a difference lets a file steer the parser past it, and the fuzzer
//! found several. So the data set is walked by dicom-parser's own lazy reader,
//! which gives each length before its value is read, and the transfer syntax
//! and its adapter are handled as `dicom-object` handles them. Only the file
//! meta group is read here, because the parser's reader for it allocates, and
//! that loop follows `FileMetaTable::read_from`. Values are seeked past, so a
//! large file costs a few hundred small reads.

use std::cell::Cell;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

use dicom_core::Tag;
use dicom_encoding::transfer_syntax::{Codec, TransferSyntax, TransferSyntaxIndex};
use dicom_parser::dataset::lazy_read::LazyDataSetReader;
use dicom_parser::dataset::LazyDataToken;
use dicom_parser::stateful::decode::StatefulDecode;
use dicom_transfer_syntax_registry::{entries, TransferSyntaxRegistry};

use crate::{DicomError, Result};

/// The message for a file whose structure does not hold together.
pub const DAMAGED: &str = "the file's structure is damaged: an element claims more data than the file holds";

const MAX_DEPTH: u32 = 64;
const UNDEFINED: u32 = 0xFFFF_FFFF;

fn damaged() -> DicomError {
    DicomError::Read(DAMAGED.to_string())
}

fn at<R: Read + Seek>(src: &mut R, pos: u64, buf: &mut [u8]) -> Option<()> {
    src.seek(SeekFrom::Start(pos)).ok()?;
    src.read_exact(buf).ok()
}

fn u16_at<R: Read + Seek>(src: &mut R, pos: u64) -> Option<u16> {
    let mut b = [0u8; 2];
    at(src, pos, &mut b)?;
    Some(u16::from_le_bytes(b))
}

fn u32_at<R: Read + Seek>(src: &mut R, pos: u64) -> Option<u32> {
    let mut b = [0u8; 4];
    at(src, pos, &mut b)?;
    Some(u32::from_le_bytes(b))
}

/// A meta element's tag, value length and header size, as the parser's
/// explicit little-endian header decoder reads them.
fn meta_header<R: Read + Seek>(src: &mut R, pos: u64) -> Option<(Tag, u32, u64)> {
    let tag = Tag(u16_at(src, pos)?, u16_at(src, pos + 2)?);
    if tag.group() == 0xFFFE {
        return Some((tag, u32_at(src, pos + 4)?, 8));
    }
    let mut vr = [0u8; 2];
    at(src, pos + 4, &mut vr)?;
    // Only these have a 16-bit length: anything unknown is UN, with 32 bits.
    if matches!(
        &vr,
        b"AE"
            | b"AS"
            | b"AT"
            | b"CS"
            | b"DA"
            | b"DS"
            | b"DT"
            | b"FL"
            | b"FD"
            | b"IS"
            | b"LO"
            | b"LT"
            | b"PN"
            | b"SH"
            | b"SL"
            | b"SS"
            | b"ST"
            | b"TM"
            | b"UI"
            | b"UL"
            | b"US"
    ) {
        Some((tag, u16_at(src, pos + 6)? as u32, 8))
    } else {
        Some((tag, u32_at(src, pos + 8)?, 12))
    }
}

/// The transfer syntax a meta group names, and where the data set starts.
/// The parser ends the group after the bytes its length gives, whatever tags
/// it meets. Without a group length it refuses the header, and the tags
/// decide here.
fn meta_group<R: Read + Seek>(src: &mut R, start: u64, end: u64) -> Option<(String, u64)> {
    let mut pos = start;
    let mut ts = None;
    let mut stop = None;
    if pos + 12 <= end {
        let (tag, len, header) = meta_header(src, pos)?;
        if tag == Tag(0x0002, 0x0000) && len == 4 {
            stop = Some(pos + header + 4 + u32_at(src, pos + header)? as u64);
            pos += header + 4;
        }
    }
    while pos + 8 <= end {
        let (tag, len, header) = meta_header(src, pos)?;
        let done = match stop {
            Some(stop) => pos >= stop,
            None => tag.group() != 0x0002,
        };
        if done {
            break;
        }
        if len == UNDEFINED {
            return None;
        }
        if tag == Tag(0x0002, 0x0010) && len < 4096 {
            let mut v = vec![0u8; len as usize];
            at(src, pos + header, &mut v)?;
            let text = String::from_utf8_lossy(&v);
            ts = Some(
                text.trim_end_matches(|c: char| c.is_whitespace() || c == '\0')
                    .to_string(),
            );
        }
        pos += header + len as u64;
        if pos > end {
            return None;
        }
    }
    Some((ts.unwrap_or_default(), pos))
}

/// A source whose reads can be counted off without touching it. The parser's
/// `seek` does not move its own position, which sequence bounds depend on, and
/// its `skip_bytes` reads every byte. So a value is seeked past for real, then
/// skipped through here to bring the position along.
struct Skipper<R> {
    inner: R,
    pending: Rc<Cell<u64>>,
}

impl<R: Read> Read for Skipper<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pending = self.pending.get();
        if pending == 0 {
            return self.inner.read(buf);
        }
        let n = pending.min(buf.len() as u64);
        self.pending.set(pending - n);
        Ok(n as usize)
    }
}

impl<R: Seek> Seek for Skipper<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A defined length that cannot fit in the file, whatever position it is at.
fn beyond(len: u32, total: u64) -> bool {
    len != UNDEFINED && len as u64 > total
}

/// Walk a data set from the source's position with the parser's own reader.
fn walk<R: Read + Seek>(src: &mut R, ts: &TransferSyntax, total: u64) -> Result<()> {
    let pending = Rc::new(Cell::new(0));
    let source = Skipper {
        inner: src,
        pending: pending.clone(),
    };
    let mut reader = LazyDataSetReader::new_with_ts(source, ts).map_err(|_| damaged())?;
    let mut depth = 0u32;
    while let Some(token) = reader.advance() {
        let (mut decoder, len) = match token.map_err(|_| damaged())? {
            LazyDataToken::SequenceStart { len, .. } => {
                depth += 1;
                if depth > MAX_DEPTH || beyond(len.0, total) {
                    return Err(damaged());
                }
                continue;
            }
            LazyDataToken::ItemStart { len } => {
                if beyond(len.0, total) {
                    return Err(damaged());
                }
                continue;
            }
            LazyDataToken::SequenceEnd => {
                depth = depth.saturating_sub(1);
                continue;
            }
            LazyDataToken::LazyValue { header, decoder } => (decoder, header.len.0),
            LazyDataToken::LazyItemValue { len, decoder } => (decoder, len),
            _ => continue,
        };
        let value_end = decoder.position() + len as u64;
        if len == UNDEFINED || value_end > total {
            return Err(damaged());
        }
        decoder.seek(value_end).map_err(|_| damaged())?;
        pending.set(len as u64);
        decoder.skip_bytes(len).map_err(|_| damaged())?;
    }
    Ok(())
}

/// Does a data set with no header hold together in this encoding? The parser
/// must try only those that do: a sound implicit file read as explicit takes
/// value bytes for lengths.
pub fn bare_holds(bytes: &[u8], explicit: bool) -> bool {
    holds(&mut Cursor::new(bytes), explicit, bytes.len() as u64)
}

fn holds<R: Read + Seek>(src: &mut R, explicit: bool, total: u64) -> bool {
    let ts = if explicit {
        entries::EXPLICIT_VR_LITTLE_ENDIAN.erased()
    } else {
        entries::IMPLICIT_VR_LITTLE_ENDIAN.erased()
    };
    src.seek(SeekFrom::Start(0)).is_ok() && walk(src, &ts, total).is_ok()
}

/// Check the data set at `pos` under the transfer syntax the meta group names.
fn check_data_set<R: Read + Seek>(src: &mut R, uid: &str, pos: u64, total: u64) -> Result<()> {
    // An unknown syntax, or one whose adapter is missing, is refused by the
    // parser before it reads the data set, with a better message than this.
    let Some(ts) = TransferSyntaxRegistry.get(uid) else {
        return Ok(());
    };
    src.seek(SeekFrom::Start(pos)).map_err(|_| damaged())?;
    match ts.codec() {
        // Deflated data sets hide their lengths, so they are inflated with
        // the parser's adapter and walked. The cap also stops a small file
        // inflating to gigabytes.
        Codec::Dataset(Some(adapter)) => {
            let cap = crate::guard::limits().series_bytes;
            let mut inflated = Vec::new();
            adapter
                .adapt_reader(Box::new(&mut *src))
                .take(cap + 1)
                .read_to_end(&mut inflated)
                .map_err(|_| damaged())?;
            if inflated.len() as u64 > cap {
                return Err(damaged());
            }
            let total = inflated.len() as u64;
            walk(&mut Cursor::new(inflated), ts, total)
        }
        Codec::Dataset(None) => Ok(()),
        Codec::None | Codec::EncapsulatedPixelData(..) => walk(src, ts, total),
    }
}

/// Check the structure of a whole DICOM stream: with a preamble, starting
/// "DICM", or a bare data set.
pub fn check<R: Read + Seek>(src: &mut R) -> Result<()> {
    let total = src.seek(SeekFrom::End(0)).map_err(|_| damaged())?;
    let mut magic = [0u8; 4];
    let start = if at(src, 128, &mut magic).is_some() && &magic == b"DICM" {
        Some(132)
    } else if at(src, 0, &mut magic).is_some() && &magic == b"DICM" {
        Some(4)
    } else {
        None
    };
    match start {
        Some(start) => {
            let (ts, data) = meta_group(src, start, total).ok_or_else(damaged)?;
            check_data_set(src, &ts, data, total)
        }
        None => {
            // A bare data set: whichever little-endian encoding holds.
            if holds(src, true, total) || holds(src, false, total) {
                return Ok(());
            }
            // No preamble and no data set that holds together: not DICOM
            // (an import skips it rather than reporting an error).
            Err(DicomError::NotDicom(
                "no DICOM preamble and no readable data set".into(),
            ))
        }
    }
}

/// `check` on a file on disk.
pub fn check_file(path: &std::path::Path) -> Result<()> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
    check(&mut file)
}

/// `check` on bytes in memory.
pub fn check_bytes(bytes: &[u8]) -> Result<()> {
    check(&mut Cursor::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(tag: (u16, u16), vr: &[u8; 2], value: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&tag.0.to_le_bytes());
        d.extend_from_slice(&tag.1.to_le_bytes());
        d.extend_from_slice(vr);
        if matches!(vr, b"OB" | b"OW" | b"SQ" | b"UN" | b"UT") {
            d.extend_from_slice(&[0, 0]);
            d.extend_from_slice(&(value.len() as u32).to_le_bytes());
        } else {
            d.extend_from_slice(&(value.len() as u16).to_le_bytes());
        }
        d.extend_from_slice(value);
        d
    }

    fn file(dataset: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 128];
        d.extend_from_slice(b"DICM");
        d.extend(element((2, 0x10), b"UI", b"1.2.840.10008.1.2.1\0"));
        d.extend_from_slice(dataset);
        d
    }

    #[test]
    fn a_sound_file_passes() {
        let mut ds = element((8, 0x60), b"CS", b"US");
        ds.extend(element((0x7FE0, 0x10), b"OW", &[0u8; 64]));
        assert!(check_bytes(&file(&ds)).is_ok());
    }

    #[test]
    fn an_element_longer_than_the_file_is_refused() {
        let mut ds = element((8, 0x60), b"CS", b"US");
        // Pixel Data claiming 4 GB with 8 bytes behind it.
        ds.extend_from_slice(&[0xE0, 0x7F, 0x10, 0x00, b'O', b'W', 0, 0, 0xF0, 0xFF, 0xFF, 0xFF]);
        ds.extend_from_slice(&[0u8; 8]);
        let e = check_bytes(&file(&ds)).unwrap_err().to_string();
        assert!(e.contains("damaged"), "{e}");
    }

    #[test]
    fn nested_lengths_must_fit_their_container() {
        // A sequence of 20 bytes whose item claims 1000.
        let mut item = vec![0xFE, 0xFF, 0x00, 0xE0];
        item.extend_from_slice(&1000u32.to_le_bytes());
        item.extend_from_slice(&[0u8; 12]);
        let ds = element((8, 0x1115), b"SQ", &item);
        assert!(check_bytes(&file(&ds)).is_err());
        // And a sound nested item passes.
        let inner = element((8, 0x1150), b"UI", b"1.2.3\0");
        let mut item = vec![0xFE, 0xFF, 0x00, 0xE0];
        item.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        item.extend_from_slice(&inner);
        assert!(check_bytes(&file(&element((8, 0x1115), b"SQ", &item))).is_ok());
    }

    #[test]
    fn fragments_must_fit_the_file() {
        let mut ds = vec![0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
        // Offset table (empty), then a fragment claiming 1 MB.
        ds.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0]);
        ds.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
        ds.extend_from_slice(&(1u32 << 20).to_le_bytes());
        ds.extend_from_slice(&[0u8; 16]);
        assert!(check_bytes(&file(&ds)).is_err());
    }

    #[test]
    fn deep_nesting_is_refused() {
        // 100 undefined-length sequences inside each other.
        let mut ds = Vec::new();
        for _ in 0..100 {
            ds.extend_from_slice(&[0x08, 0x00, 0x15, 0x11, b'S', b'Q', 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
            ds.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0, 0xFF, 0xFF, 0xFF, 0xFF]);
        }
        assert!(check_bytes(&file(&ds)).is_err());
    }

    #[test]
    fn every_sample_passes() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "dcm") {
                check_file(&p).unwrap_or_else(|err| panic!("{}: {err}", p.display()));
            }
        }
    }
}
