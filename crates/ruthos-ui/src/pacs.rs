//! What the PACS windows share: background tasks, the bridge from
//! settings to `ruthos_net`, presets, and showing a problem in plain words.

use std::sync::Arc;
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
    Node {
        host: n.host.trim().to_string(),
        port: n.port,
        ae_title: n.ae_title.trim().to_string(),
    }
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
