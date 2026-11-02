//! Ruthos: a Horos-inspired DICOM viewer and patient archive for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn,naga=warn,egui_wgpu=warn,eframe=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ruthos")
            .with_app_id("org.ruthos.Ruthos")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([900.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Ruthos",
        options,
        Box::new(|cc| match ruthos_ui::RuthosApp::new(cc) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => {
                tracing::error!("startup failed: {e:#}");
                Err(Box::new(std::io::Error::other(e.to_string())))
            }
        }),
    )
}
