//! Alert fan-out.
//!
//! # The seed never leaves the machine
//!
//! ntfy, Telegram and SMTP all relay through servers we do not control, and a
//! push notification is stored, logged and mirrored in places nobody audits. A
//! mnemonic in a notification body is a mnemonic on somebody else's
//! infrastructure, permanently.
//!
//! So [`AlertPayload`] has no field capable of holding one. This is deliberate:
//! the guarantee is enforced by the type, not by remembering to omit it at
//! every call site. `alert_payload_never_contains_seed` pins it down.
//!
//! # Delivery
//!
//! Every configured channel is attempted concurrently on its own thread, so a
//! black-holed SMTP server delays nothing but itself. Each channel retries with
//! exponential backoff. If *all* of them fail, the payload is persisted to
//! `pending_alerts.jsonl` and a background thread keeps retrying until one
//! succeeds — a hit must not be lost because the WLAN was down.

pub mod channels;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hits::{append_durable, read_jsonl, Hit};
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertKind {
    Hit,
    Heartbeat,
    Test,
}

/// What is allowed to leave this machine.
///
/// Note what is absent: no mnemonic, no entropy, no private key, no extended
/// key. Adding such a field would defeat the module's entire purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertPayload {
    pub id: String,
    pub kind: AlertKind,
    pub timestamp: String,
    pub hostname: String,
    pub derivation_path: String,
    pub script_type: String,
    pub address: String,
    pub balance_sats: u64,
    pub balance_btc: String,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seeds_tested: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
}

impl AlertPayload {
    /// Projects a [`Hit`] down to the publishable subset.
    pub fn from_hit(hit: &Hit) -> AlertPayload {
        AlertPayload {
            id: hit.id.clone(),
            kind: if hit.is_synthetic() {
                AlertKind::Test
            } else {
                AlertKind::Hit
            },
            timestamp: hit.timestamp.clone(),
            hostname: hit.hostname.clone(),
            derivation_path: hit.derivation_path.clone(),
            script_type: hit.script_type.clone(),
            address: hit.address.clone(),
            balance_sats: hit.balance_sats,
            balance_btc: hit.balance_btc.clone(),
            note: "Seed is stored locally in hits.jsonl on the host above. \
                   It is deliberately not included in this message."
                .to_string(),
            seeds_tested: None,
            uptime: None,
        }
    }

    pub fn heartbeat(seeds: u64, uptime: Duration) -> AlertPayload {
        let now = util::unix_now();
        AlertPayload {
            // One id per hour keeps dedup from suppressing daily heartbeats.
            id: format!("heartbeat-{}", now / 3600),
            kind: AlertKind::Heartbeat,
            timestamp: util::rfc3339(now),
            hostname: util::hostname(),
            derivation_path: String::new(),
            script_type: String::new(),
            address: String::new(),
            balance_sats: 0,
            balance_btc: String::new(),
            note: "seed-collider is still running.".to_string(),
            seeds_tested: Some(seeds),
            uptime: Some(util::format_duration(uptime.as_secs())),
        }
    }

    pub fn title(&self) -> String {
        match self.kind {
            AlertKind::Hit => format!("FUNDED SEED FOUND ({})", self.balance_btc),
            AlertKind::Test => "seed-collider test alert".to_string(),
            AlertKind::Heartbeat => format!("seed-collider alive on {}", self.hostname),
        }
    }

    /// Human-readable body used by every channel.
    pub fn body(&self) -> String {
        match self.kind {
            AlertKind::Heartbeat => format!(
                "host:    {}\ntime:    {}\nuptime:  {}\nseeds:   {}\n\n{}",
                self.hostname,
                self.timestamp,
                self.uptime.as_deref().unwrap_or("-"),
                self.seeds_tested.unwrap_or(0),
                self.note
            ),
            _ => format!(
                "time:    {}\nhost:    {}\npath:    {}\ntype:    {}\naddress: {}\nbalance: {}\n\n{}",
                self.timestamp,
                self.hostname,
                self.derivation_path,
                self.script_type,
                self.address,
                self.balance_btc,
                self.note
            ),
        }
    }
}

/// A delivery channel.
pub trait Notifier: Send + Sync {
    fn name(&self) -> &str;
    /// Attempts one delivery. Errors are returned as text for the UI.
    fn send(&self, payload: &AlertPayload) -> Result<(), String>;
}

/// A payload that has not been delivered to every channel yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    payload: AlertPayload,
    /// Channels still owed a copy.
    remaining: Vec<String>,
    attempts: u32,
    first_seen: u64,
}

/// Outcome of one fan-out, for display.
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub per_channel: Vec<(String, Result<(), String>)>,
    pub queued_for_retry: bool,
}

impl DispatchResult {
    pub fn any_succeeded(&self) -> bool {
        self.per_channel.iter().any(|(_, r)| r.is_ok())
    }
}

pub struct Dispatcher {
    channels: Vec<Arc<dyn Notifier>>,
    pending_path: PathBuf,
    max_attempts: u32,
    retry_interval: Duration,
    /// Payload ids already fanned out, so a repeat find does not re-alarm.
    seen: Mutex<HashSet<String>>,
}

impl Dispatcher {
    pub fn new(
        channels: Vec<Arc<dyn Notifier>>,
        pending_path: PathBuf,
        max_attempts: u32,
        retry_interval: Duration,
    ) -> Dispatcher {
        Dispatcher {
            channels,
            pending_path,
            max_attempts: max_attempts.max(1),
            retry_interval,
            seen: Mutex::new(HashSet::new()),
        }
    }

    pub fn channel_names(&self) -> Vec<&str> {
        self.channels.iter().map(|c| c.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// True if this id has already been dispatched.
    pub fn already_seen(&self, id: &str) -> bool {
        self.seen.lock().map(|s| s.contains(id)).unwrap_or(false)
    }

    /// Fans out to every channel concurrently and waits for the round to finish.
    ///
    /// Callers on the hot path should use [`Dispatcher::dispatch_async`].
    pub fn dispatch(self: &Arc<Self>, payload: &AlertPayload) -> DispatchResult {
        {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.insert(payload.id.clone()) {
                return DispatchResult {
                    per_channel: vec![],
                    queued_for_retry: false,
                };
            }
        }
        let targets: Vec<String> = self.channels.iter().map(|c| c.name().to_string()).collect();
        self.deliver(payload, &targets)
    }

    /// Fires and forgets, so the collider never blocks on the network.
    pub fn dispatch_async(self: &Arc<Self>, payload: AlertPayload) {
        let me = Arc::clone(self);
        thread::spawn(move || {
            me.dispatch(&payload);
        });
    }

    /// Attempts `targets`, each on its own thread with exponential backoff.
    fn deliver(self: &Arc<Self>, payload: &AlertPayload, targets: &[String]) -> DispatchResult {
        let mut handles = Vec::new();

        for ch in &self.channels {
            if !targets.iter().any(|t| t == ch.name()) {
                continue;
            }
            let ch = Arc::clone(ch);
            let payload = payload.clone();
            let max = self.max_attempts;
            handles.push(thread::spawn(move || {
                let mut last = Err("not attempted".to_string());
                for attempt in 0..max {
                    match ch.send(&payload) {
                        Ok(()) => return (ch.name().to_string(), Ok(())),
                        Err(e) => last = Err(e),
                    }
                    if attempt + 1 < max {
                        // 1s, 2s, 4s, 8s — capped so a five-attempt round
                        // finishes well inside a minute.
                        let backoff = Duration::from_secs(1u64 << attempt.min(3));
                        thread::sleep(backoff);
                    }
                }
                (ch.name().to_string(), last)
            }));
        }

        let mut per_channel = Vec::new();
        for h in handles {
            match h.join() {
                Ok(r) => per_channel.push(r),
                Err(_) => {
                    per_channel.push(("<panicked>".to_string(), Err("thread panicked".into())))
                }
            }
        }

        let failed: Vec<String> = per_channel
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(n, _)| n.clone())
            .collect();

        // Queue only when nothing got through. If even one channel delivered,
        // the operator has been told, and re-firing the rest later would be the
        // alarm flood the spec warns about.
        let queued = if !per_channel.is_empty() && failed.len() == per_channel.len() {
            self.queue(payload, &failed).is_ok()
        } else {
            false
        };

        DispatchResult {
            per_channel,
            queued_for_retry: queued,
        }
    }

    fn queue(&self, payload: &AlertPayload, remaining: &[String]) -> std::io::Result<()> {
        let entry = Pending {
            payload: payload.clone(),
            remaining: remaining.to_vec(),
            attempts: 0,
            first_seen: util::unix_now(),
        };
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        append_durable(&self.pending_path, line.as_bytes())
    }

    /// Starts the background retry loop for anything in `pending_alerts.jsonl`.
    ///
    /// Runs until the process exits. Entries are rewritten each pass, so a
    /// delivered payload disappears and a still-failing one keeps its place.
    pub fn spawn_retry_loop(self: &Arc<Self>) {
        let me = Arc::clone(self);
        thread::spawn(move || loop {
            thread::sleep(me.retry_interval);
            if let Err(e) = me.retry_pending() {
                // Nothing to report to but the file itself; a failure here is
                // transient (the file will be retried next pass).
                let _ = e;
            }
        });
    }

    fn retry_pending(&self) -> std::io::Result<()> {
        let entries: Vec<Pending> = read_jsonl(&self.pending_path)?;
        if entries.is_empty() {
            return Ok(());
        }

        let mut still_pending: Vec<Pending> = Vec::new();
        for mut entry in entries {
            let mut remaining = Vec::new();
            for name in &entry.remaining {
                let Some(ch) = self.channels.iter().find(|c| c.name() == name) else {
                    continue; // channel was removed from the config
                };
                if ch.send(&entry.payload).is_err() {
                    remaining.push(name.clone());
                }
            }
            entry.attempts += 1;
            if remaining.is_empty() {
                continue; // fully delivered, drop it
            }
            entry.remaining = remaining;
            still_pending.push(entry);
        }

        self.rewrite_pending(&still_pending)
    }

    /// Replaces the queue file atomically so a crash mid-rewrite cannot leave a
    /// half-written queue.
    fn rewrite_pending(&self, entries: &[Pending]) -> std::io::Result<()> {
        let tmp = self.pending_path.with_extension("jsonl.tmp");
        if entries.is_empty() {
            let _ = std::fs::remove_file(&self.pending_path);
            let _ = std::fs::remove_file(&tmp);
            return Ok(());
        }

        let _ = std::fs::remove_file(&tmp);
        for e in entries {
            let mut line = serde_json::to_string(e)?;
            line.push('\n');
            append_durable(&tmp, line.as_bytes())?;
        }
        std::fs::rename(&tmp, &self.pending_path)
    }

    pub fn pending_count(&self) -> usize {
        read_jsonl::<Pending>(&self.pending_path)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    pub fn pending_path(&self) -> &Path {
        &self.pending_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Always {
        name: String,
        ok: bool,
        calls: AtomicU32,
    }

    impl Always {
        fn new(name: &str, ok: bool) -> Arc<Always> {
            Arc::new(Always {
                name: name.to_string(),
                ok,
                calls: AtomicU32::new(0),
            })
        }
    }

    impl Notifier for Always {
        fn name(&self) -> &str {
            &self.name
        }
        fn send(&self, _p: &AlertPayload) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err("nope".into())
            }
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sc-alert-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("pending.jsonl")
    }

    /// The load-bearing guarantee of this module.
    #[test]
    fn alert_payload_never_contains_seed() {
        let mut hit = Hit::synthetic();
        hit.mnemonic = "correct horse battery staple sentinel phrase".into();
        hit.entropy_hex = "deadbeefcafebabe".into();
        hit.private_key_wif = "L1SentinelPrivateKeyValue".into();

        let payload = AlertPayload::from_hit(&hit);
        let json = serde_json::to_string(&payload).unwrap();
        let body = payload.body();
        let title = payload.title();

        for secret in [
            "correct horse",
            "sentinel phrase",
            "deadbeefcafebabe",
            "L1Sentinel",
        ] {
            assert!(
                !json.contains(secret),
                "secret {secret:?} leaked into alert JSON"
            );
            assert!(
                !body.contains(secret),
                "secret {secret:?} leaked into alert body"
            );
            assert!(
                !title.contains(secret),
                "secret {secret:?} leaked into alert title"
            );
        }
        // The parts that *should* travel are present.
        assert!(json.contains(&hit.address));
        assert!(json.contains(&hit.derivation_path));
    }

    #[test]
    fn one_dead_channel_does_not_stop_the_others() {
        let good = Always::new("good", true);
        let bad = Always::new("bad", false);
        let d = Arc::new(Dispatcher::new(
            vec![good.clone(), bad.clone()],
            tmp("mixed"),
            2,
            Duration::from_secs(60),
        ));

        let r = d.dispatch(&AlertPayload::from_hit(&Hit::synthetic()));
        assert!(r.any_succeeded());
        assert!(!r.queued_for_retry, "one success means no queueing");
        assert_eq!(good.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            bad.calls.load(Ordering::SeqCst),
            2,
            "failed channel retried"
        );
    }

    #[test]
    fn total_failure_is_queued_and_later_drained() {
        let path = tmp("queue");
        let bad = Always::new("bad", false);
        let d = Arc::new(Dispatcher::new(
            vec![bad.clone()],
            path.clone(),
            2,
            Duration::from_secs(60),
        ));

        let r = d.dispatch(&AlertPayload::from_hit(&Hit::synthetic()));
        assert!(!r.any_succeeded());
        assert!(r.queued_for_retry, "total failure must persist the payload");
        assert_eq!(d.pending_count(), 1);

        // Swap in a working channel and drain.
        let good = Always::new("bad", true); // same name, now succeeds
        let d2 = Arc::new(Dispatcher::new(
            vec![good],
            path.clone(),
            2,
            Duration::from_secs(60),
        ));
        d2.retry_pending().unwrap();
        assert_eq!(d2.pending_count(), 0, "queue must drain after success");
    }

    #[test]
    fn duplicate_ids_do_not_refire() {
        let good = Always::new("good", true);
        let d = Arc::new(Dispatcher::new(
            vec![good.clone()],
            tmp("dedup"),
            1,
            Duration::from_secs(60),
        ));

        let p = AlertPayload::from_hit(&Hit::synthetic());
        d.dispatch(&p);
        d.dispatch(&p);
        d.dispatch(&p);
        assert_eq!(good.calls.load(Ordering::SeqCst), 1, "dedup by hit id");
    }

    #[test]
    fn heartbeats_carry_no_address_fields() {
        let p = AlertPayload::heartbeat(1234, Duration::from_secs(90_061));
        assert_eq!(p.kind, AlertKind::Heartbeat);
        assert!(p.address.is_empty());
        assert!(p.body().contains("1234"));
        assert!(p.body().contains("1d 01h 01m 01s"));
    }
}
