# Ruthos 0.1.7-alpha

Ruthos is a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## New in 0.1.7-alpha

This release is about licences and decoder safety. Everyone on an earlier alpha should update.

- **Movie export now uses Cisco's own encoder.** H.264 is covered by patents, and Cisco's cover applies only to the copy of OpenH264 it distributes. Earlier alphas compiled the encoder into Ruthos; this one does not contain it. The first time you export a movie, Ruthos shows Cisco's licence and, if you agree, fetches the binary from Cisco and checks it against its published hash. Nothing is fetched unless you agree. OpenH264 Video Codec provided by Cisco Systems, Inc.
- **Third-party licences.** Every package now carries `THIRD_PARTY_LICENSES` in HTML and text, listing every component's copyright and licence, and CI fails if a dependency is missing from it or its licence is not one we accept.
- **About Ruthos.** A new window, from the row at the foot of the sidebar, with the version, build, acknowledgements and the licences.
- **Decoder hardening.** OpenJPEG moves to 2.5.4 with a further upstream fix that is in no release, built from a copy in this repository, as upstream now describes itself as unmaintained. The checks made before a file reaches a decoder now look at exactly what the decoder receives, for every frame, and they also apply when a file is converted to send to a PACS. Upstream changes to the C libraries are now reviewed nightly as well as their CVEs.

## Previously, in 0.1.6-alpha

- **3D in the MPR window.** GPU volume rendering in the fourth pane, with shaded, MIP and MinIP modes and presets. Rotate, window, move and zoom from a tool menu.
- **Cutting.** Build a box or a path, check it from any side, then cut inside or outside it. Remove piece takes everything connected to a click, such as the table. Undo and restore.
- **Segments.** A Segments panel with threshold, region grow from a click, keep largest, split into pieces, and morphology. Segments are coloured on the slices and in 3D, each with its volume and statistics. They are not saved with the study yet.
- **AI segmentation, on your machine.** Click a structure and SAM 2.1 Tiny finds it and follows it through the stack, with include and exclude clicks to correct it. It is an optional download of about 400 MB, shown with its licence and training data first, and no image leaves the computer. It was not trained on veterinary images: check every result against the slices. See `docs/segmentation.md`.
- **STL export** of a segment, or of each shown segment as its own file, in patient coordinates, for planning and education.
- **Guided review.** A client for guided teaching sessions, with follow, free view, hand-over of control, invitations and signed profiles. Ruthos includes no server: one comes from your educator or institution. See `docs/guided-review.md`.
- **Frame rates and speed.** Cine follows the file's rate and the screen's refresh, says when it is skipping frames, and offers slower speeds.
- **Hardening.** Files are checked before they reach a decoder, with limits you can set in Preferences; decoders are fuzzed continuously; and the listener accepts only devices you have listed. If you use the listener, add your devices under Preferences > Listener. SECURITY.md explains how to report a problem.
- **A Ruthos toolbar in MPR**, and a database sidebar that scrolls.

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
- Saving segments and cuts with the study (DICOM SEG), surface rendering in the 3D pane, and Horos's volume rendering opacity presets.
- macOS and Linux for the AI tool, which runs on Windows only for now.
- A signed installer. This build is an unsigned zip: Windows SmartScreen will warn on first run, and a PC with Smart App Control in enforce mode will refuse it. Building from source avoids both.

## Running

Unzip anywhere and run `ruthos.exe`. The archive and logs live in `%LOCALAPPDATA%\Ruthos\Ruthos Data`, settings in `%APPDATA%\Ruthos\settings.json`. To receive from a modality, turn the listener on in Preferences > Listener, point the modality at this computer's address, AE title RUTHOS, port 11112, and allow Ruthos through Windows Firewall when asked.

The archive format may change before 1.0. Keep your original files.

## Reporting problems

A file that does not import or display correctly is the most useful report you can make. Open an issue with the "file does not import" template and attach `ruthos.log` from the data folder.

## Verifying a download

`SHA256SUMS.txt` lists the checksum of every file in this release, and each file carries a GitHub build-provenance attestation recording the commit and workflow run that built it:

```
gh attestation verify Ruthos-0.1.7-alpha-win64.zip --repo Ruthos/Ruthos64
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
