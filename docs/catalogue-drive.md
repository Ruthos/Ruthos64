# Catalogue a drive

**Catalogue Drive…**, in Preferences > Import, lists every DICOM study
on a drive or folder without copying its images. Ruthos records where each file
is, so an archive of many terabytes stays on its disk while its studies can be
searched, sorted and put in albums like any others.

## Scanning

Choose the drive or a folder on it. Ruthos reads each file's header, and the scan
can be stopped at any time with **Stop**; what it has found so far stays. Nothing
is ever written to the drive.

Folders with no patient images, such as `$RECYCLE.BIN`, `System Volume Information`,
`Windows` and `Program Files`, are skipped, as are hidden folders and Ruthos's own
data folder. Automatic routing rules do not run on a catalogue.

Right-click a drive and choose **Scan again** to add what is new. Images already in
the database are not added twice, so a study that was copied in stays where it is.

## When the drive is not connected

The drive's row says **(not connected)**. Its studies can still be searched, and
opening one says which drive to connect. Thumbnails appear only for series already
viewed while the drive was connected.

A drive that comes back under a different letter is recognised by a few of its
images, at the same place on the drive and the same size, and the catalogue follows
it. Ruthos checks every 15 seconds, so there is nothing to do.

## Forgetting a drive

Right-click it and choose **Forget this drive**. Its studies leave the database,
with any reports and measurements made on them. Nothing on the drive changes. A
study that also holds images from elsewhere stays.

## Good to know

- Measurements, reports and key images are kept in Ruthos, not on the drive, so they
  are there whether it is connected or not.
- Deleting a catalogued study from Ruthos never deletes its files.
- If you move folders around on the drive, its studies stop opening. Forget the
  drive and catalogue it again.
