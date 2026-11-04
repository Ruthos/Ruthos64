//! Site deployment: `ruthos.ini` and the administrator's policy file.
//!
//! `ruthos.ini` is how a developer or a training centre points Ruthos at a
//! server without that address being in the code or the repository. It sits
//! beside the executable (a lab image, or a zip a centre hands to students
//! for their own laptops), in the user's Ruthos config folder, or wherever
//! `RUTHOS_INI` says. It holds no credentials and can set nothing but the
//! keys below; `ruthos.ini.example` documents them.
//!
//! The policy file is the stronger form: it lives where only an administrator
//! can write, and what it sets is locked.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discovery::normalise;

/// What the site has decided, from the ini and the policy together.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Deployment {
    /// The server to use when the user has set none.
    pub server: Option<String>,
    /// When not empty, the only servers Ruthos may use.
    pub allowed_servers: Vec<String>,
    /// When not empty, the only issuers whose profiles may be imported.
    pub allowed_issuers: Vec<String>,
    /// The server and the allow lists are fixed by an administrator.
    pub locked: bool,
    /// The proxy for session traffic.
    pub proxy: Option<String>,
    /// Which file set this, for the Guided Review tab.
    pub source: Option<PathBuf>,
}

impl Deployment {
    /// True when `server` may be used here.
    pub fn permits(&self, server: &str) -> bool {
        self.allowed_servers.is_empty() || self.allowed_servers.iter().any(|s| s == server)
    }

    pub fn permits_issuer(&self, issuer: &str) -> bool {
        self.allowed_issuers.is_empty() || self.allowed_issuers.iter().any(|s| s == issuer)
    }

    /// Read what this machine has: the policy wins over the ini.
    pub fn load() -> Self {
        let mut d = Self::default();
        if let Some(path) = ini_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                d = from_ini(&text);
                d.source = Some(path);
            }
        }
        if let Some(path) = policy_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                match serde_json::from_str::<Policy>(&text) {
                    Ok(p) => d.lay_policy(p, path),
                    Err(e) => tracing::warn!("policy file ignored: {e}"),
                }
            }
        }
        d
    }

    fn lay_policy(&mut self, p: Policy, path: PathBuf) {
        if let Some(s) = p.server.as_deref().and_then(|s| normalise(s).ok()) {
            self.server = Some(s);
        }
        if !p.servers.is_empty() {
            self.allowed_servers = normalised(&p.servers);
        }
        if !p.issuers.is_empty() {
            self.allowed_issuers = normalised(&p.issuers);
        }
        self.locked = true;
        self.source = Some(path);
    }
}

fn normalised(list: &[String]) -> Vec<String> {
    list.iter().filter_map(|s| normalise(s).ok()).collect()
}

/// The administrator's policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Policy {
    server: Option<String>,
    servers: Vec<String>,
    issuers: Vec<String>,
}

fn policy_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map(|p| Path::new(&p).join("Ruthos").join("policy.json"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support/Ruthos/policy.json"))
    } else {
        Some(PathBuf::from("/etc/ruthos/policy.json"))
    }
}

/// The first `ruthos.ini` that exists: `RUTHOS_INI`, beside the executable,
/// then the user's config folder.
fn ini_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RUTHOS_INI") {
        return Some(PathBuf::from(p));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("ruthos.ini")));
    let config = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| Path::new(&p).join("Ruthos").join("ruthos.ini"))
    } else {
        std::env::var_os("HOME").map(|p| Path::new(&p).join(".config").join("ruthos").join("ruthos.ini"))
    };
    [beside, config].into_iter().flatten().find(|p| p.is_file())
}

/// The `[section] key` values of an ini file. `;` and `#` start comments,
/// and unknown keys are ignored.
pub fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_ascii_lowercase();
        } else if let Some((k, v)) = line.split_once('=') {
            if current == section && k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn list(text: &str, key: &str) -> Vec<String> {
    ini_value(text, "guided-review", key)
        .map(|v| {
            let parts: Vec<String> = v.split(',').map(|s| s.trim().to_string()).collect();
            normalised(&parts)
        })
        .unwrap_or_default()
}

/// The settings an ini may carry.
pub fn from_ini(text: &str) -> Deployment {
    let value = |key: &str| ini_value(text, "guided-review", key).filter(|v| !v.is_empty());
    Deployment {
        server: value("server").and_then(|s| normalise(&s).ok()),
        allowed_servers: list(text, "allowed_servers"),
        allowed_issuers: list(text, "allowed_issuers"),
        locked: false,
        proxy: value("proxy"),
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "; a training centre\n[guided-review]\nserver = https://relay.example.org/teach/\n\
        allowed_servers = https://relay.example.org/teach, https://b.example.org\nproxy = http://proxy.example.org:3128\n\
        password = nothing reads this\n[other]\nserver = https://wrong.example.org\n";

    #[test]
    fn the_ini_sets_only_its_own_keys() {
        let d = from_ini(INI);
        assert_eq!(d.server.as_deref(), Some("https://relay.example.org/teach"));
        assert_eq!(d.allowed_servers.len(), 2);
        assert_eq!(d.proxy.as_deref(), Some("http://proxy.example.org:3128"));
        assert!(!d.locked);
    }

    #[test]
    fn an_insecure_server_in_an_ini_is_dropped() {
        let d = from_ini("[guided-review]\nserver = http://relay.example.org\n");
        assert_eq!(d.server, None);
    }

    #[test]
    fn the_allow_list_decides() {
        let d = from_ini(INI);
        assert!(d.permits("https://b.example.org"));
        assert!(!d.permits("https://c.example.org"));
        assert!(Deployment::default().permits("https://c.example.org"));
    }

    #[test]
    fn a_policy_locks_and_wins() {
        let mut d = from_ini(INI);
        let p: Policy = serde_json::from_str(
            r#"{"server":"https://uni.example.org","servers":["https://uni.example.org"],"issuers":["https://uni.example.org"]}"#,
        )
        .unwrap();
        d.lay_policy(p, PathBuf::from("policy.json"));
        assert!(d.locked);
        assert_eq!(d.server.as_deref(), Some("https://uni.example.org"));
        assert!(!d.permits("https://relay.example.org/teach"));
        assert!(!d.permits_issuer("https://other.example.org"));
    }
}
