//! Ruthos: a Horos-inspired DICOM viewer and patient archive for Windows, macOS and Linux.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    // First, as logging and settings touch the disk.
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let viewing = ruthos_ui::package_ui::start_viewer(&mut args);
    if !viewing && ruthos_ui::package_ui::stranded() {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Info)
            .set_title("Ruthos Viewer")
            .set_description(ruthos_ui::package_ui::STRANDED_TEXT)
            .show();
        return Ok(());
    }
    let log_file = init_logging(viewing);

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
    // Invitation links and profiles, which a running copy takes over.
    let startup_links: Vec<String> = args
        .iter()
        .filter_map(|a| a.to_str())
        .filter(|a| ruthos_ui::platform::handover::is_session_argument(a))
        .map(str::to_string)
        .collect();
    if !viewing {
        if !startup_links.is_empty() && ruthos_ui::platform::handover::hand_to_running(&startup_links) {
            return Ok(());
        }
        // A viewer may be on a stick: links must not end up pointing at it.
        std::thread::spawn(ruthos_ui::platform::handover::register_links);
    }
    let startup_paths: Vec<std::path::PathBuf> = args
        .into_iter()
        .filter(|a| {
            !a.to_str()
                .is_some_and(ruthos_ui::platform::handover::is_session_argument)
        })
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .collect();

    let result = eframe::run_native(
        "Ruthos",
        options,
        Box::new(
            move |cc| match ruthos_ui::RuthosApp::new(cc, startup_paths, startup_links) {
                Ok(app) => Ok(Box::new(app)),
                Err(e) => {
                    tracing::error!("startup failed: {e:#}");
                    Err(Box::new(std::io::Error::other(e.to_string())))
                }
            },
        ),
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
    ruthos_core::portable::leave();
    result
}

/// Log to stderr and to `ruthos.log` in the data folder, so a failed start
/// on a machine without a console still leaves a trace.
fn init_logging(viewing: bool) -> Option<std::path::PathBuf> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn,naga=warn,egui_wgpu=warn,eframe=warn,jpeg2k=warn")
    });
    // A viewer keeps no log: it might name a patient, and it leaves no trace.
    let path = (!viewing).then(|| ruthos_core::Settings::load().data_root().join("ruthos.log"));
    if let Some(root) = path.as_ref().and_then(|p| p.parent()) {
        let _ = std::fs::create_dir_all(root);
    }
    let file = path.as_ref().and_then(|p| std::fs::File::create(p).ok());
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
    path
}

/// Plain words for the errors a clinic PC is likely to produce.
fn startup_error_text(e: &eframe::Error) -> String {
    let text = e.to_string();
    let lower = text.to_lowercase();
    if lower.contains("adapter") || lower.contains("gpu") || lower.contains("device") {
        format!(
            "Ruthos could not find a graphics device it can draw with.\n\n\
             It runs on any DirectX 12, Metal, Vulkan or OpenGL adapter, including Windows' \
             built-in software renderer, so this usually means the graphics driver is missing \
             or broken. Updating the graphics driver normally fixes it.\n\n\
             Details: {text}"
        )
    } else {
        format!("Details: {text}")
    }
}

/// How the GPU is chosen.
///
/// The native backends (DirectX 12, Metal, Vulkan) and OpenGL are
/// enumerated and the adapters ranked: a discrete GPU first, then
/// integrated, then virtual, then a CPU renderer such as Windows' WARP or
/// Linux's llvmpipe, preferring the platform's own API within a class.
/// `RUTHOS_RENDERER=software` forces the CPU renderer where one exists and
/// `WGPU_BACKEND=dx12|metal|vulkan|gl` limits the backends, for
/// troubleshooting.
fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
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

/// The platform's own API first: DirectX 12 on Windows, Metal on macOS,
/// Vulkan on Linux; OpenGL is the fallback everywhere.
fn backend_rank(backend: wgpu::Backend) -> u8 {
    match backend {
        wgpu::Backend::Dx12 => {
            if cfg!(target_os = "windows") {
                0
            } else {
                3
            }
        }
        wgpu::Backend::Metal => {
            if cfg!(target_os = "macos") {
                0
            } else {
                3
            }
        }
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Gl => 2,
        _ => 4,
    }
}

fn select_adapter(adapters: &[wgpu::Adapter], _surface: Option<&wgpu::Surface<'_>>) -> Result<wgpu::Adapter, String> {
    for a in adapters {
        let info = a.get_info();
        tracing::info!(
            "graphics adapter: {} ({:?}, {:?}, driver {})",
            info.name,
            info.backend,
            info.device_type,
            info.driver
        );
    }
    let software_requested = std::env::var("RUTHOS_RENDERER")
        .map(|v| v.eq_ignore_ascii_case("software"))
        .unwrap_or(false);
    // WARP on Windows and llvmpipe on Linux are CPU adapters; every
    // supported Mac has a Metal GPU, so the request is noted and ignored.
    let want_software = software_requested && ruthos_ui::platform::software_renderer_available();
    if software_requested && !want_software {
        tracing::info!("RUTHOS_RENDERER=software is ignored on macOS; Metal is used");
    }
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
            if info.device_type == wgpu::DeviceType::Cpu {
                0
            } else {
                class + 1
            }
        } else {
            class
        };
        (class, backend_rank(info.backend))
    };
    let best = adapters
        .iter()
        .min_by_key(|a| rank(a))
        .cloned()
        .ok_or_else(|| "no graphics adapter found on DirectX 12, Metal, Vulkan or OpenGL".to_string())?;
    let info = best.get_info();
    tracing::info!(
        "using graphics adapter: {} ({:?}, {:?})",
        info.name,
        info.backend,
        info.device_type
    );
    Ok(best)
}
