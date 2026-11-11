# Trying Ruthos on a Mac

Ruthos builds and passes its tests on macOS in CI, but nobody has used it on a
Mac yet. This is how to try it on a borrowed or spare Mac, what to check, and
how to leave the Mac as you found it. Allow 30 to 60 minutes.

## Get the test build

1. On the Mac, sign in to GitHub and open **Actions > macOS test build** in the
   Ruthos64 repository. Open the latest successful run and download
   **ruthos-macos-test** under Artifacts (kept for 14 days). A new one is made
   with **Run workflow** on that page.
2. The download is a zip holding another zip. Open both; you get `Ruthos.app`.
   It can stay in Downloads: there is no need to put it in Applications.
3. Open `Ruthos.app`. macOS refuses it the first time, because test builds are
   not signed by Apple. Open **System Settings > Privacy & Security**, scroll
   down to the message about Ruthos, and choose **Open Anyway**. This allows
   this one app and nothing else.

The build is for Apple Silicon (any M1 to M4 Mac).

## Before you start

- **Use anonymised or teaching studies, not real patient files.** On the PC,
  make teaching copies with the anonymiser and bring them on a USB stick.
- Leave the **listener** off unless you are testing it. When it is switched on,
  macOS asks whether Ruthos may accept incoming connections; allow it only for
  the test.

## What to check

Note anything that looks wrong, slow or different from Windows. A screenshot
(Shift-Cmd-4) is enough for each.

**Starting and the database window**
- [ ] It opens, and the database window looks right on the Mac's screen: text
      sharp, nothing too small or too large.
- [ ] The menu bar at the top of the screen holds Ruthos's menus.
- [ ] Import a folder from the USB stick (the import button, then the folder).
- [ ] Drag a folder or file from Finder onto the database window.
- [ ] Smart folders, albums, search, thumbnails and the preview all work.
- [ ] Preferences opens on Show All, and each pane opens. Manage shows, hides
      and orders the smart folders.

**Viewer**
- [ ] Open a study. Window and level with the mouse or trackpad drag.
- [ ] Zoom (pinch, and Ctrl-scroll), pan, and scrolling through a series with
      two fingers. Does scrolling go the expected way, and at a sensible speed?
- [ ] Play a loop. Is it smooth? Try a long echo loop too.
- [ ] Tile a series and a study.
- [ ] Colour tables, and exporting a PNG and an MP4.

**Measurements**
- [ ] Caliper, angle, areas and text on a 2D image.
- [ ] On an M-mode strip: the caliper keeps to depth or time, lets go past 45
      degrees, Ctrl frees it, and the crosshairs and dotted lines show.
- [ ] LV dimensions, and naming a measurement.
- [ ] Anatomical M-mode on a loop (Tools menu, or J).
- [ ] Open the report, and export it as a PDF.

**3D and MPR** (if you have a CT or MR)
- [ ] MPR opens and the planes move together.
- [ ] Volume rendering, the presets, and rotating it.

**Files and dialogs**
- [ ] Every open and save dialog is a normal Mac dialog and works.
- [ ] Copy a value and paste it into Notes.

**Network** (only if you have a test PACS or modality)
- [ ] Preferences > PACS: test a connection, query and retrieve.
- [ ] The listener receives a study.

**Ending**
- [ ] Quit with Cmd-Q, and open it again: the studies are still there.

## Reporting what you find

For each problem, send the screenshot and say what you did. Also send the log:
in Finder choose **Go > Go to Folder**, enter
`~/Library/Application Support/Ruthos/Ruthos Data` and copy `ruthos.log` (and
`crash.log`, if there is one). The log holds no images, but it can name the
folders you imported from.

## Leaving the Mac as you found it

1. Quit Ruthos.
2. Delete `Ruthos.app` and the downloaded zips.
3. In Finder choose **Go > Go to Folder**, enter
   `~/Library/Application Support/Ruthos` and delete that folder. It holds the
   test archive, settings and logs. macOS may also have kept a small window
   state file under `~/Library/Saved Application State`; it is harmless, and
   can be deleted too.
4. If you allowed incoming connections, remove Ruthos from **System Settings >
   Network > Firewall > Options**.
