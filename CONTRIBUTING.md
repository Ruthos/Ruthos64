# Contributing to Ruthos

Ruthos is a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. Contributions of every size are welcome: a vendor's sample file that decodes wrongly is as useful as a new window.

## Building

1. Install the Rust toolchain from [rustup.rs](https://rustup.rs) (stable, `x86_64-pc-windows-msvc`).
2. Install the Visual C++ Build Tools ("Desktop development with C++").
3. Install [CMake](https://cmake.org/download/) and put it on the `PATH`. It builds the three C codecs (OpenJPEG, CharLS, libjpeg-turbo). [NASM](https://www.nasm.us/) is optional and makes JPEG decoding faster.
4. Clone and build:

```powershell
git clone https://github.com/Ruthos/Ruthos64.git
cd Ruthos64
cargo build --release
cargo test --workspace
```

The binary is `target\release\ruthos.exe`. If Windows 11 Smart App Control is in enforce mode it will block the unsigned executables every Rust build produces; developers need it off on their build machine.

To try changes against a throwaway archive instead of your real one:

```powershell
$env:RUTHOS_DATA_ROOT = "C:\scratch\ruthos-data"
.\target\release\ruthos.exe
```

Other developer switches are listed at the end of the README.

## Where things live

One crate per job, one module per Horos window, so people who know Horos find their way around.

| Path | What |
|---|---|
| `crates/ruthos-core` | The archive: SQLite schema (`db.rs`), import pipeline, thumbnails, settings, reports as PDF |
| `crates/ruthos-dicom` | Reading DICOM: metadata (`meta.rs`), pixel decoding (`pixels.rs`, `jpeg.rs`), CLUTs and presets from the Horos definition files, the picture importer (`sc.rs`), vendor quirks (`quirks.rs`) |
| `crates/ruthos-net` | DICOM networking: the C-STORE listener, echo |
| `crates/ruthos-render` | The `wgpu` image pipeline: texture upload, window/level and CLUT shader, the view transform |
| `crates/ruthos-ui` | The windows: `browser.rs` (database window), `viewer.rs` (2D viewer), `report.rs`, `prefs.rs`, plus `widgets.rs`, `theme.rs`, `icons.rs` |
| `crates/ruthos` | The binary: logging, GPU selection, startup |
| `assets/horos` | CLUTs, annotation defaults and icons taken from Horos under the LGPL |
| `testdata/pydicom` | Sample files (MIT) that the codec tests run over |

## Ground rules

- **Features, not workarounds.** Vendor-specific behaviour goes in `quirks.rs` behind one trait, with a test file, so it never leaks into feature code.
- **Match Horos unless there is a strong reason not to.** When in doubt about a layout, a shortcut or a default, do what Horos does and say so in the PR.
- **No patient data in the repository.** Sample files must be redistributable and free of identifying data. Screenshots in PRs and issues must not show real patients.
- **Tests run on `cargo test --workspace`.** A codec change needs a sample that exercises it; a database change needs a round-trip test in `db.rs`.
- **UI changes come with a screenshot** in the PR, before and after if it is a change to something that exists.
- Format with `cargo fmt` (the repo has a `rustfmt.toml`) and keep `cargo clippy` quiet.

## Reporting a file that does not import or display

This is the most valuable report we get, because every modality writes DICOM a little differently. Please include:

- The modality and vendor (for example "GE Vivid q", "Fujifilm CR").
- What happened: not imported, imported but blank, wrong colours, wrong measurements.
- `ruthos.log` and, if Ruthos showed an error dialog, `crash.log`. Both are in the data folder, `%LOCALAPPDATA%\Ruthos\Ruthos Data` by default.
- If you can share a file, anonymise it first (we will add anonymisation to Ruthos; until then DICOM Cleaner or dcmtk's `dcmodify` work). A single still image is usually enough.

## Licence

Ruthos is LGPL-3.0, the same licence as Horos. By contributing you agree your contribution is licensed the same way.
