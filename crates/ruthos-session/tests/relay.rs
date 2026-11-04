//! The client against a scripted relay on a loopback port. It speaks the
//! relay's protocol from the client specification; it is not the relay, so
//! these tests prove the client's behaviour, not their agreement.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use ruthos_session::auth::{MemoryStore, TokenStore};
use ruthos_session::ws::Timing;
use ruthos_session::{Applied, Change, Command, ConnectionState, Event, Options, SessionClient};

fn token(exp: i64) -> String {
    let claims = json!({"name":"Ada Lovelace","email":"ada@example.org","roles":["host"],"exp":exp});
    format!("{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

fn far() -> i64 {
    chrono::Utc::now().timestamp() + 12 * 3600
}

/// One WebSocket connection, from the relay's side.
struct Peer {
    ws: WebSocketStream<TcpStream>,
    /// The Authorization header of the upgrade.
    auth: Option<String>,
}

impl Peer {
    async fn recv(&mut self) -> Option<Value> {
        loop {
            match self.ws.next().await? {
                Ok(Message::Text(t)) => return serde_json::from_str(t.as_str()).ok(),
                Ok(Message::Close(_)) | Err(_) => return None,
                _ => {}
            }
        }
    }

    /// The next message that is not a pong.
    async fn recv_t(&mut self, t: &str) -> Value {
        loop {
            let m = self.recv().await.expect("the connection stayed open");
            if m["t"] == t {
                return m;
            }
        }
    }

    async fn send(&mut self, v: Value) {
        self.ws.send(Message::text(v.to_string())).await.unwrap();
    }

    async fn close(&mut self, code: u16) {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        };
        let _ = self.ws.close(Some(frame)).await;
    }

    /// Answer `hello` and `join`, the common opening.
    async fn welcome(&mut self) -> Value {
        self.recv_t("hello").await;
        self.send(json!({"v":1,"t":"welcome","body":{"account":{"sub":"u7","name":"Ada","host":true,"guest":false},"ping":20}}))
            .await;
        self.recv_t("join").await
    }

    async fn admit(&mut self, seq: u64) {
        self.send(json!({"v":1,"t":"admitted","body":{"sid":"s1"}})).await;
        self.send(json!({"v":1,"t":"state","seq":seq,"body":{"layout":{"schema":1,"rows":1,"cols":1},"cells":{},"measurements":{}}}))
            .await;
    }
}

type Http = Arc<dyn Fn(&str, &HashMap<String, String>, &str) -> (u16, String) + Send + Sync>;

struct Relay {
    address: String,
    connections: Arc<AtomicUsize>,
}

fn info(port: u16) -> String {
    json!({"name":"Test relay","protocol_versions":[1],"auth":{"type":"token","login_url":format!("http://127.0.0.1:{port}/login")},
           "guests":true,"case_store":true,"ws_path":"/v1/ws"})
    .to_string()
}

async fn answer_http(mut s: TcpStream, port: u16, http: Option<Http>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + length {
        let n = s.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
    let (status, reply) = if request.starts_with("GET /v1/info") {
        (200, info(port))
    } else if let Some(h) = http {
        h(&request, &headers, &body)
    } else {
        (404, "{}".to_string())
    };
    // A status of 0 stands for the connection dropping before the reply.
    if status == 0 {
        return;
    }
    let text = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = s.write_all(text.as_bytes()).await;
}

/// Start a relay. `script` runs once per WebSocket connection, numbered from 1.
async fn relay<S, F>(http: Option<Http>, script: S) -> Relay
where
    S: Fn(usize, Peer) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    tokio::spawn(async move {
        let script = Arc::new(script);
        loop {
            let Ok((s, _)) = listener.accept().await else { return };
            let mut peek = [0u8; 2048];
            let n = s.peek(&mut peek).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&peek[..n]).to_ascii_lowercase();
            if head.contains("upgrade: websocket") {
                let auth = head
                    .lines()
                    .find_map(|l| l.strip_prefix("authorization: ").map(str::to_string));
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let script = script.clone();
                tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(s).await {
                        script(n, Peer { ws, auth }).await;
                    }
                });
            } else {
                tokio::spawn(answer_http(s, port, http.clone()));
            }
        }
    });
    Relay {
        address: format!("http://127.0.0.1:{port}"),
        connections,
    }
}

fn client_with(relay: &Relay, timing: Timing, exp: i64) -> SessionClient {
    let store = Arc::new(MemoryStore::default());
    store.save(&relay.address, &token(exp)).unwrap();
    let options = Options {
        store,
        timing,
        ..Default::default()
    };
    SessionClient::with_options(&relay.address, || {}, options).unwrap()
}

fn fast() -> Timing {
    Timing {
        dead_after: Duration::from_secs(2),
        give_up: Duration::from_secs(5),
        first_retry: Duration::from_millis(50),
        retry_ceiling: Duration::from_millis(200),
        state_retry: Duration::from_secs(1),
    }
}

/// Wait for an event that `want` accepts.
async fn until<T>(c: &SessionClient, mut want: impl FnMut(&Event) -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        while let Ok(e) = c.events().try_recv() {
            if let Some(t) = want(&e) {
                return t;
            }
        }
        assert!(Instant::now() < end, "the event did not arrive");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn discovered(c: &SessionClient) {
    c.send(Command::Discover);
    until(c, |e| matches!(e, Event::ServerInfo(_)).then_some(())).await;
}

async fn live(c: &SessionClient) {
    until(c, |e| {
        matches!(e, Event::Connection(ConnectionState::Live)).then_some(())
    })
    .await;
}

fn set(key: &str, v: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_string(), v.into());
    m
}

#[tokio::test]
async fn a_participant_waits_is_admitted_and_receives_state() {
    let seen = Arc::new(Mutex::new(None));
    let saw = seen.clone();
    let r = relay(None, move |_, mut p| {
        let saw = saw.clone();
        async move {
            *saw.lock() = p.auth.clone();
            let join = p.welcome().await;
            assert_eq!(join["body"]["code"], "482913");
            p.send(json!({"v":1,"t":"waiting","body":{"sid":"s1","title":"Cardiac","host_name":"Dr A"}}))
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            p.admit(5).await;
            while p.recv().await.is_some() {}
        }
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    let waiting = until(&c, |e| match e {
        Event::Connection(ConnectionState::Waiting { title, host_name }) => Some((title.clone(), host_name.clone())),
        _ => None,
    })
    .await;
    assert_eq!(waiting, ("Cardiac".to_string(), "Dr A".to_string()));
    live(&c).await;
    until(&c, |e| matches!(e, Event::State(_)).then_some(())).await;
    assert_eq!(c.model().unwrap().seq, 5);
    // The token goes in the upgrade's header, never in the URL.
    assert!(seen.lock().as_ref().unwrap().starts_with("bearer "));
}

#[tokio::test]
async fn final_closes_never_reconnect() {
    for code in 4000..=4004u16 {
        let r = relay(None, move |_, mut p| async move {
            p.welcome().await;
            p.close(code).await;
        })
        .await;
        let c = client_with(&r, fast(), far());
        discovered(&c).await;
        c.send(Command::Join { code: "482913".into() });
        let got = until(&c, |e| match e {
            Event::Connection(ConnectionState::Closed { code, .. }) => Some(*code),
            _ => None,
        })
        .await;
        assert_eq!(got, code);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(r.connections.load(Ordering::SeqCst), 1, "close {code}");
    }
}

#[tokio::test]
async fn a_gap_requests_the_rest_and_discards_until_the_reply() {
    let r = relay(None, |_, mut p| async move {
        p.welcome().await;
        p.admit(0).await;
        let change = |seq: u64, frame: i64| json!({"v":1,"t":"change","seq":seq,"from":"u9","body":{"el":"cell","cell":0,"set":{"frame":frame}}});
        p.send(change(1, 1)).await;
        p.send(change(2, 2)).await;
        p.send(change(4, 4)).await;
        let asked = p.recv_t("state_request").await;
        assert_eq!(asked["body"]["seq"], 2);
        p.send(change(3, 3)).await;
        p.send(change(4, 44)).await;
        while p.recv().await.is_some() {}
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    until(&c, |e| matches!(e, Event::Applied { seq: 4, .. }).then_some(())).await;
    let model = c.model().unwrap();
    // The discarded 4 carried frame 4; the replay's 4 is what stands.
    assert_eq!(model.cells[&0]["frame"], 44);
    assert_eq!(model.seq, 4);
}

#[tokio::test]
async fn scrolling_64_cells_stays_within_60_messages_a_second() {
    let times = Arc::new(Mutex::new(Vec::new()));
    let log = times.clone();
    let r = relay(None, move |_, mut p| {
        let log = log.clone();
        async move {
            p.welcome().await;
            p.admit(0).await;
            while let Some(m) = p.recv().await {
                if m["t"] == "change" {
                    log.lock().push(Instant::now());
                }
            }
        }
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    live(&c).await;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(2500) {
        for cell in 0..64u8 {
            c.send(Command::Change(Change::Cell {
                cell,
                set: set("frame", start.elapsed().as_millis() as i64),
            }));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = times.lock().clone();
    assert!(t.len() > 80, "only {} sent", t.len());
    let most = (0..t.len())
        .map(|i| {
            t[i..]
                .iter()
                .take_while(|x| **x - t[i] < Duration::from_secs(1))
                .count()
        })
        .max()
        .unwrap();
    assert!(most <= 60, "{most} in a second");
}

#[tokio::test]
async fn a_refused_change_rolls_back_and_losing_control_too() {
    let r = relay(None, |_, mut p| async move {
        p.welcome().await;
        p.admit(0).await;
        let m = p.recv_t("change").await;
        let cid = m["cid"].clone();
        p.send(json!({"v":1,"t":"error","cid":cid,"body":{"code":"locked","message":"The session is locked"}}))
            .await;
        p.send(json!({"v":1,"t":"presence","seq":1,"body":{"members":[],"control":"u7","locked":false,"waiting":[]}}))
            .await;
        p.send(json!({"v":1,"t":"presence","seq":2,"body":{"members":[],"control":"u9","locked":false,"waiting":[]}}))
            .await;
        while p.recv().await.is_some() {}
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    until(&c, |e| matches!(e, Event::State(_)).then_some(())).await;
    c.send(Command::Change(Change::Cell {
        cell: 0,
        set: set("frame", 3),
    }));
    let message = until(&c, |e| match e {
        Event::Error { message, .. } => Some(message.clone()),
        _ => None,
    })
    .await;
    assert_eq!(message, "The session is locked");
    until(&c, |e| matches!(e, Event::State(_)).then_some(())).await;
    until(&c, |e| matches!(e, Event::State(_)).then_some(())).await;
}

#[tokio::test]
async fn a_silent_connection_reconnects_rejoins_by_id_and_recovers() {
    let rejoined = Arc::new(Mutex::new(None));
    let saw = rejoined.clone();
    let r = relay(None, move |n, mut p| {
        let saw = saw.clone();
        async move {
            let join = p.welcome().await;
            if n == 1 {
                p.admit(7).await;
                // Silence, with the connection open.
                tokio::time::sleep(Duration::from_secs(10)).await;
            } else {
                p.send(json!({"v":1,"t":"admitted","body":{"sid":"s1"}})).await;
                let asked = p.recv_t("state_request").await;
                *saw.lock() = Some((join["body"]["sid"].clone(), asked["body"]["seq"].clone()));
                p.send(json!({"v":1,"t":"change","seq":8,"body":{"el":"layout","set":{"rows":3}}}))
                    .await;
                while p.recv().await.is_some() {}
            }
        }
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    until(&c, |e| matches!(e, Event::State(_)).then_some(())).await;
    until(&c, |e| matches!(e, Event::Applied { seq: 8, .. }).then_some(())).await;
    assert_eq!(*rejoined.lock(), Some((json!("s1"), json!(7))));
    assert_eq!(c.model().unwrap().layout["rows"], 3);
}

#[tokio::test]
async fn near_expiry_warns_and_a_new_token_goes_as_reauth() {
    let got = Arc::new(Mutex::new(None));
    let saw = got.clone();
    let r = relay(None, move |_, mut p| {
        let saw = saw.clone();
        async move {
            p.welcome().await;
            p.admit(0).await;
            let m = p.recv_t("reauth").await;
            *saw.lock() = m["body"]["token"].as_str().map(str::to_string);
            while p.recv().await.is_some() {}
        }
    })
    .await;
    let c = client_with(&r, fast(), chrono::Utc::now().timestamp() + 20 * 60);
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    let minutes = until(&c, |e| match e {
        Event::ExpiresSoon { minutes } => Some(*minutes),
        _ => None,
    })
    .await;
    assert!(minutes <= 20);
    let fresh = token(far());
    c.send(Command::PasteToken(fresh.clone()));
    let end = Instant::now() + Duration::from_secs(5);
    while got.lock().is_none() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(got.lock().clone(), Some(fresh));
}

#[tokio::test]
async fn a_server_without_version_1_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 2048];
        let _ = s.read(&mut b).await;
        let body = r#"{"name":"Old","protocol_versions":[2],"ws_path":"/w"}"#;
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(reply.as_bytes()).await;
    });
    let options = Options {
        store: Arc::new(MemoryStore::default()),
        ..Default::default()
    };
    let c = SessionClient::with_options(&format!("http://127.0.0.1:{port}"), || {}, options).unwrap();
    c.send(Command::Discover);
    let m = until(&c, |e| match e {
        Event::Error { message, .. } => Some(message.clone()),
        _ => None,
    })
    .await;
    assert!(m.contains('2'), "{m}");
}

#[tokio::test]
async fn sign_in_goes_through_the_browser_and_back() {
    let r = relay(None, |_, _| async {}).await;
    let browser = |url: &str| {
        // The sign-in page's Open Ruthos button.
        let u = url::Url::parse(url).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        let (port, state) = (q["port"].clone(), q["state"].clone());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            let t = token(far());
            write!(s, "GET /callback?token={t}&state={state} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
        });
        true
    };
    let store = Arc::new(MemoryStore::default());
    let options = Options {
        store: store.clone(),
        browser: Arc::new(browser),
        ..Default::default()
    };
    let c = SessionClient::with_options(&r.address, || {}, options).unwrap();
    discovered(&c).await;
    c.send(Command::SignIn);
    let name = until(&c, |e| match e {
        Event::Account(Some(a)) => Some(a.name.clone()),
        _ => None,
    })
    .await;
    assert_eq!(name, "Ada Lovelace");
    assert!(store.load(&r.address).is_some());
    c.send(Command::SignOut);
    until(&c, |e| matches!(e, Event::Account(None)).then_some(())).await;
    assert!(store.load(&r.address).is_none());
}

#[tokio::test]
async fn a_retried_create_sends_the_same_key() {
    let keys = Arc::new(Mutex::new(Vec::new()));
    let log = keys.clone();
    let tries = Arc::new(AtomicUsize::new(0));
    let http: Http = Arc::new(move |request, headers, _| {
        if request.starts_with("POST /v1/sessions") {
            log.lock()
                .push(headers.get("idempotency-key").cloned().unwrap_or_default());
            if tries.fetch_add(1, Ordering::SeqCst) == 0 {
                return (0, String::new());
            }
            return (
                200,
                r#"{"id":"s1","code":"482913","guests":false,"cases":[]}"#.to_string(),
            );
        }
        (404, "{}".to_string())
    });
    let r = relay(Some(http), |_, _| async {}).await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Create {
        title: Some("Cardiac".into()),
        cases: vec![],
        guests: false,
    });
    let code = until(&c, |e| match e {
        Event::Created { code, .. } => Some(code.clone()),
        _ => None,
    })
    .await;
    assert_eq!(code, "482913");
    let keys = keys.lock();
    assert_eq!(keys.len(), 2, "the dropped attempt was retried");
    assert_eq!(keys[0], keys[1]);
    assert!(uuid::Uuid::parse_str(&keys[0]).is_ok());
}

#[tokio::test]
async fn a_lock_is_applied_at_once_and_end_is_reported() {
    let r = relay(None, |_, mut p| async move {
        p.welcome().await;
        p.admit(0).await;
        p.send(json!({"v":1,"t":"lock","seq":1,"body":{}})).await;
        p.send(json!({"v":1,"t":"end","seq":2,"body":{"reason":"ended"}})).await;
        p.close(4000).await;
    })
    .await;
    let c = client_with(&r, fast(), far());
    discovered(&c).await;
    c.send(Command::Join { code: "482913".into() });
    until(&c, |e| {
        matches!(
            e,
            Event::Applied {
                msg: Applied::Locked(true),
                ..
            }
        )
        .then_some(())
    })
    .await;
    until(&c, |e| {
        matches!(
            e,
            Event::Applied {
                msg: Applied::End(_),
                ..
            }
        )
        .then_some(())
    })
    .await;
    until(&c, |e| {
        matches!(e, Event::Connection(ConnectionState::Closed { code: 4000, .. })).then_some(())
    })
    .await;
}
