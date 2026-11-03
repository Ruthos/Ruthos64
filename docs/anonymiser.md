# Anonymiser

The anonymiser produces a teaching copy of a study in which every attribute and every pixel that identifies the owner, the practice or the referring vet has been removed, under a teaching name and reference the user chooses. The source study is never modified.

## How it works

- **Allowlist, not deletion.** `crates/ruthos-dicom/src/deid/allowlist.rs` lists every attribute a teaching copy may contain. Anything else, including all private attributes, overlays and curves, is not copied. A test fails when a display path reads an attribute the allowlist does not copy.
- **UIDs** are derived with HMAC-SHA256 keyed by a per-case secret (`deid/uid.rs`), so a case reproduces the same UIDs on re-export and two cases from one study cannot be linked.
- **Dates** are shifted by one random offset per case, 365 to 3650 days, or whole years with "preserve season" (`deid/dates.rs`). Times are kept. Age is written explicitly.
- **Pixels** (`deid/mask.rs`): ultrasound keeps only its regions, with extra rectangles blanked for machines whose region includes the header band; other series use a device template or a human review; CT, MR, CR and DX with Burned In Annotation NO pass through byte for byte.
- **Verification** (`deid/verify.rs`) reads every output file back from disk and refuses the copy if an attribute is off the allowlist, a sensitive token appears in any text at any depth, or an original UID or date survives.
- **The register** (`crates/ruthos-core/src/teaching.rs`, tables `teaching_case`, `teaching_source`, `teaching_export`) is the only link from a reference to its sources and never leaves the archive. Deleting a case severs it and removes the case's lines from `anonymiser.log`.
- **Measurements, calibrations and key images** travel in `ruthos-case.json` beside the files and are applied when the folder is imported.

## Known limits of the first release

No OCR of burned-in text: review is the control for series without regions or a template. Report text is checked only against known tokens. No sending over DICOM yet.
