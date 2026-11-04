//! A structure check run before dicom-rs parses a file.
//!
//! dicom-rs allocates (and zero-fills) an element's declared length before
//! reading its value, so a file of a few bytes that claims a 4 GB element
//! makes it take 4 GB: a denial of service through any import or the
//! listener, found by the fuzzer. This walks the element headers, seeking
//! past values without reading them, and refuses any element, item or
//! fragment that claims more bytes than its container (or the file) holds.
//! Nesting is limited in depth. It reads only headers, so a large file costs
//! a few hundred small reads.

use std::io::{Read, Seek, SeekFrom};

use dicom_core::dictionary::DataDictionary;
use dicom_core::Tag;
use dicom_dictionary_std::StandardDataDictionary;

use crate::{DicomError, Result};

/// The message for a file whose structure does not hold together.
pub const DAMAGED: &str = "the file's structure is damaged: an element claims more data than the file holds";

const MAX_DEPTH: u32 = 64;
const UNDEFINED: u32 = 0xFFFF_FFFF;

/// Random access to the bytes being checked.
pub trait Source {
    fn size(&self) -> u64;
    /// Fill `buf` from `pos`; false if the bytes are not there.
    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool;
}

impl Source for &[u8] {
    fn size(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool {
        let start = pos as usize;
        match self.get(start..start + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    }
}

/// A file read a header at a time.
pub struct FileSource {
    file: std::fs::File,
    len: u64,
}

impl FileSource {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool {
        if pos + buf.len() as u64 > self.len {
            return false;
        }
        self.file.seek(SeekFrom::Start(pos)).is_ok() && self.file.read_exact(buf).is_ok()
    }
}

#[derive(Clone, Copy)]
struct Encoding {
    explicit: bool,
    big_endian: bool,
}

struct Walker<'a, S: Source> {
    src: &'a mut S,
    enc: Encoding,
}

fn damaged() -> DicomError {
    DicomError::Read(DAMAGED.into())
}

impl<S: Source> Walker<'_, S> {
    fn u16(&mut self, pos: u64) -> Option<u16> {
        let mut b = [0u8; 2];
        self.src.read_at(pos, &mut b).then(|| {
            if self.enc.big_endian {
                u16::from_be_bytes(b)
            } else {
                u16::from_le_bytes(b)
            }
        })
    }

    fn u32(&mut self, pos: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.src.read_at(pos, &mut b).then(|| {
            if self.enc.big_endian {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            }
        })
    }

    /// Item tags are little-endian in every little-endian syntax; in
    /// big-endian they follow the syntax.
    fn tag(&mut self, pos: u64) -> Option<Tag> {
        Some(Tag(self.u16(pos)?, self.u16(pos + 2)?))
    }

    /// Walk a data set from `pos` to `end` (or, when `end` is `None`, to an
    /// item delimitation item). Returns the position after it.
    fn dataset(&mut self, mut pos: u64, end: Option<u64>, depth: u32) -> Result<u64> {
        if depth > MAX_DEPTH {
            return Err(damaged());
        }
        loop {
            if let Some(e) = end {
                if pos == e {
                    return Ok(pos);
                }
                if pos > e {
                    return Err(damaged());
                }
            } else if pos >= self.src.size() {
                // Ran off the end inside an undefined-length item.
                return Ok(pos);
            }
            let tag = self.tag(pos).ok_or_else(damaged)?;
            if tag == Tag(0xFFFE, 0xE00D) {
                // Item delimitation: the end of an undefined-length item.
                return Ok(pos + 8);
            }
            if tag.group() == 0xFFFE {
                return Err(damaged());
            }
            let (vr, len, header) = self.header(pos, tag)?;
            let value = pos + header;
            if len == UNDEFINED {
                pos = if tag == Tag(0x7FE0, 0x0010) {
                    self.fragments(value)?
                } else {
                    self.sequence(value, None, depth + 1)?
                };
                continue;
            }
            let value_end = value + len as u64;
            let limit = end.unwrap_or(self.src.size()).min(self.src.size());
            if value_end > limit {
                return Err(damaged());
            }
            let is_sq = vr == *b"SQ" || (!self.enc.explicit && is_sequence_tag(tag));
            pos = if is_sq {
                self.sequence(value, Some(value_end), depth + 1)?
            } else {
                value_end
            };
        }
    }

    /// The VR (or "??" for implicit), the value length and the header size.
    fn header(&mut self, pos: u64, tag: Tag) -> Result<([u8; 2], u32, u64)> {
        // The file meta group is always explicit little-endian.
        let explicit = self.enc.explicit || tag.group() == 0x0002;
        if !explicit {
            let len = self.u32(pos + 4).ok_or_else(damaged)?;
            return Ok((*b"??", len, 8));
        }
        let mut vr = [0u8; 2];
        if !self.src.read_at(pos + 4, &mut vr) {
            return Err(damaged());
        }
        if matches!(
            &vr,
            b"OB" | b"OD" | b"OF" | b"OL" | b"OV" | b"OW" | b"SQ" | b"UC" | b"UN" | b"UR" | b"UT" | b"SV" | b"UV"
        ) {
            let len = self.u32(pos + 8).ok_or_else(damaged)?;
            Ok((vr, len, 12))
        } else {
            let len = self.u16(pos + 6).ok_or_else(damaged)? as u32;
            Ok((vr, len, 8))
        }
    }

    /// Items of a sequence, each a data set, to `end` or a sequence
    /// delimitation item.
    fn sequence(&mut self, mut pos: u64, end: Option<u64>, depth: u32) -> Result<u64> {
        if depth > MAX_DEPTH {
            return Err(damaged());
        }
        loop {
            if let Some(e) = end {
                if pos == e {
                    return Ok(pos);
                }
                if pos > e {
                    return Err(damaged());
                }
            }
            let tag = self.tag(pos).ok_or_else(damaged)?;
            let len = self.u32(pos + 4).ok_or_else(damaged)?;
            match tag {
                Tag(0xFFFE, 0xE0DD) => return Ok(pos + 8),
                Tag(0xFFFE, 0xE000) => {
                    let start = pos + 8;
                    pos = if len == UNDEFINED {
                        self.dataset(start, None, depth + 1)?
                    } else {
                        let item_end = start + len as u64;
                        if item_end > end.unwrap_or(self.src.size()).min(self.src.size()) {
                            return Err(damaged());
                        }
                        self.dataset(start, Some(item_end), depth + 1)?
                    };
                }
                _ => return Err(damaged()),
            }
        }
    }

    /// Encapsulated pixel data: the offset table and fragments, each a
    /// defined-length item of raw bytes, to the sequence delimitation item.
    fn fragments(&mut self, mut pos: u64) -> Result<u64> {
        loop {
            let tag = self.tag(pos).ok_or_else(damaged)?;
            let len = self.u32(pos + 4).ok_or_else(damaged)?;
            match tag {
                Tag(0xFFFE, 0xE0DD) => return Ok(pos + 8),
                Tag(0xFFFE, 0xE000) if len != UNDEFINED => {
                    let next = pos + 8 + len as u64;
                    if next > self.src.size() {
                        return Err(damaged());
                    }
                    pos = next;
                }
                _ => return Err(damaged()),
            }
        }
    }
}

fn is_sequence_tag(tag: Tag) -> bool {
    StandardDataDictionary
        .by_tag(tag)
        .is_some_and(|e| e.vr.relaxed() == dicom_core::VR::SQ)
}

/// The transfer syntax a file meta group names, read from (0002,0010).
fn meta_transfer_syntax<S: Source>(src: &mut S, start: u64, end: u64) -> Option<(String, u64)> {
    let mut w = Walker {
        src,
        enc: Encoding {
            explicit: true,
            big_endian: false,
        },
    };
    let mut pos = start;
    let mut ts = None;
    while pos + 8 <= end {
        let tag = w.tag(pos)?;
        if tag.group() != 0x0002 {
            break;
        }
        let (_, len, header) = w.header(pos, tag).ok()?;
        if len == UNDEFINED {
            return None;
        }
        if tag == Tag(0x0002, 0x0010) && len < 128 {
            let mut v = vec![0u8; len as usize];
            if w.src.read_at(pos + header, &mut v) {
                ts = Some(String::from_utf8_lossy(&v).trim_end_matches(['\0', ' ']).to_string());
            }
        }
        pos += header + len as u64;
        if pos > end {
            return None;
        }
    }
    Some((ts.unwrap_or_default(), pos))
}

const DEFLATED: &str = "1.2.840.10008.1.2.1.99";

/// Reads a `Source` from a position, for the inflater.
struct SourceReader<'a, S: Source> {
    src: &'a mut S,
    pos: u64,
}

impl<S: Source> std::io::Read for SourceReader<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.src.size().saturating_sub(self.pos)) as usize;
        if n > 0 && !self.src.read_at(self.pos, &mut buf[..n]) {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.pos += n as u64;
        Ok(n)
    }
}

/// Deflated data sets hide their lengths, so they are inflated and walked
/// like any other, in explicit little-endian. The cap also stops a small
/// file inflating to gigabytes.
fn check_deflated<S: Source>(src: &mut S, data: u64) -> Result<()> {
    use std::io::Read;
    let cap = crate::guard::limits().series_bytes;
    let mut inflated = Vec::new();
    flate2::read::DeflateDecoder::new(SourceReader { src, pos: data })
        .take(cap + 1)
        .read_to_end(&mut inflated)
        .map_err(|_| damaged())?;
    if inflated.len() as u64 > cap {
        return Err(damaged());
    }
    let total = inflated.len() as u64;
    let enc = Encoding {
        explicit: true,
        big_endian: false,
    };
    let mut plain: &[u8] = &inflated;
    Walker { src: &mut plain, enc }.dataset(0, Some(total), 0).map(|_| ())
}

fn encoding_of(ts: &str) -> Encoding {
    match ts {
        "1.2.840.10008.1.2" => Encoding {
            explicit: false,
            big_endian: false,
        },
        "1.2.840.10008.1.2.2" => Encoding {
            explicit: true,
            big_endian: true,
        },
        _ => Encoding {
            explicit: true,
            big_endian: false,
        },
    }
}

/// Check the structure of a whole DICOM stream: with a preamble, starting
/// at "DICM", or a bare data set (explicit or implicit little-endian).
pub fn check<S: Source>(src: &mut S) -> Result<()> {
    let total = src.size();
    let mut magic = [0u8; 4];
    let start = if src.read_at(128, &mut magic) && &magic == b"DICM" {
        Some(132)
    } else if src.read_at(0, &mut magic) && &magic == b"DICM" {
        Some(4)
    } else {
        None
    };
    match start {
        Some(start) => {
            let (ts, data) = meta_transfer_syntax(src, start, total).ok_or_else(damaged)?;
            if ts == DEFLATED {
                return check_deflated(src, data);
            }
            let enc = encoding_of(&ts);
            Walker { src, enc }.dataset(data, Some(total), 0).map(|_| ())
        }
        None => {
            // A bare data set: whichever little-endian encoding holds.
            for enc in [
                Encoding {
                    explicit: true,
                    big_endian: false,
                },
                Encoding {
                    explicit: false,
                    big_endian: false,
                },
            ] {
                if (Walker { src: &mut *src, enc }).dataset(0, Some(total), 0).is_ok() {
                    return Ok(());
                }
            }
            // No preamble and no data set that holds together: not DICOM
            // (an import skips it rather than reporting an error).
            Err(DicomError::NotDicom(
                "no DICOM preamble and no readable data set".into(),
            ))
        }
    }
}

/// Check a file on disk.
pub fn check_file(path: &std::path::Path) -> Result<()> {
    let mut src = FileSource::open(path)?;
    check(&mut src)
}

/// Check bytes in memory.
pub fn check_bytes(mut bytes: &[u8]) -> Result<()> {
    check(&mut bytes)
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
