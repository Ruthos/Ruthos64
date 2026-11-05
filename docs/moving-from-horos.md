# Moving from Horos

**Import Horos Database…**, near the foot of the database sidebar (File > Import Horos Database on macOS), brings a Horos (or OsiriX) library into Ruthos with its albums, comments, status and key images.

## What to copy from the Mac

A Horos library is the folder called **Horos Data**. By default it is in your Documents folder on the Mac. Copy the whole folder to the Windows computer, for example on an external drive. Quit Horos first, so the database is not half-written.

The folder holds `Database.sql`, the database, and `DATABASE.noindex`, where the images are.

## Importing

1. Choose **Import Horos Database…** and pick the Horos Data folder, or the folder that contains it.
2. Ruthos reads the database and shows what it found: studies, images, albums and smart albums, comments and key images. Images Horos kept outside its library, such as on a CD or another disk, are listed as not found.
3. Choose how to bring the images over:
   - **Copy them into Ruthos.** This is the default. The Horos library is left exactly as it was.
   - **Use them where they are.** This saves space, but Ruthos then needs the folder to stay where it is.
4. Press Import. The import can be cancelled; what was imported so far stays.

Horos's own files are never changed. The database is copied to Ruthos's temporary folder and read from there.

## What comes over

| From Horos | In Ruthos |
|---|---|
| Studies, series and images | Imported like any other import, so duplicates are skipped |
| Study comments (all four fields) | The study comment, unless Ruthos already has one |
| Study status (unread, reviewed, dictated, validated) | The study status, unless Ruthos already has one |
| Date added and date opened | Kept for studies the import creates, so Just Added does not fill with the whole library |
| Key images and image comments | Key images and image comments |
| Albums | Albums; an album with the same name gets the studies added |
| Smart albums | Smart albums, when the rule can be expressed in Ruthos |

Smart album rules that combine modality, a text field, a comment and dates with AND are converted. Rules with OR, NOT or other fields, such as the number of images, are listed at the end with their rule so they can be made again by hand. Horos's built-in albums (Just Added, Just Opened, Cases with comments, Today) are not copied because Ruthos has its own.

## What does not come over

- **Horos reports** (Word, Pages or RTF files in the REPORTS folder). The summary says how many studies have one; the files stay in the Horos folder.
- **Horos measurements (ROIs).** Horos saves them as its own structured report objects, which Ruthos does not read yet. They are counted and skipped.
- **Images outside the library.** If Horos imported files in place from a disk that is not here, those images are skipped unless Ruthos already has them.

## Keeping a large archive on its own disk

If a Horos library is too large to copy, choose **Use them where they are** above.
For a drive of DICOM folders that were never in Horos, see
[Catalogue a drive](catalogue-drive.md).

## Running it again

The import can be run again at any time, for example after more work in Horos. Images already in Ruthos are not copied twice, comments and status set in Ruthos are kept, and albums are merged by name.
