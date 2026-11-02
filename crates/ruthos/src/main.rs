//! Ruthos: a Horos-inspired DICOM viewer and patient archive for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    let log_file = init_logging();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ruthos")
            .with_app_id("org.ruthos.Ruthos")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([900.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_configuration(),
        ..Default::default()
    };

    // Files or folders on the command line are imported and opened. This is
    // what "Open with Ruthos" from Explorer hands us.
    let startup_paths: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .collect();

    let result = eframe::run_native(
        "Ruthos",
        options,
        Box::new(move |cc| match ruthos_ui::RuthosApp::new(cc, startup_paths) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => {
                tracing::error!("startup failed: {e:#}");
                Err(Box::new(std::io::Error::other(e.to_string())))
            }
        }),
    );

    if let Err(e) = &result {
        // The window never opened (no graphics adapter, no device, a database
        // that would not open). Say so in a dialog, since there is no console.
        tracing::error!("Ruthos could not start: {e}");
        let where_logged = log_file
            .as_ref()
            .map(|p| format!("\n\nThe log is at {}", p.display()))
            .unwrap_or_default();
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Ruthos could not start")
            .set_description(format!("{}{where_logged}", startup_error_text(e)))
            .show();
    }
    result
}

/// Log to stderr and to `ruthos.log` in the data folder, so a failed start
/// on a machine without a console still leaves a trace.
fn init_logging() -> Option<std::path::PathBuf> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn,naga=warn,egui_wgpu=warn,eframe=warn,jpeg2k=warn")
    });
    let root = ruthos_core::Settings::load().data_root();
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("ruthos.log");
    let file = std::fs::File::create(&path).ok();
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .init();
    Some(path)
}

/// Plain words for the errors a clinic PC is likely to produce.
fn startup_error_text(e: &eframe::Error) -> String {
    let text = e.to_string();
    let lower = text.to_lowercase();
    if lower.contains("adapter") || lower.contains("gpu") || lower.contains("device") {
        format!(
            "Ruthos could not find a graphics device it can draw with.\n\n\
             It runs on any DirectX 12, Vulkan or OpenGL adapter, including Windows' \
             built-in software renderer, so this usually means the graphics driver or the \
             DirectX runtime is missing or broken. Updating the graphics driver normally fixes it.\n\n\
             Details: {text}"
        )
    } else {
        format!("Details: {text}")
    }
}

/// How the GPU is chosen.
///
/// All three native backends are enumerated and the adapters ranked: a
/// discrete GPU first, then integrated, then virtual, then a CPU renderer
/// such as Windows' WARP, preferring DirectX 12 over Vulkan over OpenGL
/// within a class. `RUTHOS_RENDERER=software` forces the CPU renderer and
/// `WGPU_BACKEND=dx12|vulkan|gl` limits the backends, for troubleshooting.
fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL);
    setup.native_adapter_selector = Some(Arc::new(select_adapter));
    setup.device_descriptor = Arc::new(|adapter| {
        let supported = adapter.limits();
        let base = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };
        wgpu::DeviceDescriptor {
            label: Some("ruthos device"),
            required_limits: wgpu::Limits {
                // Take the adapter's real maximum: 16384 on most GPUs, less on
                // WARP and old integrated chips. Images beyond it are
                // downsampled for display.
                max_texture_dimension_2d: supported.max_texture_dimension_2d.max(base.max_texture_dimension_2d),
                ..base
            },
            ..Default::default()
        }
    });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

fn select_adapter(adapters: &[wgpu::Adapter], _surface: Option<&wgpu::Surface<'_>>) -> Result<wgpu::Adapter, String> {
    for a in adapters {
        let info = a.get_info();
        tracing::info!("graphics adapter: {} ({:?}, {:?}, driver {})", info.name, info.backend, info.device_type, info.driver);
    }
    let want_software = std::env::var("RUTHOS_RENDERER").map(|v| v.eq_ignore_ascii_case("software")).unwrap_or(false);
    let rank = |a: &wgpu::Adapter| -> (u8, u8) {
        let info = a.get_info();
        let class = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            wgpu::DeviceType::Other => 4,
        };
        let class = if want_software {
            if info.device_type == wgpu::DeviceType::Cpu { 0 } else { class + 1 }
        } else {
            class
        };
        let backend = match info.backend {
            wgpu::Backend::Dx12 => 0,
            wgpu::Backend::Vulkan => 1,
            wgpu::Backend::Gl => 2,
            _ => 3,
        };
        (class, backend)
    };
    let best = adapters
        .iter()
        .min_by_key(|a| rank(a))
        .cloned()
        .ok_or_else(|| "no graphics adapter found on DirectX 12, Vulkan or OpenGL".to_string())?;
    let info = best.get_info();
    tracing::info!("using graphics adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
    Ok(best)
}
