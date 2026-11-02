---
name: A file does not import or display correctly
about: A modality's DICOM files are rejected, blank, wrongly coloured or wrongly measured
title: '[import] '
labels: codec, help wanted
assignees: ''
---

**Modality and vendor** (for example "Fujifilm CR", "GE Vivid q", "Esaote MyLab")

**What happened**

- [ ] Not imported (count as non-DICOM or error)
- [ ] Imported but shows "Could not decode image"
- [ ] Shows, but wrong colours or windowing
- [ ] Measurements wrong or "not calibrated"
- [ ] Other:

**Transfer syntax, if you know it** (visible in the viewer's bottom-left annotation, for example "JPEG 2000")

**Logs**

Attach `ruthos.log` from the data folder.

**Sample file**

If you can share one, anonymise it first and attach a single image. If you cannot share, the header dumped by `dcmdump` or pydicom without pixel data is the next best thing.
