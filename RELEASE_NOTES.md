# Ruthos 0.1.9-alpha

Ruthos is a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## New in 0.1.9-alpha

- **Import Horos Database now works on Windows.** The command was only ever on the macOS menu, so Windows users could not reach it. It is now near the foot of the database sidebar.
- **Catalogue a drive.** Catalogue Drive, under DRIVES in the sidebar, lists every DICOM study on a drive or folder without copying its images or writing to the drive. Studies stay searchable with the drive unplugged and open when it is connected, and a drive that comes back under another letter is found again. See `docs/catalogue-drive.md`.
- **Hide smart folders.** Right-click a smart folder to hide it, and Show hidden brings them all back.

## Previously, in 0.1.8-alpha

- **Finer AI segmentation with the cut box.** The model stretches what it is given to a square and makes its mask in 256 by 256 cells, so on a whole slice thin bone can come out too thick and a fine fracture line can be filled in. Draw a Cut box around the object in the 3D pane and the model sees only the box and follows only through its slices, which keeps much more detail. Nothing is cut from the volume. The panel shows the detail you are getting and warns when the box is mostly empty, too loose or long and thin, or when the object reaches its side and may be cut off. Check every result against the slices. See `docs/segmentation.md`.
- **Hardening.** Two problems the nightly fuzzing found are fixed: a small malformed file could make Ruthos ask for a very large amount of memory, and an unusual date value could stop an anonymisation run.
- **Acknowledgements** now name Ruthos's author and are on the website as well as in the About window and the README.

## What works

- **Archive.** Import folders, files, drag-and-drop, watch folders or a whole Horos database, or catalogue a drive where it is; a DICOM listener (C-STORE SCP, AE title RUTHOS, port 11112), off until you turn it on in Preferences, that files what modalities send; a Horos-style database window with albums, smart albums, the built-in Horos albums, search and filters, series thumbnails and a preview pane.
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
gh attestation verify Ruthos-0.1.9-alpha-win64.zip --repo Ruthos/Ruthos64
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
