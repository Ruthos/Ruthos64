//! ruthos-ui: the windows.
//!
//! One module per Horos window so contributors who know Horos find their way:
//! `browser` is the Database window (BrowserController), `viewer` is the 2D
//! viewer (ViewerController). `app` wires them into eframe with the viewer
//! windows as native viewports.

pub mod annotations;
pub mod anonymize;
pub mod app;
pub mod browser;
pub mod crash;
pub mod dial;
pub mod histogram;
pub mod horos_import;
pub mod icons;
pub mod loader;
pub mod loupe;
pub mod measure_prefs;
pub mod mpr;
pub mod pacs;
pub mod platform;
pub mod prefs;
pub mod query;
pub mod report;
pub mod roi;
pub mod routing;
pub mod session;
pub mod shared;
pub mod theme;
pub mod viewer;
pub mod widgets;

pub use app::RuthosApp;
