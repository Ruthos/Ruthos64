//! What the PACS windows share: background tasks, the bridge from
//! settings to `ruthos_net`, presets, and showing a problem in plain words.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ruthos_core::credentials::{self, Secret};
use ruthos_core::Database;
use ruthos_net::dicomweb::{
    access_sign_in, block_on, check_address, oidc_sign_in, parse_pin, test_connection, AccessToken, Basic, NoSignIn,
    Oidc, OidcSettings, OidcTokens, ServiceToken, SignIn, TestStep, Transfer, WebClient, WebNode, ACCESS_STORE,
};
use ruthos_net::{RetrieveOutcome, RetrieveProgress, SendOutcome, SeriesResult, StudyQuery, StudyResult};

use std::time::Instant;

use egui::{Color32, RichText, Ui};
use parking_lot::Mutex;
use ruthos_core::settings::PacsNode;
use ruthos_core::Settings;
use ruthos_net::{Me, NetError, Node, Problem, RetrieveMethod};

/// Work running on its own thread, polled by the interface each frame.
pub struct Task<T> {
    slot: Arc<Mutex<Option<T>>>,
    pub started: Instant,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &egui::Context, name: &str, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            let r = f();
            *slot2.lock() = Some(r);
            ctx.request_repaint();
        });
        Self {
            slot,
            started: Instant::now(),
        }
    }

    /// The result, once, when the work has finished.
    pub fn take(&self) -> Option<T> {
        self.slot.lock().take()
    }
}

pub fn node(n: &PacsNode) -> Node {
    if n.is_web() {
        // Problems name the server, so give them its host.
        let host = url::Url::parse(n.base_url.trim())
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_else(|| n.base_url.trim().to_string());
        return Node {
            host,
            port: 443,
            ae_title: String::new(),
        };
    }
    Node {
        host: n.host.trim().to_string(),
        port: n.port,
        ae_title: n.ae_title.trim().to_string(),
    }
}

fn sign_in_for(n: &PacsNode) -> Result<Arc<dyn SignIn>, NetError> {
    let session = || credentials::load(&n.id, Secret::Session).ok_or(NetError::SignInNeeded);
    let password = || credentials::load(&n.id, Secret::Password).ok_or(NetError::SignInNeeded);
    Ok(match n.sign_in.as_str() {
        "none" => Arc::new(NoSignIn),
        "basic" => Arc::new(Basic {
            user: n.basic_user.clone(),
            password: password()?,
        }),
        "service" => Arc::new(ServiceToken {
            client_id: n.service_client_id.clone(),
            secret: password()?,
        }),
        "access" => Arc::new(AccessToken { token: session()? }),
        "oidc" => {
            let tokens: OidcTokens = serde_json::from_str(&session()?).map_err(|_| NetError::SignInNeeded)?;
            let id = n.id.clone();
            let changed = Arc::new(move |t: &OidcTokens| {
                if let Ok(text) = serde_json::to_string(t) {
                    if let Err(e) = credentials::store(&id, Secret::Session, &text) {
                        tracing::warn!("a renewed sign-in could not be kept: {e}");
                    }
                }
            });
            Arc::new(Oidc::new(tokens, &n.oidc_client_id, changed)?)
        }
        _ => return Err(NetError::Insecure("choose how this node signs in".into())),
    })
}

fn root_or(specific: &str, base: &str, signs_in: bool) -> Result<String, NetError> {
    check_address(if specific.trim().is_empty() { base } else { specific }, signs_in)
}

/// A client for a DICOMweb node, with its secrets from the credential store.
pub fn web_client(n: &PacsNode) -> Result<WebClient, NetError> {
    let signs_in = n.sign_in != "none";
    let base = check_address(&n.base_url, signs_in)?;
    let pin = n.pin.trim();
    let pin = match pin {
        "" => None,
        text => Some(parse_pin(text).ok_or_else(|| NetError::Insecure("that certificate pin can't be read".into()))?),
    };
    WebClient::new(WebNode {
        name: n.name.clone(),
        qido: root_or(&n.qido_url, &base, signs_in)?,
        wado: root_or(&n.wado_url, &base, signs_in)?,
        stow: root_or(&n.stow_url, &base, signs_in)?,
        sign_in: sign_in_for(n)?,
        pin,
        client_certificate: credentials::load(&n.id, Secret::Certificate).map(String::into_bytes),
        proxy: ruthos_session::deployment::Deployment::load().proxy,
        transfer: match n.transfer.as_str() {
            "explicit" => Transfer::ExplicitLittleEndian,
            "jpeg2000" => Transfer::Jpeg2000Lossless,
            _ => Transfer::Original,
        },
        page_size: n.page_size.max(1),
        include_all: n.include_all,
        parallel: n.parallel.clamp(1, 8) as usize,
        most_parts: ruthos_net::dicomweb::multipart::MOST_PARTS,
    })
}

/// Search a node by whichever route it speaks. `narrow` is set when a DICOMweb
/// search had more results than can be shown.
pub fn search(
    n: &PacsNode,
    ae: &str,
    q: &StudyQuery,
    cancel: &AtomicBool,
    narrow: &AtomicBool,
) -> Result<Vec<StudyResult>, NetError> {
    if !n.is_web() {
        return ruthos_net::find_studies(&node(n), ae, q, cancel);
    }
    let found = web_client(n)?.find_studies(q, cancel)?;
    narrow.store(found.stopped_early, Ordering::Relaxed);
    Ok(found.studies)
}

pub fn series_of(n: &PacsNode, ae: &str, study: &str, cancel: &AtomicBool) -> Result<Vec<SeriesResult>, NetError> {
    if n.is_web() {
        web_client(n)?.find_series(study, cancel)
    } else {
        ruthos_net::find_series(&node(n), ae, study, cancel)
    }
}

/// Bring a study or series from a DICOMweb node, skipping what is already held.
pub fn web_retrieve(
    n: &PacsNode,
    db: &Database,
    study: &str,
    series: Option<&str>,
    out: &std::path::Path,
    report: &(dyn Fn(RetrieveProgress) + Sync),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    let have = |uid: &str| matches!(db.image_by_sop_uid(uid), Ok(Some(_)));
    let started = |uid: &str| db.series_in_archive(uid).unwrap_or(false);
    web_client(n)?.retrieve(study, series, out, &have, &started, report, cancel)
}

/// Send by whichever route a node speaks. Only a teaching copy may go to a
/// node marked as a teaching node, matching the rule for teaching sessions.
pub fn send(
    n: &PacsNode,
    ae: &str,
    files: &[PathBuf],
    teaching_copy: bool,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<SendOutcome, NetError> {
    if n.teaching_node && !teaching_copy {
        return Err(NetError::Insecure(format!(
            "only teaching copies can be sent to {}, and this study isn't one",
            n.name
        )));
    }
    if n.is_web() {
        web_client(n)?.send_files(files, progress, cancel)
    } else {
        ruthos_net::send_files(&node(n), ae, files, progress, cancel)
    }
}

/// Sign a node in through the browser and keep the result.
pub fn web_sign_in(n: &PacsNode) -> Result<(), NetError> {
    let open = |address: &str| crate::platform::open_url(address);
    let session = match n.sign_in.as_str() {
        "oidc" => {
            let settings = OidcSettings {
                issuer: n.oidc_issuer.trim().to_string(),
                client_id: n.oidc_client_id.trim().to_string(),
                scopes: n.oidc_scopes.trim().to_string(),
            };
            let tokens = block_on(oidc_sign_in(&settings, &open))?;
            serde_json::to_string(&tokens).map_err(|e| NetError::Other(e.to_string()))?
        }
        "access" => {
            let base = check_address(&n.base_url, true)?;
            block_on(access_sign_in(&base, n.access_audience.trim(), ACCESS_STORE, &open))?
        }
        _ => return Ok(()),
    };
    credentials::store(&n.id, Secret::Session, &session).map_err(NetError::Other)
}

/// A node's connection test, step by step, and whether `includefield=all` works.
pub fn web_test(n: &PacsNode) -> Result<(Vec<TestStep>, Option<bool>), NetError> {
    Ok(test_connection(&web_client(n)?))
}

pub fn method(n: &PacsNode) -> RetrieveMethod {
    match n.retrieve.as_str() {
        "get" => RetrieveMethod::Get,
        "move" => RetrieveMethod::Move,
        _ => RetrieveMethod::Auto,
    }
}

/// This computer as a PACS must be told about it.
pub fn me(settings: &Settings, listener_on: bool) -> Me {
    Me {
        ae_title: settings.listener_ae_title.clone(),
        port: settings.listener_port,
        address: ruthos_net::local_address().unwrap_or_else(|| "this computer's IP address".into()),
        listener_on,
    }
}

/// The text to hand a PACS administrator.
pub fn admin_text(me: &Me) -> String {
    format!(
        "Please add Ruthos as a DICOM node on the PACS:\n  AE title: {}\n  Address: {}\n  Port: {}\nIt needs to query (C-FIND), retrieve (C-GET or C-MOVE) and store (C-STORE).",
        me.ae_title, me.address, me.port
    )
}

/// Starting points for common systems: name, port, AE title, a note.
pub const PRESETS: &[(&str, u16, &str, &str)] = &[
    ("Orthanc", 4242, "ORTHANC", "Orthanc's out-of-the-box settings."),
    (
        "Horos or OsiriX",
        11112,
        "HOROS",
        "Horos's listener; OsiriX uses AE title OSIRIX.",
    ),
    ("dcm4chee", 11112, "DCM4CHEE", "dcm4chee-arc's default archive AE."),
    ("Conquest", 5678, "CONQUESTSRV1", "Conquest DICOM server's defaults."),
    (
        "Other PACS",
        104,
        "",
        "104 is the standard DICOM port; ask for the AE title.",
    ),
];

/// An AE title as typed: printable characters, at most 16, case kept
/// (AE titles must match exactly).
pub fn clean_ae(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(16)
        .collect()
}

/// A problem as a short red line, what to do, and the technical detail
/// folded away.
pub fn show_problem(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, p: &Problem) {
    ui.label(RichText::new(&p.summary).color(Color32::from_rgb(190, 40, 30)).strong());
    if !p.fix.is_empty() {
        ui.label(RichText::new(&p.fix).size(12.5));
    }
    egui::CollapsingHeader::new(
        RichText::new("Technical details")
            .size(11.5)
            .color(Color32::from_gray(120)),
    )
    .id_salt(id)
    .default_open(false)
    .show(ui, |ui| {
        ui.label(RichText::new(&p.technical).monospace().size(11.5));
    });
}

/// The explanation of an error for a node.
pub fn problem(e: &NetError, n: &PacsNode, me: &Me) -> Problem {
    ruthos_net::explain(e, &node(n), me)
}

/// Retrieval needs Ruthos's listener for C-MOVE; when it is off, Automatic
/// stays with C-GET and C-MOVE is refused with a reason.
pub fn effective_method(n: &PacsNode, listener_on: bool) -> Result<RetrieveMethod, Problem> {
    if n.is_web() {
        return Ok(RetrieveMethod::Web);
    }
    match (method(n), listener_on) {
        (RetrieveMethod::Move, false) => Err(Problem {
            summary: "C-MOVE needs Ruthos's listener, and it is off.".into(),
            fix: "Turn the listener on in Preferences > Listener, or set \"Retrieve with\" to Automatic for this PACS."
                .into(),
            technical: "listener disabled".into(),
        }),
        (RetrieveMethod::Auto, false) => Ok(RetrieveMethod::Get),
        (m, _) => Ok(m),
    }
}
