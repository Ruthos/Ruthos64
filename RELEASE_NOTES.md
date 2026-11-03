# Ruthos 0.1.1-alpha

The first public build of Ruthos, a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## New in 0.1.1-alpha

- **One caliper, many quantities.** The Length tool is now the Caliper. In a 2D image it measures distance; in an M-mode strip depth and time; in a spectral Doppler strip time, the velocity at each end and the slope. A point in a Doppler strip gives velocity and the 4v² gradient. The scale comes from the ultrasound regions the scanner writes.
- **The name decides what is recorded.** Name a caliper RR interval and the time is stored (with the heart rate shown); AR or MV pressure half-time and the half-time is stored with its slope; VSD and the distance is stored. Where a quantity is not available the label says so instead of recording something else.
- **LV dimensions in 2D as well as M-mode,** with several sets per image. Each systole pairs with its diastole (left to right in M-mode, across frames in a 2D loop), FS is shown per set, and the report gives every set and the mean.
- **Measurement points move freely.** Drag an end anywhere to rotate, shorten or lengthen a line; LV inner points stay on the line. Grabbing a point now works first time: the press position is used, not where the pointer is once the drag starts.
- **Areas:** rectangle, ellipse and a new polygon, computed from the outline, so colour ultrasound shows an area too.
- **Tools and Measurements menus side by side:** Tools holds the mouse tools and one Measure row; Measurements picks what Measure draws.
- **Doppler set and calculations:** MV E and A, deceleration time and slope, MV and AR pressure half-time, Ao, PA, MR, TR and AR Vmax, RR interval; E/A, heart rate, MVA by pressure half-time and peak gradients. Repeated measurements are averaged in the report.
- The calibration basis panel names every unit and shows the Doppler baseline.
- **Fixed:** the anonymiser occasionally refused a valid teaching copy when the random date shift moved one date exactly onto another original date of the study (a study date onto the birth date, say). A correctly shifted date is now recognised as such; an unshifted original date is still refused.

## What works

- **Archive.** Import folders, files or drag-and-drop; a DICOM listener (C-STORE SCP, AE title RUTHOS, port 11112), off until you turn it on in Preferences, that files what modalities send; a Horos-style database window with albums, smart albums, the built-in Horos albums, search and filters, series thumbnails and a preview pane.
- **Codecs.** Uncompressed, RLE, deflate, JPEG baseline and 12-bit, JPEG lossless, JPEG-LS and JPEG 2000. Files without a meta header are accepted.
- **Viewer.** GPU window/level and the Horos colour lookup tables, zoom, pan, rotate, flip, cine at the file's frame rate, tiling of a series or of a study, per-cell lock, Horos corner annotations, key images, export to PNG and JPEG.
- **Measurements.** Caliper (distance, time, velocity), LV dimensions in M-mode and 2D with fractional shortening, angle, rectangle, ellipse and polygon areas, arrow and point. Every value states its scale basis (ultrasound region, calibrated pixel spacing, patient plane, estimated magnification, detector plane, hand calibration or uncalibrated) and the report flags values that are not at the patient's scale. Handles on every measurement with keyboard nudging and undo, a GPU loupe while placing points, a Measurements menu, and a library of definitions, sets and calculations (Teichholz, LA/Ao, VHS and more) that is edited in Preferences and shared as JSON.
- **Reports.** A report per study with the named measurements, the LV M-mode block, calculations, findings and conclusion, exported to PDF with the key images.
- **Anonymisation for teaching.** Teaching copies built by allowlist with derived UIDs, shifted dates, pixel masking of ultrasound regions or saved templates with review, a named teaching case register, a sidecar that carries measurements, and an independent verification pass before anything is written.
- **Pictures.** Import JPEG or PNG photographs as DICOM with a patient form; grayscale radiograph exports become monochrome so they window.
- **Preferences** window, and error containment: a fault in one window closes that window and writes `crash.log` instead of closing the program.

## Not yet

- DICOM send and query/retrieve.
- Importing an existing Horos database.
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
gh attestation verify Ruthos-0.1.1-alpha-win64.zip --repo Ruthos/Ruthos64
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
