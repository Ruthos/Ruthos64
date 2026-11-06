# Sharing images

**Share** in the database window puts a patient's images in a folder or a zip file
that an owner or a referring vet can open, with no installation and no database.

## What the recipient gets

| In the folder | What it is for |
|---|---|
| `Ruthos Viewer.exe` | Ruthos, renamed and otherwise unchanged. Opens the images in this folder on Windows. |
| `Read me first.html` | Opens on any device. Shows the studies, key images and a link to the report. |
| `Report.pdf` | The report, if the study has one. |
| `Key images\` | The key images as JPEG. |
| `DICOMDIR`, `DICOM\` | The images, laid out to the DICOM standard, for any viewer or PACS. |
| `ruthos.ini` | Makes the program open this folder as a viewer. |
| `ruthos-package.json` | Measurements, calibrations and key image marks. |
| `Licences\` | Ruthos's licence and notices. |

Images are copied byte for byte, and every file is read back after writing. The
folder takes its real name only when everything has checked out, so a package that
exists is complete. If an image is missing, nothing is exported.

**Leave out the owner's name and contact details** rewrites each file without the
standard owner fields, and takes the family name off the Patient Name, as in veterinary
use this is often the owner's surname. Pixel data is not touched. It does not reach
words typed into the report or names burned into images, which ultrasound machines
often do. Packages are not encrypted: anyone who has one can read it.

## What the viewer does

A run started beside a `ruthos.ini` with a `[viewer]` section, or with `--view`, is
separate from any Ruthos on the same computer. It uses an in-memory database and a
temporary folder deleted when it closes. It reads no settings, writes nothing beside
the images, changes no registry keys, keeps no log, listens on no port and makes no
network connection. Measurements made in it are not saved.

Only a `ruthos.ini` beside the program counts, so nothing elsewhere on a computer can
turn someone's own Ruthos into a viewer.

If the program is run from inside a zip that was never extracted, or copied away from
its images, it says so and stops.

## Opening a DICOMDIR

Importing a DICOMDIR, or a folder with one in it, lists what is there first. **Import**
copies the ticked studies into the archive. **View without importing** opens a
separate viewer, so the archive is not touched.

A DICOMDIR is treated as an index only: stored details come from the files it names.
Discs from other systems are read tolerantly (names in any case, ISO 9660 version
suffixes, broken record links), and a path can lead nowhere outside the DICOMDIR's
folder. If a DICOMDIR cannot be read, the folder is scanned instead.

## Limits

- On Windows, an unsigned program from a download shows a SmartScreen warning, and a
  PC with Smart App Control on refuses it. `Read me first.html` explains this, and the
  report and key images are still readable.
- Macs, phones and tablets get the page, report and key images, not the full images.
- A file over 4 GB will not fit on a FAT32 stick. The dialog warns about this.
