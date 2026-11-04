# Ruthos

**A Horos-inspired DICOM viewer for Windows, written in Rust.** Website: <https://ruthos.github.io/Ruthos64/>

Ruthos is a community project to bring the Horos / OsiriX experience to Windows: the same database window, the same 2D viewer, the same tools, the same corner annotations, built as a single native binary on a modern GPU stack. It is not a port of Horos. Horos is Objective-C and Cocoa to the bone, so Ruthos re-implements its feature set and user interface on top of Rust libraries.

> **Status: alpha.** It works end to end on real studies and is rough in places. Tier 1, the complete 2D viewer, is done: a patient archive that imports folders, files, pictures, watch folders and whole Horos databases; a DICOM listener, PACS query/retrieve, send and automatic routing; a Horos-style database window with albums, smart albums, thumbnails and a preview pane; a GPU viewer with window/level, CLUTs, zoom, pan, rotate, flip, cine, tiling and the Horos measurement tools, calibrated from DICOM and ultrasound regions, with ROI Manager, ROI Info, histograms and plots; reports to PDF; anonymised teaching copies; and export to DICOM, PNG, JPEG and MP4. See [RELEASE_NOTES.md](RELEASE_NOTES.md) for what the current build does and does not do, and [CONTRIBUTING.md](CONTRIBUTING.md) if you want to help.

![The database window](docs/screenshots/database.png)

![The 2D viewer](docs/screenshots/viewer.png)

---

## Why

Horos is the free DICOM viewer of choice in a large number of veterinary practices. It is also Mac-only, and its upstream has slowed. Practices on Windows have Weasis, OHIF in a browser, or paid tools, none of which feel like Horos. Ruthos exists to fill that gap: a free, open viewer that a Horos user can sit down at and already know how to use.

Three things justify a new codebase rather than another wrapper:

- **One small native binary.** No Java runtime, no bundled browser, no Python. Install it and open studies.
- **GPU first.** Window/level, colour lookup tables, MPR and volume rendering all run on the GPU through `wgpu`, so large CT and CBCT volumes stay interactive on ordinary clinic hardware.
- **The Horos workflow.** Albums and smart albums, the series thumbnail drawer, propagate settings, key images, ROI tools and the familiar toolbars. Where Horos made a design decision, Ruthos follows it unless there is a strong reason not to.

## What it will look like

Ruthos mirrors the two windows that make up a Horos session.

**Database window.** A source list on the left (local database, albums, smart albums, PACS nodes), an outline table of studies with series as expandable rows, a series thumbnail strip along the bottom, and the Horos toolbar: Import, Export, Query/Retrieve, Send, Anonymize, Trash, 2D Viewer, Key Images and ROIs, Report, modality and date filters, Search.

**2D viewer window.** A series thumbnail drawer on the left, the image viewport with the Horos four-corner annotation layout, and the viewer toolbar: Database, Window tiling, Series, Annotations, Patient, Tools, WL/WW and CLUT, Reconstruction, Orientation, Fusion, Export, Sync, Propagate settings, Play and Speed.

The annotation layout follows the Horos defaults:

| Position | Content |
|---|---|
| Top left | Image size, view size, WL/WW, mouse position in pixels and mm |
| Top right | Patient name, ID and age, study description, series description, series number |
| Bottom left | Zoom and rotation, image position, compression, slice thickness and location |
| Bottom right | MRI timing (TE/TR), field strength, acquisition date |
| Edge midpoints | Orientation letters |

**Tool palette.** The 2D tools from Horos: window/level, zoom, pan, rotate, caliper (length, time, velocity), LV dimensions, angle, Cobb angle, dynamic angle, rectangle, ellipse, open and closed polygon, pencil, point, text, arrow, brush ROI and repulsor; selecting, moving and editing any measurement by its handles.

Colour lookup tables and volume-rendering presets are read directly from the Horos definition files, so "Hot Iron", "Jet", "VR Bones" and the rest look the same.

## The dial

On Windows, a Surface Dial, an ASUS Dial or a ProArt DialPad (set to Microsoft Wheel Device) steps frames and slices and fine-tunes window width and level, a measurement point and the loupe. A press changes the mode; Preferences > Viewer > Dial sets it up. Every action also has a keyboard shortcut, and a DialPad that cannot act as a wheel device can be mapped to them in ProArt Creator Hub. See `docs/dial.md`.

| Action | Anticlockwise | Clockwise |
|---|---|---|
| Navigate | Ctrl+Shift+Left | Ctrl+Shift+Right |
| Window width | Ctrl+Shift+Down | Ctrl+Shift+Up |
| Window level | Ctrl+Shift+Page Down | Ctrl+Shift+Page Up |
| Measurement point | Alt+Left | Alt+Right |
| Loupe magnification | Ctrl+Shift+Minus | Ctrl+Shift+Plus |
| Next mode | | Ctrl+Shift+M |

## Roadmap

Work is organised in tiers. Each tier is usable on its own.

### Tier 1: a complete 2D viewer (done)

- Import files and folders into a local database; watch folders
- Import an existing Horos or OsiriX database, including albums, smart albums, comments, status and key images (see `docs/moving-from-horos.md`)
- Database window with albums, smart albums, search and filters
- 2D viewer: window/level, presets, CLUTs, zoom, pan, rotate, flip, scroll, cine
- Multi-series tiling, sync and propagate settings
- Measurement and ROI tools with ROI Info and the ROI Manager
- Key images, comments and study state
- Pixel value readout, histograms and line plots
- DICOM listener (C-STORE SCP), send (C-STORE SCU), query/retrieve (C-FIND, C-GET, C-MOVE), echo and automatic routing (see `docs/pacs.md`)
- Export to DICOM, JPEG, PNG and MP4
- Measurements with a stated scale basis, editable handles with undo, a GPU loupe, a Measurements menu, an LV tool with fractional shortening in M-mode and 2D, and a library of definitions, sets and calculations with JSON import and export (see `docs/measurements.md`)
- Anonymisation for teaching: allowlist header profile, derived UIDs, shifted dates, ultrasound-region and template pixel masking with review, named teaching cases, sidecar for measurements, independent verification pass (see `docs/anonymiser.md`)
- Preferences window

### Tier 2: MPR, data and integration

- Orthogonal and oblique MPR with linked crosshairs, thick slab MIP, MinIP and mean (done, see `docs/mpr.md`); curved MPR; reference lines and 3D position across series in the 2D viewer
- Measurements, calculations and findings saved as DICOM Structured Reports (measurement reports, TID 1500) and read back in, so they travel with the study to a PACS and other viewers; key object selection documents for key images
- Measurements and reports exported as data (CSV and JSON) for research, audit and teaching
- API ready for practice management systems (PMS). Comments, measurements, reports and annotated still images are produced through one documented, versioned interface, so a PMS can be told when a study is reported and receive its findings, values and key images with their annotations. No particular PMS integration is promised yet; the principle is that connecting one needs a small connector, not changes to Ruthos

### Tier 3: volume and surface rendering (in progress, see `docs/segmentation.md`)

- GPU volume rendering with presets, MIP and MinIP, and cutting by box, path and connected piece (done); the Horos opacity presets and a transfer function editor
- Segments: threshold, region grow, islands and morphology, on the slices and in 3D, with volumes (done); saving as DICOM SEG
- AI segmentation on the user's machine with SAM 2.1 (done, Windows)
- Mesh export to STL (done); surface rendering in the 3D pane, decimation, and OBJ, PLY and 3MF

### Tier 4: the future

- PET-CT and other fusion
- Hanging protocols
- A plugin interface (Horos plugins are Objective-C bundles and do not carry over)
- DICOM print

Not planned: anything that depended on Apple-only services in Horos (iChat, Mail scripting, disc burning, QuickTime, iCloud).

## Architecture

Ruthos is a Cargo workspace. Each crate has a single job and a clear boundary.

| Crate | Job |
|---|---|
| `ruthos-core` | Data model (Study, Series, Image, Album), SQLite storage, import pipeline, file store, smart-album predicates, Horos database migration |
| `ruthos-dicom` | Wraps [dicom-rs](https://github.com/Enet4/dicom-rs). Decoding to typed pixel buffers, rescale, geometry, default WL, CLUT and preset loaders |
| `ruthos-net` | DICOM networking on the dicom-rs upper layer: listener, send, query/retrieve, echo |
| `ruthos-render` | `wgpu` pipelines: texture upload, WL/CLUT shader, ROI geometry, annotation text, and the volume ray marcher |
| `ruthos-segment` | Segments: label maps, classical tools, surface meshes and export, and the optional AI tool (Cargo feature `ai`) |
| `ruthos-session` | The client side of guided teaching sessions through a session relay |
| `ruthos-ui` | The windows, one module per Horos window, built with `egui` |
| `ruthos` | The application binary, settings, updates |

### Technology choices

- **Language:** Rust, stable toolchain, MSVC target on Windows.
- **DICOM:** dicom-rs, with its own decoders for RLE and deflate, libjpeg-turbo for JPEG, CharLS for JPEG-LS and OpenJPEG for JPEG 2000 (rather than Grok, to stay clear of AGPL).
- **Rendering:** `wgpu` over DirectX 12, with Vulkan as a fallback.
- **UI:** `egui` with a custom theme and a small set of purpose-built widgets (toolbar, source list, outline table, thumbnail strip) that give the Horos look.
- **Storage:** SQLite via `rusqlite`.
- **Minimum platform:** Windows 10 version 1809 or later, 64-bit.

### Features, not workarounds

dicom-rs is strict and standards-based, and Ruthos leans on that. Vendor-specific quirks are handled in one isolated module behind a single trait, each with a test file, so they never leak into feature code.

## Relationship to Horos

Ruthos is an independent project. It is not affiliated with or endorsed by the Horos Project or Nimble Co LLC. "Horos" and "OsiriX" are trademarks of their respective owners.

Ruthos borrows from Horos under the terms of its licence: icons, colour lookup tables, volume-rendering presets, annotation defaults, window-level presets, and the shape of its database schema. Where Horos logic is translated into Rust, the source file is credited in a comment.

### Uninstalling

Ruthos installs nothing. Quit it and delete the folder you unzipped. To remove the archive as well, delete `%LOCALAPPDATA%\Ruthos` (this deletes every imported study, so export what you need first), and delete `%APPDATA%\Ruthos` for the settings.

## Code signing policy

Free code signing on Windows is provided by [SignPath.io](https://about.signpath.io), with a certificate by the [SignPath Foundation](https://signpath.org). macOS builds are signed with an Apple Developer ID and notarised by Apple.

Every signed release is built from this repository by the release workflow on GitHub-hosted runners, from a tagged commit. Each release carries SHA256 checksums and a GitHub build-provenance attestation; verify a download with `gh attestation verify <file> --repo Ruthos/Ruthos64`.

Team roles:

- Committers and reviewers: Mark Bowen
- Approvers of signing requests: Mark Bowen

## Privacy

Ruthos does not transmit any data to its developers or to third parties. It makes network connections only when the user configures them: the DICOM listener, which is off by default and receives only what the user's own devices send it, and DICOM connections the user initiates.

## Licence

Ruthos is licensed under the **GNU Lesser General Public License v3.0**, the same licence as Horos. This is deliberate: it keeps Ruthos free to reuse Horos assets and logic, and it keeps Ruthos free.

## Building

You will need:

1. The Rust toolchain via [rustup](https://rustup.rs). Stable channel, `x86_64-pc-windows-msvc` target.
2. Microsoft Visual C++ Build Tools (the "Desktop development with C++" workload). The MSVC compiler links Rust and builds the three C/C++ codecs below.
3. [CMake](https://cmake.org/download/) on the `PATH` (or in the `CMAKE` environment variable). It builds the three C/C++ codecs: OpenJPEG (JPEG 2000), CharLS (JPEG-LS) and libjpeg-turbo (JPEG, including 12-bit).
4. [NASM](https://www.nasm.us/) on the `PATH`, optional. Without it libjpeg-turbo builds without SIMD and decodes more slowly.
5. Git.

Then:

```powershell
git clone https://github.com/Ruthos/Ruthos64.git
cd Ruthos64
cargo build --release
cargo test --workspace
```

The binary is `target\release\ruthos.exe`. The database and imported files live in `%LOCALAPPDATA%\Ruthos\Ruthos Data`, settings in `%APPDATA%\Ruthos\settings.json` (edit them in the Preferences window, Ctrl+comma), and `ruthos.log` and `crash.log` in the data folder.

**macOS and Linux.** The code is cross-platform and continuous integration tests it on all three (`.github/workflows/ci.yml`), but Ruthos is built and used on Windows today. On macOS you need Rust, the Xcode command-line tools (`xcode-select --install`) and CMake (`brew install cmake`); on Linux, Rust, CMake, NASM and the GTK 3, xkbcommon, Wayland and X11 development packages. `cargo build --release` then works the same way. Data lives in `~/Library/Application Support/Ruthos` on macOS and under the XDG directories on Linux. See `docs/cross-platform.md` for what differs per platform and what still needs checking by hand on a Mac.

Developer options, all environment variables: `RUTHOS_DATA_ROOT` (use another data folder), `RUTHOS_OPEN_STUDY`, `RUTHOS_OPEN_SERIES`, `RUTHOS_OPEN_IMAGE`, `RUTHOS_TILING` and `RUTHOS_SELECT_ROI` (open a study, series and tiling at startup with a stored measurement selected), `RUTHOS_LOUPE_AT=x,y` (show the loupe at an image point), `RUTHOS_BASIS_PANEL=1` (open the calibration basis panel), `RUTHOS_ZOOM=0.8` (scale the interface), `RUTHOS_SETTINGS=<file>` (use another settings file), `RUTHOS_OPEN_QUERY=<PACS index>` (open Query/Retrieve and search), `RUTHOS_QUERY_EXPAND=1` (show the first result's series), `RUTHOS_OPEN_PREFS` (open Preferences at a section), `RUTHOS_RENDERER=software` (force the CPU renderer), `WGPU_BACKEND` (dx12, metal, vulkan or gl) and `RUTHOS_DEBUG_PANIC=viewer` (exercise the error dialog).

**Windows 11 Smart App Control.** If it is on, it will block the unsigned executables that every Rust build produces (build scripts, tests, the app). Developers need it off on their build machine. Released builds will be signed so end users can keep it on.

### Codec coverage

Verified by `crates/ruthos-dicom/tests/samples.rs` against real files:

| Transfer syntax | Status |
|---|---|
| Implicit and Explicit VR Little Endian, Big Endian, Deflated | Decodes |
| RLE Lossless | Decodes |
| JPEG Baseline, JPEG Extended 12-bit (Process 2 & 4), JPEG Lossless 8/12/16-bit | Decodes (libjpeg-turbo 3; non-standard scan headers from some modalities are repaired) |
| JPEG-LS Lossless and Near-lossless | Decodes (CharLS) |
| JPEG 2000 Lossless and Lossy | Decodes (OpenJPEG) |
| 32-bit pixel data (RT Dose) | Not yet |
| Files with no DICOM meta header | Accepted (Explicit, then Implicit VR LE) |

## Contributing

The project is at alpha, so the most useful contributions right now are:

- Horos users describing the workflows they rely on, as issues
- Sample DICOM studies that can be redistributed, especially veterinary CR, DR, CT and MRI
- Review of the architecture above before it hardens into code

If you know Horos well, the `ruthos-ui` crate is organised by Horos window name (Browser, Viewer, ThumbnailsList, Query, Send, ROI, ROIManager, Preferences and so on), so you will find your way around.

## Not for diagnostic use

Ruthos is not a medical device and has not been cleared or approved by any regulatory body. Its intended purpose is viewing, measuring and archiving veterinary images, and research and education. It is not intended for use on human patients. Do not use it as the sole basis for clinical decisions.
