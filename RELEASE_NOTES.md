# Ruthos 0.1.5-alpha

Ruthos is a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## New in 0.1.5-alpha

- **Tools in MPR.** A tool bar as in the 2D viewer: Crosshair, WL/WW, Move and Zoom, and Caliper, Angle, Rectangle, Ellipse, Arrow, Point and Text, with the same letter shortcuts. Measurements are in millimetres and belong to the plane they were drawn on; select one to move its handles, Delete removes it.
- **Lock planes**, so measuring cannot move or tilt them.
- **Layouts:** 1×3, 2×2 (the fourth pane is the original slice through the crosshair) or a single pane.
- **Save to study:** the plane, or a stack of planes across the volume, as a new series in the same study with its measurements; it opens, measures and reports like any other series. **Save image** writes a pane as a picture.
- **The dial is reliable:** every turn and press reaches the viewer you last used, whatever Windows says has focus, and the dial's own menu shows the same mode as Ruthos.

## Previously, in 0.1.4-alpha

- Orthogonal and oblique MPR with thick slabs, and the dial control (Surface Dial, ASUS Dial and DialPad) with keyboard shortcuts.

## What works

- **Archive.** Import folders, files, drag-and-drop, watch folders or a whole Horos database; a DICOM listener (C-STORE SCP, AE title RUTHOS, port 11112), off until you turn it on in Preferences, that files what modalities send; a Horos-style database window with albums, smart albums, the built-in Horos albums, search and filters, series thumbnails and a preview pane.
- **PACS.** Connection test, Query/Retrieve by study and series (C-FIND, C-GET, C-MOVE), Send (C-STORE) with conversion where needed, and automatic routing rules, all set up in Preferences with plain-language errors. The user guide is `docs/pacs.md`.
- **Codecs.** Uncompressed, RLE, deflate, JPEG baseline and 12-bit, JPEG lossless, JPEG-LS and JPEG 2000. Files without a meta header are accepted.
- **Viewer.** GPU window/level and the Horos colour lookup tables, zoom, pan, rotate, flip, cine at the file's frame rate, tiling of a series or of a study, per-cell lock, Horos corner annotations, key images, export to PNG, JPEG and MP4.
- **MPR.** Orthogonal and oblique planes through CT, MR and cone-beam series in 1×3, 2×2 or single layouts, with thick slab MIP, MinIP and mean, measuring and annotation tools, and saving planes or stacks to the study.
- **Measurements.** Caliper (distance, time, velocity), LV dimensions in M-mode and 2D with fractional shortening, angle, Cobb angle, angle between two lines, open polygon, rectangle, ellipse, polygon, pencil and brush areas, arrow, point and text, with ROI Manager, ROI Info, histograms and line plots. Every value states its scale basis (ultrasound region, calibrated pixel spacing, patient plane, estimated magnification, detector plane, hand calibration or uncalibrated) and the report flags values that are not at the patient's scale. Handles on every measurement with keyboard nudging and undo, a GPU loupe while placing points, a Measurements menu, and a library of definitions, sets and calculations (Teichholz, LA/Ao, VHS and more) that is edited in Preferences and shared as JSON.
- **Reports.** A report per study with the named measurements, the LV M-mode block, calculations, findings and conclusion, exported to PDF with the key images.
- **Anonymisation for teaching.** Teaching copies built by allowlist with derived UIDs, shifted dates, pixel masking of ultrasound regions or saved templates with review, a named teaching case register, a sidecar that carries measurements, and an independent verification pass before anything is written.
- **Pictures.** Import JPEG or PNG photographs as DICOM with a patient form; grayscale radiograph exports become monochrome so they window.
- **Preferences** window, and error containment: a fault in one window closes that window and writes `crash.log` instead of closing the program.

## Not yet

- Query by worklist (Modality Worklist) and storage commitment.
- Reading Horos report files and Horos's saved measurements (ROIs).
- Anything 3D.
- A signed installer. This build is an unsigned zip: Windows SmartScreen will warn on first run, and a PC with Smart App Control in enforce mode will refuse it. Building from source avoids both.

## Running

Unzip anywhere and run `ruthos.exe`. The archive and logs live in `%LOCALAPPDATA%\Ruthos\Ruthos Data`, settings in `%APPDATA%\Ruthos\settings.json`. To receive from a modality, turn the listener on in Preferences > Listener, point the modality at this computer's address, AE title RUTHOS, port 11112, and allow Ruthos through Windows Firewall when asked.

The archive format may change before 1.0. Keep your original files.

## Reporting problems

A file that does not import or display correctly is the most useful report you can make. Open an issue with the "file does not import" template and attach `ruthos.log` from the data folder.

## Verifying a download

`SHA256SUMS.txt` lists the checksum of every file in this release, and each file carries a GitHub build-provenance attestation recording the commit and workflow run that built it:

```
gh attestation verify Ruthos-0.1.5-alpha-win64.zip --repo Ruthos/Ruthos64
```

This alpha is not code-signed yet: Windows SmartScreen warns on first run (choose More info, then Run anyway), and Smart App Control in enforce mode refuses it. Signed builds through the SignPath Foundation follow once the project is accepted; even then SmartScreen can warn on the first downloads of a new version until its reputation builds.

## Code signing policy

Free code signing on Windows is provided by [SignPath.io](https://about.signpath.io), with a certificate by the [SignPath Foundation](https://signpath.org). macOS builds are signed with an Apple Developer ID and notarised by Apple.

Every signed release is built from this repository by the release workflow on GitHub-hosted runners, from a tagged commit. Each release carries SHA256 checksums and a GitHub build-provenance attestation; verify a download with `gh attestation verify <file> --repo Ruthos/Ruthos64`.

Team roles:

- Committers and reviewers: Mark Bowen
- Approvers of signing requests: Mark Bowen

## Privacy

Ruthos does not transmit any data to its developers or to third parties. It makes network connections only when the user configures them: the DICOM listener, which is off by default and receives only what the user's own devices send it, and DICOM connections the user initiates.
