//! Invitation links and profiles reach Ruthos as command-line arguments of a
//! new process. When a copy is already running, this one hands them over
//! through a small file in the config folder and exits. Whatever arrives is
//! parsed strictly by the receiver, so the file carries no authority.
//!
//! The `ruthos:` scheme and the `.ruthosprofile` file type are registered for
//! the current user: keys under HKCU on Windows, which need no administrator
//! rights and are updated when the executable moves. On macOS the bundle's
//! Info.plist declares them.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// A running copy touches its heartbeat this often at the longest, give or take.
const STALE: Duration = Duration::from_secs(6);

fn folder() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
    }?;
    let dir = base.join("Ruthos").join("handover");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn is_session_argument(arg: &str) -> bool {
    arg.starts_with("ruthos:") || arg.to_ascii_lowercase().ends_with(".ruthosprofile")
}

/// Say this copy is running.
pub fn beat() {
    if let Some(dir) = folder() {
        let _ = std::fs::write(dir.join("running"), b"");
    }
}

/// Give the arguments to a running copy; false when there is none.
pub fn hand_to_running(args: &[String]) -> bool {
    let Some(dir) = folder() else { return false };
    let fresh = std::fs::metadata(dir.join("running"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < STALE);
    if !fresh {
        return false;
    }
    let name = format!("{}.txt", uuid::Uuid::new_v4());
    std::fs::write(dir.join(name), args.join("\n")).is_ok()
}

/// What other copies have handed over.
pub fn take() -> Vec<String> {
    let Some(dir) = folder() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "txt") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.extend(text.lines().map(str::to_string));
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    out
}

/// Register the scheme and file type for this user, if not already pointing
/// at this executable.
#[cfg(windows)]
pub fn register_links() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|e| e.to_str().map(str::to_string))
    else {
        return;
    };
    let open = format!("\"{exe}\" \"%1\"");
    let reg = |args: &[&str]| {
        Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
    };
    let key = r"HKCU\Software\Classes\ruthos\shell\open\command";
    if let Some(o) = reg(&["query", key, "/ve"]) {
        if String::from_utf8_lossy(&o.stdout).contains(&open) {
            return;
        }
    }
    reg(&[
        "add",
        r"HKCU\Software\Classes\ruthos",
        "/ve",
        "/d",
        "URL:Ruthos invitation",
        "/f",
    ]);
    reg(&[
        "add",
        r"HKCU\Software\Classes\ruthos",
        "/v",
        "URL Protocol",
        "/d",
        "",
        "/f",
    ]);
    reg(&["add", key, "/ve", "/d", &open, "/f"]);
    reg(&[
        "add",
        r"HKCU\Software\Classes\.ruthosprofile",
        "/ve",
        "/d",
        "Ruthos.Profile",
        "/f",
    ]);
    reg(&[
        "add",
        r"HKCU\Software\Classes\Ruthos.Profile",
        "/ve",
        "/d",
        "Ruthos profile",
        "/f",
    ]);
    reg(&[
        "add",
        r"HKCU\Software\Classes\Ruthos.Profile\shell\open\command",
        "/ve",
        "/d",
        &open,
        "/f",
    ]);
}

#[cfg(not(windows))]
pub fn register_links() {}
