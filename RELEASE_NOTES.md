# Ruthos 0.1.14-alpha

Ruthos is a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## Read this first

- **Ruthos is for veterinary, research and teaching use.** It is not for human patients and it is not a medical device. It has no regulatory clearance of any kind, and it must not be the basis of a clinical decision.
- **Refine edges is experimental and off by default.** AI segments of thin bone can still come out thick, and teeth can pull the level up for the cortex beside them. Check every AI result against the slices before you use its volume.
- **DICOMweb is tested against Orthanc.** Cloudflare Access, OpenID Connect, dcm4chee-arc and the cloud DICOMweb services are not yet tested; `docs/dicomweb.md` keeps the compatibility table.
- **The AI tool runs on Windows only.** macOS and Linux builds have everything else.
- **The archive format may change before 1.0.** Keep your original files.
- **Research-mode models carry their own licences.** nnInteractive is non-commercial (CC BY-NC-SA 4.0) and VISTA3D is for research and non-clinical use only (NVIDIA's model card and the NVIDIA Open Model License). Each shows its terms before first use, and whether your use is allowed is your responsibility.

## New in 0.1.14-alpha

- **Measurements travel with the study as DICOM.** Save as DICOM in the report window writes the measurements, their derived values, the calculations, the findings and the conclusion into the study as a structured report (TID 1500), and the key images as a key object selection document, so they go with it to a PACS or another viewer. Reports and key image documents that arrive with a study become measurements and key images once their images are in the archive. Other software is not yet confirmed to read Ruthos's reports. See `docs/measurements.md`.
- **Measurements as data.** Right-click a study to export its measurements, derived values, calculations and report as CSV or JSON, for one study or the whole list, with or without the patient's identity.
- **Horos's 3D presets and an editor.** Horos's 29 volume rendering presets, read as Horos reads them, and an editor for colours and opacity over a histogram of the volume, with presets of your own. Cuts in 3D are now kept with the study.
- **Surfaces and 3D printing.** Segments can be shown as smooth surfaces in the 3D pane. Meshes export as STL, OBJ, PLY or 3MF, with fewer triangles if you like, staying closed for printing. See `docs/segmentation.md`.
- **Worklist and storage commitment.** Import Pictures can fill in the patient and accession from a Modality Worklist, and after sending, Ruthos can ask a PACS to confirm it holds every image. Neither has been tried against a real worklist server or PACS yet. See `docs/pacs.md`.
- **The study list sorts by any heading,** and again to reverse it. A still on the echo timeline is one image, not a stack.

## Previously, in 0.1.13-alpha

- **Echo timeline.** Stills and loops of an echo are numbered in one sequence, but each loop is its own series, so the stills used to stack apart from them. The viewer's drawer now offers Timeline, which lists every image in the order it was stored, with its time and any long pause, and Grouped, the old view. Page Up and Page Down move along the timeline. Times are read on import, or the first time an older study opens. Files and series do not change. See `docs/measurements.md`.
- **Smooth in anatomical M-mode.** A new option, on by default, blends between frames on screen and in a saved strip, and the saved file records it. It adds no readings: untick it to see and save each frame as a block.
- **A saved strip appears straight away.** Open viewers now show new series as soon as an import finishes, so a study arriving over the network appears too.
- **A playing loop no longer holds up the app.** On Windows a viewer playing a loop kept the database window from updating at all, and a viewer could not close until the loop stopped. Both are fixed. Playback pauses very briefly about every 0.7 seconds to let the main window paint.

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

- Reading Horos report files and Horos's saved measurements (ROIs).
- Clipping surfaces with the live box in 3D; a cut made with it does apply.
- Downloading VISTA3D and nnInteractive from within Ruthos.
- Opening saved segments in other viewers, which has not been confirmed.
- macOS and Linux for the AI tool, which runs on Windows only for now.
- An installer and a Microsoft Store listing. This build is a zip; see below for signing.
- dcm4chee-arc and cloud DICOMweb services, IHE AIW-I and UPS-RS.

## Running

Unzip anywhere and run `ruthos.exe`. The archive and logs live in `%LOCALAPPDATA%\Ruthos\Ruthos Data`, settings in `%APPDATA%\Ruthos\settings.json`. To receive from a modality, turn the listener on in Preferences > Listener, point the modality at this computer's address, AE title RUTHOS, port 11112, and allow Ruthos through Windows Firewall when asked.

The archive format may change before 1.0. Keep your original files.

## Reporting problems

A file that does not import or display correctly is the most useful report you can make. Open an issue with the "file does not import" template and attach `ruthos.log` from the data folder.

## Verifying a download

`SHA256SUMS.txt` lists the checksum of every file in this release, and each file carries a GitHub build-provenance attestation recording the commit and workflow run that built it:

```
gh attestation verify Ruthos-0.1.14-alpha-win64.zip --repo Ruthos/Ruthos64
```

This alpha is not code-signed unless the release page says so: Windows SmartScreen warns on first run (choose More info, then Run anyway), and Smart App Control in enforce mode refuses it. Signed builds through the SignPath Foundation follow once the project is accepted; even then SmartScreen can warn on the first downloads of a new version until its reputation builds.

## Code signing policy

Ruthos has applied to the [SignPath Foundation](https://signpath.org) for free code signing of its Windows releases, and the application is pending. Until it is accepted, releases are unsigned: Windows SmartScreen warns on first run, and a PC with Smart App Control switched on refuses to run them.

Once accepted, free code signing on Windows will be provided by [SignPath.io](https://about.signpath.io), with a certificate by the SignPath Foundation, and macOS builds will be signed with an Apple Developer ID and notarised by Apple. Every release, signed or not, is built from this repository by the release workflow on GitHub-hosted runners, from a tagged commit, and carries SHA256 checksums and a GitHub build-provenance attestation; verify a download with `gh attestation verify <file> --repo Ruthos/Ruthos64`.

Team roles:

- Committers and reviewers: Mark Bowen
- Approvers of signing requests: Mark Bowen

## Privacy

Ruthos does not transmit any data to its developers or to third parties. It makes network connections only when the user configures them or asks for them: the DICOM listener, which is off by default and accepts only devices the user has listed; DICOM and DICOMweb connections the user initiates; a guided review server the user chooses to join, which receives the view state of a teaching copy and the participant's name; the optional AI model, fetched once on request; and Cisco's video encoder, fetched once from Cisco with the user's consent, the first time a movie is exported. AI segmentation runs on the user's own computer, and no image data leaves it. A Share package's viewer makes no network connection at all.
