//! The About Ruthos window: the version, the build, the licence, the
//! acknowledgements and the way to the third-party licences.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::RichText;

use crate::shared::Shared;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit this was built from, set by build.rs.
const COMMIT: &str = env!("RUTHOS_COMMIT");

const IJG: &str = "This software is based in part on the work of the Independent JPEG Group.";
const ACKNOWLEDGEMENTS: &str = "Ruthos is built on DICOM-rs by Eduardo Pinho and contributors, including Paul Knopf \
    (upper-layer networking) and Peter Evers (pixel data), which provides its DICOM parsing, pixel decoding and \
    networking. The interface is built with egui by Emil Ernerfeldt and contributors, and rendering uses wgpu. Image \
    decoding uses libjpeg-turbo, OpenJPEG and CharLS. Ruthos's workflow, icons, colour tables and presets follow \
    Horos, its interface uses the Inter typeface and Phosphor icons, and its test files come from pydicom.";

/// What a bug report needs.
fn versions(shared: &Shared) -> String {
    let encoder = if ruthos_core::movie::encoder_installed(&shared.paths.root) {
        "installed"
    } else {
        "not installed"
    };
    format!(
        "Ruthos {VERSION} ({COMMIT})\nlibjpeg-turbo 3.1.0\nOpenJPEG 2.5.4 with fixes\nCharLS 2.4.5\nOpenH264 2.6.0 from Cisco: {encoder}\n"
    )
}

/// The notices beside the executable, or in the application bundle.
fn notices() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let folder = exe.parent()?;
    [
        folder.join("THIRD_PARTY_LICENSES.html"),
        folder.join("../Resources/THIRD_PARTY_LICENSES.html"),
        folder.join("../lib/ruthos/THIRD_PARTY_LICENSES.html"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Developer option: `RUTHOS_ABOUT_SCREENSHOT=<file.png>` opens the window and
/// saves it as drawn two seconds in.
fn dev_screenshot(ctx: &egui::Context) {
    use std::sync::atomic::AtomicBool;
    static TAKEN: AtomicBool = AtomicBool::new(false);
    let Some(path) = std::env::var_os("RUTHOS_ABOUT_SCREENSHOT") else {
        return;
    };
    if ctx.input(|i| i.time) < 2.0 {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    } else if !TAKEN.swap(true, Ordering::Relaxed) {
        ctx.request_screenshot(move |img| {
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(png) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                let _ = png.save(&path);
            }
        });
        ctx.request_repaint();
    }
}

/// The About window, while it is open.
pub fn window(ctx: &egui::Context, shared: &Shared, open: &mut bool) {
    if shared.open_about.swap(false, Ordering::Relaxed) {
        *open = true;
    }
    if !*open {
        return;
    }
    dev_screenshot(ctx);
    egui::Window::new("About Ruthos")
        .open(open)
        .collapsible(false)
        .default_width(520.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(RichText::new("Ruthos").size(24.0).strong());
                ui.label(format!("Version {VERSION}, build {COMMIT}"));
                ui.add_space(8.0);
                ui.label("Licensed under the GNU Lesser General Public License v3.0.");
                ui.label(RichText::new("Not a medical device. Not for diagnostic use.").strong());
                ui.add_space(8.0);
                ui.label(RichText::new("Acknowledgements").strong());
                ui.label(ACKNOWLEDGEMENTS);
                ui.add_space(4.0);
                ui.label(IJG);
                ui.add_space(4.0);
                ui.label(
                    "OpenH264 Video Codec provided by Cisco Systems, Inc. The encoder is fetched from Cisco, with your consent, the first time you export a movie.",
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let found = notices();
                    let button = egui::Button::new("Third-party licences");
                    if ui
                        .add_enabled(found.is_some(), button)
                        .on_hover_text("Every component's copyright and licence, in your browser")
                        .on_disabled_hover_text("Packaged builds include this file; a build from source does not")
                        .clicked()
                    {
                        if let Some(path) = &found {
                            let _ = open::that_detached(path);
                        }
                    }
                    if ui.button("Copy versions").on_hover_text("For a bug report").clicked() {
                        ui.ctx().copy_text(versions(shared));
                    }
                });
                ui.add_space(8.0);
                ui.collapsing("Cisco's licence for the OpenH264 binary", |ui| {
                    ui.label(RichText::new(ruthos_core::movie::CISCO_LICENCE).small());
                });
            });
        });
}
