//! ruthos-session: the client side of a guided teaching session.
//!
//! One handle, one command enum and one event enum. Every method returns at
//! once; the work happens on a dedicated thread running a Tokio runtime, and
//! results arrive as events. No token, socket or runtime type appears in this
//! interface, so the viewer cannot misuse a credential or block on a network
//! call by accident. The crate has no dependency on the user interface.
//!
//! Ruthos ships with no server configured. A server is reached through an
//! invitation, a profile, `ruthos.ini` or an address the user types.

pub mod auth;
pub mod cases;
pub mod deployment;
pub mod discovery;
pub mod http;
pub mod links;
pub mod profile;
pub mod proto;
pub mod sync;
pub mod ws;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

pub use auth::Account;
pub use discovery::ServerInfo;
pub use proto::{Change, Member, Presence, SharedModel, WelcomeAccount};

use auth::{Store, SIGN_IN_TIMEOUT};
use discovery::AuthKind;
use http::{new_key, Http};
use ws::{JoinBy, Out, Params, Timing};

#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionError {
    /// Not possible, for a reason the user should read.
    #[error("{0}")]
    Refused(String),
    #[error("The server could not be reached: {0}")]
    Network(String),
    #[error("{message}")]
    Api { status: u16, code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Idle,
    Connecting,
    Waiting { title: String, host_name: String },
    Live,
    Reconnecting { attempt: u32 },
    Closed { code: u16, reason: String },
}

/// A sequenced message that was applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Change,
    Locked(bool),
    Reveal(Vec<String>),
    End(String),
}

/// A teaching case and how far it has got on this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseStatus {
    pub entry: cases::CaseEntry,
    pub state: CaseState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaseState {
    Absent,
    Downloading,
    /// Verified, and unpacked into this folder for import.
    Downloaded(PathBuf),
    Failed(String),
}

pub enum Command {
    Discover,
    /// The browser loopback flow.
    SignIn,
    PasteToken(String),
    SignOut,
    Guest {
        code: String,
        name: String,
    },
    /// Host, for Rejoin.
    ListMySessions,
    Create {
        title: Option<String>,
        cases: Vec<String>,
        guests: bool,
    },
    SetRoster {
        sid: String,
        emails: Vec<String>,
    },
    Join {
        code: String,
    },
    Rejoin {
        sid: String,
    },
    Leave,
    End,
    Admit(String),
    Deny(String),
    Remove(String),
    PassControl(String),
    TakeControl,
    Lock,
    Unlock,
    Reveal(Vec<String>),
    Follow,
    FreeView,
    RaiseHand,
    LowerHand,
    /// Coalesced by element.
    Change(Change),
    /// `None` hides it.
    Pointer(Option<Pointer>),
    Drag {
        id: String,
        i: u16,
        x: f32,
        y: f32,
    },
    UploadCase {
        reference: String,
        folder: PathBuf,
    },
    /// List the session's cases; those in `have` are not downloaded.
    FetchCases {
        have: Vec<String>,
        into: PathBuf,
    },
    /// Check a profile against its issuer and emit `Review`.
    OpenProfile {
        text: String,
        pins: profile::Pins,
    },
    /// Fetch a profile over HTTPS, then as `OpenProfile`.
    FetchProfile {
        url: String,
        pins: profile::Pins,
    },
    /// Ask the server to sign these settings; the result is written to `to`.
    CreateProfile {
        profile: Value,
        to: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub cell: u8,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub enum Event {
    ServerInfo(ServerInfo),
    /// Who is signed in; `None` after signing out.
    Account(Option<Account>),
    /// The relay's view of this connection.
    Welcome(WelcomeAccount),
    ExpiresSoon {
        minutes: u32,
    },
    MySessions(Vec<http::SessionSummary>),
    Created {
        sid: String,
        code: String,
    },
    Connection(ConnectionState),
    /// After admission, a gap, or a rollback.
    State(Arc<SharedModel>),
    Applied {
        seq: u64,
        msg: Applied,
    },
    Presence(Presence),
    Pointer {
        cell: u8,
        x: f32,
        y: f32,
    },
    PointerHidden,
    Drag {
        id: String,
        i: u16,
        x: f32,
        y: f32,
    },
    Cases(Vec<CaseStatus>),
    Review(Box<profile::Checked>),
    ProfileCreated(PathBuf),
    Error {
        code: String,
        message: String,
    },
}

/// Queues events and wakes the interface, so an idle viewer repaints only
/// when something arrives.
#[derive(Clone)]
pub struct Emitter {
    tx: Sender<Event>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl Emitter {
    pub fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        (self.waker)();
    }

    fn error(&self, e: SessionError) {
        let code = match &e {
            SessionError::Api { code, .. } => code.clone(),
            SessionError::Network(_) => "network".to_string(),
            SessionError::Refused(_) => "refused".to_string(),
        };
        self.send(Event::Error {
            code,
            message: e.to_string(),
        });
    }
}

/// Everything that can vary between the application and its tests.
#[derive(Clone)]
pub struct Options {
    pub store: Store,
    pub proxy: Option<String>,
    pub timing: Timing,
    pub sign_in_timeout: Duration,
    /// Opens the sign-in page; the system browser in use.
    pub browser: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            store: Arc::new(auth::Keychain),
            proxy: None,
            timing: Timing::default(),
            sign_in_timeout: SIGN_IN_TIMEOUT,
            browser: Arc::new(|url| open::that_detached(url).is_ok()),
        }
    }
}

pub struct SessionClient {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    shared: Arc<Mutex<Option<Arc<SharedModel>>>>,
}

impl SessionClient {
    pub fn new(server: &str, waker: impl Fn() + Send + Sync + 'static) -> Result<Self, SessionError> {
        Self::with_options(server, waker, Options::default())
    }

    pub fn with_options(
        server: &str,
        waker: impl Fn() + Send + Sync + 'static,
        options: Options,
    ) -> Result<Self, SessionError> {
        let server = discovery::normalise(server)?;
        let http = Http::new(&server, options.proxy.as_deref())?;
        let (commands, rx) = unbounded_channel();
        let (tx, events) = crossbeam_channel::unbounded();
        let shared = Arc::new(Mutex::new(None));
        let emit = Emitter {
            tx,
            waker: Arc::new(waker),
        };
        let actor = Actor {
            token: Arc::new(Mutex::new(options.store.load(&server))),
            guest: false,
            server,
            http,
            options,
            emit,
            info: Arc::new(Mutex::new(None)),
            shared: shared.clone(),
            connection: None,
            sid: Arc::new(Mutex::new(None)),
        };
        std::thread::Builder::new()
            .name("ruthos-session".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the session runtime");
                runtime.block_on(actor.run(rx));
            })
            .map_err(|e| SessionError::Network(e.to_string()))?;
        Ok(Self {
            commands,
            events,
            shared,
        })
    }

    /// Never blocks.
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// The confirmed shared state.
    pub fn model(&self) -> Option<Arc<SharedModel>> {
        self.shared.lock().clone()
    }
}

struct Actor {
    server: String,
    http: Http,
    options: Options,
    emit: Emitter,
    token: Arc<Mutex<Option<String>>>,
    guest: bool,
    info: Arc<Mutex<Option<ServerInfo>>>,
    shared: Arc<Mutex<Option<Arc<SharedModel>>>>,
    connection: Option<UnboundedSender<Out>>,
    sid: Arc<Mutex<Option<String>>>,
}

impl Actor {
    async fn run(mut self, mut rx: UnboundedReceiver<Command>) {
        self.announce_account();
        while let Some(cmd) = rx.recv().await {
            self.command(cmd);
        }
    }

    fn announce_account(&self) {
        let account = self.token.lock().as_deref().and_then(|t| auth::decode(t).ok());
        self.emit.send(Event::Account(account));
    }

    fn token(&self) -> Result<String, SessionError> {
        self.token
            .lock()
            .clone()
            .ok_or_else(|| SessionError::Refused("Sign in first".to_string()))
    }

    fn out(&self, o: Out) {
        if let Some(c) = &self.connection {
            let _ = c.send(o);
        }
    }

    fn control(&self, t: &'static str, body: Value) {
        self.out(Out::Control(t, body));
    }

    fn connect(&mut self, join: JoinBy) {
        let info = self.info.lock().clone();
        let Some(info) = info else {
            self.emit.error(SessionError::Refused(
                "The server has not been contacted yet".to_string(),
            ));
            return;
        };
        let url = match info.ws_url(&self.server) {
            Ok(u) => u,
            Err(e) => return self.emit.error(e),
        };
        if let Some(old) = self.connection.take() {
            let _ = old.send(Out::Leave);
        }
        *self.shared.lock() = None;
        if let JoinBy::Sid(s) = &join {
            *self.sid.lock() = Some(s.clone());
        }
        let (tx, rx) = unbounded_channel();
        self.connection = Some(tx);
        let params = Params {
            url,
            token: self.token.clone(),
            join,
            proxy: self.options.proxy.clone(),
            timing: self.options.timing.clone(),
            shared: self.shared.clone(),
        };
        tokio::spawn(ws::run(params, rx, self.emit.clone()));
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Discover => self.discover(),
            Command::SignIn => self.sign_in(),
            Command::PasteToken(t) => self.take_token(t.trim()),
            Command::SignOut => {
                self.options.store.delete(&self.server);
                *self.token.lock() = None;
                self.guest = false;
                self.out(Out::Leave);
                self.connection = None;
                self.announce_account();
            }
            Command::Guest { code, name } => self.guest(code, name),
            Command::ListMySessions => self.list_sessions(),
            Command::Create { title, cases, guests } => self.create(title, cases, guests),
            Command::SetRoster { sid, emails } => self.set_roster(sid, emails),
            Command::Join { code } => self.connect(JoinBy::Code(code)),
            Command::Rejoin { sid } => self.connect(JoinBy::Sid(sid)),
            Command::Leave => {
                self.out(Out::Leave);
                self.connection = None;
            }
            Command::End => self.control("end", json!({})),
            Command::Admit(a) => self.control("admit", json!({"acct": a})),
            Command::Deny(a) => self.control("deny", json!({"acct": a})),
            Command::Remove(a) => self.control("remove", json!({"acct": a})),
            Command::PassControl(a) => self.control("pass_control", json!({"acct": a})),
            Command::TakeControl => self.control("take_control", json!({})),
            Command::Lock => self.control("lock", json!({})),
            Command::Unlock => self.control("unlock", json!({})),
            Command::Reveal(ids) => self.control("reveal", json!({"ids": ids})),
            Command::Follow => self.control("follow", json!({})),
            Command::FreeView => self.control("free_view", json!({})),
            Command::RaiseHand => self.control("request_control", json!({})),
            Command::LowerHand => self.control("lower_hand", json!({})),
            Command::Change(c) => self.out(Out::Change(c)),
            Command::Pointer(p) => self.out(Out::Pointer(match p {
                Some(p) => json!({"x": p.x, "y": p.y, "cell": p.cell}),
                None => json!({}),
            })),
            Command::Drag { id, i, x, y } => self.out(Out::Drag { id, i, x, y }),
            Command::UploadCase { reference, folder } => self.upload_case(reference, folder),
            Command::FetchCases { have, into } => self.fetch_cases(have, into),
            Command::OpenProfile { text, pins } => self.open_profile(text, pins),
            Command::FetchProfile { url, pins } => self.fetch_profile(url, pins),
            Command::CreateProfile { profile, to } => self.create_profile(profile, to),
        }
    }

    fn discover(&self) {
        let (http, info, emit) = (self.http.clone(), self.info.clone(), self.emit.clone());
        tokio::spawn(async move {
            match http.info().await.and_then(|i| i.supported().map(|_| i)) {
                Ok(i) => {
                    *info.lock() = Some(i.clone());
                    emit.send(Event::ServerInfo(i));
                }
                Err(e) => emit.error(e),
            }
        });
    }

    fn take_token(&mut self, token: &str) {
        match auth::decode(token) {
            Ok(_) => {
                if let Err(e) = self.options.store.save(&self.server, token) {
                    self.emit.error(e);
                }
                self.guest = false;
                *self.token.lock() = Some(token.to_string());
                self.announce_account();
                self.out(Out::Reauth);
            }
            Err(e) => self.emit.error(e),
        }
    }

    fn sign_in(&self) {
        let info = self.info.lock().clone();
        let Some(info) = info else {
            return self.emit.error(SessionError::Refused(
                "The server has not been contacted yet".to_string(),
            ));
        };
        let login_url = match info.auth_kind() {
            AuthKind::Token { login_url } => login_url,
            AuthKind::Oidc => {
                return self.emit.error(SessionError::Refused(
                    "This server's sign-in type (OpenID Connect) is not supported yet".to_string(),
                ))
            }
            AuthKind::Unsupported(t) => {
                return self.emit.error(SessionError::Refused(format!(
                    "This server's sign-in type ({t}) is not supported"
                )))
            }
        };
        let (server, options, emit, token) = (
            self.server.clone(),
            self.options.clone(),
            self.emit.clone(),
            self.token.clone(),
        );
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                Ok(l) => l,
                Err(e) => return emit.error(SessionError::Network(e.to_string())),
            };
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            let state = auth::new_state();
            let address = match auth::login_address(&login_url, port, &state) {
                Ok(a) => a,
                Err(e) => return emit.error(e),
            };
            if !(options.browser)(&address) {
                return emit.error(SessionError::Refused(
                    "The browser could not be opened. Use Paste token instead".to_string(),
                ));
            }
            match auth::await_callback(listener, state, options.sign_in_timeout).await {
                Ok(t) => match auth::decode(&t) {
                    Ok(account) => {
                        if let Err(e) = options.store.save(&server, &t) {
                            emit.error(e);
                        }
                        *token.lock() = Some(t);
                        emit.send(Event::Account(Some(account)));
                        if let Some(c) = connection {
                            let _ = c.send(Out::Reauth);
                        }
                    }
                    Err(e) => emit.error(e),
                },
                Err(e) => emit.error(e),
            }
        });
    }

    /// A guest token works only for this session, so it stays in memory.
    fn guest(&mut self, code: String, name: String) {
        self.guest = true;
        let (http, emit, token) = (self.http.clone(), self.emit.clone(), self.token.clone());
        let (info, shared, proxy, timing, server) = (
            self.info.clone(),
            self.shared.clone(),
            self.options.proxy.clone(),
            self.options.timing.clone(),
            self.server.clone(),
        );
        let (tx, rx) = unbounded_channel();
        self.connection = Some(tx);
        let key = new_key();
        tokio::spawn(async move {
            let entry = match http.guest(&code, &name, &key).await {
                Ok(e) => e,
                Err(e) => return emit.error(e),
            };
            let Some(url) = info.lock().as_ref().and_then(|i| i.ws_url(&server).ok()) else {
                return;
            };
            *token.lock() = Some(entry.token);
            let params = Params {
                url,
                token,
                join: JoinBy::Code(code),
                proxy,
                timing,
                shared,
            };
            ws::run(params, rx, emit).await;
        });
    }

    fn list_sessions(&self) {
        let token = match self.token() {
            Ok(t) => t,
            Err(_) => return,
        };
        let (http, emit) = (self.http.clone(), self.emit.clone());
        tokio::spawn(async move {
            match http.my_sessions(&token).await {
                Ok(s) => emit.send(Event::MySessions(s)),
                Err(e) => emit.error(e),
            }
        });
    }

    fn create(&mut self, title: Option<String>, cases: Vec<String>, guests: bool) {
        let token = match self.token() {
            Ok(t) => t,
            Err(e) => return self.emit.error(e),
        };
        let (http, emit) = (self.http.clone(), self.emit.clone());
        let key = new_key();
        tokio::spawn(async move {
            match http
                .create_session(&token, title.as_deref(), &cases, guests, &key)
                .await
            {
                Ok(s) => emit.send(Event::Created {
                    sid: s.id,
                    code: s.code,
                }),
                Err(e) => emit.error(e),
            }
        });
    }

    fn set_roster(&self, sid: String, emails: Vec<String>) {
        let token = match self.token() {
            Ok(t) => t,
            Err(e) => return self.emit.error(e),
        };
        let emails: Vec<String> = emails
            .iter()
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        let (http, emit) = (self.http.clone(), self.emit.clone());
        tokio::spawn(async move {
            if let Err(e) = http.set_roster(&token, &sid, &emails).await {
                emit.error(e);
            }
        });
    }

    fn upload_case(&self, reference: String, folder: PathBuf) {
        let token = match self.token() {
            Ok(t) => t,
            Err(e) => return self.emit.error(e),
        };
        let (http, emit) = (self.http.clone(), self.emit.clone());
        tokio::spawn(async move {
            let done = match tokio::task::spawn_blocking(move || cases::package(&folder)).await {
                Ok(Ok(bytes)) => cases::upload(&http, &token, &reference, bytes).await,
                Ok(Err(e)) => Err(e),
                Err(e) => Err(SessionError::Refused(e.to_string())),
            };
            if let Err(e) = done {
                emit.error(e);
            }
        });
    }

    fn fetch_cases(&self, have: Vec<String>, into: PathBuf) {
        let (Ok(token), Some(sid)) = (self.token(), self.sid.lock().clone()) else {
            return;
        };
        let (http, emit) = (self.http.clone(), self.emit.clone());
        tokio::spawn(async move {
            let list = match cases::list(&http, &token, &sid).await {
                Ok(l) => l,
                Err(e) => return emit.error(e),
            };
            let mut status: Vec<CaseStatus> = list
                .into_iter()
                .map(|entry| {
                    let state = if have.contains(&entry.reference) {
                        CaseState::Downloaded(PathBuf::new())
                    } else {
                        CaseState::Absent
                    };
                    CaseStatus { entry, state }
                })
                .collect();
            emit.send(Event::Cases(status.clone()));
            for i in 0..status.len() {
                if status[i].state != CaseState::Absent {
                    continue;
                }
                status[i].state = CaseState::Downloading;
                emit.send(Event::Cases(status.clone()));
                let entry = status[i].entry.clone();
                let dest = into.join(&entry.reference);
                let url = entry.download_url.clone().unwrap_or_default();
                let got = match http.get_bytes(&url).await {
                    Ok(bytes) => cases::verify(&bytes, &entry).and_then(|_| cases::unpack(&bytes, &dest)),
                    Err(e) => Err(e),
                };
                status[i].state = match got {
                    Ok(p) => CaseState::Downloaded(p),
                    Err(e) => CaseState::Failed(e.to_string()),
                };
                emit.send(Event::Cases(status.clone()));
            }
        });
    }

    fn fetch_profile(&self, url: String, pins: profile::Pins) {
        let (http, emit, proxy) = (self.http.clone(), self.emit.clone(), self.options.proxy.clone());
        tokio::spawn(async move {
            match http.get_bytes(&url).await {
                Ok(b) if b.len() <= profile::MAX_PROFILE => {
                    check_profile(String::from_utf8_lossy(&b).into_owned(), pins, proxy, emit).await
                }
                Ok(_) => emit.error(SessionError::Refused("That profile is too large".to_string())),
                Err(e) => emit.error(e),
            }
        });
    }

    fn open_profile(&self, text: String, pins: profile::Pins) {
        tokio::spawn(check_profile(text, pins, self.options.proxy.clone(), self.emit.clone()));
    }
    fn create_profile(&self, profile: Value, to: PathBuf) {
        let token = match self.token() {
            Ok(t) => t,
            Err(e) => return self.emit.error(e),
        };
        let (http, emit) = (self.http.clone(), self.emit.clone());
        let key = new_key();
        tokio::spawn(async move {
            let signed = http.post_json(&token, "/v1/profiles/sign", &profile, &key).await;
            match signed {
                Ok(v) => match std::fs::write(&to, serde_json::to_vec_pretty(&v).unwrap_or_default()) {
                    Ok(()) => emit.send(Event::ProfileCreated(to)),
                    Err(e) => emit.error(SessionError::Refused(e.to_string())),
                },
                Err(e) => emit.error(e),
            }
        });
    }
}

/// When a token expires, for those who hold one.
pub fn expiry(token: &str) -> Option<DateTime<Utc>> {
    auth::decode(token).ok().map(|a| a.expires)
}

/// The signing keys come from the server the profile names, not from this
/// client's server, or a forwarded file could name anyone.
async fn check_profile(text: String, pins: profile::Pins, proxy: Option<String>, emit: Emitter) {
    let issuer = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.pointer("/issuer/server")?.as_str().map(str::to_string));
    let mut keys = Default::default();
    if let Some(server) = issuer.as_deref().and_then(|s| discovery::normalise(s).ok()) {
        if let Ok(info) = async { Http::new(&server, proxy.as_deref())?.info().await }.await {
            keys = info.provisioning_keys;
        }
    }
    match profile::check(&text, Utc::now(), &keys, &pins) {
        Ok(c) => emit.send(Event::Review(Box::new(c))),
        Err(e) => emit.error(e),
    }
}
