# Ruthos 0.1.0-alpha

The first public build of Ruthos, a Horos-inspired DICOM viewer and patient archive for Windows, written in Rust. It is an alpha: it works end to end on real studies, and it is rough in places. It is not for diagnostic use.

## What works

- **Archive.** Import folders, files or drag-and-drop; a DICOM listener (C-STORE SCP, AE title RUTHOS, port 11112) that files what modalities send; a Horos-style database window with albums, smart albums, the built-in Horos albums, search and filters, series thumbnails and a preview pane.
- **Codecs.** Uncompressed, RLE, deflate, JPEG baseline and 12-bit, JPEG lossless, JPEG-LS and JPEG 2000. Files without a meta header are accepted.
- **Viewer.** GPU window/level and the Horos colour lookup tables, zoom, pan, rotate, flip, cine at the file's frame rate, tiling of a series or of a study, per-cell lock, Horos corner annotations, key images, export to PNG and JPEG.
- **Measurements.** Length, angle, rectangle, oval, arrow and point, calibrated from Pixel Spacing or from the ultrasound region under the measurement (M-mode strips measure vertically). A hand calibration from a known length when the file has none. Click a measurement's number to name it; the names come from editable sets (echo LV M-mode, 2D, radiography, orthopaedic).
- **Reports.** A report per study with the named measurements, findings and conclusion, exported to PDF with the key images.
- **Pictures.** Import JPEG or PNG photographs as DICOM with a patient form; grayscale radiograph exports become monochrome so they window.
- **Preferences** window, and error containment: a fault in one window closes that window and writes `crash.log` instead of closing the program.

## Not yet

- DICOM send, query/retrieve and anonymisation.
- Importing an existing Horos database.
- Anything 3D.
- A signed installer. This build is an unsigned zip: Windows SmartScreen will warn on first run, and a PC with Smart App Control in enforce mode will refuse it. Building from source avoids both.

## Running

Unzip anywhere and run `ruthos.exe`. The archive and logs live in `%LOCALAPPDATA%\Ruthos\Ruthos Data`, settings in `%APPDATA%\Ruthos\settings.json`. Point a modality at this computer's address, AE title RUTHOS, port 11112, and allow Ruthos through Windows Firewall when asked.

The archive format may change before 1.0. Keep your original files.

## Reporting problems

A file that does not import or display correctly is the most useful report you can make. Open an issue with the "file does not import" template and attach `ruthos.log` from the data folder.
