//! The promptable segmentation model: SAM 2.1 Tiny through ONNX Runtime.
//!
//! The user clicks on a structure and the model returns the object meant, then
//! follows it through the stack as it would through the frames of a video. It
//! does not know anatomy: it has learned what a whole object looks like, and
//! the click says which. It runs only when asked and entirely on this machine,
//! and its result is an ordinary segment that every classical tool can edit.
//!
//! Neither the model nor ONNX Runtime is in the executable. Both are fetched on
//! first use, at versions and checksums fixed here, and verified before they
//! are loaded. Runtime and weights live in the data folder.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::ep::DirectML;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use sha2::{Digest, Sha256};

use crate::refine::{refine_slice, resample, Edge};

/// The side of the map the decoder's logits come in.
pub const SIZE: usize = 1024;
const FEATURES: usize = 64 * 64;
/// Tokens in a memory block.
const MEMORY_DIM: usize = 64;
const HIDDEN: usize = 256;
/// One conditioning frame and six recent ones.
const BLOCKS: usize = 7;
const POINTERS: usize = 16;

/// An address in the one export the checksums below describe.
macro_rules! export {
    ($path:literal) => {
        concat!(
            "https://huggingface.co/square-zero-labs/sam2.1-tiny-video-onnx/resolve/",
            "3b2984dd865f6e9d2cc6aed0be6a5a5c2eb352ce/",
            $path
        )
    };
}

/// A file the tool needs.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    pub name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    /// Inside a package: the entry to take. The checksum is the package's.
    pub entry: Option<&'static str>,
}

/// How a model works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Slice by slice, following an object through the stack.
    Slices,
    /// A window of the volume at once, one network, on the series' own voxels.
    NnInteractive,
    /// A window of the volume at 1.5 mm, an encoder and a decoder for each click.
    Vista,
}

/// What the download dialog shows before anything is fetched.
#[derive(Debug, Clone, Copy)]
pub struct Manifest {
    /// Stable, for settings and folders.
    pub id: &'static str,
    pub engine: Engine,
    /// Terms to agree to first, for a model offered only in research mode.
    pub research: Option<&'static str>,
    pub name: &'static str,
    pub version: &'static str,
    pub licence: &'static str,
    pub source: &'static str,
    pub training: &'static str,
    pub model: &'static [Item],
    pub runtime: &'static [Item],
}

pub const SAM2_TINY: Manifest = Manifest {
    id: "sam2.1-tiny",
    engine: Engine::Slices,
    research: None,
    name: "SAM 2.1 Tiny",
    version: "2.1",
    licence: "Apache 2.0",
    source: "Meta's facebook/sam2.1-hiera-tiny, exported to ONNX by a third party",
    training: "Trained on natural images and videos, not on medical or veterinary images. How well it does \
               on an animal is for the user to judge against the slices.",
    model: &[
        Item {
            name: "constants.json",
            url: export!("constants.json"),
            bytes: 9_922,
            sha256: "172edc70e892aab0aef6caedf97b8e4091fbf6c5d26dac5fd9ea69d093d927d3",
            entry: None,
        },
        Item {
            name: "vision_encoder.onnx",
            url: export!("onnx/vision_encoder.onnx"),
            bytes: 134_335_567,
            sha256: "aa7a8542942f042e235a993a1ab0ccf5f049918500577802a7f10ec1b39bb873",
            entry: None,
        },
        Item {
            name: "mask_decoder.onnx",
            url: export!("onnx/mask_decoder.onnx"),
            bytes: 17_794_355,
            sha256: "0461896de3db00936fe1643506f129d71cb6d5d2ae15754811756b7ea1b070c6",
            entry: None,
        },
        Item {
            name: "memory_encoder.onnx",
            url: export!("onnx/memory_encoder.onnx"),
            bytes: 5_616_496,
            sha256: "580d246c109de88838f600ba7c1c0d03d1fe267f7641b06f8c88c5f0dc5834cd",
            entry: None,
        },
        Item {
            name: "memory_attention.onnx",
            url: export!("onnx/memory_attention.onnx"),
            bytes: 32_259_165,
            sha256: "791e648ce8f5ef91ad00ba06e83066ff261ae5a645f0b042b4f46a89fd054baf",
            entry: None,
        },
        Item {
            name: "pointer_tpos.onnx",
            url: export!("onnx/pointer_tpos.onnx"),
            bytes: 67_289,
            sha256: "7e71df1d75dba09bc18dd4ae745c2a2cceebdd14d84eadea2fe65d0205f101fc",
            entry: None,
        },
    ],
    runtime: &[
        Item {
            name: "onnxruntime.dll",
            url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
            bytes: 12_458_649,
            sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
            entry: Some("runtimes/win-x64/native/onnxruntime.dll"),
        },
        Item {
            name: "DirectML.dll",
            url: "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
            bytes: 202_292_617,
            sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9",
            entry: Some("bin/x64-win/DirectML.dll"),
        },
    ],
};

/// No address yet: these exports are installed from a folder while they are tested.
const FROM_A_FOLDER: &str = "";

pub const VISTA3D: Manifest = Manifest {
    id: "vista3d",
    engine: Engine::Vista,
    research: None,
    name: "VISTA3D",
    version: "NV-Segment-CT",
    licence: "NVIDIA Open Model License",
    source: "NVIDIA's NV-Segment-CT, exported to ONNX by Ruthos",
    training: "Trained on human CT. NVIDIA's model card says it is for research and not for clinical use. \
               It works at 1.5 mm and treats everything above about 1050 HU alike, so dense bone and teeth \
               can run together.",
    model: &[
        Item {
            name: "vista3d-encoder-fp16.onnx",
            url: FROM_A_FOLDER,
            bytes: 392_255_543,
            sha256: "68504be099b836784d29f2d1538e97f89dacd9a2c93b8355f6a2c0c4eecf8d17",
            entry: None,
        },
        Item {
            name: "vista3d-decoder.onnx",
            url: FROM_A_FOLDER,
            bytes: 2_807_470,
            sha256: "a29b650799e83dcc1542b22e05d052c4565c59769878254b11e0b8e9006382eb",
            entry: None,
        },
    ],
    runtime: SAM2_TINY.runtime,
};

pub const NN_INTERACTIVE: Manifest = Manifest {
    id: "nninteractive",
    engine: Engine::NnInteractive,
    research: Some(
        "nnInteractive's weights are licensed by DKFZ under CC BY-NC-SA 4.0, for non-commercial use only. \
         Paid clinical work, including work in a practice that charges its clients, is not covered unless DKFZ \
         has given you permission. Whether your use is allowed is your responsibility. Agree only if your use \
         is non-commercial, such as research, teaching or testing.",
    ),
    name: "nnInteractive",
    version: "1.0",
    licence: "CC BY-NC-SA 4.0 (non-commercial)",
    source: "DKFZ's nnInteractive v1.0, exported to ONNX",
    training: "Trained on over 120 volumetric datasets, mostly human. It works on the series' own voxels and \
               needs about 5 GB of GPU memory, or about 9 s a click on the CPU.",
    model: &[Item {
        name: "nninteractive-fp16.onnx",
        url: FROM_A_FOLDER,
        bytes: 204_800_627,
        sha256: "9d3f2332d396203bdac5fb90469c484ccb9be72fc69c069126e954a1bad566ac",
        entry: None,
    }],
    runtime: SAM2_TINY.runtime,
};

/// Every model Ruthos can use, default first.
pub const AVAILABLE: [Manifest; 3] = [SAM2_TINY, VISTA3D, NN_INTERACTIVE];

/// Why the tool cannot run, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AiError(pub String);

impl AiError {
    fn from(what: &str, e: impl std::fmt::Display) -> Self {
        Self(format!("{what}: {e}"))
    }
}

type Result<T> = std::result::Result<T, AiError>;

pub fn runtime_folder(data_root: &Path) -> PathBuf {
    data_root.join("models").join("runtime")
}

impl Manifest {
    pub fn folder(&self, data_root: &Path) -> PathBuf {
        data_root.join("models").join(self.id)
    }

    /// Whether this model can be fetched, or only installed from a folder.
    pub fn downloadable(&self) -> bool {
        self.model.iter().all(|i| !i.url.is_empty())
    }

    /// Whether the list shows this model, with research mode on or off. Research mode
    /// also holds models still being tested, which cannot be fetched yet.
    pub fn offered(&self, research_mode: bool) -> bool {
        research_mode || (self.research.is_none() && self.downloadable())
    }

    /// Bytes still to fetch, for what can be fetched.
    pub fn download_bytes(&self, data_root: &Path) -> u64 {
        let missing = |folder: PathBuf, items: &[Item]| -> u64 {
            items
                .iter()
                .filter(|i| !i.url.is_empty() && !folder.join(i.name).is_file())
                .map(|i| i.bytes)
                .sum()
        };
        missing(self.folder(data_root), self.model) + missing(runtime_folder(data_root), self.runtime)
    }

    pub fn has_model(&self, data_root: &Path) -> bool {
        self.model.iter().all(|i| self.folder(data_root).join(i.name).is_file())
    }

    pub fn installed(&self, data_root: &Path) -> bool {
        self.has_model(data_root) && self.download_bytes(data_root) == 0
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Refuse a file that is not what the manifest says. Nothing unverified loads.
pub fn verify(path: &Path, item: &Item) -> Result<()> {
    let actual = sha256_file(path).map_err(|e| AiError::from(item.name, e))?;
    if actual == item.sha256 {
        Ok(())
    } else {
        Err(AiError(format!(
            "{} failed its checksum and will not be loaded",
            item.name
        )))
    }
}

/// Fetch one item into `folder`.
fn fetch(
    item: &Item,
    folder: &Path,
    done: &mut u64,
    total: u64,
    progress: &(dyn Fn(u64, u64) + Sync),
    cancel: &AtomicBool,
) -> Result<()> {
    let target = folder.join(item.name);
    if target.is_file() {
        return Ok(());
    }
    let temporary = folder.join(format!("{}.part", item.name));
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| AiError::from("The download could not start", e))?;
    let mut response = client
        .get(item.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| AiError::from("The download failed", e.without_url()))?;
    let mut out = std::fs::File::create(&temporary).map_err(|e| AiError::from(item.name, e))?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&temporary);
            return Err(AiError("Download cancelled".to_string()));
        }
        let n = response
            .read(&mut buf)
            .map_err(|e| AiError::from("The download failed", e))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| AiError::from(item.name, e))?;
        *done += n as u64;
        progress(*done, total);
    }
    drop(out);
    if let Err(e) = verify(&temporary, item) {
        let _ = std::fs::remove_file(&temporary);
        return Err(e);
    }
    match item.entry {
        None => std::fs::rename(&temporary, &target).map_err(|e| AiError::from(item.name, e))?,
        Some(entry) => {
            let file = std::fs::File::open(&temporary).map_err(|e| AiError::from(item.name, e))?;
            let mut zip = zip::ZipArchive::new(file).map_err(|e| AiError::from(item.name, e))?;
            let mut inner = zip.by_name(entry).map_err(|e| AiError::from(item.name, e))?;
            let mut out = std::fs::File::create(&target).map_err(|e| AiError::from(item.name, e))?;
            std::io::copy(&mut inner, &mut out).map_err(|e| AiError::from(item.name, e))?;
            drop(inner);
            drop(zip);
            let _ = std::fs::remove_file(&temporary);
        }
    }
    Ok(())
}

/// Fetch and verify what is missing. `progress` gets bytes done and in all.
pub fn install(
    manifest: &Manifest,
    data_root: &Path,
    progress: &(dyn Fn(u64, u64) + Sync),
    cancel: &AtomicBool,
) -> Result<()> {
    let (model, runtime) = (manifest.folder(data_root), runtime_folder(data_root));
    for folder in [&model, &runtime] {
        std::fs::create_dir_all(folder).map_err(|e| AiError::from("The model folder", e))?;
    }
    let total = manifest.download_bytes(data_root);
    let mut done = 0;
    for item in manifest.model.iter().filter(|i| !i.url.is_empty()) {
        fetch(item, &model, &mut done, total, progress, cancel)?;
    }
    for item in manifest.runtime {
        fetch(item, &runtime, &mut done, total, progress, cancel)?;
    }
    Ok(())
}

/// Install the model from a folder, for a practice without internet access.
/// Every file is verified before it is copied in.
pub fn install_from_folder(manifest: &Manifest, from: &Path, data_root: &Path) -> Result<()> {
    let model = manifest.folder(data_root);
    std::fs::create_dir_all(&model).map_err(|e| AiError::from("The model folder", e))?;
    for item in manifest.model {
        let source = from.join(item.name);
        verify(&source, item)?;
        std::fs::copy(&source, model.join(item.name)).map_err(|e| AiError::from(item.name, e))?;
    }
    Ok(())
}

/// The side of the mask the decoder really makes. `high_res_mask` is this scaled
/// up, so this, not the 1024 of the input, sets the finest detail a mask can hold.
pub const MASK_CELLS: usize = 256;

/// A gray value at or below this is taken as empty when judging a crop.
const EMPTY: f32 = 0.02;

/// The part of a slice given to the model, in pixels. Whatever goes in is
/// stretched to 1024 square and the mask comes back in 256 cells, so a smaller
/// region means finer detail: on a whole 600 pixel slice thin bone came out 75%
/// too large with its fracture gap filled; a tight crop greatly reduced both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Region {
    pub fn whole(width: usize, height: usize) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The pixels between two corners, where pixel `i` covers `i - 0.5` to
    /// `i + 0.5`, held to the slice. None if too little is left to work with.
    pub fn between(low: [f64; 2], high: [f64; 2], width: usize, height: usize) -> Option<Self> {
        let span = |low: f64, high: f64, size: usize| {
            let start = low.ceil().max(0.0) as usize;
            let end = ((high.floor() + 1.0).max(0.0) as usize).min(size);
            (start, end.saturating_sub(start))
        };
        let (x, w) = span(low[0], high[0], width);
        let (y, h) = span(low[1], high[1], height);
        (w >= 8 && h >= 8).then_some(Self {
            x,
            y,
            width: w,
            height: h,
        })
    }

    pub fn holds(&self, x: f32, y: f32) -> bool {
        let (left, top) = (self.x as f32, self.y as f32);
        x >= left && y >= top && x < left + self.width as f32 && y < top + self.height as f32
    }

    /// This part of a slice `full_width` pixels wide.
    pub fn take(&self, slice: &[f32], full_width: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.width * self.height);
        for y in self.y..self.y + self.height {
            let start = self.x + full_width * y;
            out.extend_from_slice(&slice[start..start + self.width]);
        }
        out
    }

    /// A click on the slice, as a click on this region.
    pub fn local(&self, click: Click) -> Click {
        Click {
            x: click.x - self.x as f32,
            y: click.y - self.y as f32,
            ..click
        }
    }

    /// A mask of this region, as a mask of the whole slice.
    pub fn place(&self, mask: &[bool], full_width: usize, full_height: usize) -> Vec<bool> {
        let mut out = vec![false; full_width * full_height];
        for (y, row) in mask.chunks_exact(self.width).enumerate() {
            let start = self.x + full_width * (self.y + y);
            out[start..start + self.width].copy_from_slice(row);
        }
        out
    }

    /// How many slice pixels one mask cell covers, on the longer side. Over 1
    /// the mask is coarser than the image.
    pub fn pixels_per_cell(&self) -> f32 {
        self.width.max(self.height) as f32 / MASK_CELLS as f32
    }

    /// How far the region is stretched to reach a square, 1 being not at all.
    pub fn stretch(&self) -> f32 {
        self.width.max(self.height) as f32 / self.width.min(self.height).max(1) as f32
    }

    /// The share of this part of a slice that is empty.
    pub fn empty_share(&self, slice: &[f32], full_width: usize) -> f32 {
        let part = self.take(slice, full_width);
        part.iter().filter(|v| **v <= EMPTY).count() as f32 / part.len().max(1) as f32
    }

    /// Whether a mask of this region runs into a side of it, other than a side
    /// that is the edge of the slice: the object may carry on past that side.
    pub fn reaches_edge(&self, mask: &[bool], full_width: usize, full_height: usize) -> bool {
        let at = |x: usize, y: usize| mask[x + self.width * y];
        (self.y > 0 && (0..self.width).any(|x| at(x, 0)))
            || (self.y + self.height < full_height && (0..self.width).any(|x| at(x, self.height - 1)))
            || (self.x > 0 && (0..self.height).any(|y| at(0, y)))
            || (self.x + self.width < full_width && (0..self.height).any(|y| at(self.width - 1, y)))
    }

    /// The share of the region's longer side that a mask of it spans, 0 if the
    /// mask is empty. Small means the region could be much tighter.
    pub fn extent(&self, mask: &[bool]) -> f32 {
        let (mut low, mut high) = ([usize::MAX; 2], [0usize; 2]);
        for (i, _) in mask.iter().enumerate().filter(|(_, on)| **on) {
            let at = [i % self.width, i / self.width];
            for axis in 0..2 {
                low[axis] = low[axis].min(at[axis]);
                high[axis] = high[axis].max(at[axis]);
            }
        }
        if low[0] == usize::MAX {
            return 0.0;
        }
        let side = |axis: usize, size: usize| (high[axis] - low[axis] + 1) as f32 / size as f32;
        side(0, self.width).max(side(1, self.height))
    }
}

/// A gray slice, 0 to 1, as the encoder wants it: squashed to 1024 square with
/// no padding, three channels, normalised by the training mean and deviation.
pub fn to_pixels(gray: &[f32], width: usize, height: usize, mean: [f32; 3], std: [f32; 3]) -> Vec<f32> {
    let mut plane = vec![0f32; SIZE * SIZE];
    for (y, row) in plane.as_chunks_mut::<SIZE>().0.iter_mut().enumerate() {
        let sy = ((y as f32 + 0.5) * height as f32 / SIZE as f32 - 0.5).max(0.0);
        let (y0, fy) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(height - 1);
        for (x, out) in row.iter_mut().enumerate() {
            let sx = ((x as f32 + 0.5) * width as f32 / SIZE as f32 - 0.5).max(0.0);
            let (x0, fx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(width - 1);
            let at = |px: usize, py: usize| gray[px + width * py.min(height - 1)];
            let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
            let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
            *out = top * (1.0 - fy) + bottom * fy;
        }
    }
    let mut out = Vec::with_capacity(3 * SIZE * SIZE);
    for c in 0..3 {
        out.extend(plane.iter().map(|v| (v - mean[c]) / std[c]));
    }
    out
}

/// A high-resolution mask reduced to the slice's own size.
pub fn to_slice(mask: &[f32], width: usize, height: usize) -> Vec<bool> {
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let my = ((y as f32 + 0.5) * SIZE as f32 / height as f32) as usize;
        for x in 0..width {
            let mx = ((x as f32 + 0.5) * SIZE as f32 / width as f32) as usize;
            out.push(mask[mx.min(SIZE - 1) + SIZE * my.min(SIZE - 1)] > 0.0);
        }
    }
    out
}

/// The model's logits for part of a slice as a mask of it, refined on `values` if
/// asked. Then the second result is the model's own, kept to compare. Without
/// refinement this is exactly `to_slice`.
pub fn masks(
    logits: &[f32],
    values: impl FnOnce() -> Vec<f32>,
    width: usize,
    height: usize,
    refine: Option<(Edge, usize)>,
) -> (Vec<bool>, Option<Vec<bool>>) {
    let own = to_slice(logits, width, height);
    let Some((edge, band)) = refine else {
        return (own, None);
    };
    let fine: Vec<bool> = resample(logits, SIZE, width, height).iter().map(|v| *v > 0.0).collect();
    let refined = refine_slice(&values(), &fine, width, height, edge, band);
    (refined.mask, Some(own))
}

/// `[C, 64, 64]` to the `[4096, 1, C]` sequence the attention graph takes.
fn flatten(values: &[f32], channels: usize) -> Vec<f32> {
    let mut out = vec![0f32; FEATURES * channels];
    for c in 0..channels {
        for i in 0..FEATURES {
            out[i * channels + c] = values[c * FEATURES + i];
        }
    }
    out
}

/// What the model remembers of a stack as it goes.
#[derive(Debug, Clone, Default)]
pub struct Bank {
    condition: Option<(Vec<f32>, Vec<f32>)>,
    /// Newest last, at most six.
    recent: VecDeque<(Vec<f32>, Vec<f32>)>,
    /// The step each pointer came from, and the pointer. The first is the
    /// conditioning frame's.
    pointers: Vec<(usize, Vec<f32>)>,
}

impl Bank {
    pub fn is_empty(&self) -> bool {
        self.condition.is_none()
    }

    fn start(&mut self, tokens: Vec<f32>, pos: Vec<f32>, pointer: Vec<f32>) {
        self.condition = Some((tokens, pos));
        self.pointers = vec![(0, pointer)];
        self.recent.clear();
    }

    fn push(&mut self, step: usize, tokens: Vec<f32>, pos: Vec<f32>, pointer: Vec<f32>) {
        self.recent.push_back((tokens, pos));
        while self.recent.len() > BLOCKS - 1 {
            self.recent.pop_front();
        }
        self.pointers.push((step, pointer));
    }

    /// The pointers in use and the distance of each, for `pointer_tpos`.
    fn chosen(&self, step: usize) -> (Vec<&[f32]>, Vec<f32>) {
        let mut pointers: Vec<(usize, &[f32])> = self.pointers.iter().take(1).map(|(s, p)| (*s, &p[..])).collect();
        pointers.extend(
            self.pointers
                .iter()
                .skip(1)
                .rev()
                .take(POINTERS - 1)
                .map(|(s, p)| (*s, &p[..])),
        );
        let denominator = (pointers.len().min(POINTERS) as f32 - 1.0).max(1.0);
        let mut diffs: Vec<f32> = pointers.iter().map(|(s, _)| (step - *s) as f32 / denominator).collect();
        let mut chosen: Vec<&[f32]> = pointers.iter().map(|(_, p)| *p).collect();
        while chosen.len() < POINTERS {
            chosen.push(chosen[chosen.len() - 1]);
            diffs.push(diffs[diffs.len() - 1]);
        }
        (chosen, diffs)
    }

    /// Blocks of memory then pointer tokens, in the fixed shape the graph
    /// takes: short banks repeat their most recent block.
    fn assemble(&self, tpe: &[Vec<f32>], pointer_pos: &[f32], step: usize) -> Option<(Vec<f32>, Vec<f32>)> {
        let (tokens, pos) = self.condition.as_ref()?;
        let block = FEATURES * MEMORY_DIM;
        let total = (BLOCKS * FEATURES + POINTERS * 4) * MEMORY_DIM;
        let (mut memory, mut memory_pos) = (Vec::with_capacity(total), Vec::with_capacity(total));
        let mut add = |tokens: &[f32], pos: &[f32], row: &[f32]| {
            memory.extend_from_slice(&tokens[..block]);
            for token in pos[..block].as_chunks::<MEMORY_DIM>().0 {
                memory_pos.extend(token.iter().zip(row).map(|(a, b)| a + b));
            }
        };
        add(tokens, pos, &tpe[BLOCKS - 1]);
        let mut last = (&tokens[..], &pos[..], BLOCKS - 1);
        for (k, (t, p)) in self.recent.iter().rev().enumerate() {
            add(t, p, &tpe[k]);
            last = (t, p, k);
        }
        for _ in 1 + self.recent.len()..BLOCKS {
            add(last.0, last.1, &tpe[last.2]);
        }
        let (chosen, _) = self.chosen(step);
        for (p, row) in chosen.iter().zip(pointer_pos.as_chunks::<MEMORY_DIM>().0) {
            memory.extend_from_slice(&p[..HIDDEN]);
            for _ in 0..4 {
                memory_pos.extend_from_slice(row);
            }
        }
        Some((memory, memory_pos))
    }
}

/// The encoder's output for one slice.
pub struct Features {
    feats0: Vec<f32>,
    feats1: Vec<f32>,
    feats2: Vec<f32>,
    feats2_no_mem: Vec<f32>,
    position: Vec<f32>,
}

/// The decoder's output for one slice.
pub struct Decoded {
    pub mask: Vec<f32>,
    pub iou: f32,
    /// At or below zero, the object is not on this slice.
    pub score: f32,
    pointer: Vec<f32>,
}

/// A click: where in the slice, in its own pixels, and whether to include.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    pub x: f32,
    pub y: f32,
    pub include: bool,
}

pub struct Sam2 {
    encoder: Session,
    decoder: Session,
    memory: Session,
    attention: Session,
    pointer: Session,
    tpe: Vec<Vec<f32>>,
    mean: [f32; 3],
    std: [f32; 3],
    /// A GPU took the sessions; otherwise the CPU is doing it, far slower.
    pub on_gpu: bool,
}

fn run_error(e: impl std::fmt::Display) -> AiError {
    AiError::from("The model failed", e)
}

fn tensor<const N: usize>(shape: [usize; N], data: Vec<f32>) -> Result<Tensor<f32>> {
    Tensor::from_array((shape, data)).map_err(run_error)
}

/// ONNX Runtime allows one environment in a process, however many times a model is
/// loaded.
fn environment(data_root: &Path) -> Result<&'static Environment> {
    static ENVIRONMENT: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    let library = runtime_folder(data_root).join("onnxruntime.dll");
    ENVIRONMENT
        .get_or_init(|| {
            ort::init_from(&library)
                .map_err(|e| e.to_string())?
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| AiError::from("ONNX Runtime could not start", e))
}

/// A session on one graph, on the GPU if asked.
pub(crate) fn open_session(data_root: &Path, file: &Path, gpu: bool) -> Result<Session> {
    let mut builder = Session::builder(environment(data_root)?).map_err(run_error)?;
    builder = builder
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(run_error)?;
    if gpu {
        // DirectML wants one thread of work and no memory patterns.
        builder = builder
            .with_execution_providers([DirectML::default().build().error_on_failure()])
            .map_err(run_error)?
            .with_parallel_execution(false)
            .map_err(run_error)?
            .with_memory_pattern(false)
            .map_err(run_error)?;
    }
    builder.commit_from_file(file).map_err(run_error)
}

impl Sam2 {
    /// Verify and load the model. It takes some seconds, so it belongs on a
    /// background thread.
    pub fn load(manifest: &Manifest, data_root: &Path) -> Result<Self> {
        let model = manifest.folder(data_root);
        for item in manifest.model {
            verify(&model.join(item.name), item)?;
        }
        let session = |name: &str, gpu: bool| open_session(data_root, &model.join(name), gpu);
        let on_gpu = session("pointer_tpos.onnx", true).is_ok();
        let constants: serde_json::Value = serde_json::from_slice(
            &std::fs::read(model.join("constants.json")).map_err(|e| AiError::from("constants.json", e))?,
        )
        .map_err(|e| AiError::from("constants.json", e))?;
        let triple =
            |key: &str| -> [f32; 3] { std::array::from_fn(|i| constants[key][i].as_f64().unwrap_or(0.0) as f32) };
        let tpe: Vec<Vec<f32>> = constants["memory_temporal_positional_encoding"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|r| {
                        r.as_array()
                            .map(|v| v.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if tpe.len() != BLOCKS || tpe[0].len() != MEMORY_DIM {
            return Err(AiError("constants.json is not as expected".to_string()));
        }
        Ok(Self {
            encoder: session("vision_encoder.onnx", on_gpu)?,
            decoder: session("mask_decoder.onnx", on_gpu)?,
            memory: session("memory_encoder.onnx", on_gpu)?,
            attention: session("memory_attention.onnx", on_gpu)?,
            pointer: session("pointer_tpos.onnx", false)?,
            tpe,
            mean: triple("image_mean"),
            std: triple("image_std"),
            on_gpu,
        })
    }

    /// Encode a slice, gray in 0 to 1. This is the expensive step.
    pub fn encode(&mut self, gray: &[f32], width: usize, height: usize) -> Result<Features> {
        let pixels = to_pixels(gray, width, height, self.mean, self.std);
        let outputs = self
            .encoder
            .run(ort::inputs!["pixel_values" => tensor([1, 3, SIZE, SIZE], pixels)?])
            .map_err(run_error)?;
        let take = |name: &str| -> Result<Vec<f32>> {
            Ok(outputs[name].try_extract_tensor::<f32>().map_err(run_error)?.1.to_vec())
        };
        Ok(Features {
            feats0: take("feats0")?,
            feats1: take("feats1")?,
            feats2: take("feats2")?,
            feats2_no_mem: take("feats2_no_mem")?,
            position: take("vision_pos_embed")?,
        })
    }

    fn decode(&mut self, f: &Features, condition: Vec<f32>, points: Vec<f32>, labels: Vec<i32>) -> Result<Decoded> {
        let n = labels.len();
        let outputs = self
            .decoder
            .run(ort::inputs![
                "feats0" => tensor([1, 32, 256, 256], f.feats0.clone())?,
                "feats1" => tensor([1, 64, 128, 128], f.feats1.clone())?,
                "feats2_cond" => tensor([1, HIDDEN, 64, 64], condition)?,
                "input_points" => tensor([1, 1, n, 2], points)?,
                "input_labels" => Tensor::from_array(([1, 1, n], labels)).map_err(run_error)?,
            ])
            .map_err(run_error)?;
        let take = |name: &str| -> Result<Vec<f32>> {
            Ok(outputs[name].try_extract_tensor::<f32>().map_err(run_error)?.1.to_vec())
        };
        Ok(Decoded {
            mask: take("high_res_mask")?,
            iou: take("iou")?.first().copied().unwrap_or(0.0),
            score: take("object_score_logits")?.first().copied().unwrap_or(0.0),
            pointer: take("object_pointer")?,
        })
    }

    fn remember(&mut self, f: &Features, d: &Decoded, binarize: f32) -> Result<(Vec<f32>, Vec<f32>)> {
        let outputs = self
            .memory
            .run(ort::inputs![
                "feats2" => tensor([1, HIDDEN, 64, 64], f.feats2.clone())?,
                "high_res_mask" => tensor([1, 1, SIZE, SIZE], d.mask.clone())?,
                "object_score_logits" => tensor([1, 1], vec![d.score])?,
                "binarize" => Tensor::from_array(((), vec![binarize])).map_err(run_error)?,
            ])
            .map_err(run_error)?;
        let take = |name: &str| -> Result<Vec<f32>> {
            Ok(outputs[name].try_extract_tensor::<f32>().map_err(run_error)?.1.to_vec())
        };
        Ok((take("memory_tokens")?, take("memory_pos")?))
    }

    /// The mask from clicks on a slice of `width` by `height` pixels.
    pub fn click(&mut self, f: &Features, clicks: &[Click], width: usize, height: usize) -> Result<Decoded> {
        let (sx, sy) = (SIZE as f32 / width as f32, SIZE as f32 / height as f32);
        let points = clicks.iter().flat_map(|c| [c.x * sx, c.y * sy]).collect();
        let labels = clicks.iter().map(|c| i32::from(c.include)).collect();
        self.decode(f, f.feats2_no_mem.clone(), points, labels)
    }

    /// Start following the object from a slice that has been clicked.
    pub fn begin(&mut self, f: &Features, d: &Decoded) -> Result<Bank> {
        let (tokens, pos) = self.remember(f, d, 1.0)?;
        let mut bank = Bank::default();
        bank.start(tokens, pos, d.pointer.clone());
        Ok(bank)
    }

    /// Find the object on the slice `step` slices away from the clicked one.
    pub fn track(&mut self, f: &Features, bank: &mut Bank, step: usize) -> Result<Decoded> {
        let (_, diffs) = bank.chosen(step);
        let outputs = self
            .pointer
            .run(ort::inputs!["normalized_diffs" => tensor([POINTERS], diffs)?])
            .map_err(run_error)?;
        let pointer_pos = outputs["pointer_pos"]
            .try_extract_tensor::<f32>()
            .map_err(run_error)?
            .1
            .to_vec();
        drop(outputs);
        let (memory, memory_pos) = bank
            .assemble(&self.tpe, &pointer_pos, step)
            .ok_or_else(|| AiError("Nothing to follow yet".to_string()))?;
        let length = memory.len() / MEMORY_DIM;
        let outputs = self
            .attention
            .run(ort::inputs![
                "current_vision_features" => tensor([FEATURES, 1, HIDDEN], flatten(&f.feats2, HIDDEN))?,
                "current_vision_position_embeddings" => tensor([FEATURES, 1, HIDDEN], flatten(&f.position, HIDDEN))?,
                "memory" => tensor([length, 1, MEMORY_DIM], memory)?,
                "memory_pos" => tensor([length, 1, MEMORY_DIM], memory_pos)?,
            ])
            .map_err(run_error)?;
        let condition = outputs["conditioned_feats"]
            .try_extract_tensor::<f32>()
            .map_err(run_error)?
            .1
            .to_vec();
        drop(outputs);
        let decoded = self.decode(f, condition, vec![0.0, 0.0], vec![-1])?;
        let (tokens, pos) = self.remember(f, &decoded, 0.0)?;
        bank.push(step, tokens, pos, decoded.pointer.clone());
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_address_is_pinned_to_one_export() {
        for item in SAM2_TINY.model {
            assert!(item.url.starts_with(export!("")), "{}", item.url);
            assert!(item.url.ends_with(item.name), "{}", item.url);
        }
        for item in SAM2_TINY.model.iter().chain(SAM2_TINY.runtime) {
            assert_eq!(item.sha256.len(), 64, "{}", item.name);
        }
    }

    #[test]
    fn a_corrupted_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("ruthos-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        std::fs::write(&path, b"abc").unwrap();
        let good = Item {
            name: "f",
            url: "",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            entry: None,
        };
        assert!(verify(&path, &good).is_ok());
        std::fs::write(&path, b"abd").unwrap();
        let e = verify(&path, &good).unwrap_err();
        assert!(e.to_string().contains("checksum"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_region_comes_from_box_corners_held_to_the_slice() {
        let r = Region::between([9.5, -0.5], [19.5, 7.5], 100, 50).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (10, 0, 10, 8));
        let r = Region::between([90.2, 40.0], [500.0, 900.0], 100, 50).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (91, 40, 9, 10));
        assert_eq!(Region::between([10.0, 10.0], [12.0, 40.0], 100, 50), None, "too narrow");
        assert_eq!(Region::between([200.0, 0.0], [300.0, 40.0], 100, 50), None, "outside");
    }

    #[test]
    fn a_region_is_cut_out_and_its_mask_goes_back_in_place() {
        let r = Region {
            x: 1,
            y: 2,
            width: 2,
            height: 2,
        };
        let slice: Vec<f32> = (0..20).map(|v| v as f32).collect();
        assert_eq!(r.take(&slice, 5), vec![11.0, 12.0, 16.0, 17.0]);
        let mask = r.place(&[true, false, false, true], 5, 4);
        let on: Vec<usize> = (0..20).filter(|i| mask[*i]).collect();
        assert_eq!(on, vec![11, 17]);
        let c = r.local(Click {
            x: 1.5,
            y: 3.5,
            include: false,
        });
        assert_eq!((c.x, c.y, c.include), (0.5, 1.5, false));
        assert!(r.holds(1.0, 2.0) && r.holds(2.9, 3.9));
        assert!(!r.holds(3.0, 2.0) && !r.holds(1.0, 1.9));
    }

    #[test]
    fn a_mask_at_a_side_of_the_slice_is_not_called_cut_off() {
        let r = Region {
            x: 0,
            y: 10,
            width: 4,
            height: 4,
        };
        let mut left = vec![false; 16];
        left[4] = true;
        assert!(!r.reaches_edge(&left, 100, 100), "the slice ends there");
        let mut top = vec![false; 16];
        top[1] = true;
        assert!(r.reaches_edge(&top, 100, 100), "the slice goes on above");
        let mut right = vec![false; 16];
        right[7] = true;
        assert!(r.reaches_edge(&right, 100, 100));
        let mut inside = vec![false; 16];
        inside[5] = true;
        assert!(!r.reaches_edge(&inside, 100, 100));
    }

    #[test]
    fn a_region_reports_its_detail_emptiness_and_fit() {
        let r = Region::whole(512, 128);
        assert_eq!(r.pixels_per_cell(), 2.0);
        assert_eq!(r.stretch(), 4.0);
        let r = Region::whole(4, 2);
        assert_eq!(r.empty_share(&[0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.5, 0.5], 4), 5.0 / 8.0);
        let mut mask = vec![false; 8];
        assert_eq!(r.extent(&mask), 0.0);
        mask[1] = true;
        mask[2] = true;
        assert_eq!(r.extent(&mask), 0.5);
    }

    #[test]
    fn a_slice_is_squashed_to_the_full_square() {
        let (w, h) = (4, 2);
        let gray = vec![1.0; w * h];
        let p = to_pixels(&gray, w, h, [0.0; 3], [1.0; 3]);
        assert_eq!(p.len(), 3 * SIZE * SIZE);
        assert!(p.iter().all(|v| (*v - 1.0).abs() < 1e-6));
        let n = to_pixels(&gray, w, h, [0.5, 0.25, 0.0], [0.5, 0.5, 0.5]);
        assert_eq!((n[0], n[SIZE * SIZE], n[2 * SIZE * SIZE]), (1.0, 1.5, 2.0));
    }

    #[test]
    fn a_mask_comes_back_to_the_slice() {
        let mut mask = vec![-1.0; SIZE * SIZE];
        for y in 0..SIZE / 2 {
            for x in 0..SIZE / 2 {
                mask[x + SIZE * y] = 1.0;
            }
        }
        let s = to_slice(&mask, 4, 2);
        assert_eq!(s, [true, true, false, false, false, false, false, false]);
    }

    /// Acceptance 6: turning refinement off gives exactly what 0.1.8 gave.
    #[test]
    fn without_refinement_the_mask_is_the_models_own() {
        let mut logits = vec![-3.0; SIZE * SIZE];
        for y in 300..700 {
            for x in 200..500 {
                logits[x + SIZE * y] = 2.0 + (x ^ y) as f32 * 0.001;
            }
        }
        let (mask, own) = masks(&logits, || panic!("no values are read"), 77, 53, None);
        assert_eq!(mask, to_slice(&logits, 77, 53));
        assert_eq!(own, None);
    }

    #[test]
    fn refinement_reports_the_models_own_mask_beside_its_own() {
        let mut logits = vec![-3.0; SIZE * SIZE];
        for y in 256..768 {
            for x in 256..768 {
                logits[x + SIZE * y] = 3.0;
            }
        }
        let (w, h) = (40, 40);
        let mut values = vec![40.0; w * h];
        for y in 12..28 {
            for x in 12..28 {
                values[x + w * y] = 1200.0;
            }
        }
        let (mask, own) = masks(&logits, move || values, w, h, Some((Edge::Bone, 3)));
        let own = own.expect("kept");
        assert_eq!(own, to_slice(&logits, w, h));
        assert_ne!(mask, own, "the edge moved to the image's");
        assert!(mask[20 + w * 20]);
        assert!(!mask[11 + w * 20] && !mask[10 + w * 20], "and no further");
    }

    #[test]
    fn features_flatten_row_by_row() {
        let channels = 2;
        let values: Vec<f32> = (0..FEATURES * channels).map(|v| v as f32).collect();
        let f = flatten(&values, channels);
        assert_eq!(&f[..2], &[0.0, FEATURES as f32]);
        assert_eq!(&f[2..4], &[1.0, FEATURES as f32 + 1.0]);
    }

    #[test]
    fn a_short_bank_fills_the_shape_the_graph_takes() {
        let block = FEATURES * MEMORY_DIM;
        let tpe = vec![vec![0.0; MEMORY_DIM]; BLOCKS];
        let mut bank = Bank::default();
        bank.start(vec![1.0; block], vec![0.0; block], vec![9.0; HIDDEN]);
        bank.push(1, vec![2.0; block], vec![0.0; block], vec![8.0; HIDDEN]);
        let pos = vec![0.0; POINTERS * MEMORY_DIM];
        let (m, p) = bank.assemble(&tpe, &pos, 2).unwrap();
        assert_eq!(m.len(), 28736 * MEMORY_DIM);
        assert_eq!(p.len(), 28736 * MEMORY_DIM);
        assert_eq!(m[0], 1.0, "the conditioning memory comes first");
        assert_eq!(m[block], 2.0, "then the most recent");
        assert_eq!(m[6 * block], 2.0, "padded by repeating it");
        assert_eq!(m[7 * block], 9.0, "then the pointers, the conditioning frame's first");
    }

    #[test]
    fn distances_are_normalised_by_the_pointers_in_use() {
        let bank = Bank {
            pointers: vec![(0, vec![0.0; HIDDEN]), (1, vec![0.0; HIDDEN]), (2, vec![0.0; HIDDEN])],
            ..Default::default()
        };
        let (chosen, diffs) = bank.chosen(3);
        assert_eq!(chosen.len(), POINTERS);
        assert_eq!(&diffs[..3], &[1.5, 0.5, 1.0]);
        assert_eq!(diffs[15], 1.0, "padded by repeating the last");
    }
}
