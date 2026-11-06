//! The session connection: hello and join, keepalive, reauth, close codes
//! and reconnection. Frames go out one per 17 ms slot, which keeps a
//! connection below the relay's 60 messages a second whatever the viewer does.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::WebSocketStream;

use crate::proto::{
    close_is_final, close_reason, Change, Envelope, Presence, SharedModel, WelcomeAccount, MAX_FRAME, PROTOCOL,
};
use crate::sync::{Coalescer, Sequenced, Tracker};
use crate::{Applied, ConnectionState, Emitter, Event, SessionError};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Stream = WebSocketStream<Box<dyn Io>>;

const EXPIRY_WARNING_MINUTES: i64 = 30;

/// How a session is reached: a code the first time, its ID afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinBy {
    Code(String),
    Sid(String),
}

/// What the adapter's commands become on this connection.
#[derive(Debug)]
pub enum Out {
    /// A message expecting an answer.
    Control(&'static str, Value),
    Change(Change),
    Pointer(Value),
    Drag {
        id: String,
        i: u16,
        x: f32,
        y: f32,
    },
    Reauth,
    Leave,
}

/// The relay's timings, which tests shorten.
#[derive(Debug, Clone)]
pub struct Timing {
    pub dead_after: Duration,
    pub give_up: Duration,
    pub first_retry: Duration,
    pub retry_ceiling: Duration,
    pub state_retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            dead_after: Duration::from_secs(60),
            give_up: Duration::from_secs(600),
            first_retry: Duration::from_secs(1),
            retry_ceiling: Duration::from_secs(30),
            state_retry: Duration::from_secs(5),
        }
    }
}

pub struct Params {
    pub url: String,
    pub token: Arc<Mutex<Option<String>>>,
    pub join: JoinBy,
    pub proxy: Option<String>,
    pub timing: Timing,
    pub shared: Arc<Mutex<Option<Arc<SharedModel>>>>,
}

/// How one connection ended.
enum Ended {
    Final(u16),
    Lost,
    Left,
}

/// The delay before attempt `n`, counting from 0: 1, 2, 4, 8, 16, then 30
/// seconds, each varied by up to a quarter so a room whose Wi-Fi drops does
/// not reconnect in lockstep.
pub fn retry_delay(n: u32, first: Duration, ceiling: Duration, jitter: f32) -> Duration {
    let base = first.saturating_mul(1u32 << n.min(16));
    let base = if n >= 5 { ceiling } else { base.min(ceiling) };
    base.mul_f32(1.0 + jitter.clamp(-1.0, 1.0) * 0.25)
}

fn jitter() -> f32 {
    let mut b = [0u8; 1];
    let _ = getrandom::fill(&mut b);
    b[0] as f32 / 127.5 - 1.0
}

/// Run a session until it ends, reconnecting as needed.
pub async fn run(params: Params, mut out: UnboundedReceiver<Out>, emit: Emitter) {
    let mut tracker = Tracker::default();
    let mut coalescer = Coalescer::default();
    let mut join = params.join.clone();
    let mut attempt = 0u32;
    let mut lost_at: Option<Instant> = None;
    loop {
        emit.send(Event::Connection(if attempt == 0 {
            ConnectionState::Connecting
        } else {
            ConnectionState::Reconnecting { attempt }
        }));
        let ended = match connect(&params).await {
            Ok(mut ws) => {
                let mut c = Conn {
                    ws: &mut ws,
                    tracker: &mut tracker,
                    coalescer: &mut coalescer,
                    join: &mut join,
                    params: &params,
                    emit: &emit,
                    pending_changes: HashSet::new(),
                    next_cid: 0,
                    sub: None,
                    has_control: false,
                    rejoining: lost_at.is_some(),
                    warned: false,
                    connected: &mut attempt,
                    lost_at: &mut lost_at,
                };
                c.drive(&mut out).await
            }
            Err(SessionError::Api { status: 401 | 403, .. }) => Ended::Final(4003),
            Err(e) => {
                tracing::info!("session connection failed: {e}");
                Ended::Lost
            }
        };
        match ended {
            Ended::Left => {
                emit.send(Event::Connection(ConnectionState::Idle));
                return;
            }
            Ended::Final(code) => {
                emit.send(Event::Connection(ConnectionState::Closed {
                    code,
                    reason: close_reason(code).to_string(),
                }));
                return;
            }
            Ended::Lost => {}
        }
        coalescer.clear();
        let since = *lost_at.get_or_insert_with(Instant::now);
        if since.elapsed() >= params.timing.give_up {
            emit.send(Event::Connection(ConnectionState::Closed {
                code: 0,
                reason: "The session was lost".to_string(),
            }));
            return;
        }
        let wait = retry_delay(
            attempt,
            params.timing.first_retry,
            params.timing.retry_ceiling,
            jitter(),
        );
        attempt += 1;
        let sleep = tokio::time::sleep(wait);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                msg = out.recv() => match msg {
                    Some(Out::Leave) | None => {
                        emit.send(Event::Connection(ConnectionState::Idle));
                        return;
                    }
                    // Nothing can be sent now, and the state after
                    // reconnecting decides what is still true.
                    Some(_) => {}
                },
            }
        }
    }
}

async fn tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream, SessionError> {
    let url = url::Url::parse(proxy).map_err(|_| SessionError::Refused("The proxy address is not valid".into()))?;
    let target = (
        url.host_str()
            .ok_or_else(|| SessionError::Refused("The proxy address is not valid".into()))?,
        url.port_or_known_default().unwrap_or(3128),
    );
    let mut s = TcpStream::connect(target).await.map_err(net)?;
    s.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())
        .await
        .map_err(net)?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 || s.read(&mut byte).await.map_err(net)? == 0 {
            return Err(SessionError::Network("the proxy closed the connection".into()));
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    if line.split_whitespace().nth(1) != Some("200") {
        return Err(SessionError::Network("the proxy refused the tunnel".into()));
    }
    Ok(s)
}

fn net(e: impl std::fmt::Display) -> SessionError {
    SessionError::Network(e.to_string())
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, SessionError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(net)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

async fn connect(p: &Params) -> Result<Stream, SessionError> {
    let url = url::Url::parse(&p.url).map_err(net)?;
    let host = url.host_str().ok_or_else(|| net("no host"))?.to_string();
    let port = url.port_or_known_default().ok_or_else(|| net("no port"))?;
    let secure = url.scheme() == "wss";
    let proxy = p.proxy.clone().filter(|_| secure).or_else(|| {
        secure
            .then(|| {
                std::env::var("HTTPS_PROXY")
                    .or_else(|_| std::env::var("https_proxy"))
                    .ok()
            })
            .flatten()
    });
    let tcp = match proxy {
        Some(proxy) => tunnel(&proxy, &host, port).await?,
        None => TcpStream::connect((host.as_str(), port)).await.map_err(net)?,
    };
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if secure {
        let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(net)?;
        let tls = tokio_rustls::TlsConnector::from(tls_config()?)
            .connect(name, tcp)
            .await
            .map_err(net)?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    let mut request = p.url.as_str().into_client_request().map_err(net)?;
    // In the upgrade's header, never in the URL.
    if let Some(token) = p.token.lock().clone() {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(net)?;
        request.headers_mut().insert("Authorization", value);
    }
    match tokio_tungstenite::client_async(request, io).await {
        Ok((ws, _)) => Ok(ws),
        Err(WsError::Http(r)) => Err(SessionError::Api {
            status: r.status().as_u16(),
            code: "upgrade".to_string(),
            message: String::new(),
        }),
        Err(e) => Err(net(e)),
    }
}

struct Conn<'a> {
    ws: &'a mut Stream,
    tracker: &'a mut Tracker,
    coalescer: &'a mut Coalescer,
    join: &'a mut JoinBy,
    params: &'a Params,
    emit: &'a Emitter,
    /// Sent changes not yet acknowledged.
    pending_changes: HashSet<String>,
    next_cid: u64,
    sub: Option<String>,
    has_control: bool,
    rejoining: bool,
    warned: bool,
    connected: &'a mut u32,
    lost_at: &'a mut Option<Instant>,
}

impl Conn<'_> {
    fn sid(&self) -> Option<String> {
        match &*self.join {
            JoinBy::Sid(s) => Some(s.clone()),
            JoinBy::Code(_) => None,
        }
    }

    fn cid(&mut self) -> String {
        self.next_cid += 1;
        format!("c{}", self.next_cid)
    }

    async fn send(&mut self, mut e: Envelope) -> Result<(), String> {
        e.sid = self.sid();
        let text = serde_json::to_string(&e).unwrap_or_default();
        if text.len() > MAX_FRAME {
            self.emit.send(Event::Error {
                code: "too_large".to_string(),
                message: "That is too large to share in a session".to_string(),
            });
            return Ok(());
        }
        tracing::debug!(t = %e.t, "send");
        self.ws.send(Message::text(text)).await.map_err(|e| e.to_string())
    }

    async fn control(&mut self, t: &str, body: Value) -> Result<(), String> {
        let mut e = Envelope::new(t, body);
        e.cid = Some(self.cid());
        self.send(e).await
    }

    fn publish(&self) {
        *self.params.shared.lock() = Some(Arc::new(self.tracker.model.clone()));
    }

    /// Undo the controller's optimistic view: back to what is confirmed.
    fn rollback(&mut self) {
        self.coalescer.clear();
        self.emit.send(Event::State(Arc::new(self.tracker.model.clone())));
    }

    async fn drive(&mut self, out: &mut UnboundedReceiver<Out>) -> Ended {
        match self.exchange(out).await {
            Ok(ended) => ended,
            Err(e) => {
                tracing::info!("session connection lost: {e}");
                Ended::Lost
            }
        }
    }

    async fn exchange(&mut self, out: &mut UnboundedReceiver<Out>) -> Result<Ended, String> {
        let hello = json!({"protocol": PROTOCOL, "client": concat!("ruthos/", env!("CARGO_PKG_VERSION"))});
        self.send(Envelope::new("hello", hello)).await?;
        let mut last_frame = Instant::now();
        let mut recovery_asked: Option<Instant> = None;
        let mut slots = tokio::time::interval(Duration::from_millis(17));
        slots.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut slow = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                frame = self.ws.next() => {
                    last_frame = Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(e) = serde_json::from_str::<Envelope>(text.as_str()) else { continue };
                            if let Some(from) = self.handle(e).await? {
                                recovery_asked = Some(Instant::now());
                                self.control("state_request", json!({"seq": from})).await?;
                            }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let code = frame.map(|f| u16::from(f.code)).unwrap_or(1006);
                            return Ok(if close_is_final(code) { Ended::Final(code) } else { Ended::Lost });
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(e.to_string()),
                        None => return Ok(Ended::Lost),
                    }
                }
                cmd = out.recv() => match cmd {
                    None | Some(Out::Leave) => {
                        let _ = self.ws.close(None).await;
                        return Ok(Ended::Left);
                    }
                    Some(Out::Control(t, body)) => self.control(t, body).await?,
                    Some(Out::Change(c)) => self.coalescer.push(c),
                    Some(Out::Pointer(b)) => self.coalescer.pointer(b),
                    Some(Out::Drag { id, i, x, y }) => self.coalescer.drag(id, i, x, y),
                    Some(Out::Reauth) => {
                        let token = self.params.token.lock().clone().unwrap_or_default();
                        self.control("reauth", json!({"token": token})).await?;
                        self.warned = false;
                    }
                },
                _ = slots.tick() => {
                    if let Some(mut e) = self.coalescer.take_next() {
                        if e.t == "change" {
                            let cid = self.cid();
                            self.pending_changes.insert(cid.clone());
                            e.cid = Some(cid);
                        }
                        self.send(e).await?;
                    }
                }
                _ = slow.tick() => {
                    if last_frame.elapsed() >= self.params.timing.dead_after {
                        return Ok(Ended::Lost);
                    }
                    self.check_expiry();
                    if self.tracker.recovering
                        && recovery_asked.is_some_and(|t| t.elapsed() >= self.params.timing.state_retry)
                    {
                        recovery_asked = Some(Instant::now());
                        let from = self.tracker.last_seq();
                        self.control("state_request", json!({"seq": from})).await?;
                    }
                }
            }
        }
    }

    fn check_expiry(&mut self) {
        if self.warned {
            return;
        }
        let Some(token) = self.params.token.lock().clone() else {
            return;
        };
        if let Ok(account) = crate::auth::decode(&token) {
            let left = (account.expires - Utc::now()).num_minutes();
            if left <= EXPIRY_WARNING_MINUTES {
                self.warned = true;
                self.emit.send(Event::ExpiresSoon {
                    minutes: left.max(0) as u32,
                });
            }
        }
    }

    /// Act on a message; a sequence number means `state_request` is needed.
    async fn handle(&mut self, e: Envelope) -> Result<Option<u64>, String> {
        if let Some(seq) = e.seq {
            if e.t != "state" {
                match self.tracker.place(seq) {
                    Sequenced::Apply => {}
                    Sequenced::Ignore | Sequenced::Discard => return Ok(None),
                    Sequenced::Request(from) => return Ok(Some(from)),
                }
            }
        }
        let seq = e.seq.unwrap_or_else(|| self.tracker.last_seq());
        match e.t.as_str() {
            "welcome" => {
                let account: WelcomeAccount =
                    serde_json::from_value(e.body.get("account").cloned().unwrap_or_default()).unwrap_or_default();
                self.sub = Some(account.sub.clone());
                self.emit.send(Event::Welcome(account));
                let body = match &*self.join {
                    JoinBy::Code(code) => json!({"code": code}),
                    JoinBy::Sid(sid) => json!({"sid": sid}),
                };
                self.control("join", body).await?;
            }
            "waiting" => {
                self.note_sid(&e.body);
                let text = |k: &str| e.body.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                self.emit.send(Event::Connection(ConnectionState::Waiting {
                    title: text("title"),
                    host_name: text("host_name"),
                }));
            }
            "admitted" => {
                self.note_sid(&e.body);
                *self.connected = 0;
                *self.lost_at = None;
                self.emit.send(Event::Connection(ConnectionState::Live));
                if self.rejoining {
                    self.rejoining = false;
                    let from = self.tracker.last_seq();
                    self.control("state_request", json!({"seq": from})).await?;
                }
            }
            "state" => {
                let seq = e
                    .seq
                    .or_else(|| e.body.get("seq").and_then(Value::as_u64))
                    .unwrap_or(seq);
                self.tracker.replace(SharedModel::from_state(seq, &e.body));
                self.publish();
                self.emit.send(Event::State(Arc::new(self.tracker.model.clone())));
                if let Some(p) = e.body.get("presence") {
                    self.presence(p);
                }
            }
            "ping" => {
                self.send(Envelope::new("pong", json!({}))).await?;
            }
            "pong" => {}
            "presence" => {
                let body = e.body.clone();
                self.presence(&body);
            }
            "change" => {
                self.tracker.model.apply_change(&e.body);
                self.publish();
                if let Some(cid) = &e.cid {
                    self.pending_changes.remove(cid);
                }
                self.emit.send(Event::Applied {
                    seq,
                    msg: Applied::Change,
                });
            }
            "lock" | "unlock" => self.emit.send(Event::Applied {
                seq,
                msg: Applied::Locked(e.t == "lock"),
            }),
            "reveal" => {
                let ids: Vec<String> =
                    serde_json::from_value(e.body.get("ids").cloned().unwrap_or_default()).unwrap_or_default();
                self.tracker.model.revealed.extend(ids.iter().cloned());
                self.publish();
                self.emit.send(Event::Applied {
                    seq,
                    msg: Applied::Reveal(ids),
                });
            }
            "end" => {
                let reason = e
                    .body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.emit.send(Event::Applied {
                    seq,
                    msg: Applied::End(reason),
                });
            }
            "pointer" => {
                let n = |k: &str| e.body.get(k).and_then(Value::as_f64);
                match (n("cell"), n("x"), n("y")) {
                    (Some(cell), Some(x), Some(y)) => self.emit.send(Event::Pointer {
                        cell: cell as u8,
                        x: x as f32,
                        y: y as f32,
                    }),
                    _ => self.emit.send(Event::PointerHidden),
                }
            }
            "drag" => {
                let n = |k: &str| e.body.get(k).and_then(Value::as_f64);
                if let (Some(id), Some(i), Some(x), Some(y)) =
                    (e.body.get("id").and_then(Value::as_str), n("i"), n("x"), n("y"))
                {
                    self.emit.send(Event::Drag {
                        id: id.to_string(),
                        i: i as u16,
                        x: x as f32,
                        y: y as f32,
                    });
                }
            }
            "ack" => {
                if let Some(cid) = &e.cid {
                    self.pending_changes.remove(cid);
                }
            }
            "error" => {
                let text = |k: &str| e.body.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                tracing::warn!(code = %text("code"), "relay reported an error");
                self.emit.send(Event::Error {
                    code: text("code"),
                    message: text("message"),
                });
                if e.cid.as_ref().is_some_and(|c| self.pending_changes.remove(c)) {
                    self.rollback();
                }
            }
            // Hints only: presence follows.
            _ => {}
        }
        Ok(None)
    }

    fn note_sid(&mut self, body: &Value) {
        if let Some(sid) = body.get("sid").and_then(Value::as_str) {
            *self.join = JoinBy::Sid(sid.to_string());
        }
    }

    fn presence(&mut self, body: &Value) {
        let p: Presence = serde_json::from_value(body.clone()).unwrap_or_default();
        let has = self.sub.is_some() && p.control == self.sub;
        if self.has_control && !has {
            self.rollback();
        }
        self.has_control = has;
        self.emit.send(Event::Presence(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_to_thirty_seconds() {
        let s = |n| retry_delay(n, Duration::from_secs(1), Duration::from_secs(30), 0.0).as_secs();
        assert_eq!([s(0), s(1), s(2), s(3), s(4), s(5), s(9)], [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn jitter_stays_within_a_quarter() {
        let d = retry_delay(3, Duration::from_secs(1), Duration::from_secs(30), 1.0);
        assert_eq!(d, Duration::from_secs(10));
        let d = retry_delay(3, Duration::from_secs(1), Duration::from_secs(30), -1.0);
        assert_eq!(d, Duration::from_secs(6));
    }
}
