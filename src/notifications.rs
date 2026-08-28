use std::{
    path::Path,
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod delivery;
mod health;
mod inbox;
pub mod model;
mod orchestrator;
pub mod pushover;
mod store;

pub use health::HealthMonitor;
pub use inbox::{AttemptRecord, ClearScope, HistoryEvent, HistoryGroup, Inbox, NotificationItem};
pub use store::{RuleRecord, RuleStoreError};

const COMMAND_CAPACITY: usize = 256;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

enum Command {
    SaveDraft {
        draft: Box<model::Rule>,
        expected_draft_revision: u64,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<RuleRecord>>,
    },
    Activate {
        rule_id: String,
        owner_id: String,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<RuleRecord>>,
    },
    Delete {
        rule_id: String,
        owner_id: String,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<()>>,
    },
    Rules {
        owner_id: String,
        reply: SyncSender<anyhow::Result<Vec<RuleRecord>>>,
    },
    Publish {
        candidate: Box<model::Candidate>,
    },
    Test {
        rule_id: String,
        owner_id: String,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<ProcessSummary>>,
    },
    Inbox {
        principal_id: String,
        limit: usize,
        reply: SyncSender<anyhow::Result<Inbox>>,
    },
    History {
        principal_id: String,
        limit: usize,
        reply: SyncSender<anyhow::Result<Vec<HistoryGroup>>>,
    },
    MarkSeen {
        logical_id: String,
        principal_id: String,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<()>>,
    },
    Acknowledge {
        logical_id: String,
        principal_id: String,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<()>>,
    },
    Clear {
        logical_id: String,
        principal_id: String,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<()>>,
    },
    ClearScope {
        principal_id: String,
        scope: ClearScope,
        now_ms: i64,
        reply: SyncSender<anyhow::Result<u64>>,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct Handle {
    tx: SyncSender<Command>,
}

impl Handle {
    pub fn save_draft(
        &self,
        draft: model::Rule,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::SaveDraft {
                draft: Box::new(draft),
                expected_draft_revision,
                now_ms,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification draft write timed out: {error}"))?
    }

    pub fn activate(
        &self,
        rule_id: impl Into<String>,
        owner_id: impl Into<String>,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Activate {
                rule_id: rule_id.into(),
                owner_id: owner_id.into(),
                expected_active_revision,
                expected_draft_revision,
                now_ms,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification activation timed out: {error}"))?
    }

    pub fn delete(
        &self,
        rule_id: impl Into<String>,
        owner_id: impl Into<String>,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Delete {
                rule_id: rule_id.into(),
                owner_id: owner_id.into(),
                expected_active_revision,
                expected_draft_revision,
                now_ms,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification rule deletion timed out: {error}"))?
    }

    pub fn rules(&self, owner_id: impl Into<String>) -> anyhow::Result<Vec<RuleRecord>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Rules {
                owner_id: owner_id.into(),
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification rule listing timed out: {error}"))?
    }

    pub fn publish(&self, candidate: model::Candidate) {
        match self.tx.try_send(Command::Publish {
            candidate: Box::new(candidate),
        }) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                tracing::warn!(
                    "notification candidate dropped because the evaluation queue is full"
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!("notification candidate dropped because the runtime stopped");
            }
        }
    }

    pub fn test_rule(
        &self,
        rule_id: impl Into<String>,
        owner_id: impl Into<String>,
        now_ms: i64,
    ) -> anyhow::Result<ProcessSummary> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Test {
                rule_id: rule_id.into(),
                owner_id: owner_id.into(),
                now_ms,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification test timed out: {error}"))?
    }

    pub fn inbox(&self, principal_id: impl Into<String>, limit: usize) -> anyhow::Result<Inbox> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::Inbox {
                principal_id: principal_id.into(),
                limit,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification inbox query timed out: {error}"))?
    }

    pub fn history(
        &self,
        principal_id: impl Into<String>,
        limit: usize,
    ) -> anyhow::Result<Vec<HistoryGroup>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::History {
                principal_id: principal_id.into(),
                limit,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification history query timed out: {error}"))?
    }

    pub fn mark_seen(
        &self,
        logical_id: impl Into<String>,
        principal_id: impl Into<String>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.receipt_command(logical_id, principal_id, now_ms, |fields| {
            Command::MarkSeen {
                logical_id: fields.0,
                principal_id: fields.1,
                now_ms: fields.2,
                reply: fields.3,
            }
        })
    }

    pub fn acknowledge(
        &self,
        logical_id: impl Into<String>,
        principal_id: impl Into<String>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.receipt_command(logical_id, principal_id, now_ms, |fields| {
            Command::Acknowledge {
                logical_id: fields.0,
                principal_id: fields.1,
                now_ms: fields.2,
                reply: fields.3,
            }
        })
    }

    pub fn clear(
        &self,
        logical_id: impl Into<String>,
        principal_id: impl Into<String>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.receipt_command(logical_id, principal_id, now_ms, |fields| Command::Clear {
            logical_id: fields.0,
            principal_id: fields.1,
            now_ms: fields.2,
            reply: fields.3,
        })
    }

    pub fn clear_scope(
        &self,
        principal_id: impl Into<String>,
        scope: ClearScope,
        now_ms: i64,
    ) -> anyhow::Result<u64> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Command::ClearScope {
                principal_id: principal_id.into(),
                scope,
                now_ms,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification scoped clear timed out: {error}"))?
    }

    fn receipt_command(
        &self,
        logical_id: impl Into<String>,
        principal_id: impl Into<String>,
        now_ms: i64,
        command: impl FnOnce((String, String, i64, SyncSender<anyhow::Result<()>>)) -> Command,
    ) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(command((
                logical_id.into(),
                principal_id.into(),
                now_ms,
                reply_tx,
            )))
            .map_err(|_| anyhow::anyhow!("notification runtime is no longer running"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| anyhow::anyhow!("notification receipt update timed out: {error}"))?
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessSummary {
    pub matched: u32,
    pub created: u32,
    pub replaced: u32,
    pub suppressed: u32,
    pub queued_attempts: u32,
}

pub struct Runtime {
    handle: Handle,
    thread: Option<std::thread::JoinHandle<()>>,
    deliveries: Option<delivery::Workers>,
}

impl Runtime {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let store = store::Store::open(path)?;
        store.recover_interrupted_deliveries()?;
        let deliveries = delivery::Workers::start(path)?;
        let (tx, rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let handle = Handle { tx };
        let thread = std::thread::Builder::new()
            .name("notifications".to_owned())
            .spawn(move || run(store, rx))?;
        Ok(Self {
            handle,
            thread: Some(thread),
            deliveries: Some(deliveries),
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        if let Some(deliveries) = self.deliveries.take() {
            deliveries.shutdown();
        }
        let _ = self.handle.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("notification runtime thread panicked");
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn run(store: store::Store, rx: Receiver<Command>) {
    while let Ok(command) = rx.recv() {
        match command {
            Command::SaveDraft {
                draft,
                expected_draft_revision,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.save_draft(*draft, expected_draft_revision, now_ms));
            }
            Command::Activate {
                rule_id,
                owner_id,
                expected_active_revision,
                expected_draft_revision,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.activate(
                    &rule_id,
                    &owner_id,
                    expected_active_revision,
                    expected_draft_revision,
                    now_ms,
                ));
            }
            Command::Delete {
                rule_id,
                owner_id,
                expected_active_revision,
                expected_draft_revision,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.delete(
                    &rule_id,
                    &owner_id,
                    expected_active_revision,
                    expected_draft_revision,
                    now_ms,
                ));
            }
            Command::Rules { owner_id, reply } => {
                let _ = reply.send(store.rules(&owner_id));
            }
            Command::Publish { candidate } => {
                if let Err(error) = store.process(*candidate) {
                    tracing::warn!(error = %error, "notification candidate evaluation failed");
                }
            }
            Command::Test {
                rule_id,
                owner_id,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.test_rule(&rule_id, &owner_id, now_ms));
            }
            Command::Inbox {
                principal_id,
                limit,
                reply,
            } => {
                let _ = reply.send(store.inbox(&principal_id, limit));
            }
            Command::History {
                principal_id,
                limit,
                reply,
            } => {
                let _ = reply.send(store.history(&principal_id, limit));
            }
            Command::MarkSeen {
                logical_id,
                principal_id,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.mark_seen(&logical_id, &principal_id, now_ms));
            }
            Command::Acknowledge {
                logical_id,
                principal_id,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.acknowledge(&logical_id, &principal_id, now_ms));
            }
            Command::Clear {
                logical_id,
                principal_id,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.clear(&logical_id, &principal_id, now_ms));
            }
            Command::ClearScope {
                principal_id,
                scope,
                now_ms,
                reply,
            } => {
                let _ = reply.send(store.clear_scope(&principal_id, &scope, now_ms));
            }
            Command::Shutdown => break,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicalId(String);

impl LogicalId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Event,
    Outage,
    Storage,
    Recording,
    Test,
}

impl Lifecycle {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Outage => "outage",
            Self::Storage => "storage",
            Self::Recording => "recording",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preliminary,
    Enriched,
    Recovery,
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub source_id: String,
    pub identity: String,
    pub lifecycle: Lifecycle,
    pub event_kind: Option<String>,
    pub group_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CooldownScope {
    Event,
    CameraEventKind,
    Group,
    Rule,
    Outage,
}

impl CooldownScope {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::CameraEventKind => "camera_event_kind",
            Self::Group => "group",
            Self::Rule => "rule",
            Self::Outage => "outage",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cooldown {
    pub scope: CooldownScope,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RulePolicy {
    pub rule_id: String,
    pub cooldowns: Vec<Cooldown>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CooldownKey {
    rule_id: String,
    scope: CooldownScope,
    value: String,
}

fn logical_id(rule: &RulePolicy, transition: &Transition) -> LogicalId {
    let mut hash = Sha256::new();
    for value in [
        rule.rule_id.as_str(),
        transition.source_id.as_str(),
        transition.identity.as_str(),
        transition.lifecycle.as_str(),
    ] {
        hash.update(value.len().to_be_bytes());
        hash.update(value.as_bytes());
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = hash.finalize();
    let mut encoded = String::with_capacity("notification-".len() + digest.len() * 2);
    encoded.push_str("notification-");
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    LogicalId(encoded)
}

fn cooldown_keys(rule: &RulePolicy, transition: &Transition) -> Vec<(CooldownKey, u64)> {
    let mut keys = Vec::with_capacity(rule.cooldowns.len().max(transition.group_ids.len()));
    for cooldown in &rule.cooldowns {
        match cooldown.scope {
            CooldownScope::Event if transition.lifecycle == Lifecycle::Event => keys.push((
                CooldownKey {
                    rule_id: rule.rule_id.clone(),
                    scope: CooldownScope::Event,
                    value: format!("{}:{}", transition.source_id, transition.identity),
                },
                cooldown.duration_ms,
            )),
            CooldownScope::CameraEventKind => keys.push((
                CooldownKey {
                    rule_id: rule.rule_id.clone(),
                    scope: CooldownScope::CameraEventKind,
                    value: format!(
                        "{}:{}",
                        transition.source_id,
                        transition.event_kind.as_deref().unwrap_or("")
                    ),
                },
                cooldown.duration_ms,
            )),
            CooldownScope::Group => {
                keys.extend(transition.group_ids.iter().map(|group_id| {
                    (
                        CooldownKey {
                            rule_id: rule.rule_id.clone(),
                            scope: CooldownScope::Group,
                            value: group_id.clone(),
                        },
                        cooldown.duration_ms,
                    )
                }));
            }
            CooldownScope::Rule => keys.push((
                CooldownKey {
                    rule_id: rule.rule_id.clone(),
                    scope: CooldownScope::Rule,
                    value: rule.rule_id.clone(),
                },
                cooldown.duration_ms,
            )),
            CooldownScope::Outage if transition.lifecycle == Lifecycle::Outage => keys.push((
                CooldownKey {
                    rule_id: rule.rule_id.clone(),
                    scope: CooldownScope::Outage,
                    value: format!("{}:{}", transition.source_id, transition.identity),
                },
                cooldown.duration_ms,
            )),
            CooldownScope::Event | CooldownScope::Outage => {}
        }
    }
    keys
}

#[cfg(test)]
mod performance {
    use std::{hint::black_box, time::Instant};

    use hdrhistogram::Histogram;

    use super::{
        Handle, Lifecycle, Stage,
        model::{Candidate, Severity, Trigger},
    };

    const ITERATIONS: usize = 20_000;
    const PUBLISH_P95_BUDGET_NS: u64 = 100_000;

    #[test]
    #[ignore = "run with cargo test --release --lib notification_publish_latency -- --ignored --nocapture"]
    fn notification_publish_latency() {
        let (tx, rx) = std::sync::mpsc::sync_channel(ITERATIONS);
        let handle = Handle { tx };
        let drain = std::thread::spawn(move || while rx.recv().is_ok() {});
        let candidate = Candidate {
            trigger: Trigger::EventCreated,
            source_id: "front-door".to_owned(),
            source_name: Some("Front Door".to_owned()),
            source_identity: "event-1".to_owned(),
            lifecycle: Lifecycle::Event,
            event_kind: Some("person".to_owned()),
            payload: None,
            group_ids: Vec::new(),
            zone: None,
            confidence: Some(0.9),
            attachment_path: None,
            canonical_attachment: None,
            icon_key: Some("person".to_owned()),
            image_available: false,
            duration_ms: None,
            severity: Severity::Info,
            reviewed: None,
            bookmarked: None,
            privacy_active: false,
            revision: 1,
            stage: Stage::Preliminary,
            occurred_at_ms: 1_000,
            deep_link: "/events?camera=front-door&event=event-1".to_owned(),
        };
        let mut baseline = Histogram::<u64>::new(3).unwrap();
        let mut publish = Histogram::<u64>::new(3).unwrap();
        for _ in 0..ITERATIONS {
            let started = Instant::now();
            black_box(candidate.clone());
            baseline.record(elapsed_ns(started)).unwrap();

            let started = Instant::now();
            handle.publish(candidate.clone());
            publish.record(elapsed_ns(started)).unwrap();
        }
        let publish_p95_ns = publish.value_at_quantile(0.95);
        println!(
            "notification_publish_latency iterations={ITERATIONS} baseline_p50_ns={} baseline_p95_ns={} publish_p50_ns={} publish_p95_ns={} delta_p95_ns={} budget_p95_ns={PUBLISH_P95_BUDGET_NS}",
            baseline.value_at_quantile(0.5),
            baseline.value_at_quantile(0.95),
            publish.value_at_quantile(0.5),
            publish_p95_ns,
            publish_p95_ns.saturating_sub(baseline.value_at_quantile(0.95)),
        );
        assert!(publish_p95_ns <= PUBLISH_P95_BUDGET_NS);
        drop(handle);
        drain.join().unwrap();
    }

    fn elapsed_ns(started: Instant) -> u64 {
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}
