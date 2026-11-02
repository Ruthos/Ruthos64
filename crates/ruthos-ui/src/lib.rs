//! ruthos-ui: the windows.
//!
//! One module per Horos window so contributors who know Horos find their way:
//! `browser` is the Database window (BrowserController), `viewer` is the 2D
//! viewer (ViewerController). `app` wires them into eframe with the viewer
//! windows as native viewports.

pub mod annotations;
pub mod app;
pub mod browser;
pub mod loader;
pub mod roi;
pub mod shared;
pub mod theme;
pub mod viewer;
pub mod widgets;

pub use app::RuthosApp;
