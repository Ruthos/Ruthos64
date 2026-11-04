//! The client against a real relay, for developers who have one. The address
//! and any token come from a git-ignored ini named by `RUTHOS_INI`, or
//! `ruthos.local.ini` at the top of the repository; without one these tests
//! pass without doing anything, as they do in CI.
//!
//! ```ini
//! [dev]
//! server = https://...
//! ; Optional: a token from the sign-in page, for the join test.
//! token = ...
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ruthos_session::auth::{MemoryStore, TokenStore};
use ruthos_session::deployment::ini_value;
use ruthos_session::{Command, ConnectionState, Event, Options, SessionClient};

fn local(key: &str) -> Option<String> {
    let path = std::env::var_os("RUTHOS_INI")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ruthos.local.ini"));
    ini_value(&std::fs::read_to_string(path).ok()?, "dev", key).filter(|v| !v.is_empty())
}

fn client(server: &str, token: Option<&str>) -> SessionClient {
    let store = Arc::new(MemoryStore::default());
    if let Some(t) = token {
        store.save(server, t).unwrap();
    }
    let options = Options {
        store,
        ..Default::default()
    };
    SessionClient::with_options(server, || {}, options).unwrap()
}

fn until<T>(c: &SessionClient, seconds: u64, mut want: impl FnMut(&Event) -> Option<T>) -> Option<T> {
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        if let Ok(e) = c.events().recv_timeout(Duration::from_millis(100)) {
            if let Event::Error { code, .. } = &e {
                println!("relay error: {code}");
            }
            if let Some(t) = want(&e) {
                return Some(t);
            }
        }
    }
    None
}

#[test]
fn discovery_speaks_version_1_over_tls() {
    let Some(server) = local("server") else { return };
    let c = client(&server, None);
    c.send(Command::Discover);
    let info = until(&c, 20, |e| match e {
        Event::ServerInfo(i) => Some(i.clone()),
        _ => None,
    })
    .expect("the server answered");
    assert!(info.protocol_versions.contains(&1));
    assert!(info.ws_url(&server).unwrap().starts_with("wss://"));
}

#[test]
fn the_websocket_refuses_a_missing_token() {
    let Some(server) = local("server") else { return };
    let c = client(&server, None);
    c.send(Command::Discover);
    until(&c, 20, |e| matches!(e, Event::ServerInfo(_)).then_some(())).expect("discovery");
    c.send(Command::Join { code: "000000".into() });
    let closed = until(&c, 30, |e| match e {
        Event::Connection(ConnectionState::Closed { code, .. }) => Some(*code),
        _ => None,
    });
    assert_eq!(
        closed,
        Some(4003),
        "the upgrade or the hello is refused without a token"
    );
}

#[test]
fn a_token_reaches_welcome() {
    let (Some(server), Some(token)) = (local("server"), local("token")) else {
        return;
    };
    let c = client(&server, Some(&token));
    c.send(Command::Discover);
    until(&c, 20, |e| matches!(e, Event::ServerInfo(_)).then_some(())).expect("discovery");
    c.send(Command::Join { code: "000000".into() });
    let sub = until(&c, 30, |e| match e {
        Event::Welcome(w) => Some(w.sub.clone()),
        _ => None,
    });
    assert!(sub.is_some(), "the relay answered hello with welcome");
    c.send(Command::Leave);
}
