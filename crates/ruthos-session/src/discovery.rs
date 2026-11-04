//! A server is known by one value, its address. Everything else comes from
//! `GET {server}/v1/info`. Ruthos ships with no address in its code or its
//! default settings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::proto::PROTOCOL;
use crate::SessionError;

/// The sign-in branch a server asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthKind {
    /// The sign-in page in the system browser, handing a token back on a
    /// loopback address.
    Token {
        login_url: String,
    },
    /// Stub: an operator running an OpenID Connect provider.
    Oidc,
    Unsupported(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthInfo {
    #[serde(rename = "type")]
    pub kind: String,
    pub login_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub name: String,
    pub protocol_versions: Vec<u32>,
    pub auth: AuthInfo,
    pub guests: bool,
    pub case_store: bool,
    pub privacy_url: Option<String>,
    pub max_members: Option<u32>,
    pub ws_path: String,
    /// Signing keys for profiles: key id to base64 Ed25519 public key.
    pub provisioning_keys: BTreeMap<String, String>,
}

impl ServerInfo {
    pub fn auth_kind(&self) -> AuthKind {
        match (self.auth.kind.as_str(), &self.auth.login_url) {
            ("oidc", _) => AuthKind::Oidc,
            (_, Some(url)) => AuthKind::Token { login_url: url.clone() },
            (other, None) => AuthKind::Unsupported(other.to_string()),
        }
    }

    pub fn supported(&self) -> Result<(), SessionError> {
        if self.protocol_versions.contains(&PROTOCOL) {
            return Ok(());
        }
        let offered: Vec<String> = self.protocol_versions.iter().map(u32::to_string).collect();
        Err(SessionError::Refused(format!(
            "This server offers protocol version {}; this Ruthos speaks version {PROTOCOL}",
            offered.join(", ")
        )))
    }

    /// The WebSocket address for a server: its host, joined to `ws_path`.
    pub fn ws_url(&self, server: &str) -> Result<String, SessionError> {
        if !self.ws_path.starts_with('/') {
            return Err(SessionError::Refused("The server gave no WebSocket path".to_string()));
        }
        let url = url::Url::parse(server).map_err(|_| bad_address())?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let host = url.host_str().ok_or_else(bad_address)?;
        let host = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        Ok(format!("{scheme}://{host}{}", self.ws_path))
    }
}

fn bad_address() -> SessionError {
    SessionError::Refused("That is not a server address".to_string())
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1")
}

/// Check and tidy a server address: https only, except on this machine; no
/// credentials, query or fragment; no trailing slash.
pub fn normalise(address: &str) -> Result<String, SessionError> {
    let trimmed = address.trim().trim_end_matches('/');
    let url = url::Url::parse(trimmed).map_err(|_| bad_address())?;
    let host = url.host_str().ok_or_else(bad_address)?;
    let secure = url.scheme() == "https" || (url.scheme() == "http" && is_local(host));
    if !secure {
        return Err(SessionError::Refused(
            "A server address must use https (http is allowed only for localhost)".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(bad_address());
    }
    Ok(trimmed.to_string())
}

/// The domain shown in large type wherever a server is named.
pub fn domain(server: &str) -> String {
    url::Url::parse(server)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| server.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(
            normalise(" https://relay.example.org/teach/ ").unwrap(),
            "https://relay.example.org/teach"
        );
        assert_eq!(normalise("http://localhost:9000").unwrap(), "http://localhost:9000");
        assert_eq!(normalise("http://127.0.0.1:9000/").unwrap(), "http://127.0.0.1:9000");
    }

    #[test]
    fn plain_http_elsewhere_and_odd_addresses_are_refused() {
        for bad in [
            "http://relay.example.org",
            "ftp://relay.example.org",
            "relay.example.org",
            "https://user:pw@relay.example.org",
            "https://relay.example.org/?x=1",
            "http://localhost.example.org",
        ] {
            assert!(normalise(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_server_without_version_1_is_refused_by_name() {
        let info = ServerInfo {
            protocol_versions: vec![2, 3],
            ..Default::default()
        };
        let e = info.supported().unwrap_err().to_string();
        assert!(e.contains("2, 3"), "{e}");
    }

    #[test]
    fn the_sign_in_branch_comes_from_the_server() {
        let mut info = ServerInfo::default();
        info.auth.kind = "oidc".into();
        assert_eq!(info.auth_kind(), AuthKind::Oidc);
        info.auth.kind = "anything".into();
        info.auth.login_url = Some("https://relay.example.org/login".into());
        assert!(matches!(info.auth_kind(), AuthKind::Token { .. }));
        info.auth.login_url = None;
        assert!(matches!(info.auth_kind(), AuthKind::Unsupported(_)));
    }

    #[test]
    fn the_websocket_address_joins_the_host_and_path() {
        let info = ServerInfo {
            ws_path: "/teach/v1/ws".into(),
            ..Default::default()
        };
        assert_eq!(
            info.ws_url("https://relay.example.org/teach").unwrap(),
            "wss://relay.example.org/teach/v1/ws"
        );
        assert_eq!(
            info.ws_url("http://127.0.0.1:81").unwrap(),
            "ws://127.0.0.1:81/teach/v1/ws"
        );
    }
}
