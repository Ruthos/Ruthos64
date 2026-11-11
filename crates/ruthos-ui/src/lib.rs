//! ruthos-ui: the windows.
//!
//! One module per Horos window so contributors who know Horos find their way:
//! `browser` is the Database window (BrowserController), `viewer` is the 2D
//! viewer (ViewerController). `app` wires them into eframe with the viewer
//! windows as native viewports.

pub mod about;
pub mod amm_ui;
pub mod annotations;
pub mod anonymize;
pub mod app;
pub mod browser;
pub mod clut_menu;
pub mod colour_pick;
pub mod crash;
pub mod dial;
pub mod histogram;
pub mod horos_import;
pub mod icons;
pub mod loader;
pub mod loupe;
pub mod mcaliper;
pub mod measure_prefs;
pub mod mpr;
pub mod package_ui;
pub mod pacs;
pub mod platform;
pub mod prefs;
pub mod prefs_web;
pub mod query;
pub mod report;
pub mod roi;
pub mod routing;
pub mod segments;
#[cfg(feature = "ai")]
pub mod segments_ai;
pub mod session;
pub mod shared;
pub mod theme;
pub mod transfer_edit;
pub mod viewer;
pub mod widgets;

pub use app::RuthosApp;

/// The Ruthos icon for a window's title bar and the taskbar. The icon inside the
/// executable only reaches Explorer: a window that is not given this one shows
/// the windowing library's own.
pub fn app_icon() -> std::sync::Arc<egui::IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let image = image::load_from_memory(include_bytes!("../../../assets/icon/ruthos-256.png"))
            .expect("the icon built into Ruthos")
            .into_rgba8();
        let (width, height) = image.dimensions();
        std::sync::Arc::new(egui::IconData {
            rgba: image.into_raw(),
            width,
            height,
        })
    })
    .clone()
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn the_window_icon_is_a_whole_square_image() {
        let icon = super::app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(icon.rgba.chunks(4).any(|p| p[3] > 0), "something is drawn");
    }
}
