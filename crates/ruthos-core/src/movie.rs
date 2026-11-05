//! Export a loop or a series as an MP4 movie (H.264), for presentations and
//! teaching. The movie plays in PowerPoint, browsers and phones.
//!
//! Frames arrive as 8-bit RGB, already windowed and coloured the way the
//! viewer shows them. They are encoded with OpenH264 and written to an MP4
//! file. H.264 needs even dimensions, so an odd width or height gets one extra
//! column or row copied from the edge.
//!
//! The encoder is not in Ruthos. H.264 is covered by patents, and Cisco pays
//! the royalties only for the copies of the binary it distributes itself, so
//! Ruthos fetches Cisco's binary from Cisco, with the user's consent, and checks
//! it against the known hashes before loading it.

use std::io::{BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, QpRange};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::OpenH264API;

use crate::{CoreError, Result};

/// Track timescale: 90 kHz, the usual video clock, so any frame rate gives
/// whole-tick frame durations to within a tick.
const TIMESCALE: u32 = 90_000;

/// The size the movie is encoded at: even width and height.
pub fn encoded_size(width: u32, height: u32) -> (u32, u32) {
    (width + (width & 1), height + (height & 1))
}

/// Copy an RGB frame into a buffer of even size, repeating the last column
/// and row when the source is odd.
fn pad_even(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w2, h2) = encoded_size(width, height);
    if (w2, h2) == (width, height) {
        return rgb.to_vec();
    }
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w2 as usize * h2 as usize * 3];
    for y in 0..h2 as usize {
        let sy = y.min(h - 1);
        for x in 0..w2 as usize {
            let sx = x.min(w - 1);
            let s = (sy * w + sx) * 3;
            let d = (y * w2 as usize + x) * 3;
            out[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
        }
    }
    out
}

/// Split an Annex B stream (start codes) into NAL units without them.
fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map(|n| n - 3).unwrap_or(stream.len());
        // A four-byte start code leaves one zero at the end of the previous unit.
        while end > s && stream[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > s {
            out.push(&stream[s..end]);
        }
    }
    out
}

/// Encode `count` frames of `width` x `height` RGB at `fps` frames per
/// second into an MP4 file at `out`. `frame(i)` supplies frame `i`;
/// Cisco's binary for this platform: its name on Cisco's server and the SHA-256
/// of the unpacked library, which the openh264 crate also holds.
struct CiscoBinary {
    file: &'static str,
    sha256: &'static str,
}

const CISCO_VERSION: &str = "2.6.0";
const CISCO_SERVER: &str = "https://ciscobinary.openh264.org";
/// Shown before the download and kept with the other licensing information.
pub const CISCO_LICENCE: &str = include_str!("../../../licenses/openh264-BINARY_LICENSE.txt");

#[cfg(all(windows, target_arch = "x86_64"))]
const CISCO_BINARY: Option<CiscoBinary> = Some(CiscoBinary {
    file: "openh264-2.6.0-win64.dll",
    sha256: "2076cb5675ec6c1a4c70e7a2a322552f547b6eeed649d6dfcd9e02a543b24691",
});
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CISCO_BINARY: Option<CiscoBinary> = Some(CiscoBinary {
    file: "libopenh264-2.6.0-mac-arm64.dylib",
    sha256: "052e98bfcf7a9167d22f3bbb3f5988ef79065591f36af8b52924b22b13624551",
});
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const CISCO_BINARY: Option<CiscoBinary> = Some(CiscoBinary {
    file: "libopenh264-2.6.0-mac-x64.dylib",
    sha256: "e3dc8bc01fe69363f61fd3c02fd27798537a585eadd38cd808f303d1ee505a19",
});
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CISCO_BINARY: Option<CiscoBinary> = Some(CiscoBinary {
    file: "libopenh264-2.6.0-linux64.8.so",
    sha256: "2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04",
});
#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "macos"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
const CISCO_BINARY: Option<CiscoBinary> = None;

/// Where the encoder lives in the data folder.
pub fn encoder_path(data_root: &Path) -> Option<std::path::PathBuf> {
    Some(data_root.join("codecs").join(CISCO_BINARY?.file))
}

pub fn encoder_installed(data_root: &Path) -> bool {
    encoder_path(data_root).is_some_and(|p| p.is_file())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Fetch Cisco's binary from Cisco. Call this only after the user has agreed.
pub fn install_encoder(data_root: &Path) -> Result<std::path::PathBuf> {
    use std::io::Read;
    let binary = CISCO_BINARY.ok_or_else(|| CoreError::Other("Cisco offers no encoder for this computer".into()))?;
    let fail = |e: &dyn std::fmt::Display| CoreError::Other(format!("the encoder could not be fetched: {e}"));
    let url = format!("{CISCO_SERVER}/{}.bz2", binary.file);
    let packed = reqwest::blocking::get(&url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| fail(&e.without_url()))?;
    let mut library = Vec::new();
    bzip2_rs::DecoderReader::new(&packed[..])
        .read_to_end(&mut library)
        .map_err(|e| fail(&e))?;
    if sha256_hex(&library) != binary.sha256 {
        return Err(fail(&format!("OpenH264 {CISCO_VERSION} did not match its checksum")));
    }
    let path = data_root.join("codecs").join(binary.file);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    // A download cut short must not look like an installed encoder.
    let partial = path.with_extension("part");
    std::fs::write(&partial, library)?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// `progress(done, count)` is called after each frame. Setting `cancel`
/// stops the export and removes the partial file.
#[allow(clippy::too_many_arguments)]
pub fn write_mp4(
    api: OpenH264API,
    out: &Path,
    width: u32,
    height: u32,
    fps: f32,
    count: usize,
    frame: &mut dyn FnMut(usize) -> Result<Vec<u8>>,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<()> {
    if count == 0 || width == 0 || height == 0 {
        return Err(CoreError::Other("nothing to export".into()));
    }
    let fps = if fps.is_finite() && fps > 0.0 {
        fps.clamp(1.0, 240.0)
    } else {
        10.0
    };
    let file = std::fs::File::create(out)?;
    let result = encode(
        api,
        BufWriter::new(file),
        width,
        height,
        fps,
        count,
        frame,
        progress,
        cancel,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn encode<W: Write + Seek>(
    api: OpenH264API,
    writer: W,
    width: u32,
    height: u32,
    fps: f32,
    count: usize,
    frame: &mut dyn FnMut(usize) -> Result<Vec<u8>>,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<()> {
    let (w2, h2) = encoded_size(width, height);
    if w2 > u16::MAX as u32 || h2 > u16::MAX as u32 {
        return Err(CoreError::Other("the image is too large for a movie".into()));
    }
    let err = |e: openh264::Error| CoreError::Other(format!("video encoder: {e}"));
    let mp4_err = |e: mp4::Error| CoreError::Other(format!("writing the movie: {e}"));

    // Generous quality: these movies are for teaching and presentations,
    // where blocky speckle hides what the loop shows.
    let bps = ((w2 as f64 * h2 as f64 * fps as f64 * 0.3) as u64).clamp(2_000_000, 50_000_000) as u32;
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bps))
        .max_frame_rate(FrameRate::from_hz(fps))
        .skip_frames(false)
        .qp(QpRange::new(8, 26))
        .intra_frame_period(IntraFramePeriod::from_num_frames((fps * 2.0).round().max(1.0) as u32));
    let mut encoder = Encoder::with_api_config(api, config).map_err(err)?;

    let mut mp4: Option<mp4::Mp4Writer<W>> = None;
    let mut writer = Some(writer);
    let tick = |i: usize| ((i as f64) * TIMESCALE as f64 / fps as f64).round() as u64;

    for i in 0..count {
        if cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Other("cancelled".into()));
        }
        let rgb = frame(i)?;
        if rgb.len() != width as usize * height as usize * 3 {
            return Err(CoreError::Other(format!("frame {} has a different size", i + 1)));
        }
        let padded = pad_even(&rgb, width, height);
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&padded, (w2 as usize, h2 as usize)));
        let stream = encoder.encode(&yuv).map_err(err)?.to_vec();

        let mut sps = None;
        let mut pps = None;
        let mut sample = Vec::with_capacity(stream.len());
        let mut is_sync = false;
        for nal in nal_units(&stream) {
            match nal[0] & 0x1f {
                7 => sps = Some(nal.to_vec()),
                8 => pps = Some(nal.to_vec()),
                t => {
                    if t == 5 {
                        is_sync = true;
                    }
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }

        // The first frame carries the parameter sets that describe the track.
        if mp4.is_none() {
            let (Some(sps), Some(pps)) = (sps, pps) else {
                return Err(CoreError::Other(
                    "the video encoder did not start with a key frame".into(),
                ));
            };
            let config = mp4::Mp4Config {
                major_brand: "isom".parse().map_err(mp4_err)?,
                minor_version: 512,
                compatible_brands: ["isom", "iso2", "avc1", "mp41"]
                    .iter()
                    .map(|b| b.parse())
                    .collect::<std::result::Result<_, _>>()
                    .map_err(mp4_err)?,
                timescale: 1000,
            };
            let mut w = mp4::Mp4Writer::write_start(writer.take().expect("writer"), &config).map_err(mp4_err)?;
            w.add_track(&mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: TIMESCALE,
                language: "und".into(),
                media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                    width: w2 as u16,
                    height: h2 as u16,
                    seq_param_set: sps,
                    pic_param_set: pps,
                }),
            })
            .map_err(mp4_err)?;
            mp4 = Some(w);
        }
        if sample.is_empty() {
            continue;
        }
        let start = tick(i);
        let w = mp4.as_mut().expect("started");
        w.write_sample(
            1,
            &mp4::Mp4Sample {
                start_time: start,
                duration: (tick(i + 1) - start) as u32,
                rendering_offset: 0,
                is_sync,
                bytes: bytes::Bytes::from(sample),
            },
        )
        .map_err(mp4_err)?;
        progress(i + 1, count);
    }
    let mut w = mp4.ok_or_else(|| CoreError::Other("no frames were encoded".into()))?;
    w.write_end().map_err(mp4_err)?;
    w.into_writer().flush()?;
    Ok(())
}

/// What goes into a movie: the frames of one multi-frame image (a loop),
/// or the images of a series in viewing order.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieSource {
    /// Files in order; each contributes all its frames.
    pub files: Vec<std::path::PathBuf>,
}

/// Export DICOM frames as an MP4, rendered with the given window and colour
/// table exactly as the viewer shows them. Frames of a different size from
/// the first are scaled to it, so a series that changes matrix still plays.
#[allow(clippy::too_many_arguments)]
pub fn export_dicom_movie(
    encoder: &Path,
    out: &Path,
    source: &MovieSource,
    center: f32,
    width: f32,
    clut: &ruthos_dicom::Clut,
    fps: f32,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<()> {
    // Count frames from the headers, so progress is right from the start.
    let mut index: Vec<(usize, u32)> = Vec::new();
    for (f, path) in source.files.iter().enumerate() {
        let n = ruthos_dicom::read_meta(path)
            .map(|m| m.number_of_frames.max(1))
            .unwrap_or(1);
        index.extend((0..n).map(|k| (f, k)));
    }
    let first = ruthos_dicom::decode_frame(
        source
            .files
            .first()
            .ok_or_else(|| CoreError::Other("nothing to export".into()))?,
        0,
    )?;
    let (w, h) = (first.width, first.height);

    // Decode a file once and keep its frames while they are being written.
    let mut cached: Option<(usize, Vec<ruthos_dicom::Frame>)> = None;
    let mut frame = |i: usize| -> Result<Vec<u8>> {
        let (f, k) = index[i];
        if cached.as_ref().map(|(c, _)| *c) != Some(f) {
            cached = Some((f, ruthos_dicom::decode_all_frames(&source.files[f])?));
        }
        let frames = &cached.as_ref().expect("cached").1;
        let fr = frames
            .get(k as usize)
            .or_else(|| frames.last())
            .ok_or_else(|| CoreError::Other("an image has no frames".into()))?;
        let rgb = ruthos_dicom::pixels::frame_to_rgb8(fr, center, width, clut);
        if (fr.width, fr.height) == (w, h) {
            return Ok(rgb);
        }
        let img = image::RgbImage::from_raw(fr.width, fr.height, rgb)
            .ok_or_else(|| CoreError::Other("frame buffer size mismatch".into()))?;
        Ok(image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle).into_raw())
    };
    let api = OpenH264API::from_blob_path(encoder).map_err(|e| CoreError::Other(format!("video encoder: {e}")))?;
    write_mp4(api, out, w, h, fps, index.len(), &mut frame, progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_sizes_are_padded_from_the_edge() {
        assert_eq!(encoded_size(5, 3), (6, 4));
        let rgb: Vec<u8> = (0..3 * 3 * 3).map(|i| i as u8).collect();
        let p = pad_even(&rgb, 3, 3);
        assert_eq!(p.len(), 4 * 4 * 3);
        // The new last column repeats the old last column.
        assert_eq!(&p[3 * 3..4 * 3], &rgb[2 * 3..3 * 3]);
        // The new last row repeats the old last row.
        assert_eq!(&p[3 * 4 * 3..3 * 4 * 3 + 9], &rgb[2 * 3 * 3..3 * 3 * 3]);
    }

    #[test]
    fn annex_b_splits_into_nal_units() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9];
        let n = nal_units(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]);
    }

    /// A short moving gradient becomes a playable MP4: the file has the
    /// MP4 boxes, the right size and one sample per frame.
    #[test]
    fn a_loop_becomes_an_mp4() {
        let out = std::env::temp_dir().join(format!("ruthos-movie-{}.mp4", std::process::id()));
        let (w, h, n) = (65u32, 47u32, 12usize);
        write_mp4(
            OpenH264API::from_source(),
            &out,
            w,
            h,
            25.0,
            n,
            &mut |i| {
                let mut v = Vec::with_capacity((w * h * 3) as usize);
                for y in 0..h {
                    for x in 0..w {
                        let g = ((x + y + i as u32 * 4) % 256) as u8;
                        v.extend_from_slice(&[g, g / 2, 255 - g]);
                    }
                }
                Ok(v)
            },
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let f = std::fs::File::open(&out).unwrap();
        let size = f.metadata().unwrap().len();
        let r = mp4::Mp4Reader::read_header(std::io::BufReader::new(f), size).unwrap();
        let track = r.tracks().get(&1).expect("video track");
        assert_eq!((track.width(), track.height()), (66, 48));
        assert_eq!(track.sample_count() as usize, n);
        assert!(
            (r.duration().as_secs_f64() - n as f64 / 25.0).abs() < 0.05,
            "{:?}",
            r.duration()
        );
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn cancelling_leaves_no_file() {
        let out = std::env::temp_dir().join(format!("ruthos-movie-cancel-{}.mp4", std::process::id()));
        let cancel = AtomicBool::new(false);
        let r = write_mp4(
            OpenH264API::from_source(),
            &out,
            16,
            16,
            10.0,
            5,
            &mut |i| {
                if i == 2 {
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(vec![0; 16 * 16 * 3])
            },
            &|_, _| {},
            &cancel,
        );
        assert!(r.is_err());
        assert!(!out.exists());
    }
}
