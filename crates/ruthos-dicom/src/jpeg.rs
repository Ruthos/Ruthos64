//! JPEG decoding through libjpeg-turbo 3.
//!
//! The pure-Rust JPEG decoder only handles 8-bit samples. Radiography systems
//! routinely write 12-bit JPEG Extended (Process 2 & 4) and 12- or 16-bit
//! JPEG Lossless, so the whole JPEG family (baseline, extended, lossless,
//! arithmetic) goes through libjpeg-turbo, which is the reference
//! implementation and is statically linked.

use std::ffi::CStr;

use crate::{DicomError, Result};

#[cfg(all(feature = "fuzz-canary", not(fuzzing)))]
compile_error!("fuzz-canary is a planted memory bug: it exists only in fuzz builds");

/// The bytes libjpeg-turbo reads. With the canary, a copy too short for the
/// length it is told, so the decoder runs off the end of the allocation inside
/// instrumented C. The nightly fuzz job fails unless AddressSanitizer reports
/// that, which proves the harness reaches the codec and the codec is watched.
#[cfg(feature = "fuzz-canary")]
fn scan_bytes(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    data[..data.len() / 2].to_vec().into()
}

#[cfg(not(feature = "fuzz-canary"))]
fn scan_bytes(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    data.into()
}

/// Decoded samples at their native precision.
#[derive(Debug, Clone)]
pub enum JpegSamples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

#[derive(Debug, Clone)]
pub struct JpegImage {
    pub width: u32,
    pub height: u32,
    /// 1 (grayscale) or 3 (RGB, already converted from YCbCr).
    pub components: u32,
    /// 8, 12 or 16.
    pub precision: u32,
    pub lossless: bool,
    pub samples: JpegSamples,
}

struct Handle(turbojpeg_sys::tjhandle);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from tj3Init and is destroyed once.
            unsafe { turbojpeg_sys::tj3Destroy(self.0) };
        }
    }
}

fn error_text(handle: &Handle, what: &str) -> DicomError {
    // SAFETY: tj3GetErrorStr returns a NUL-terminated string owned by the handle.
    let msg = unsafe {
        let p = turbojpeg_sys::tj3GetErrorStr(handle.0);
        if p.is_null() {
            String::from("unknown error")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    DicomError::Pixels(format!("libjpeg-turbo {what}: {msg}"))
}

/// Repair a scan header that some DICOM encoders write wrongly.
///
/// For sequential DCT frames (SOF0, SOF1) the standard requires the scan
/// parameters Ss = 0, Se = 63, Ah = Al = 0. Several modality encoders write
/// Se = 0, which libjpeg rejects as "Invalid SOS parameters for sequential
/// JPEG" while DCMTK's IJG decoder silently accepts it. Patch in place.
/// Progressive and lossless frames are left alone.
pub fn sanitize_sequential_sos(data: &mut [u8]) -> bool {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return false;
    }
    let mut i = 2usize;
    let mut sequential = false;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return false;
        }
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => {
                i += 2; // standalone markers
                continue;
            }
            0xC0 | 0xC1 => sequential = true,
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return false,
            _ => {}
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if marker == 0xDA {
            // SOS: FF DA | Lh Ll | Ns | (Cs, Td/Ta) * Ns | Ss | Se | AhAl
            if !sequential || i + 2 + len > data.len() || len < 6 {
                return false;
            }
            let ns = data[i + 4] as usize;
            let p = i + 5 + ns * 2;
            if p + 3 > data.len() {
                return false;
            }
            let (ss, se, ahal) = (data[p], data[p + 1], data[p + 2]);
            if ss == 0 && se == 63 && ahal == 0 {
                return false;
            }
            data[p] = 0;
            data[p + 1] = 63;
            data[p + 2] = 0;
            return true;
        }
        i += 2 + len;
    }
    false
}

/// Decode one JPEG codestream.
pub fn decode(data: &[u8]) -> Result<JpegImage> {
    if data.len() < 4 {
        return Err(DicomError::Pixels("JPEG codestream too short".into()));
    }
    let mut patched = data.to_vec();
    if sanitize_sequential_sos(&mut patched) {
        tracing::debug!("repaired non-standard SOS parameters in a sequential JPEG");
    }
    decode_inner(&patched)
}

fn decode_inner(data: &[u8]) -> Result<JpegImage> {
    use turbojpeg_sys as tj;
    let scan = scan_bytes(data);
    // SAFETY: plain FFI into libjpeg-turbo with buffers we own and sizes we
    // compute from the header the library itself reports.
    unsafe {
        let handle = Handle(tj::tj3Init(tj::TJINIT_TJINIT_DECOMPRESS as libc::c_int));
        if handle.0.is_null() {
            return Err(DicomError::Pixels(
                "libjpeg-turbo could not initialise a decoder".into(),
            ));
        }
        // Keep going on warnings (e.g. slightly truncated streams), like Horos does.
        tj::tj3Set(handle.0, tj::TJPARAM_TJPARAM_STOPONWARNING as libc::c_int, 0);

        if tj::tj3DecompressHeader(handle.0, data.as_ptr(), data.len() as _) != 0 {
            return Err(error_text(&handle, "header"));
        }
        let width = tj::tj3Get(handle.0, tj::TJPARAM_TJPARAM_JPEGWIDTH as libc::c_int);
        let height = tj::tj3Get(handle.0, tj::TJPARAM_TJPARAM_JPEGHEIGHT as libc::c_int);
        let precision = tj::tj3Get(handle.0, tj::TJPARAM_TJPARAM_PRECISION as libc::c_int);
        let colorspace = tj::tj3Get(handle.0, tj::TJPARAM_TJPARAM_COLORSPACE as libc::c_int);
        let lossless = tj::tj3Get(handle.0, tj::TJPARAM_TJPARAM_LOSSLESS as libc::c_int) != 0;
        if width <= 0 || height <= 0 {
            return Err(DicomError::Pixels(format!("JPEG reports size {width} x {height}")));
        }
        let (width, height) = (width as u32, height as u32);
        let gray = colorspace == tj::TJCS_TJCS_GRAY as libc::c_int;
        let (pixel_format, components) = if gray {
            (tj::TJPF_TJPF_GRAY as libc::c_int, 1u32)
        } else {
            (tj::TJPF_TJPF_RGB as libc::c_int, 3u32)
        };
        let n = (width as usize) * (height as usize) * components as usize;

        let samples = match precision {
            8 => {
                let mut buf = crate::guard::try_zeroed::<u8>(n)?;
                if tj::tj3Decompress8(
                    handle.0,
                    scan.as_ptr(),
                    data.len() as _,
                    buf.as_mut_ptr(),
                    0,
                    pixel_format,
                ) != 0
                {
                    return Err(error_text(&handle, "8-bit decode"));
                }
                JpegSamples::U8(buf)
            }
            12 => {
                let mut buf = crate::guard::try_zeroed::<i16>(n)?;
                if tj::tj3Decompress12(
                    handle.0,
                    scan.as_ptr(),
                    data.len() as _,
                    buf.as_mut_ptr(),
                    0,
                    pixel_format,
                ) != 0
                {
                    return Err(error_text(&handle, "12-bit decode"));
                }
                // 12-bit samples are non-negative; reinterpret bit-for-bit so
                // signed lossless data survives the round trip too.
                JpegSamples::U16(buf.into_iter().map(|v| v as u16).collect())
            }
            16 => {
                let mut buf = crate::guard::try_zeroed::<u16>(n)?;
                if tj::tj3Decompress16(
                    handle.0,
                    scan.as_ptr(),
                    data.len() as _,
                    buf.as_mut_ptr(),
                    0,
                    pixel_format,
                ) != 0
                {
                    return Err(error_text(&handle, "16-bit decode"));
                }
                JpegSamples::U16(buf)
            }
            other => return Err(DicomError::Unsupported(format!("JPEG sample precision {other}"))),
        };

        Ok(JpegImage {
            width,
            height,
            components,
            precision: precision as u32,
            lossless,
            samples,
        })
    }
}

/// Transfer syntaxes handled by this module.
pub fn is_jpeg_transfer_syntax(uid: &str) -> bool {
    matches!(
        uid.trim_end_matches('\0'),
        "1.2.840.10008.1.2.4.50" // Baseline (Process 1)
            | "1.2.840.10008.1.2.4.51" // Extended (Process 2 & 4)
            | "1.2.840.10008.1.2.4.57" // Lossless, Non-Hierarchical (Process 14)
            | "1.2.840.10008.1.2.4.70" // Lossless, Process 14 SV1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_an_8bit_jpeg_made_by_image_crate() {
        let mut img = image::GrayImage::new(32, 16);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            p.0[0] = (x * 8) as u8;
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(
            (decoded.width, decoded.height, decoded.components, decoded.precision),
            (32, 16, 1, 8)
        );
        match decoded.samples {
            JpegSamples::U8(v) => {
                assert_eq!(v.len(), 32 * 16);
                assert!(v[0] < 40 && v[31] > 200);
            }
            _ => panic!("expected 8-bit samples"),
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[1, 2, 3, 4, 5, 6, 7, 8]).is_err());
    }

    #[test]
    fn repairs_bad_sos_only_for_sequential() {
        // SOI, SOF1 (minimal), SOS with Se = 0.
        let mut seq = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xC1, 0x00, 0x0B, 0x0C, 0x00, 0x10, 0x00, 0x10, 0x01, 0x01, 0x11, 0x00, // SOF1
            0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, // SOS Ss=0 Se=0
            0x12, 0x34,
        ];
        assert!(sanitize_sequential_sos(&mut seq));
        assert_eq!(&seq[22..25], &[0, 63, 0]);
        assert!(!sanitize_sequential_sos(&mut seq)); // already fine

        let mut prog = seq.clone();
        prog[3] = 0xC2; // progressive: never touched
        prog[23] = 0;
        assert!(!sanitize_sequential_sos(&mut prog));
        assert_eq!(prog[23], 0);
    }
}
