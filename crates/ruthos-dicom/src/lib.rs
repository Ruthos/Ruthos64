//! ruthos-dicom: a thin, opinionated layer over dicom-rs.
//!
//! This crate owns everything that turns a DICOM file into something the rest
//! of Ruthos can use: header metadata for the database, decoded pixel frames
//! for the viewer, and the Horos colour lookup tables and window presets.
//!
//! The rule here is "features, not workarounds": decoding relies on the strict,
//! standards-based behaviour of dicom-rs. Anything vendor specific belongs in
//! [`quirks`] behind one trait and never leaks into feature code.

pub mod clut;
pub mod jpeg;
pub mod meta;
pub mod open;
pub mod pixels;
pub mod presets;
pub mod quirks;
pub mod sc;

pub use clut::{Clut, ClutLibrary};
pub use meta::{read_meta, InstanceMeta, PatientName};
pub use pixels::{decode_frame, Frame, FramePixels};
pub use presets::{WindowPreset, WindowPresets};

/// Errors produced while reading or decoding DICOM data.
#[derive(Debug, thiserror::Error)]
pub enum DicomError {
    #[error("not a DICOM file: {0}")]
    NotDicom(String),
    #[error("failed to read DICOM file: {0}")]
    Read(String),
    #[error("missing required attribute {0}")]
    Missing(&'static str),
    #[error("pixel data could not be decoded: {0}")]
    Pixels(String),
    #[error("unsupported image: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DicomError>;
