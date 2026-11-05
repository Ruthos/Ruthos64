//! The decode guard: every frame passes here before a decoder sees it.
//!
//! Three of Ruthos's decoders are C libraries (libjpeg-turbo, OpenJPEG and
//! CharLS) outside Rust's bounds checks. A crafted file can ask them for a
//! huge image or describe its pixels one way in the DICOM attributes and
//! another in the codestream. The guard refuses both before any decoding:
//!
//! - the attributes must describe an image of a sane size and layout
//!   (sides, pixels per frame, frames, samples, bits);
//! - native pixel data must be exactly as long as the attributes imply;
//! - encapsulated frames must not be enormous;
//! - for JPEG, JPEG 2000 and JPEG-LS, the codec's own header (read by the
//!   small parsers below, which read only the marker segments they need)
//!   must agree with the attributes.
//!
//! The limits sit well above real veterinary images and can be raised in
//! Preferences to at most four times the defaults; nothing in a file can
//! raise them. Every failure is an `Err`, never a panic.

use dicom_encoding::adapters::PixelDataObject;
use std::sync::RwLock;

use dicom_dictionary_std::tags;

use crate::meta::InstanceMeta;
use crate::{DicomError, Result};

/// The guard's limits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Rows and Columns each at most this.
    pub side: u32,
    /// Rows x Columns x Samples per Pixel at most this.
    pub pixels: u64,
    pub frames: u32,
    /// One encapsulated frame's fragments together.
    pub fragment_bytes: u64,
    /// The estimated decoded size of a whole series or loop.
    pub series_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            side: 16_384,
            pixels: 67_108_864,
            frames: 10_000,
            fragment_bytes: 512 * 1024 * 1024,
            series_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// The most a user can raise each limit to: four times the default.
    pub fn ceiling() -> Limits {
        let d = Limits::default();
        Limits {
            side: d.side * 4,
            pixels: d.pixels * 4,
            frames: d.frames * 4,
            fragment_bytes: d.fragment_bytes * 4,
            series_bytes: d.series_bytes * 4,
        }
    }

    /// These limits held to the ceiling and to at least the defaults'
    /// tenth, so a settings file cannot switch the guard off.
    pub fn clamped(self) -> Limits {
        let (c, d) = (Limits::ceiling(), Limits::default());
        Limits {
            side: self.side.clamp(d.side / 10, c.side),
            pixels: self.pixels.clamp(d.pixels / 10, c.pixels),
            frames: self.frames.clamp(d.frames / 10, c.frames),
            fragment_bytes: self.fragment_bytes.clamp(d.fragment_bytes / 10, c.fragment_bytes),
            series_bytes: self.series_bytes.clamp(d.series_bytes / 10, c.series_bytes),
        }
    }
}

static LIMITS: RwLock<Option<Limits>> = RwLock::new(None);

/// The limits in force.
pub fn limits() -> Limits {
    LIMITS.read().ok().and_then(|l| *l).unwrap_or_default()
}

/// Set the limits (from Preferences); held to the ceiling.
pub fn set_limits(l: Limits) {
    if let Ok(mut g) = LIMITS.write() {
        *g = Some(l.clamped());
    }
}

/// Set limits exactly, below the floor `set_limits` keeps: for tests only.
#[doc(hidden)]
pub fn set_limits_exact(l: Limits) {
    if let Ok(mut g) = LIMITS.write() {
        *g = Some(l);
    }
}

/// The message for a file whose codestream disagrees with its attributes.
pub const MISMATCH: &str = "the image data does not match its description in the file";

fn refuse(msg: impl Into<String>) -> DicomError {
    DicomError::Unsupported(msg.into())
}

/// Check the attributes that describe the pixels.
pub fn check_attributes(meta: &InstanceMeta) -> Result<()> {
    let l = limits();
    let (rows, cols) = (meta.rows, meta.columns);
    if rows == 0 || cols == 0 || rows > l.side || cols > l.side {
        return Err(refuse(format!(
            "the image is {cols} x {rows} pixels; Ruthos opens images up to {} pixels a side",
            l.side
        )));
    }
    let spp = meta.samples_per_pixel.max(1) as u64;
    if spp != 1 && spp != 3 {
        return Err(refuse(format!(
            "{spp} samples per pixel is not an image layout Ruthos reads"
        )));
    }
    let pixels = rows as u64 * cols as u64 * spp;
    if pixels > l.pixels {
        return Err(refuse(format!(
            "a frame of {pixels} samples is larger than Ruthos opens ({})",
            l.pixels
        )));
    }
    let frames = meta.number_of_frames.max(1);
    if frames > l.frames {
        return Err(refuse(format!(
            "{frames} frames is more than Ruthos opens ({})",
            l.frames
        )));
    }
    let ba = meta.bits_allocated;
    if ![1, 8, 16, 32].contains(&ba) {
        return Err(refuse(format!("{ba} bits allocated is not a layout Ruthos reads")));
    }
    if meta.bits_stored == 0 || meta.bits_stored > ba {
        return Err(refuse(format!(
            "{} bits stored in {ba} bits allocated is not a valid layout",
            meta.bits_stored
        )));
    }
    Ok(())
}

/// Bytes native pixel data must hold for these attributes.
pub fn native_length(meta: &InstanceMeta) -> u64 {
    let samples = meta.rows as u64
        * meta.columns as u64
        * meta.samples_per_pixel.max(1) as u64
        * meta.number_of_frames.max(1) as u64;
    if meta.bits_allocated == 1 {
        samples.div_ceil(8)
    } else {
        samples * (meta.bits_allocated as u64 / 8)
    }
}

/// Check native pixel data's length against the attributes: equal, give or
/// take one padding byte. A file with a frame or two of trailing data is
/// accepted only if it is at least as long as needed; a short one never.
pub fn check_native_length(meta: &InstanceMeta, actual: u64) -> Result<()> {
    let want = native_length(meta);
    if actual + 1 < want {
        tracing::warn!("pixel data is {actual} bytes, attributes imply {want}");
        return Err(refuse(format!(
            "{MISMATCH} (the pixel data is shorter than the image it describes)"
        )));
    }
    if actual > want + 1 {
        tracing::warn!("pixel data is {actual} bytes, attributes imply {want}");
        return Err(refuse(format!(
            "{MISMATCH} (the pixel data is longer than the image it describes)"
        )));
    }
    Ok(())
}

/// The kind of codestream a transfer syntax carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Jpeg,
    Jpeg2000,
    JpegLs,
    Rle,
    Other,
}

pub fn codec_of(transfer_syntax: &str) -> Codec {
    match transfer_syntax.trim_end_matches('\0') {
        "1.2.840.10008.1.2.4.50" | "1.2.840.10008.1.2.4.51" | "1.2.840.10008.1.2.4.57" | "1.2.840.10008.1.2.4.70" => {
            Codec::Jpeg
        }
        "1.2.840.10008.1.2.4.90"
        | "1.2.840.10008.1.2.4.91"
        | "1.2.840.10008.1.2.4.92"
        | "1.2.840.10008.1.2.4.93"
        | "1.2.840.10008.1.2.4.201"
        | "1.2.840.10008.1.2.4.202"
        | "1.2.840.10008.1.2.4.203" => Codec::Jpeg2000,
        "1.2.840.10008.1.2.4.80" | "1.2.840.10008.1.2.4.81" => Codec::JpegLs,
        "1.2.840.10008.1.2.5" => Codec::Rle,
        _ => Codec::Other,
    }
}

/// Width, height, components and precision from a codec header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecHeader {
    pub width: u32,
    pub height: u32,
    pub components: u32,
    pub precision: u32,
}

fn be16(d: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]) as u32)
}

fn be32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(i)?,
        *d.get(i + 1)?,
        *d.get(i + 2)?,
        *d.get(i + 3)?,
    ]))
}

/// The frame header of a JPEG (SOF0 to SOF15) or JPEG-LS (SOF55) stream:
/// walks marker segments from SOI, reading only their lengths, to the
/// first frame header.
fn sof(data: &[u8], jpeg_ls: bool) -> Option<CodecHeader> {
    if data.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    // A real header comes within the first few kilobytes; a stream of
    // markers that never reaches one is not walked for ever.
    for _ in 0..512 {
        // Skip fill bytes.
        while *data.get(i)? == 0xFF && *data.get(i + 1)? == 0xFF {
            i += 1;
        }
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        i += 2;
        // Markers without a segment.
        if matches!(marker, 0x01 | 0xD0..=0xD8) {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return None; // end of image or start of scan before a frame header
        }
        let len = be16(data, i)? as usize;
        if len < 2 {
            return None;
        }
        let is_sof = if jpeg_ls {
            marker == 0xF7
        } else {
            (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
        };
        if is_sof {
            return Some(CodecHeader {
                precision: *data.get(i + 2)? as u32,
                height: be16(data, i + 3)?,
                width: be16(data, i + 5)?,
                components: *data.get(i + 7)? as u32,
            });
        }
        i += len;
    }
    None
}

pub fn jpeg_header(data: &[u8]) -> Option<CodecHeader> {
    sof(data, false)
}

pub fn jpeg_ls_header(data: &[u8]) -> Option<CodecHeader> {
    sof(data, true)
}

/// The SIZ marker of a JPEG 2000 codestream: image size less its offset,
/// and the component count. DICOM carries the bare codestream, which must
/// start with SOC (FF4F) and SIZ (FF51); a JP2 file wrapper is refused, as
/// it is not valid in DICOM and would reach a box parser Ruthos has no use
/// for.
pub fn jpeg2000_header(data: &[u8]) -> Option<CodecHeader> {
    if data.get(0..4)? != [0xFF, 0x4F, 0xFF, 0x51] {
        return None;
    }
    let i = 4; // at Lsiz
    let xsiz = be32(data, i + 4)?;
    let ysiz = be32(data, i + 8)?;
    let xo = be32(data, i + 12)?;
    let yo = be32(data, i + 16)?;
    let csiz = be16(data, i + 36)?;
    let ssiz = *data.get(i + 38)? as u32;
    if xo > xsiz || yo > ysiz {
        return None;
    }
    Some(CodecHeader {
        width: xsiz - xo,
        height: ysiz - yo,
        components: csiz,
        precision: (ssiz & 0x7F) + 1,
    })
}

/// Check one frame's codestream against the attributes.
/// How many bytes a PackBits segment decodes to, stopping at `enough`.
fn packbits_length(segment: &[u8], enough: u64) -> u64 {
    let (mut i, mut out) = (0, 0u64);
    while i < segment.len() && out < enough {
        let h = segment[i] as i8;
        i += 1;
        if h >= 0 {
            let n = h as usize + 1;
            out += n.min(segment.len() - i) as u64;
            i += n;
        } else if h != -128 {
            if i >= segment.len() {
                break;
            }
            out += 1 + h.unsigned_abs() as u64;
            i += 1;
        }
    }
    out
}

/// An RLE frame: its 64-byte header (DICOM part 5, annex G) must describe the
/// segments the image needs, and each must hold a full plane. The decoder
/// behind dicom-pixeldata indexes all of this unchecked and panics.
fn check_rle(meta: &InstanceMeta, data: &[u8]) -> Result<()> {
    let bad = |what: &str| {
        tracing::warn!("RLE frame: {what}");
        refuse(format!("{MISMATCH} ({what})"))
    };
    if !matches!(meta.bits_allocated, 8 | 16) {
        return Err(bad("RLE needs 8 or 16 bits allocated"));
    }
    if data.len() < 64 {
        return Err(bad("the RLE header is cut short"));
    }
    let word = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
    let count = word(0);
    let needed = meta.samples_per_pixel.max(1) as usize * (meta.bits_allocated as usize / 8);
    if count > 15 || count < needed {
        return Err(bad("the wrong number of RLE segments"));
    }
    let plane = meta.rows as u64 * meta.columns as u64;
    for n in 0..needed {
        let start = word(4 + 4 * n);
        let end = if n + 1 < count { word(8 + 4 * n) } else { data.len() };
        let Some(segment) = data.get(start..end) else {
            return Err(bad("an RLE segment lies outside its frame"));
        };
        if packbits_length(segment, plane) < plane {
            return Err(bad("an RLE segment is too short for the image"));
        }
    }
    Ok(())
}

pub fn check_codestream(meta: &InstanceMeta, codec: Codec, data: &[u8]) -> Result<()> {
    let l = limits();
    if data.len() as u64 > l.fragment_bytes {
        return Err(refuse(format!(
            "a compressed frame of {} MB is larger than Ruthos opens",
            data.len() / (1024 * 1024)
        )));
    }
    let header = match codec {
        Codec::Jpeg => jpeg_header(data),
        Codec::Jpeg2000 => jpeg2000_header(data),
        Codec::JpegLs => jpeg_ls_header(data),
        Codec::Rle => return check_rle(meta, data),
        Codec::Other => return Ok(()),
    };
    let Some(h) = header else {
        tracing::warn!("no {codec:?} frame header found in the codestream");
        return Err(refuse(format!("{MISMATCH} (no image header in the compressed data)")));
    };
    let spp = meta.samples_per_pixel.max(1) as u32;
    // A JPEG may give its height as 0 and define it later (DNL); the width
    // and components must still agree.
    let height_ok = h.height == meta.rows || (codec == Codec::Jpeg && h.height == 0);
    let precision_ok = codec != Codec::Jpeg || h.precision >= meta.bits_stored as u32;
    if h.width != meta.columns || !height_ok || h.components != spp || !precision_ok {
        tracing::warn!(
            "{codec:?} header {}x{}, {} components, precision {} against attributes {}x{}, {} samples, {} bits stored",
            h.width,
            h.height,
            h.components,
            h.precision,
            meta.columns,
            meta.rows,
            spp,
            meta.bits_stored
        );
        return Err(refuse(MISMATCH));
    }
    Ok(())
}

/// Check a file's pixel data before decoding: attributes always; native
/// length, or each requested frame's codestream (`frames` lists them,
/// empty for all).
pub fn check_object(obj: &dicom_object::DefaultDicomObject, meta: &InstanceMeta, frames: &[u32]) -> Result<()> {
    check_attributes(meta)?;
    let element = obj
        .element(tags::PIXEL_DATA)
        .map_err(|_| DicomError::Missing("Pixel Data"))?;
    let codec = codec_of(&meta.transfer_syntax);
    match element.fragments() {
        None => {
            let len = element
                .value()
                .primitive()
                .map(|p| p.calculate_byte_len() as u64)
                .unwrap_or(0);
            check_native_length(meta, len)
        }
        Some(fragments) => {
            let total: u64 = fragments.iter().map(|f| f.len() as u64).sum();
            let n = meta.number_of_frames.max(1) as usize;
            if total > limits().fragment_bytes * n as u64 {
                return Err(refuse("the compressed image data is larger than Ruthos opens"));
            }
            if codec == Codec::Other {
                return Ok(());
            }
            // Part 5 gives each RLE frame exactly one fragment, and every
            // fragment goes through the decoder.
            if codec == Codec::Rle && fragments.len() != n {
                return Err(refuse(format!("{MISMATCH} (RLE needs one fragment per frame)")));
            }
            // Check what the decoder will be given. It finds a frame through
            // the object's own offset table, which a file can point anywhere,
            // so this asks the same code rather than guessing at the layout.
            let wanted: Vec<u32> = if frames.is_empty() {
                (0..n as u32).collect()
            } else {
                frames.to_vec()
            };
            for f in wanted {
                let data = obj
                    .frame_pixel_data(f)
                    .ok_or_else(|| refuse(format!("{MISMATCH} (frame {f} is not in the image data)")))?;
                check_codestream(meta, codec, &data)?;
            }
            Ok(())
        }
    }
}

/// All the guard asks of an object that a decoder is about to read whole, for
/// any route other than the viewer's: converting a file to send it, say.
pub fn check_decoding(obj: &dicom_object::DefaultDicomObject) -> Result<()> {
    let mut meta = crate::meta::meta_from_object(obj);
    meta.transfer_syntax = obj.meta().transfer_syntax().trim_end_matches('\0').to_string();
    check_object(obj, &meta, &[])?;
    check_series_bytes(decoded_bytes(&meta))
}

/// The decoded size of an image, as Ruthos holds it: four bytes a sample
/// for greyscale (32-bit float) and four for colour (RGBA).
pub fn decoded_bytes(meta: &InstanceMeta) -> u64 {
    meta.rows as u64 * meta.columns as u64 * 4 * meta.number_of_frames.max(1) as u64
}

/// Whether decoding `bytes` at once stays within the series limit.
pub fn check_series_bytes(bytes: u64) -> Result<()> {
    let l = limits().series_bytes;
    if bytes > l {
        return Err(refuse(format!(
            "this needs {} MB of memory once decoded, more than the {} MB Ruthos allows",
            bytes / (1024 * 1024),
            l / (1024 * 1024)
        )));
    }
    Ok(())
}

/// A zero-filled buffer of `n` items, or an error if memory cannot be had,
/// instead of aborting the process.
pub fn try_zeroed<T: Clone + Default>(n: usize) -> Result<Vec<T>> {
    let mut v: Vec<T> = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| DicomError::Pixels(format!("not enough memory for {n} samples")))?;
    v.resize(n, T::default());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(cols: u32, rows: u32, spp: u16, bits: u16, frames: u32) -> InstanceMeta {
        InstanceMeta {
            rows,
            columns: cols,
            samples_per_pixel: spp,
            bits_allocated: bits,
            bits_stored: bits,
            number_of_frames: frames,
            ..Default::default()
        }
    }

    /// A minimal JPEG prefix: SOI, an APP0 segment, then SOFn.
    fn jpeg(marker: u8, w: u16, h: u16, comps: u8, precision: u8) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46];
        d.extend_from_slice(&[0xFF, marker, 0x00, 0x08 + 3 * comps, precision]);
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&w.to_be_bytes());
        d.push(comps);
        for c in 0..comps {
            d.extend_from_slice(&[c + 1, 0x11, 0]);
        }
        d
    }

    fn j2k(w: u32, h: u32, xo: u32, yo: u32, comps: u16) -> Vec<u8> {
        let mut d = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 41, 0x00, 0x00];
        for v in [w, h, xo, yo, w, h, 0, 0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d.extend_from_slice(&comps.to_be_bytes());
        for _ in 0..comps {
            d.extend_from_slice(&[7, 1, 1]);
        }
        d
    }

    #[test]
    fn a_jpeg_claiming_a_huge_image_is_refused() {
        let m = meta(512, 512, 1, 8, 1);
        let huge = jpeg(0xC0, 65_535, 65_535, 1, 8);
        assert_eq!(jpeg_header(&huge).unwrap().width, 65_535);
        let e = check_codestream(&m, Codec::Jpeg, &huge).unwrap_err().to_string();
        assert!(e.contains(MISMATCH), "{e}");
        assert!(check_codestream(&m, Codec::Jpeg, &jpeg(0xC0, 512, 512, 1, 8)).is_ok());
        // Lossless (SOF3) at 12 bits for 10 bits stored is fine; 8 is not.
        let mut ten = m;
        ten.bits_stored = 10;
        ten.bits_allocated = 16;
        assert!(check_codestream(&ten, Codec::Jpeg, &jpeg(0xC3, 512, 512, 1, 12)).is_ok());
        assert!(check_codestream(&ten, Codec::Jpeg, &jpeg(0xC3, 512, 512, 1, 8)).is_err());
        // DHT (C4) is not a frame header.
        assert!(jpeg_header(&jpeg(0xC4, 1, 1, 1, 8)).is_none());
    }

    #[test]
    fn jpeg_2000_and_jpeg_ls_headers_must_agree() {
        let m = meta(300, 200, 1, 16, 1);
        assert!(check_codestream(&m, Codec::Jpeg2000, &j2k(300, 200, 0, 0, 1)).is_ok());
        // With an offset the image is the size less the offset.
        assert!(check_codestream(&m, Codec::Jpeg2000, &j2k(310, 210, 10, 10, 1)).is_ok());
        assert!(check_codestream(&m, Codec::Jpeg2000, &j2k(4000, 200, 0, 0, 1)).is_err());
        assert!(check_codestream(&m, Codec::Jpeg2000, &j2k(300, 200, 0, 0, 3)).is_err());
        let ls = jpeg(0xF7, 300, 200, 1, 16);
        assert!(check_codestream(&m, Codec::JpegLs, &ls).is_ok());
        assert!(check_codestream(&m, Codec::JpegLs, &jpeg(0xF7, 301, 200, 1, 16)).is_err());
        // A JP2 file wrapper is not DICOM and is refused.
        let mut jp2 = vec![0, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10];
        jp2.extend(j2k(300, 200, 0, 0, 1));
        assert!(check_codestream(&m, Codec::Jpeg2000, &jp2).is_err());
        // Garbage is refused, not trusted.
        assert!(check_codestream(&m, Codec::Jpeg2000, &[1, 2, 3]).is_err());
    }

    #[test]
    fn rle_headers_must_describe_full_segments() {
        let m = meta(4, 2, 1, 8, 1);
        // One segment: a literal run of 8 bytes.
        let frame = |count: u32, offset: u32, body: &[u8]| {
            let mut d = vec![0u8; 64];
            d[0..4].copy_from_slice(&count.to_le_bytes());
            d[4..8].copy_from_slice(&offset.to_le_bytes());
            d.extend_from_slice(body);
            d
        };
        let good = [7u8, 1, 2, 3, 4, 5, 6, 7, 8];
        assert!(check_codestream(&m, Codec::Rle, &frame(1, 64, &good)).is_ok());
        // A replicate run counts for its full length: -7 gives 8 bytes.
        assert!(check_codestream(&m, Codec::Rle, &frame(1, 64, &[0xF9, 9])).is_ok());
        // What the fuzzer found: a segment that decodes short of the image.
        let short = check_codestream(&m, Codec::Rle, &frame(1, 64, &[3, 1, 2, 3, 4])).unwrap_err();
        assert!(short.to_string().contains("too short"), "{short}");
        // A literal run cut off by the end of the data.
        assert!(check_codestream(&m, Codec::Rle, &frame(1, 64, &[7, 1, 2])).is_err());
        // A header cut short, a huge count, no segments, offsets outside.
        assert!(check_codestream(&m, Codec::Rle, &[1, 0, 0]).is_err());
        assert!(check_codestream(&m, Codec::Rle, &frame(u32::MAX, 64, &good)).is_err());
        assert!(check_codestream(&m, Codec::Rle, &frame(0, 64, &good)).is_err());
        assert!(check_codestream(&m, Codec::Rle, &frame(1, 5000, &good)).is_err());
        // 12 bits allocated has no RLE form, and 16 needs two segments.
        assert!(check_codestream(&meta(4, 2, 1, 12, 1), Codec::Rle, &frame(1, 64, &good)).is_err());
        assert!(check_codestream(&meta(4, 2, 1, 16, 1), Codec::Rle, &frame(1, 64, &good)).is_err());
    }

    #[test]
    fn attributes_must_describe_a_sane_image() {
        assert!(
            check_attributes(&meta(4300, 3500, 1, 16, 1)).is_ok(),
            "a large DR plate"
        );
        assert!(check_attributes(&meta(512, 512, 1, 16, 600)).is_ok(), "a CBCT");
        assert!(check_attributes(&meta(800, 600, 3, 8, 400)).is_ok(), "an echo loop");
        assert!(check_attributes(&meta(20_000, 10, 1, 8, 1)).is_err());
        assert!(
            check_attributes(&meta(16_384, 16_384, 1, 8, 1)).is_err(),
            "too many pixels"
        );
        assert!(check_attributes(&meta(10, 10, 4, 8, 1)).is_err());
        assert!(check_attributes(&meta(10, 10, 1, 12, 1)).is_err());
        assert!(check_attributes(&meta(10, 10, 1, 8, 20_000)).is_err());
        let mut bad = meta(10, 10, 1, 8, 1);
        bad.bits_stored = 12;
        assert!(check_attributes(&bad).is_err());
    }

    #[test]
    fn native_pixel_data_must_be_the_right_length() {
        let m = meta(10, 10, 1, 16, 2);
        assert_eq!(native_length(&m), 400);
        assert!(check_native_length(&m, 400).is_ok());
        assert!(check_native_length(&m, 401).is_ok(), "a padding byte");
        assert!(check_native_length(&m, 300).is_err(), "short");
        assert!(check_native_length(&m, 800).is_err(), "long");
        assert_eq!(native_length(&meta(10, 10, 1, 1, 1)), 13, "1-bit data rounds up");
    }

    #[test]
    fn limits_cannot_be_switched_off_or_raised_without_bound() {
        let silly = Limits {
            side: u32::MAX,
            pixels: 0,
            frames: 0,
            fragment_bytes: u64::MAX,
            series_bytes: 1,
        }
        .clamped();
        assert_eq!(silly.side, Limits::ceiling().side);
        assert!(silly.pixels >= Limits::default().pixels / 10);
        assert!(silly.series_bytes >= Limits::default().series_bytes / 10);
        assert!(check_series_bytes(3 * 1024 * 1024 * 1024).is_err());
        assert!(try_zeroed::<u8>(16).unwrap().len() == 16);
        assert!(
            try_zeroed::<u64>(usize::MAX / 4).is_err(),
            "an impossible allocation is an error"
        );
    }
}
