//! ruthos-render: GPU presentation of DICOM frames.
//!
//! The viewer window hands this crate a decoded [`ruthos_dicom::Frame`]; it
//! uploads the frame once as a texture and draws it through a window/level
//! and colour-lookup shader inside an egui paint callback. All zoom, pan,
//! rotation and flip math lives in [`view::ViewTransform`] so the UI can map
//! mouse positions and ROI points between screen and image space with the
//! exact same numbers the shader uses.

pub mod gpu;
pub mod surface;
pub mod view;
pub mod volume3d;

pub use gpu::{GpuImage, ImageCallback, ImageRenderer, Interpolation};
pub use view::{ViewState, ViewTransform};
