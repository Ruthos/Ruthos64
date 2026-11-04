//! Sign-in. The server's sign-in page runs in the system browser, where the
//! address bar shows the real domain, and hands a token back on a loopback
//! address, so Ruthos never sees a password.
//!
//! The claims decoded here are for display and for deciding which controls
//! to show. Every real decision is the relay's: it verifies the signature,
//! and this client cannot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::SessionError;

pub const KEYCHAIN_SERVICE: &str = "org.ruthos.Ruthos";
/// How long the browser has to come back.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// Who a token says it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub email: String,
    pub roles: Vec<String>,
    pub expires: DateTime<Utc>,
}

impl Account {
    pub fn is_host(&self) -> bool {
        self.roles.iter().any(|r| r == "host")
    }

    /// Whole minutes left, or zero.
    pub fn minutes_left(&self, now: DateTime<Utc>) -> i64 {
        (self.expires - now).num_minutes().max(0)
    }
}

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    roles: Vec<String>,
    exp: i64,
}

/// Decode a token's first dot-separated part, unverified.
pub fn decode(token: &str) -> Result<Account, SessionError> {
    let bad = || SessionError::Refused("That does not look like a sign-in token".to_string());
    let first = token.trim().split('.').next().ok_or_else(bad)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(first.trim_end_matches('='))
        .or_else(|_| URL_SAFE.decode(first))
        .map_err(|_| bad())?;
    let c: Claims = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    Ok(Account {
        name: c.name,
        email: c.email,
        roles: c.roles,
        expires: Utc.timestamp_opt(c.exp, 0).single().ok_or_else(bad)?,
    })
}

/// Where a token is kept. The operating system's credential store in use;
/// memory in tests and for guests.
pub trait TokenStore: Send + Sync {
    fn load(&self, server: &str) -> Option<String>;
    fn save(&self, server: &str, token: &str) -> Result<(), SessionError>;
    fn delete(&self, server: &str);
}

/// Windows Credential Manager, the macOS Keychain, the Secret Service.
pub struct Keychain;

impl TokenStore for Keychain {
    fn load(&self, server: &str) -> Option<String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, server).ok()?.get_password().ok()
    }

    fn save(&self, server: &str, token: &str) -> Result<(), SessionError> {
        keyring::Entry::new(KEYCHAIN_SERVICE, server)
            .and_then(|e| e.set_password(token))
            .map_err(|e| SessionError::Refused(format!("The sign-in could not be stored: {e}")))
    }

    fn delete(&self, server: &str) {
        if let Ok(e) = keyring::Entry::new(KEYCHAIN_SERVICE, server) {
            let _ = e.delete_credential();
        }
    }
}

#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl TokenStore for MemoryStore {
    fn load(&self, server: &str) -> Option<String> {
        self.0.lock().get(server).cloned()
    }

    fn save(&self, server: &str, token: &str) -> Result<(), SessionError> {
        self.0.lock().insert(server.to_string(), token.to_string());
        Ok(())
    }

    fn delete(&self, server: &str) {
        self.0.lock().remove(server);
    }
}

pub type Store = Arc<dyn TokenStore>;

/// At least 16 random bytes, base64url.
pub fn new_state() -> String {
    let mut b = [0u8; 24];
    getrandom::getrandom(&mut b).expect("the system random source");
    URL_SAFE_NO_PAD.encode(b)
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (
            bytes[i],
            s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()),
        ) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The sign-in page's address for a loopback port.
pub fn login_address(login_url: &str, port: u16, state: &str) -> Result<String, SessionError> {
    let mut url = url::Url::parse(login_url)
        .map_err(|_| SessionError::Refused("The server gave no usable sign-in page".to_string()))?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(SessionError::Refused(
            "The server gave no usable sign-in page".to_string(),
        ));
    }
    url.query_pairs_mut()
        .append_pair("port", &port.to_string())
        .append_pair("state", state);
    Ok(url.into())
}

const PAGE: &str = "<!doctype html><meta charset=utf-8><title>Ruthos</title>\
    <p style=\"font:16px system-ui;margin:3em\">You are signed in. You can return to Ruthos.</p>";

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Serve `GET /callback` until a request carries `state`, or the time runs
/// out. A wrong state changes nothing, and the state is used once.
pub async fn await_callback(listener: TcpListener, state: String, timeout: Duration) -> Result<String, SessionError> {
    let serve = async {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let mut buf = [0u8; 8192];
            let mut got = 0;
            while got < buf.len() {
                match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf[got..])).await {
                    Ok(Ok(n)) if n > 0 => got += n,
                    _ => break,
                }
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf[..got]);
            let target = head
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("GET "))
                .and_then(|l| l.split(' ').next());
            let Some(query) = target.and_then(|t| t.strip_prefix("/callback?")) else {
                respond(&mut stream, "404 Not Found", "").await;
                continue;
            };
            let good_state = query_value(query, "state").map(|s| percent_decode(&s)) == Some(state.clone());
            match (good_state, query_value(query, "token")) {
                (true, Some(token)) if !token.is_empty() => {
                    respond(&mut stream, "200 OK", PAGE).await;
                    return percent_decode(&token);
                }
                _ => respond(&mut stream, "400 Bad Request", "").await,
            }
        }
    };
    tokio::time::timeout(timeout, serve)
        .await
        .map_err(|_| SessionError::Refused("Sign-in timed out".to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    /// The relay's pinned claims, signed with nothing that matters here.
    pub fn pinned_token() -> String {
        let claims = r#"{"v":1,"site":"exam.example.org","uid":7,"name":"Ada Lovelace","email":"ada@example.org","roles":["host"],"iat":1700000000,"exp":1700043200}"#;
        format!("{}.signature", URL_SAFE_NO_PAD.encode(claims))
    }

    #[test]
    fn the_pinned_token_decodes() {
        let a = decode(&pinned_token()).unwrap();
        assert_eq!(a.name, "Ada Lovelace");
        assert!(a.is_host());
        assert_eq!(a.expires.timestamp(), 1700043200);
    }

    #[test]
    fn rubbish_is_not_a_token() {
        assert!(decode("not a token").is_err());
    }

    #[test]
    fn the_login_address_carries_port_and_state() {
        let a = login_address("https://relay.example.org/login.php", 5000, "abc").unwrap();
        assert_eq!(a, "https://relay.example.org/login.php?port=5000&state=abc");
        assert!(login_address("file:///c:/x", 1, "a").is_err());
    }

    async fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    }

    #[tokio::test]
    async fn a_wrong_state_is_refused_and_the_right_one_is_used_once() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let task = tokio::spawn(await_callback(l, "good".into(), Duration::from_secs(5)));
        assert!(get(port, "/callback?token=evil&state=bad").await.contains("400"));
        assert!(get(port, "/other").await.contains("404"));
        assert!(get(port, "/callback?token=t%2B1&state=good").await.contains("200"));
        assert_eq!(task.await.unwrap().unwrap(), "t+1");
        // The listener has closed, so the state cannot be used again.
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn sign_in_times_out() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let e = await_callback(l, "s".into(), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("timed out"));
    }
}
