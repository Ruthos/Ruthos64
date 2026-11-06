//! The DICOMweb part of a PACS node in Preferences.
//!
//! Secrets go straight to the credential store and never into the draft of the
//! settings, so nothing typed here can end up in `settings.json`.

use egui::{Color32, RichText, Ui};

use ruthos_core::credentials::{self, Secret};
use ruthos_core::settings::PacsNode;
use ruthos_net::dicomweb::{check_address, parse_pin};

/// What the person asked for.
pub enum Action {
    Nothing,
    Test,
    SignIn,
}

const METHODS: [(&str, &str, &str); 5] = [
    (
        "oidc",
        "OpenID Connect",
        "For a hospital or university system. You sign in through your browser, and Ruthos never sees your password.",
    ),
    (
        "access",
        "Cloudflare Access",
        "For a practice gateway behind Cloudflare. You sign in through your browser.",
    ),
    (
        "service",
        "Access service token",
        "For unattended systems. The Access application needs a Service Auth policy for this token.",
    ),
    (
        "basic",
        "Basic (testing only)",
        "A user name and password over HTTPS, with no expiry and no second factor. Suitable for testing only.",
    ),
    (
        "none",
        "None (this computer only)",
        "For a test server on this computer.",
    ),
];

fn grey(text: &str) -> RichText {
    RichText::new(text).size(11.5).color(Color32::from_gray(110))
}

fn line(ui: &mut Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(320.0).hint_text(hint));
    ui.end_row();
}

/// A secret's box, stored when Keep is pressed.
fn secret_box(ui: &mut Ui, n: &PacsNode, label: &str, typed: &mut String) {
    let held = credentials::load(&n.id, Secret::Password).is_some();
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::TextEdit::singleline(typed)
                .password(true)
                .desired_width(220.0)
                .hint_text(if held { "Kept securely" } else { "" }),
        );
        if ui.add_enabled(!typed.is_empty(), egui::Button::new("Keep")).clicked() {
            match credentials::store(&n.id, Secret::Password, typed) {
                Ok(()) => typed.clear(),
                Err(e) => tracing::warn!("a secret could not be kept: {e}"),
            }
        }
    });
}

fn method_details(ui: &mut Ui, n: &mut PacsNode, typed: &mut String) {
    match n.sign_in.as_str() {
        "oidc" => {
            egui::Grid::new("web-oidc")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    line(
                        ui,
                        "Issuer",
                        &mut n.oidc_issuer,
                        "https://login.example.org/realms/imaging",
                    );
                    line(ui, "Client ID", &mut n.oidc_client_id, "");
                    line(ui, "Scopes", &mut n.oidc_scopes, "openid profile");
                });
        }
        "access" => {
            egui::Grid::new("web-access")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    line(
                        ui,
                        "Audience tag",
                        &mut n.access_audience,
                        "From the Access application",
                    );
                });
        }
        "service" => {
            egui::Grid::new("web-service")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    line(ui, "Client ID", &mut n.service_client_id, "");
                });
            secret_box(ui, n, "Client secret", typed);
        }
        "basic" => {
            egui::Grid::new("web-basic")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    line(ui, "User name", &mut n.basic_user, "");
                });
            secret_box(ui, n, "Password", typed);
        }
        _ => {}
    }
}

fn advanced(ui: &mut Ui, n: &mut PacsNode) {
    egui::CollapsingHeader::new("More settings")
        .id_salt("web-more")
        .show(ui, |ui| {
            egui::Grid::new("web-more-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    line(
                        ui,
                        "QIDO root",
                        &mut n.qido_url,
                        "Only if the server splits its services",
                    );
                    line(ui, "WADO root", &mut n.wado_url, "");
                    line(ui, "STOW root", &mut n.stow_url, "");
                    line(
                        ui,
                        "Certificate pin",
                        &mut n.pin,
                        "SHA-256 of a self-signed gateway's public key",
                    );
                    ui.label("Ask for images as");
                    egui::ComboBox::from_id_salt("web-transfer")
                        .selected_text(match n.transfer.as_str() {
                            "explicit" => "Explicit VR Little Endian",
                            "jpeg2000" => "JPEG 2000 lossless",
                            _ => "As stored",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut n.transfer, "*".to_string(), "As stored");
                            ui.selectable_value(&mut n.transfer, "explicit".to_string(), "Explicit VR Little Endian");
                            ui.selectable_value(&mut n.transfer, "jpeg2000".to_string(), "JPEG 2000 lossless");
                        });
                    ui.end_row();
                    ui.label("Results per page");
                    ui.add(egui::DragValue::new(&mut n.page_size).range(1..=1000));
                    ui.end_row();
                    ui.label("Series at once");
                    ui.add(egui::DragValue::new(&mut n.parallel).range(1..=8));
                    ui.end_row();
                });
            ui.checkbox(
                &mut n.teaching_node,
                "Teaching node: refuse anything but teaching copies",
            );
            ui.horizontal(|ui| {
                let held = credentials::load(&n.id, Secret::Certificate).is_some();
                if ui.button("Client certificate\u{2026}").clicked() {
                    pick_certificate(n);
                }
                if held && ui.button("Remove it").clicked() {
                    credentials::delete(&n.id, Secret::Certificate);
                }
                ui.label(grey(if held {
                    "One is kept securely."
                } else {
                    "For mutual TLS. A PEM file with its key."
                }));
            });
        });
}

fn pick_certificate(n: &PacsNode) {
    let picked = rfd::FileDialog::new()
        .set_title("Choose a client certificate")
        .add_filter("PEM", &["pem", "crt", "key"])
        .pick_file();
    let Some(path) = picked else { return };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            if let Err(e) = credentials::store(&n.id, Secret::Certificate, &text) {
                tracing::warn!("a client certificate could not be kept: {e}");
            }
        }
        Err(e) => tracing::warn!("a client certificate could not be read: {e}"),
    }
}

/// Draw a DICOMweb node's fields.
pub fn fields(ui: &mut Ui, n: &mut PacsNode, typed: &mut String) -> Action {
    let mut action = Action::Nothing;
    egui::Grid::new("web-edit")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            line(ui, "Name", &mut n.name, "Anything you like");
            line(
                ui,
                "Base address",
                &mut n.base_url,
                "https://gateway.example.org/dicom-web",
            );
            ui.label("Sign-in");
            let chosen = METHODS.iter().find(|(key, _, _)| *key == n.sign_in);
            egui::ComboBox::from_id_salt("web-sign-in")
                .selected_text(chosen.map_or("Choose\u{2026}", |(_, label, _)| *label))
                .width(240.0)
                .show_ui(ui, |ui| {
                    for (key, label, explain) in METHODS {
                        ui.selectable_value(&mut n.sign_in, key.to_string(), label)
                            .on_hover_text(explain);
                    }
                });
            ui.end_row();
        });
    if let Some((_, _, explain)) = METHODS.iter().find(|(key, _, _)| *key == n.sign_in) {
        ui.label(grey(explain));
    }
    ui.add_space(4.0);
    method_details(ui, n, typed);
    ui.add_space(4.0);
    advanced(ui, n);

    let signs_in = n.sign_in != "none";
    let refused = if n.base_url.trim().is_empty() {
        None
    } else {
        check_address(&n.base_url, signs_in).err()
    };
    if let Some(problem) = &refused {
        ui.label(RichText::new(format!("{problem}.")).color(Color32::from_rgb(190, 40, 30)));
    }
    let bad_pin = !n.pin.trim().is_empty() && parse_pin(&n.pin).is_none();
    if bad_pin {
        ui.label(RichText::new("That certificate pin can't be read.").color(Color32::from_rgb(190, 40, 30)));
    }
    ui.add_space(8.0);
    let ready = !n.base_url.trim().is_empty() && !n.sign_in.is_empty() && refused.is_none() && !bad_pin;
    ui.horizontal(|ui| {
        if matches!(n.sign_in.as_str(), "oidc" | "access")
            && ui.add_enabled(ready, egui::Button::new("Sign in")).clicked()
        {
            action = Action::SignIn;
        }
        if ui.add_enabled(ready, egui::Button::new("Test connection")).clicked() {
            action = Action::Test;
        }
        if !ready {
            ui.label(grey("Fill in the address and choose a sign-in to test."));
        }
    });
    action
}
