//! Guided review: the thin layer between `ruthos-session` and the windows.
//!
//! `Hub` holds what the session looks like to the interface and drains the
//! client's events at the start of each frame; the viewer's side of the
//! shared view is in `session_view.rs`. Below are the session panel, the
//! review dialog every invitation and profile passes through, and the
//! Guided Review tab of Preferences.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use egui::{Color32, RichText, Ui, ViewportBuilder, ViewportCommand, ViewportId};
use serde_json::{json, Value};

use ruthos_core::settings::{AppliedProfile, SessionPin};
use ruthos_session::deployment::Deployment;
use ruthos_session::discovery::{domain, normalise};
use ruthos_session::http::SessionSummary;
use ruthos_session::links::{self, valid_code, Link};
use ruthos_session::profile::{Checked, Pins, Section, Verification};
use ruthos_session::{
    Account, Applied, Command, ConnectionState, Event, Member, Options, Pointer, Presence, ServerInfo, SessionClient,
    SharedModel,
};

use crate::shared::Shared;
use crate::theme;

/// Below this, sign in again before starting anything.
const FRESH_MINUTES: i64 = 30;
/// How long a follower takes to glide to the next pointer position.
pub const GLIDE: Duration = Duration::from_millis(50);

/// What this client is doing in the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    None,
    Controller,
    Follower,
    FreeView,
}

/// The pointer, moved smoothly between updates that may not all arrive.
#[derive(Debug, Clone, Copy)]
pub struct Glide {
    pub cell: u8,
    from: [f32; 2],
    to: [f32; 2],
    at: Instant,
}

impl Glide {
    pub fn position(&self) -> [f32; 2] {
        let t = (self.at.elapsed().as_secs_f32() / GLIDE.as_secs_f32()).clamp(0.0, 1.0);
        [
            self.from[0] + (self.to[0] - self.from[0]) * t,
            self.from[1] + (self.to[1] - self.from[1]) * t,
        ]
    }

    pub fn moving(&self) -> bool {
        self.at.elapsed() < GLIDE
    }
}

/// What is waiting in the review dialog.
pub enum Review {
    Invitation {
        server: String,
        code: String,
    },
    Profile {
        checked: Box<Checked>,
        ticked: Vec<Section>,
    },
}

pub struct Hub {
    pub open: bool,
    pub client: Option<SessionClient>,
    pub server: Option<String>,
    pub deployment: Deployment,
    pub info: Option<ServerInfo>,
    pub account: Option<Account>,
    pub sub: Option<String>,
    pub connection: ConnectionState,
    pub presence: Presence,
    pub model: Option<Arc<SharedModel>>,
    /// Bumped when the shared view must be rebuilt: a new state or a rollback.
    pub rebuild: u64,
    /// In free view, with the shared view no longer driving the screen.
    pub free_view: bool,
    pub hand_raised: bool,
    pub sid: Option<String>,
    pub code: Option<String>,
    pub title: String,
    pub pointer: Option<Glide>,
    pub drags: std::collections::HashMap<(String, u16), [f32; 2]>,
    pub expires_in: Option<u32>,
    pub message: Option<String>,
    pub my_sessions: Vec<SessionSummary>,
    pub review: Option<Review>,
    /// A join waiting for discovery and sign-in.
    pending_join: Option<String>,
    /// The session's viewer, by viewer id.
    pub viewer: Option<u64>,
    /// A follower's study this machine does not hold.
    pub missing: Option<String>,
    // The panel's fields.
    new_title: String,
    allow_guests: bool,
    join_code: String,
    guest_name: String,
    roster: String,
    paste_token: String,
    show_paste: bool,
    manual_address: String,
    show_manual: bool,
}

fn set_message(hub: &mut Hub, text: impl Into<String>) {
    hub.message = Some(text.into());
}

impl Hub {
    pub fn new(settings: &ruthos_core::Settings) -> Self {
        let deployment = Deployment::load();
        let server = settings.session_server.clone().or_else(|| deployment.server.clone());
        let server = match (&server, &deployment.server) {
            // A locked site's server wins, and a saved one it forbids is dropped.
            (_, Some(s)) if deployment.locked => Some(s.clone()),
            (Some(s), _) if !deployment.permits(s) => None,
            _ => server,
        };
        Self {
            open: false,
            client: None,
            server,
            deployment,
            info: None,
            account: None,
            sub: None,
            connection: ConnectionState::Idle,
            presence: Presence::default(),
            model: None,
            rebuild: 0,
            free_view: false,
            hand_raised: false,
            sid: None,
            code: None,
            title: String::new(),
            pointer: None,
            drags: Default::default(),
            expires_in: None,
            message: None,
            my_sessions: Vec::new(),
            review: None,
            pending_join: None,
            viewer: None,
            missing: None,
            new_title: String::new(),
            allow_guests: false,
            join_code: String::new(),
            guest_name: String::new(),
            roster: String::new(),
            paste_token: String::new(),
            show_paste: false,
            manual_address: String::new(),
            show_manual: false,
        }
    }

    pub fn send(&self, cmd: Command) {
        if let Some(c) = &self.client {
            c.send(cmd);
        }
    }

    pub fn live(&self) -> bool {
        self.connection == ConnectionState::Live
    }

    pub fn has_control(&self) -> bool {
        self.sub.is_some() && self.presence.control == self.sub
    }

    pub fn role(&self) -> Role {
        if !self.live() {
            Role::None
        } else if self.has_control() {
            Role::Controller
        } else if self.free_view {
            Role::FreeView
        } else {
            Role::Follower
        }
    }

    fn is_host(&self) -> bool {
        self.account.as_ref().is_some_and(Account::is_host)
    }

    /// True while a connection exists or is being made.
    fn in_session(&self) -> bool {
        !matches!(self.connection, ConnectionState::Idle | ConnectionState::Closed { .. })
    }

    /// Start the client for the current server, once.
    fn ensure_client(&mut self, ctx: &egui::Context) {
        if self.client.is_some() {
            return;
        }
        let Some(server) = self.server.clone() else { return };
        let ctx = ctx.clone();
        let options = Options {
            proxy: self.deployment.proxy.clone(),
            ..Default::default()
        };
        match SessionClient::with_options(&server, move || ctx.request_repaint(), options) {
            Ok(c) => {
                c.send(Command::Discover);
                self.client = Some(c);
            }
            Err(e) => set_message(self, e.to_string()),
        }
    }

    /// Change server; a running session stays on the one it started with.
    fn use_server(&mut self, server: String, ctx: &egui::Context) -> bool {
        if !self.deployment.permits(&server) {
            set_message(self, format!("{} is not permitted on this machine", domain(&server)));
            return false;
        }
        if self.server.as_deref() == Some(server.as_str()) {
            self.ensure_client(ctx);
            return true;
        }
        if self.in_session() {
            set_message(self, "Leave the current session first");
            return false;
        }
        self.client = None;
        self.info = None;
        self.account = None;
        self.server = Some(server);
        self.ensure_client(ctx);
        true
    }

    fn reset_session(&mut self) {
        self.presence = Presence::default();
        self.model = None;
        self.free_view = false;
        self.hand_raised = false;
        self.pointer = None;
        self.drags.clear();
        self.expires_in = None;
        self.missing = None;
        self.viewer = None;
    }

    /// Drain the client's events; called once a frame by the app.
    pub fn pump(&mut self, ctx: &egui::Context) {
        if self.open {
            self.ensure_client(ctx);
        }
        let events: Vec<Event> = match &self.client {
            Some(c) => c.events().try_iter().collect(),
            None => Vec::new(),
        };
        for e in events {
            self.event(e);
        }
        if let Some(c) = &self.client {
            self.model = c.model();
        }
        self.proceed_with_join();
        if self.pointer.is_some_and(|g| g.moving()) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn proceed_with_join(&mut self) {
        let Some(code) = self.pending_join.clone() else { return };
        if self.info.is_none() || self.client.is_none() {
            return;
        }
        match &self.account {
            Some(a) if a.minutes_left(Utc::now()) >= FRESH_MINUTES => {
                self.pending_join = None;
                self.send(Command::Join { code });
            }
            // Sign-in is under way, or the user has to ask for it.
            _ => {}
        }
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::ServerInfo(i) => {
                let needs_sign_in = self.pending_join.is_some() && self.account.is_none();
                self.info = Some(i);
                if needs_sign_in {
                    self.send(Command::SignIn);
                }
            }
            Event::Account(a) => {
                self.expires_in = None;
                let has_host_role = a.as_ref().is_some_and(Account::is_host);
                self.account = a;
                if has_host_role && !self.in_session() {
                    self.send(Command::ListMySessions);
                }
            }
            Event::Welcome(w) => self.sub = Some(w.sub),
            Event::ExpiresSoon { minutes } => self.expires_in = Some(minutes),
            Event::MySessions(s) => self.my_sessions = s,
            Event::Created { sid, code } => {
                self.code = Some(code);
                self.sid = Some(sid.clone());
                self.send(Command::Rejoin { sid });
            }
            Event::Connection(c) => {
                match &c {
                    ConnectionState::Waiting { title, .. } => self.title = title.clone(),
                    ConnectionState::Closed { code, .. } if *code == 4003 => self.account = None,
                    _ => {}
                }
                if matches!(c, ConnectionState::Idle | ConnectionState::Closed { .. }) {
                    self.reset_session();
                    if self.is_host() {
                        self.send(Command::ListMySessions);
                    }
                }
                self.connection = c;
            }
            Event::State(m) => {
                self.model = Some(m);
                self.rebuild += 1;
            }
            Event::Applied { msg, .. } => match msg {
                Applied::Locked(true) => {
                    self.presence.locked = true;
                    self.back_to_shared();
                }
                Applied::Locked(false) => self.presence.locked = false,
                Applied::End(_) | Applied::Change | Applied::Reveal(_) => {}
            },
            Event::Presence(p) => {
                self.presence = p;
                if self.presence.locked {
                    self.back_to_shared();
                }
                if self.has_control() {
                    self.free_view = false;
                }
                self.hand_raised = self
                    .presence
                    .members
                    .iter()
                    .any(|m| Some(&m.sub) == self.sub.as_ref() && m.hand);
            }
            Event::Pointer { cell, x, y } => {
                let from = self
                    .pointer
                    .filter(|g| g.cell == cell)
                    .map(|g| g.position())
                    .unwrap_or([x, y]);
                self.pointer = Some(Glide {
                    cell,
                    from,
                    to: [x, y],
                    at: Instant::now(),
                });
            }
            Event::PointerHidden => self.pointer = None,
            Event::Drag { id, i, x, y } => {
                self.drags.insert((id, i), [x, y]);
            }
            Event::Cases(_) => {}
            Event::Review(c) => {
                let ticked = c.sections.clone();
                self.review = Some(Review::Profile { checked: c, ticked });
                self.open = true;
            }
            Event::ProfileCreated(p) => set_message(self, format!("Profile saved as {}", p.display())),
            Event::Error { message, .. } => set_message(self, message),
        }
    }

    /// Back to the shared view, which a lock also forces.
    fn back_to_shared(&mut self) {
        if self.free_view {
            self.free_view = false;
            self.rebuild += 1;
        }
    }

    /// A `ruthos:` link, whichever way it arrived.
    pub fn open_link(&mut self, link: &str, shared: &Shared, ctx: &egui::Context) {
        self.open = true;
        match links::parse(link) {
            Ok(Link::Join { server, code }) => {
                if !self.deployment.permits(&server) {
                    set_message(self, format!("{} is not permitted on this machine", domain(&server)));
                    return;
                }
                self.review = Some(Review::Invitation { server, code });
            }
            Ok(Link::Profile { url }) => {
                self.ensure_client(ctx);
                if self.client.is_none() {
                    self.ensure_probe(ctx);
                }
                let pins = self.pins(shared);
                self.send(Command::FetchProfile { url, pins });
            }
            Err(e) => set_message(self, e.to_string()),
        }
    }

    /// A `.ruthosprofile` file.
    pub fn open_profile(&mut self, path: &Path, shared: &Shared, ctx: &egui::Context) {
        self.open = true;
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => return set_message(self, format!("The profile could not be read: {e}")),
        };
        let pins = self.pins(shared);
        // Any client will do for reading a profile: the keys come from the
        // server the profile names.
        self.ensure_client(ctx);
        if self.client.is_none() {
            self.ensure_probe(ctx);
        }
        self.send(Command::OpenProfile { text, pins });
    }

    fn pins(&self, shared: &Shared) -> Pins {
        shared
            .settings
            .lock()
            .session_pins
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ruthos_session::profile::PinnedKey {
                        kid: v.kid.clone(),
                        key: v.key.clone(),
                    },
                )
            })
            .collect()
    }

    /// With no server set, a client that talks to nobody but can still read
    /// a profile from the server it names.
    fn ensure_probe(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let options = Options {
            proxy: self.deployment.proxy.clone(),
            ..Default::default()
        };
        match SessionClient::with_options("http://localhost", move || ctx.request_repaint(), options) {
            Ok(c) => self.client = Some(c),
            Err(e) => set_message(self, e.to_string()),
        }
    }

    /// Join `code` on `server`, signing in first if need be.
    fn accept_invitation(&mut self, server: String, code: String, shared: &Shared, ctx: &egui::Context) {
        if !self.use_server(server.clone(), ctx) {
            return;
        }
        save_server(shared, &server);
        self.pending_join = Some(code);
        let stale = self
            .account
            .as_ref()
            .is_none_or(|a| a.minutes_left(Utc::now()) < FRESH_MINUTES);
        if stale && self.info.is_some() {
            self.send(Command::SignIn);
        }
    }

    fn sign_in_or_paste(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("Sign in").clicked() {
                self.send(Command::SignIn);
            }
            if ui.link("Paste token").clicked() {
                self.show_paste = !self.show_paste;
            }
        });
        if self.show_paste {
            ui.label(
                RichText::new("Open the sign-in page without the port, copy the token it shows, and paste it here.")
                    .small(),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.paste_token)
                        .password(true)
                        .desired_width(280.0),
                );
                if ui.button("Use token").clicked() && !self.paste_token.is_empty() {
                    let t = std::mem::take(&mut self.paste_token);
                    self.send(Command::PasteToken(t));
                    self.show_paste = false;
                }
            });
        }
    }

    fn account_line(&mut self, ui: &mut Ui) {
        match self.account.clone() {
            Some(a) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&a.name).strong());
                    if a.is_host() {
                        ui.label(RichText::new("Host").small().color(theme::ROI_SELECTED));
                    }
                    let local = a.expires.with_timezone(&chrono::Local);
                    ui.label(RichText::new(format!("until {}", local.format("%H:%M"))).small());
                    if ui.small_button("Sign out").clicked() {
                        self.send(Command::SignOut);
                    }
                });
            }
            None => self.sign_in_or_paste(ui),
        }
    }

    fn server_line(&self, ui: &mut Ui) {
        match (&self.server, &self.info) {
            (Some(s), Some(i)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(domain(s)).size(16.0).strong());
                    ui.label(RichText::new(&i.name).small());
                    if self.deployment.locked {
                        ui.label(crate::icons::LOCKED)
                            .on_hover_text("Set by your administrator");
                    }
                });
                if let Some(url) = &i.privacy_url {
                    ui.hyperlink_to(RichText::new("Privacy notice").small(), url);
                }
            }
            (Some(s), None) => {
                ui.label(RichText::new(domain(s)).size(16.0).strong());
                ui.label(RichText::new("Contacting the server\u{2026}").small());
            }
            _ => {}
        }
    }

    /// The Guided review window's contents.
    pub fn panel(&mut self, ui: &mut Ui, shared: &Shared, ctx: &egui::Context) {
        ui.style_mut().visuals = theme::browser_visuals();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(msg) = self.message.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(msg).color(Color32::from_rgb(240, 190, 90)));
                    if ui.small_button("Dismiss").clicked() {
                        self.message = None;
                    }
                });
                ui.separator();
            }
            if self.review.is_some() {
                self.review_dialog(ui, shared, ctx);
                return;
            }
            if self.server.is_none() {
                ui.label("No session server is set.");
                ui.label(RichText::new("Click an invitation link from your educator, or open a profile.").small());
                if ui.button("Preferences\u{2026}").clicked() {
                    *shared.prefs_section.lock() = Some("guided");
                    shared.open_prefs.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                return;
            }
            self.server_line(ui);
            ui.separator();
            match self.connection.clone() {
                ConnectionState::Idle => self.idle(ui),
                ConnectionState::Closed { reason, .. } => {
                    ui.label(RichText::new(reason).strong());
                    ui.add_space(6.0);
                    self.idle(ui);
                }
                ConnectionState::Connecting => {
                    ui.label("Connecting\u{2026}");
                }
                ConnectionState::Waiting { title, host_name } => {
                    ui.label(
                        RichText::new(if title.is_empty() { "Session".to_string() } else { title })
                            .size(16.0)
                            .strong(),
                    );
                    ui.label(format!("Waiting for {host_name} to admit you\u{2026}"));
                    if ui.button("Leave").clicked() {
                        self.send(Command::Leave);
                    }
                }
                ConnectionState::Reconnecting { attempt } => {
                    ui.label(format!("Reconnecting (attempt {attempt})\u{2026}"));
                    if ui.button("Leave").clicked() {
                        self.send(Command::Leave);
                    }
                }
                ConnectionState::Live => self.live_panel(ui),
            }
        });
    }

    fn idle(&mut self, ui: &mut Ui) {
        self.account_line(ui);
        ui.add_space(6.0);
        if self.account.is_some() {
            ui.horizontal(|ui| {
                ui.label("Session code");
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .desired_width(80.0)
                        .char_limit(6),
                );
                if ui
                    .add_enabled(valid_code(&self.join_code), egui::Button::new("Join"))
                    .clicked()
                {
                    self.pending_join = Some(self.join_code.clone());
                }
            });
        }
        if self.info.as_ref().is_some_and(|i| i.guests) && self.account.is_none() {
            ui.horizontal(|ui| {
                ui.label("Code");
                ui.add(
                    egui::TextEdit::singleline(&mut self.join_code)
                        .desired_width(80.0)
                        .char_limit(6),
                );
                ui.label("Your name");
                ui.add(egui::TextEdit::singleline(&mut self.guest_name).desired_width(120.0));
                let ok = valid_code(&self.join_code) && !self.guest_name.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Join as guest")).clicked() {
                    self.send(Command::Guest {
                        code: self.join_code.clone(),
                        name: self.guest_name.trim().to_string(),
                    });
                }
            });
        }
        if self.is_host() {
            ui.separator();
            ui.label(RichText::new("New session").strong());
            ui.horizontal(|ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut self.new_title).desired_width(220.0));
            });
            if self.info.as_ref().is_some_and(|i| i.guests) {
                ui.checkbox(&mut self.allow_guests, "Allow guests");
            }
            if let Some(n) = self.info.as_ref().and_then(|i| i.max_members) {
                ui.label(RichText::new(format!("Up to {n} members")).small());
            }
            if ui.button("Start session").clicked() {
                let title = Some(self.new_title.trim().to_string()).filter(|t| !t.is_empty());
                self.title = title.clone().unwrap_or_default();
                self.send(Command::Create {
                    title,
                    cases: Vec::new(),
                    guests: self.allow_guests,
                });
            }
            if !self.my_sessions.is_empty() {
                ui.separator();
                ui.label(RichText::new("Rejoin").strong());
                for s in self.my_sessions.clone() {
                    let name = s.title.clone().unwrap_or_else(|| s.code.clone());
                    if ui.button(format!("{name} ({})", s.code)).clicked() {
                        self.code = Some(s.code);
                        self.title = s.title.unwrap_or_default();
                        self.sid = Some(s.id.clone());
                        self.send(Command::Rejoin { sid: s.id });
                    }
                }
            }
        }
    }

    fn member_row(&self, ui: &mut Ui, m: &Member, host: bool) {
        let online = self.presence.control.as_ref() == Some(&m.sub);
        ui.horizontal(|ui| {
            ui.label(if m.name.is_empty() { &m.sub } else { &m.name });
            if online {
                ui.label(RichText::new("control").small().color(theme::ROI_SELECTED));
            }
            if !m.online {
                ui.label(RichText::new("offline").small());
            } else if !m.following {
                ui.label(RichText::new("looking around").small());
            }
            if m.hand {
                ui.label(RichText::new("hand up").small().color(Color32::from_rgb(240, 190, 90)));
            }
            if host && Some(&m.sub) != self.sub.as_ref() {
                // The relay refuses anyone who is not admitted and online.
                if ui
                    .add_enabled(m.online, egui::Button::new("Pass control").small())
                    .clicked()
                {
                    self.send(Command::PassControl(m.sub.clone()));
                }
                if ui.small_button("Remove").clicked() {
                    self.send(Command::Remove(m.sub.clone()));
                }
            }
        });
    }

    fn live_panel(&mut self, ui: &mut Ui) {
        let host = self.is_host();
        if !self.title.is_empty() {
            ui.label(RichText::new(&self.title).size(15.0).strong());
        }
        if let Some(code) = self.code.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&code).size(26.0).monospace().strong());
                if ui.small_button("Copy code").clicked() {
                    ui.ctx().copy_text(code.clone());
                }
                if let Some(s) = &self.server {
                    if ui.small_button("Copy invitation").clicked() {
                        ui.ctx().copy_text(format!("ruthos://join?server={s}&code={code}"));
                    }
                }
            });
        }
        let controller = self
            .presence
            .members
            .iter()
            .find(|m| Some(&m.sub) == self.presence.control.as_ref())
            .map(|m| m.name.clone());
        let line = if self.has_control() {
            "You have control".to_string()
        } else {
            match controller {
                Some(n) => format!("{n} has control"),
                None => "Session frozen (host away)".to_string(),
            }
        };
        ui.label(RichText::new(line).strong());
        if let Some(m) = self.expires_in {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Your sign-in ends in {m} minutes")).color(Color32::from_rgb(240, 190, 90)),
                );
                if ui.small_button("Sign in again").clicked() {
                    self.send(Command::SignIn);
                }
            });
        }
        if let Some(study) = &self.missing {
            ui.label(RichText::new(format!("This machine does not hold the case on screen ({study})")).small());
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if !self.has_control() {
                if self.free_view {
                    if ui.button("Rejoin the shared view").clicked() {
                        self.free_view = false;
                        self.rebuild += 1;
                        self.send(Command::Follow);
                    }
                } else if !self.presence.locked && ui.button("Look around").clicked() {
                    self.free_view = true;
                    self.send(Command::FreeView);
                }
                if self.hand_raised {
                    if ui.button("Lower hand").clicked() {
                        self.send(Command::LowerHand);
                    }
                } else if ui.button("Raise hand").clicked() {
                    self.send(Command::RaiseHand);
                }
            }
            if host {
                if !self.has_control() && ui.button("Take control").clicked() {
                    self.send(Command::TakeControl);
                }
                if self.presence.locked {
                    if ui.button("Unlock").clicked() {
                        self.send(Command::Unlock);
                    }
                } else if ui.button("Lock").clicked() {
                    self.send(Command::Lock);
                }
                let keys: Vec<String> = self
                    .model
                    .iter()
                    .flat_map(|m| m.measurements.iter())
                    .filter(|(_, v)| v.get("key").and_then(Value::as_bool) == Some(true))
                    .map(|(k, _)| k.clone())
                    .collect();
                if !keys.is_empty() && ui.button("Reveal answer key").clicked() {
                    self.send(Command::Reveal(keys));
                }
                if ui.button("End session").clicked() {
                    self.send(Command::End);
                }
            } else if ui.button("Leave").clicked() {
                self.send(Command::Leave);
            }
        });
        if host && !self.presence.waiting.is_empty() {
            ui.separator();
            ui.label(RichText::new("Waiting").strong());
            for m in self.presence.waiting.clone() {
                ui.horizontal(|ui| {
                    ui.label(&m.name);
                    if ui.small_button("Admit").clicked() {
                        self.send(Command::Admit(m.sub.clone()));
                    }
                    if ui.small_button("Deny").clicked() {
                        self.send(Command::Deny(m.sub.clone()));
                    }
                });
            }
        }
        ui.separator();
        ui.label(RichText::new(format!("Members ({})", self.presence.members.len())).strong());
        for m in self.presence.members.clone() {
            self.member_row(ui, &m, host);
        }
        if host {
            ui.separator();
            ui.collapsing("Roster", |ui| {
                ui.label(RichText::new("Addresses listed here are admitted without waiting.").small());
                ui.add(
                    egui::TextEdit::multiline(&mut self.roster)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if ui.button("Save roster").clicked() {
                    if let Some(sid) = self.sid.clone() {
                        let emails = self.roster.split([',', '\n']).map(str::to_string).collect();
                        self.send(Command::SetRoster { sid, emails });
                    }
                }
            });
        }
    }

    fn review_dialog(&mut self, ui: &mut Ui, shared: &Shared, ctx: &egui::Context) {
        let Some(review) = self.review.take() else { return };
        let mut keep = true;
        match review {
            Review::Invitation { server, code } => {
                ui.label(RichText::new(domain(&server)).size(22.0).strong());
                ui.label(RichText::new("Invitation to a guided session").strong());
                ui.label(format!("Session code {code}"));
                ui.label(RichText::new(&server).small());
                ui.label(
                    RichText::new("Sign-in happens in your browser. Check the address there before typing a password.")
                        .small(),
                );
                let mut chosen = false;
                ui.horizontal(|ui| {
                    if ui.button("Join").clicked() {
                        chosen = true;
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
                if chosen {
                    self.accept_invitation(server, code, shared, ctx);
                } else if keep {
                    self.review = Some(Review::Invitation { server, code });
                }
            }
            Review::Profile { checked, mut ticked } => {
                ui.label(RichText::new(checked.headline()).size(22.0).strong());
                let (word, colour) = match checked.verification {
                    Verification::Verified => ("Verified", Color32::from_rgb(120, 200, 130)),
                    Verification::Unsigned => ("Unsigned", Color32::from_rgb(240, 190, 90)),
                };
                ui.label(RichText::new(word).strong().color(colour));
                if !checked.issuer_by.is_empty() {
                    ui.label(format!("Issued by {}", checked.issuer_by));
                }
                if let Some(k) = &checked.key_change {
                    ui.label(
                        RichText::new(format!(
                            "Warning: this server's signing key has changed, from {} to {}",
                            k.pinned, k.presented
                        ))
                        .color(Color32::from_rgb(240, 120, 100)),
                    );
                }
                ui.separator();
                let settings = shared.settings.lock().clone();
                for section in &checked.sections {
                    let mut on = ticked.contains(section);
                    if ui.checkbox(&mut on, section.label()).changed() {
                        if on {
                            ticked.push(*section);
                        } else {
                            ticked.retain(|s| s != section);
                        }
                    }
                    for line in change_lines(*section, &checked.value, &settings) {
                        ui.label(RichText::new(format!("    {line}")).small());
                    }
                }
                let mut apply = false;
                ui.horizontal(|ui| {
                    apply = ui.button("Apply").clicked();
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
                if apply {
                    self.apply_profile(&checked, &ticked, shared, ctx);
                } else if keep {
                    self.review = Some(Review::Profile { checked, ticked });
                }
            }
        }
    }

    fn apply_profile(&mut self, c: &Checked, ticked: &[Section], shared: &Shared, ctx: &egui::Context) {
        let server = ticked
            .contains(&Section::Server)
            .then(|| {
                c.value
                    .get("server")
                    .and_then(Value::as_str)
                    .and_then(|s| normalise(s).ok())
            })
            .flatten();
        if server.as_deref().is_some_and(|s| !self.deployment.permits(s))
            || c.issuer_server
                .as_deref()
                .is_some_and(|s| !self.deployment.permits_issuer(s))
        {
            return set_message(self, "This profile's issuer is not permitted on this machine");
        }
        let mut library = None;
        if ticked.contains(&Section::Measurements) {
            if let Some(l) = c.value.pointer("/measurements/library") {
                match ruthos_core::measure::Library::from_json(&l.to_string()) {
                    Ok(l) => library = Some(l),
                    Err(e) => return set_message(self, format!("The profile's library could not be read: {e}")),
                }
            }
        }
        let mut s = shared.settings.lock().clone();
        let previous = snapshot(&s);
        if let Some(srv) = &server {
            s.session_server = Some(srv.clone());
        }
        if let Some(l) = library {
            s.measurements.merge(l);
        }
        if ticked.contains(&Section::Viewer) {
            if let Some(u) = c.value.pointer("/viewer/echo_length_unit").and_then(Value::as_str) {
                if matches!(u, "mm" | "cm") {
                    s.echo_length_unit = u.to_string();
                }
            }
            if let Some(m) = c.value.pointer("/viewer/loupe/magnification").and_then(Value::as_u64) {
                if matches!(m, 2 | 3 | 4 | 6 | 8) {
                    s.loupe_magnification = m as u32;
                }
            }
            if let Some(n) = c.value.pointer("/viewer/loupe/size").and_then(Value::as_u64) {
                if matches!(n, 120 | 160 | 200) {
                    s.loupe_size = n as u32;
                }
            }
            if let Some(b) = c.value.pointer("/viewer/loupe/enabled").and_then(Value::as_bool) {
                s.loupe_enabled = b;
            }
        }
        if let (Some(iss), Some((kid, key))) = (&c.issuer_server, &c.pin) {
            s.session_pins.insert(
                iss.clone(),
                SessionPin {
                    kid: kid.clone(),
                    key: key.clone(),
                },
            );
        }
        s.applied_profiles.push(AppliedProfile {
            issuer: c.headline(),
            by: c.issuer_by.clone(),
            verified: c.verification == Verification::Verified,
            applied: Utc::now().to_rfc3339(),
            previous,
        });
        if let Err(e) = commit(shared, s) {
            return set_message(self, format!("Could not save: {e}"));
        }
        if let Some(srv) = server {
            self.use_server(srv, ctx);
        }
        if ticked.contains(&Section::Session) {
            if let Some(code) = c.value.pointer("/session/code").and_then(Value::as_str) {
                self.pending_join = Some(code.to_string());
                let stale = self.account.is_none();
                if stale && self.info.is_some() {
                    self.send(Command::SignIn);
                }
            }
        }
        set_message(self, "Profile applied");
    }

    /// The Guided Review tab of Preferences.
    pub fn tab(&mut self, ui: &mut Ui, shared: &Shared, ctx: &egui::Context) {
        self.ensure_client(ctx);
        if let Some(msg) = self.message.clone() {
            ui.label(RichText::new(msg).color(Color32::from_rgb(240, 190, 90)));
        }
        if self.server.is_some() {
            self.server_line(ui);
            if let Some(path) = &self.deployment.source {
                ui.label(RichText::new(format!("Site settings from {}", path.display())).small());
            }
            if self.client.is_some() && !self.in_session() {
                self.account_line(ui);
            }
        } else {
            ui.label("No session server is set. An invitation link or a profile sets one.");
        }
        ui.add_space(6.0);
        if !self.deployment.locked {
            if ui.link("Enter address manually").clicked() {
                self.show_manual = !self.show_manual;
            }
            if self.show_manual {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.manual_address).desired_width(320.0));
                    if ui.button("Use").clicked() {
                        match normalise(&self.manual_address) {
                            Ok(s) => {
                                if self.use_server(s.clone(), ctx) {
                                    save_server(shared, &s);
                                }
                            }
                            Err(e) => set_message(self, e.to_string()),
                        }
                    }
                });
            }
        }
        if ui.button("Import profile\u{2026}").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Ruthos profile", &["ruthosprofile"])
                .pick_file()
            {
                self.open_profile(&p, shared, ctx);
                shared.open_session.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let profiles = shared.settings.lock().applied_profiles.clone();
        if !profiles.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Applied profiles").strong());
            for (i, p) in profiles.iter().enumerate().rev() {
                ui.horizontal(|ui| {
                    let tag = if p.verified { "Verified" } else { "Unsigned" };
                    ui.label(format!("{} ({tag}) {}", p.issuer, p.applied.get(..10).unwrap_or("")));
                    // Later profiles were applied over this one, so only
                    // the latest can be undone cleanly.
                    if i + 1 == profiles.len() && ui.small_button("Revert").clicked() {
                        let mut s = shared.settings.lock().clone();
                        restore(&mut s, &p.previous);
                        s.applied_profiles.pop();
                        let server = s.session_server.clone();
                        match commit(shared, s) {
                            Ok(()) => {
                                if let Some(srv) = server {
                                    self.use_server(srv, ctx);
                                }
                            }
                            Err(e) => set_message(self, format!("Could not save: {e}")),
                        }
                    }
                });
            }
        }
        let pins = shared.settings.lock().session_pins.clone();
        if !pins.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Pinned signing keys").strong());
            for (server, pin) in pins {
                ui.label(RichText::new(format!("{}: {}", domain(&server), pin.kid)).small());
            }
        }
    }
}

/// What a profile would change, as current and new values.
fn change_lines(section: Section, value: &Value, s: &ruthos_core::Settings) -> Vec<String> {
    let text = |p: &str| {
        value
            .pointer(p)
            .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
    };
    let mut out = Vec::new();
    match section {
        Section::Server => {
            if let Some(new) = text("/server") {
                let old = s.session_server.clone().unwrap_or_else(|| "none".to_string());
                out.push(format!("{old} \u{2192} {new}"));
            }
        }
        Section::Session => {
            if let Some(code) = text("/session/code") {
                out.push(format!("Join session {code}"));
            }
        }
        Section::Cases => {
            if let Some(list) = value.get("cases").and_then(Value::as_array) {
                out.push(format!("{} case(s)", list.len()));
            }
        }
        Section::Measurements => out.push("Merged by key; changed formulas keep your confirmation".to_string()),
        Section::Viewer => {
            if let Some(new) = text("/viewer/echo_length_unit") {
                out.push(format!("Echo length unit: {} \u{2192} {new}", s.echo_length_unit));
            }
            if let Some(new) = text("/viewer/loupe/magnification") {
                out.push(format!("Loupe magnification: {} \u{2192} {new}", s.loupe_magnification));
            }
        }
    }
    out
}

/// The settings a profile can touch, for Revert.
fn snapshot(s: &ruthos_core::Settings) -> Value {
    json!({
        "session_server": s.session_server,
        "echo_length_unit": s.echo_length_unit,
        "loupe_enabled": s.loupe_enabled,
        "loupe_magnification": s.loupe_magnification,
        "loupe_size": s.loupe_size,
        "measurements": s.measurements,
    })
}

fn restore(s: &mut ruthos_core::Settings, v: &Value) {
    s.session_server = v.get("session_server").and_then(Value::as_str).map(str::to_string);
    if let Some(u) = v.get("echo_length_unit").and_then(Value::as_str) {
        s.echo_length_unit = u.to_string();
    }
    if let Some(b) = v.get("loupe_enabled").and_then(Value::as_bool) {
        s.loupe_enabled = b;
    }
    if let Some(n) = v.get("loupe_magnification").and_then(Value::as_u64) {
        s.loupe_magnification = n as u32;
    }
    if let Some(n) = v.get("loupe_size").and_then(Value::as_u64) {
        s.loupe_size = n as u32;
    }
    if let Some(l) = v
        .get("measurements")
        .and_then(|l| serde_json::from_value(l.clone()).ok())
    {
        s.measurements = l;
    }
}

fn commit(shared: &Shared, s: ruthos_core::Settings) -> Result<(), String> {
    s.save().map_err(|e| e.to_string())?;
    *shared.library.lock() = Arc::new(s.library());
    *shared.settings.lock() = s;
    Ok(())
}

fn save_server(shared: &Shared, server: &str) {
    let mut s = shared.settings.lock().clone();
    s.session_server = Some(server.to_string());
    if let Err(e) = commit(shared, s) {
        tracing::warn!("settings not saved: {e}");
    }
}

/// The Guided review window.
pub fn window(ctx: &egui::Context, shared: &Shared) {
    let id = ViewportId::from_hash_of("ruthos-session");
    if shared.open_session.swap(false, std::sync::atomic::Ordering::Relaxed) {
        shared.session.lock().open = true;
        ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
    }
    if !shared.session.lock().open {
        return;
    }
    ctx.show_viewport_immediate(
        id,
        ViewportBuilder::default()
            .with_title("Guided review \u{2014} Ruthos")
            .with_inner_size([440.0, 620.0])
            .with_min_inner_size([360.0, 320.0]),
        |ctx, _class| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let root = ui.ctx().clone();
                if let Err(msg) = crate::crash::guarded(|| shared.session.lock().panel(ui, shared, &root)) {
                    shared
                        .errors
                        .lock()
                        .push(format!("The Guided review window hit an internal error: {msg}"));
                }
                if ui.input(|i| i.viewport().close_requested()) {
                    shared.session.lock().open = false;
                }
            });
        },
    );
}

/// Files and links handed to Ruthos on the command line or by a second copy.
pub fn route_argument(arg: &str, hub: &mut Hub, shared: &Shared, ctx: &egui::Context) {
    if arg.starts_with("ruthos:") {
        hub.open_link(arg, shared, ctx);
    } else if arg.to_ascii_lowercase().ends_with(".ruthosprofile") {
        hub.open_profile(&PathBuf::from(arg), shared, ctx);
    }
}

/// The controller's pointer in image pixels, as sent.
pub fn pointer_command(p: Option<(usize, [f32; 2])>) -> Command {
    Command::Pointer(p.map(|(cell, [x, y])| Pointer {
        cell: cell as u8,
        x: (x * 100.0).round() / 100.0,
        y: (y * 100.0).round() / 100.0,
    }))
}
