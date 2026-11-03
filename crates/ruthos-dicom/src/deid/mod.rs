//! De-identification of single datasets for teaching copies.
//!
//! Everything here operates on one DICOM dataset and knows nothing about
//! cases, the archive or files, so the rules can be tested on sample files
//! alone. The case register, export orchestration and the dialog live in
//! `ruthos-core` and `ruthos-ui`.
//!
//! The teaching copy is built by allowlist: a new dataset containing only
//! the attributes the viewer needs, with identifiers replaced, UIDs derived
//! and dates shifted. Everything else is left behind, so an attribute nobody
//! thought of is simply not copied.

pub mod allowlist;
pub mod dates;
pub mod mask;
pub mod uid;
pub mod verify;

pub use allowlist::{build_teaching_dataset, is_allowed, is_type_uid, DeidParams, DEID_METHOD_CODES};
pub use dates::{age_string, DateShift};
pub use mask::{apply_mask, regions_union, MaskPlan, MaskTemplate, MaskedPixels, Rect};
pub use uid::derive_uid;
pub use verify::{collect_context, verify_file, verify_text, Finding, VerifyContext};

/// The first sensitive token found in a label, for live validation in a dialog.
pub fn conflicts_with_tokens_text(label: &str, ctx: &VerifyContext) -> Option<String> {
    verify_text("label", label, ctx, false).into_iter().next().map(|f| f.matched)
}
