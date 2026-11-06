# Beta gate

Ruthos 0.2.0-beta.1 is tagged only when everything here is done. An alpha may go
out unsigned; a beta may not, and `release.yml` stops any tag without `-alpha` if
signing is not set up.

## Needed before the tag

- [ ] **Windows signing.** SignPath Foundation approved, and `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG` and `SIGNPATH_API_TOKEN` set. See [release-signing.md](release-signing.md).
- [ ] **Cloudflare Access.** Sign in through the browser against a real Access application, then search, retrieve and send, and do the same with a service token. Update the Cloudflare lines in `RELEASE_NOTES.md` and [dicomweb.md](dicomweb.md) only after this passes.
- [ ] **The flagged slice** (refinement test 7). In the panel, click a flagged slice and confirm it jumps there.
- [ ] **Sharing, by hand.** Press Share for a folder and a zip, with a report and key images, then open the package on a PC that has never had developer tools, from a real stick, and note what SmartScreen says about the signed viewer.
- [ ] **The release workflow.** Checkout, artefact download and the draft release moved to new major versions and have not yet run on a tag. Check the draft as in [release-signing.md](release-signing.md).
- [ ] **After publishing.** Run `stamp_release.py`, then change "alpha" by hand in the README and the site.

## Decisions to make

- **Decoder sandbox.** The DICOMweb specification expects one, and Ruthos has the decode guard instead.

## SEG tests

| # | Test | Evidence |
|---|---|---|
| 1 | Three segments pass `dciodvfy` with no errors | CI, Interoperability job |
| 2 | Opens in 3D Slicer and OHIF with matching labels, colours and voxels | Deferred. Ruthos makes no claim that other viewers open its SEG, and the maintainer could not find SEG support in their version of Weasis |
| 3 | A highdicom SEG loads with identical voxels | CI, Interoperability job |
| 4 | Save, close, reopen keeps voxels, label, colour and algorithm | `a_seg_comes_back_as_it_went_in`, `saved_segments_come_back_from_the_archive` |
| 5 | A teaching copy keeps segments aligned, with no original UIDs or names | `a_teaching_copy_keeps_its_segments_aligned_and_clean` |
| 6 | A SEG on other slice positions is refused with the reason | `a_seg_that_does_not_line_up_is_refused` |
| 7 | A small segment writes only its own frames | `a_small_segment_writes_only_its_own_frames` |
| 8 | Through Orthanc by C-STORE and back with identical pixel data | `a_seg_comes_back_from_orthanc_byte_for_byte`, CI |

## Refinement tests

| # | Test | Evidence |
|---|---|---|
| 1 | Plates of 1, 2 and 4 mm within 10% of true thickness | `plates_come_to_their_true_thickness` in CI. `the_real_model_on_the_phantom` by hand: its limit is 11%, and the worst case was +10% |
| 2 | Surface Dice at 1 mm improves for every plate | `the_surface_comes_closer_to_the_truth` in CI. Accepted by the maintainer: with the real SAM 2.1 Tiny it holds at or near 1.000, with two small falls (1.000 to 0.985 at 0.5 mm, 0.994 to 0.988 at 1 mm), both better at 0.5 mm tolerance. Refine edges stays off by default |
| 3 | A block across a 2-voxel gap is not merged | `a_block_across_a_two_voxel_gap_stays_out` in CI. By hand on the real model, at most 5 of about 2,500 block voxels joined |
| 4 | Nothing joins from beyond the band | `nothing_joins_from_beyond_the_band` |
| 5 | 300 slices in under 10 seconds on a CPU | `three_hundred_slices_take_under_ten_seconds` |
| 6 | With refinement off, the result is the model's own mask | `without_refinement_the_mask_is_the_models_own` |
| 7 | A flagged slice shows in the panel and is jumped to on click | `a_slice_unlike_its_neighbours_is_flagged` covers the flag. The click is by hand, above |

## Tests that pass without running

These return at once without their environment variables, so "ok" in a normal
test run proves nothing:

- `the_real_model_on_the_phantom` and `a_fracture_line_on_the_real_model` need `RUTHOS_SAM_ROOT` and run by hand on Windows, with `--features ai`.
- `interoperates_with_other_tools` needs `RUTHOS_SEG_INTEROP`; the Interoperability job sets it.
- Everything in `crates/ruthos-net/tests/orthanc.rs` needs its Orthanc variables; the same job sets them.
