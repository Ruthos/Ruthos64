//! The routing engine: sends arriving images on to a PACS by the rules in
//! Preferences > Routing (Horos: auto-routing).
//!
//! Each finished import is matched against the rules. The images a rule
//! takes are collected by study and sent once no new image of that study
//! has arrived for a few seconds, so a study that arrives over several
//! scans of the incoming folder goes as one association. A send that fails
//! is retried three times, a minute apart. What happened is kept in a
//! short log the Routing page shows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use ruthos_core::routing::assign;
use ruthos_net::{NetError, SendOutcome};

use crate::pacs::{me, node, problem, Task};
use crate::shared::Shared;

/// How long a study must be quiet before its images are sent.
const QUIET: Duration = Duration::from_secs(5);
const RETRY_AFTER: Duration = Duration::from_secs(60);
const ATTEMPTS: u32 = 3;
const AT_ONCE: usize = 2;

/// One line of the routing log.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteLogEntry {
    pub when: String,
    pub text: String,
    pub ok: bool,
}

/// Images that finished importing, waiting for the router.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrival {
    pub from_network: bool,
    pub image_ids: Vec<i64>,
}

struct Batch {
    destination: String,
    study_label: String,
    files: Vec<PathBuf>,
    last_added: Instant,
    attempts: u32,
    not_before: Instant,
}

struct Running {
    key: (String, i64),
    batch: Batch,
    task: Task<Result<SendOutcome, NetError>>,
}

#[derive(Default)]
pub struct Router {
    /// By destination name and study id.
    pending: HashMap<(String, i64), Batch>,
    running: Vec<Running>,
}

pub fn log(shared: &Shared, text: impl Into<String>, ok: bool) {
    let text = text.into();
    tracing::info!("routing: {text}");
    let mut l = shared.route_log.lock();
    l.push_front(RouteLogEntry {
        when: chrono::Local::now().format("%H:%M:%S").to_string(),
        text,
        ok,
    });
    l.truncate(200);
}

impl Router {
    /// Whether anything is waiting or being sent.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty() || !self.running.is_empty()
    }

    pub fn pump(&mut self, ctx: &egui::Context, shared: &Shared) {
        let settings = shared.settings.lock().clone();
        let arrivals: Vec<Arrival> = std::mem::take(&mut *shared.route_inbox.lock());
        if !settings.routing_rules.is_empty() {
            let retrieved = shared.retrieved_from.lock().clone();
            for a in arrivals {
                let items = shared.db.route_items(&a.image_ids).unwrap_or_default();
                for (rule, taken) in assign(&settings.routing_rules, a.from_network, &items, &|uid| {
                    retrieved.get(uid).cloned()
                }) {
                    let destination = settings.routing_rules[rule].destination.trim().to_string();
                    for it in taken {
                        let b = self
                            .pending
                            .entry((destination.clone(), it.study_id))
                            .or_insert_with(|| Batch {
                                destination: destination.clone(),
                                study_label: it.study_label.clone(),
                                files: Vec::new(),
                                last_added: Instant::now(),
                                attempts: 0,
                                not_before: Instant::now(),
                            });
                        let f = shared.resolve(&it.path);
                        if !b.files.contains(&f) {
                            b.files.push(f);
                        }
                        b.last_added = Instant::now();
                    }
                }
            }
        }

        // Start the batches that have been quiet long enough.
        let now = Instant::now();
        let ready: Vec<(String, i64)> = self
            .pending
            .iter()
            .filter(|(_, b)| now.duration_since(b.last_added) >= QUIET && now >= b.not_before)
            .map(|(k, _)| k.clone())
            .collect();
        for key in ready {
            if self.running.len() >= AT_ONCE {
                break;
            }
            let Some(batch) = self.pending.remove(&key) else {
                continue;
            };
            let Some(n) = settings
                .pacs_nodes
                .iter()
                .find(|n| n.name.trim().eq_ignore_ascii_case(&batch.destination))
            else {
                log(
                    shared,
                    format!(
                        "{}: no PACS called \"{}\" in Preferences > PACS; not sent",
                        batch.study_label, batch.destination
                    ),
                    false,
                );
                continue;
            };
            let target = node(n);
            let ae = settings.listener_ae_title.clone();
            let files = batch.files.clone();
            let task = Task::spawn(ctx, "ruthos-route", move || {
                ruthos_net::send_files(&target, &ae, &files, &|_, _| {}, &AtomicBool::new(false))
            });
            self.running.push(Running { key, batch, task });
        }

        // Finished sends.
        let mut still = Vec::new();
        for mut r in std::mem::take(&mut self.running) {
            let Some(result) = r.task.take() else {
                still.push(r);
                continue;
            };
            let b = &mut r.batch;
            match result {
                Ok(o) if o.failed.is_empty() => log(
                    shared,
                    format!(
                        "{}: {} image{} sent to {}",
                        b.study_label,
                        o.sent,
                        if o.sent == 1 { "" } else { "s" },
                        b.destination
                    ),
                    true,
                ),
                Ok(o) => log(
                    shared,
                    format!(
                        "{}: {} sent to {}, {} refused ({})",
                        b.study_label,
                        o.sent,
                        b.destination,
                        o.failed.len(),
                        o.failed.first().map(|(_, why)| why.as_str()).unwrap_or("")
                    ),
                    false,
                ),
                Err(e) => {
                    b.attempts += 1;
                    let n = settings
                        .pacs_nodes
                        .iter()
                        .find(|n| n.name.trim().eq_ignore_ascii_case(&b.destination));
                    let why = n
                        .map(|n| problem(&e, n, &me(&settings, shared.net_status.lock().running)).summary)
                        .unwrap_or_else(|| e.to_string());
                    if b.attempts < ATTEMPTS {
                        log(
                            shared,
                            format!(
                                "{}: not sent to {} ({why}); trying again in a minute",
                                b.study_label, b.destination
                            ),
                            false,
                        );
                        b.not_before = Instant::now() + RETRY_AFTER;
                        b.last_added = Instant::now() - QUIET;
                        let key = r.key.clone();
                        self.pending.insert(key, r.batch);
                    } else {
                        log(
                            shared,
                            format!(
                                "{}: not sent to {} after {ATTEMPTS} tries ({why})",
                                b.study_label, b.destination
                            ),
                            false,
                        );
                    }
                }
            }
        }
        self.running.extend(still);
        if self.busy() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}
