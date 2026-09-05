use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, atomic::Ordering},
};

use super::{NotificationMetrics, decrement_counter, model::Rule, state::RuntimeState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_RULE_JSON_BYTES: usize = 64 * 1_024;
const MAX_RULES: usize = 128;
const NOTIFICATION_SECRET_PREFIX: &str = "KEEPPEEK_NOTIFICATION_";

#[derive(Debug, Clone, PartialEq)]
pub struct RuleRecord {
    pub id: String,
    pub owner_id: String,
    pub active: Option<Rule>,
    pub active_revision: u64,
    pub draft: Rule,
    pub draft_revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_match_at_ms: Option<i64>,
    pub last_delivery_at_ms: Option<i64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NotificationConfiguration {
    #[serde(default)]
    rules: Vec<PersistedRuleRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedRuleRecord {
    id: String,
    owner_id: String,
    active: Option<Rule>,
    active_revision: u64,
    draft: Rule,
    draft_revision: u64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStoreError {
    Conflict {
        active_revision: u64,
        draft_revision: u64,
    },
    NotFound,
    NotAuthorized,
}

impl fmt::Display for RuleStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                active_revision,
                draft_revision,
            } => write!(
                formatter,
                "notification rule revision conflict (active {active_revision}, draft {draft_revision})"
            ),
            Self::NotFound => formatter.write_str("notification rule was not found"),
            Self::NotAuthorized => {
                formatter.write_str("notification rule is owned by another principal")
            }
        }
    }
}

impl std::error::Error for RuleStoreError {}

#[derive(Clone)]
pub(super) struct Store {
    pub(super) config_path: Arc<PathBuf>,
    pub(super) metrics: Arc<NotificationMetrics>,
    config_update: Arc<Mutex<()>>,
    state: Arc<Mutex<RuntimeState>>,
}

impl Store {
    #[cfg(test)]
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_with_config_update(path, Arc::new(Mutex::new(())))
    }

    pub(super) fn open_with_config_update(
        path: &Path,
        config_update: Arc<Mutex<()>>,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (mut rules, configured) = Self::load_configured_rules(path)?;
        let legacy_path = path.with_file_name("notifications.db");
        let migrate_legacy = !configured && legacy_path.is_file();
        if migrate_legacy {
            rules = load_legacy_rules(&legacy_path, path)?;
        }
        let metrics = Arc::new(NotificationMetrics::default());
        metrics
            .configured_rules
            .store(u64::try_from(rules.len())?, Ordering::Relaxed);
        let store = Self {
            config_path: Arc::new(path.to_owned()),
            metrics,
            config_update,
            state: Arc::new(Mutex::new(RuntimeState::new(rules))),
        };
        if migrate_legacy {
            let mut state = store.lock_state();
            store.persist_configured_rules(&mut state.rules)?;
            tracing::info!(
                event = "notification_rules_migrated",
                rule_count = state.rules.len()
            );
        }
        remove_legacy_database_family(&legacy_path)?;
        Ok(store)
    }

    fn load_configured_rules(path: &Path) -> anyhow::Result<(BTreeMap<String, RuleRecord>, bool)> {
        if !path.exists() {
            return Ok((BTreeMap::new(), false));
        }
        let root = crate::config::load_configuration_table(path)?;
        let Some(value) = root.get("notifications") else {
            return Ok((BTreeMap::new(), false));
        };
        let configured: NotificationConfiguration = value.clone().try_into()?;
        if configured.rules.len() > MAX_RULES {
            anyhow::bail!("notification configuration exceeds {MAX_RULES} rules");
        }
        let mut rules = BTreeMap::new();
        for persisted in configured.rules {
            validate_persisted_rule(path, &persisted)?;
            let record = RuleRecord {
                id: persisted.id,
                owner_id: persisted.owner_id,
                active: persisted.active,
                active_revision: persisted.active_revision,
                draft: persisted.draft,
                draft_revision: persisted.draft_revision,
                created_at_ms: persisted.created_at_ms,
                updated_at_ms: persisted.updated_at_ms,
                last_match_at_ms: None,
                last_delivery_at_ms: None,
            };
            if rules.insert(record.id.clone(), record).is_some() {
                anyhow::bail!("notification configuration contains duplicate rule IDs");
            }
        }
        Ok((rules, true))
    }

    fn restore_rule(
        rules: &mut BTreeMap<String, RuleRecord>,
        rule_id: &str,
        previous: Option<RuleRecord>,
    ) {
        match previous {
            Some(previous) => {
                rules.insert(rule_id.to_owned(), previous);
            }
            None => {
                rules.remove(rule_id);
            }
        }
    }

    pub(super) fn save_draft(
        &self,
        mut draft: Rule,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        validate_draft_identity(&draft)?;
        let mut state = self.lock_state();
        let previous = state.rules.get(&draft.id).cloned();
        let next_draft_revision = match &previous {
            Some(existing) => {
                ensure_owner(existing, &draft.owner_id)?;
                ensure_revisions(existing, existing.active_revision, expected_draft_revision)?;
                existing
                    .draft_revision
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("notification draft revision is exhausted"))?
            }
            None if expected_draft_revision == 0 && state.rules.len() < MAX_RULES => 1,
            None if expected_draft_revision == 0 => {
                anyhow::bail!("notification configuration exceeds {MAX_RULES} rules")
            }
            None => {
                return Err(RuleStoreError::Conflict {
                    active_revision: 0,
                    draft_revision: 0,
                }
                .into());
            }
        };
        draft.revision = next_draft_revision;
        serialize_rule(&draft)?;
        let owner_id = draft.owner_id.clone();
        let record = RuleRecord {
            id: draft.id.clone(),
            owner_id: draft.owner_id.clone(),
            active: previous.as_ref().and_then(|record| record.active.clone()),
            active_revision: previous.as_ref().map_or(0, |record| record.active_revision),
            draft,
            draft_revision: next_draft_revision,
            created_at_ms: previous
                .as_ref()
                .map_or(now_ms, |record| record.created_at_ms),
            updated_at_ms: now_ms,
            last_match_at_ms: previous.as_ref().and_then(|record| record.last_match_at_ms),
            last_delivery_at_ms: previous
                .as_ref()
                .and_then(|record| record.last_delivery_at_ms),
        };
        let record_id = record.id.clone();
        state.rules.insert(record_id.clone(), record);
        if let Err(error) = self.persist_configured_rules(&mut state.rules) {
            Self::restore_rule(&mut state.rules, &record_id, previous);
            return Err(error);
        }
        self.metrics
            .configured_rules
            .store(u64::try_from(state.rules.len())?, Ordering::Relaxed);
        tracing::info!(
            event = "notification_rule_draft_saved",
            rule_id = %record_id,
            owner_id = %owner_id,
            draft_revision = next_draft_revision
        );
        let record = state
            .rules
            .get(&record_id)
            .cloned()
            .expect("the persisted notification rule must remain in memory");
        self.resolve_record_destinations(record)
    }

    pub(super) fn activate(
        &self,
        rule_id: &str,
        owner_id: &str,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        let mut state = self.lock_state();
        let previous = state
            .rules
            .get(rule_id)
            .cloned()
            .ok_or(RuleStoreError::NotFound)?;
        ensure_owner(&previous, owner_id)?;
        ensure_revisions(&previous, expected_active_revision, expected_draft_revision)?;
        let mut active = self.resolve_rule_destinations(&previous.draft)?;
        active.validate()?;
        let active_revision = previous
            .active_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("notification active revision is exhausted"))?;
        let mut record = previous.clone();
        active.revision = active_revision;
        record.active = Some(active);
        record.active_revision = active_revision;
        record.updated_at_ms = now_ms;
        state.rules.insert(rule_id.to_owned(), record);
        if let Err(error) = self.persist_configured_rules(&mut state.rules) {
            state.rules.insert(rule_id.to_owned(), previous);
            return Err(error);
        }
        let record = state
            .rules
            .get(rule_id)
            .cloned()
            .expect("the activated notification rule must remain in memory");
        let cancelled = state.cancel_disabled_outbox(rule_id, record.active.as_ref(), now_ms);
        decrement_counter(&self.metrics.pending_deliveries, cancelled);
        tracing::info!(
            event = "notification_rule_activated",
            rule_id,
            owner_id,
            active_revision
        );
        self.resolve_record_destinations(record)
    }

    pub(super) fn delete(
        &self,
        rule_id: &str,
        owner_id: &str,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let mut state = self.lock_state();
        let existing = state
            .rules
            .get(rule_id)
            .cloned()
            .ok_or(RuleStoreError::NotFound)?;
        ensure_owner(&existing, owner_id)?;
        ensure_revisions(&existing, expected_active_revision, expected_draft_revision)?;
        state.rules.remove(rule_id);
        if let Err(error) = self.persist_configured_rules(&mut state.rules) {
            state.rules.insert(rule_id.to_owned(), existing);
            return Err(error);
        }
        self.metrics
            .configured_rules
            .store(u64::try_from(state.rules.len())?, Ordering::Relaxed);
        let cancelled = state.cancel_disabled_outbox(rule_id, None, now_ms);
        decrement_counter(&self.metrics.pending_deliveries, cancelled);
        tracing::info!(event = "notification_rule_deleted", rule_id, owner_id);
        Ok(())
    }

    pub(super) fn rules(&self, owner_id: &str) -> anyhow::Result<Vec<RuleRecord>> {
        let mut records = self
            .lock_state()
            .rules
            .values()
            .filter(|record| record.owner_id == owner_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_unstable_by(|left, right| {
            left.draft
                .name
                .to_lowercase()
                .cmp(&right.draft.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        records
            .into_iter()
            .map(|record| self.resolve_record_destinations(record))
            .collect()
    }

    fn persist_configured_rules(
        &self,
        rules: &mut BTreeMap<String, RuleRecord>,
    ) -> anyhow::Result<()> {
        let _config_update = self
            .config_update
            .lock()
            .expect("configuration update mutex must not be poisoned");
        let mut canonical_rules = rules.clone();
        let mut secret_values = BTreeMap::new();
        for record in canonical_rules.values_mut() {
            if let Some(active) = &mut record.active {
                externalize_destinations(
                    &self.config_path,
                    &record.id,
                    "ACTIVE",
                    active,
                    &mut secret_values,
                )?;
            }
            externalize_destinations(
                &self.config_path,
                &record.id,
                "DRAFT",
                &mut record.draft,
                &mut secret_values,
            )?;
        }
        let configuration = NotificationConfiguration {
            rules: canonical_rules
                .values()
                .cloned()
                .map(PersistedRuleRecord::from)
                .collect(),
        };
        let mut root = if self.config_path.exists() {
            crate::config::load_configuration_table(&self.config_path)?
        } else {
            toml::Table::new()
        };
        if configuration.rules.is_empty() {
            root.remove("notifications");
        } else {
            root.insert(
                "notifications".to_owned(),
                toml::Value::try_from(configuration)?,
            );
        }
        crate::config::write_configuration_table_with_managed_secrets(
            &self.config_path,
            &root,
            NOTIFICATION_SECRET_PREFIX,
            &secret_values,
        )?;
        *rules = canonical_rules;
        Ok(())
    }

    pub(super) fn lock_state(&self) -> MutexGuard<'_, RuntimeState> {
        self.state
            .lock()
            .expect("notification runtime state mutex must not be poisoned")
    }

    pub(super) fn resolve_rule_destinations(&self, rule: &Rule) -> anyhow::Result<Rule> {
        resolve_rule_destinations(&self.config_path, rule)
    }

    fn resolve_record_destinations(&self, mut record: RuleRecord) -> anyhow::Result<RuleRecord> {
        record.active = record
            .active
            .as_ref()
            .map(|active| self.resolve_rule_destinations(active))
            .transpose()?;
        record.draft = self.resolve_rule_destinations(&record.draft)?;
        Ok(record)
    }

    #[cfg(test)]
    pub(super) fn rule(&self, rule_id: &str) -> anyhow::Result<Option<RuleRecord>> {
        Ok(self.lock_state().rules.get(rule_id).cloned())
    }
}

fn validate_persisted_rule(path: &Path, record: &PersistedRuleRecord) -> anyhow::Result<()> {
    if record.id != record.draft.id || record.owner_id != record.draft.owner_id {
        anyhow::bail!("notification rule identity does not match its draft");
    }
    if record.draft_revision == 0 || record.draft.revision != record.draft_revision {
        anyhow::bail!("notification draft revision is invalid");
    }
    validate_draft_identity(&record.draft)?;
    let Some(active) = &record.active else {
        if record.active_revision == 0 {
            return Ok(());
        }
        anyhow::bail!("notification active rule metadata is invalid");
    };
    if record.active_revision == 0 {
        anyhow::bail!("notification active rule revision is invalid");
    }
    if active.id != record.id || active.owner_id != record.owner_id {
        anyhow::bail!("notification active rule identity is invalid");
    }
    if active.revision != record.active_revision {
        anyhow::bail!("notification active rule metadata is invalid");
    }
    resolve_rule_destinations(path, active)?.validate()
}

pub(super) fn validate_configuration(path: &Path, root: &toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get("notifications") else {
        return Ok(());
    };
    let configured: NotificationConfiguration = value.clone().try_into()?;
    if configured.rules.len() > MAX_RULES {
        anyhow::bail!("notification configuration exceeds {MAX_RULES} rules");
    }
    let mut ids = BTreeMap::new();
    for record in configured.rules {
        validate_persisted_rule(path, &record)?;
        if ids.insert(record.id.clone(), ()).is_some() {
            anyhow::bail!("notification configuration contains duplicate rule IDs");
        }
    }
    Ok(())
}

fn load_legacy_rules(
    database_path: &Path,
    config_path: &Path,
) -> anyhow::Result<BTreeMap<String, RuleRecord>> {
    let path = database_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy notification database path is not valid UTF-8"))?;
    let database = pollster::block_on(turso::Builder::new_local(path).build())?;
    let connection = database.connect()?;
    let records = pollster::block_on(async {
        let mut rows = connection
            .query(
                "SELECT id, owner_id, active_revision, active_json, draft_revision,
                        draft_json, created_at_ms, updated_at_ms
                 FROM notification_rules ORDER BY id LIMIT ?1",
                turso::params![i64::try_from(MAX_RULES + 1)?],
            )
            .await?;
        let mut records = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            let active_revision = row
                .get::<Option<i64>>(2)?
                .map(u64::try_from)
                .transpose()?
                .unwrap_or(0);
            let active = row
                .get::<Option<String>>(3)?
                .map(|value| serde_json::from_str(&value))
                .transpose()?;
            let persisted = PersistedRuleRecord {
                id: row.get(0)?,
                owner_id: row.get(1)?,
                active,
                active_revision,
                draft_revision: u64::try_from(row.get::<i64>(4)?)?,
                draft: serde_json::from_str(&row.get::<String>(5)?)?,
                created_at_ms: row.get(6)?,
                updated_at_ms: row.get(7)?,
            };
            validate_persisted_rule(config_path, &persisted)?;
            let record = RuleRecord {
                id: persisted.id,
                owner_id: persisted.owner_id,
                active: persisted.active,
                active_revision: persisted.active_revision,
                draft: persisted.draft,
                draft_revision: persisted.draft_revision,
                created_at_ms: persisted.created_at_ms,
                updated_at_ms: persisted.updated_at_ms,
                last_match_at_ms: None,
                last_delivery_at_ms: None,
            };
            if records.insert(record.id.clone(), record).is_some() {
                anyhow::bail!("legacy notification database contains duplicate rule IDs");
            }
        }
        anyhow::Ok(records)
    })?;
    if records.len() > MAX_RULES {
        anyhow::bail!("legacy notification database exceeds {MAX_RULES} rules");
    }
    Ok(records)
}

fn remove_legacy_database_family(path: &Path) -> std::io::Result<()> {
    remove_file_if_exists(path)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        remove_file_if_exists(&PathBuf::from(sidecar))?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn resolve_rule_destinations(path: &Path, rule: &Rule) -> anyhow::Result<Rule> {
    let mut resolved = rule.clone();
    for action in &mut resolved.actions {
        action.destination = crate::config::resolve_secret_references(path, &action.destination)?;
    }
    Ok(resolved)
}

fn externalize_destinations(
    config_path: &Path,
    rule_id: &str,
    version: &str,
    rule: &mut Rule,
    secret_values: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for (index, action) in rule.actions.iter_mut().enumerate() {
        if action.destination.is_empty() {
            continue;
        }
        if let Some(key) = managed_secret_key(&action.destination) {
            let value = crate::config::resolve_secret_references(config_path, &action.destination)?;
            secret_values.insert(key.to_owned(), value);
            continue;
        }
        if crate::config::is_secret_reference(&action.destination) {
            continue;
        }
        let key = notification_secret_key(rule_id, version, rule.revision, index);
        let value = std::mem::replace(&mut action.destination, format!("{{secret:{key}}}"));
        secret_values.insert(key, value);
    }
    Ok(())
}

fn managed_secret_key(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("{secret:")
        .and_then(|value| value.strip_suffix('}'))
        .filter(|key| key.starts_with(NOTIFICATION_SECRET_PREFIX))
}

fn notification_secret_key(
    rule_id: &str,
    version: &str,
    revision: u64,
    action_index: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rule_id.as_bytes());
    hasher.update([0]);
    hasher.update(version.as_bytes());
    hasher.update([0]);
    hasher.update(revision.to_be_bytes());
    hasher.update([0]);
    hasher.update(action_index.to_be_bytes());
    let digest = hasher.finalize();
    let mut key = String::from(NOTIFICATION_SECRET_PREFIX);
    for byte in &digest[..12] {
        write!(&mut key, "{byte:02X}").expect("writing to a String cannot fail");
    }
    key
}

impl From<RuleRecord> for PersistedRuleRecord {
    fn from(record: RuleRecord) -> Self {
        Self {
            id: record.id,
            owner_id: record.owner_id,
            active: record.active,
            active_revision: record.active_revision,
            draft: record.draft,
            draft_revision: record.draft_revision,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
        }
    }
}

fn serialize_rule(rule: &Rule) -> anyhow::Result<String> {
    let serialized = serde_json::to_string(rule)?;
    if serialized.len() > MAX_RULE_JSON_BYTES {
        anyhow::bail!("notification rule exceeds {MAX_RULE_JSON_BYTES} serialized bytes");
    }
    Ok(serialized)
}

fn validate_draft_identity(rule: &Rule) -> anyhow::Result<()> {
    if rule.id.is_empty() || rule.owner_id.is_empty() {
        anyhow::bail!("notification draft requires a rule ID and owner");
    }
    serialize_rule(rule).map(|_| ())
}

fn ensure_owner(record: &RuleRecord, owner_id: &str) -> anyhow::Result<()> {
    if record.owner_id == owner_id {
        Ok(())
    } else {
        Err(RuleStoreError::NotAuthorized.into())
    }
}

fn ensure_revisions(
    record: &RuleRecord,
    expected_active_revision: u64,
    expected_draft_revision: u64,
) -> anyhow::Result<()> {
    if record.active_revision == expected_active_revision
        && record.draft_revision == expected_draft_revision
    {
        Ok(())
    } else {
        Err(RuleStoreError::Conflict {
            active_revision: record.active_revision,
            draft_revision: record.draft_revision,
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::notifications::{
        Cooldown, CooldownScope, Stage,
        model::{
            Action, AttachmentPolicy, Channel, EnrichmentPolicy, FailurePolicy, Filter, Schedule,
            Template, Trigger,
        },
        state::{MAX_HISTORY_EVENTS, PendingHistoryEntry},
    };

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn draft(id: &str) -> Rule {
        Rule {
            id: id.to_owned(),
            name: "Front door person".to_owned(),
            enabled: true,
            revision: 0,
            owner_id: "owner-1".to_owned(),
            triggers: vec![Trigger::EventCreated, Trigger::EventUpdated],
            filter: Filter::default(),
            schedule: Schedule {
                timezone: "UTC".to_owned(),
                active_windows: Vec::new(),
                quiet_hours: None,
            },
            cooldowns: vec![Cooldown {
                scope: CooldownScope::Event,
                duration_ms: 30_000,
            }],
            rate_limits: Vec::new(),
            critical_bypass: None,
            enrichment: EnrichmentPolicy {
                deadline_ms: 10_000,
                maximum_revisions: 4,
                maximum_attempts: 2,
                maximum_attachment_bytes: 1_048_576,
                wake_after_deadline: false,
            },
            actions: vec![Action {
                enabled: true,
                channel: Channel::Browser,
                destination: String::new(),
                template: Template {
                    title: "Person detected".to_owned(),
                    body: "Open {{notification.deep_link}}".to_owned(),
                },
                attachment: AttachmentPolicy::WhenAvailable,
                allow_second_delivery: false,
            }],
            failure: FailurePolicy {
                maximum_attempts: 3,
                maximum_retry_interval_ms: 60_000,
                expiry_ms: 3_600_000,
            },
        }
    }

    #[test]
    fn activation_is_atomic_and_preserves_an_invalid_draft() {
        let directory = test_dir("notification-rule-activation");
        let config_path = directory.join("config.toml");
        let store = Store::open(&config_path).unwrap();

        let saved = store.save_draft(draft("rule-1"), 0, 1_000).unwrap();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        assert_eq!(active.active_revision, 1);

        let mut invalid = active.draft;
        invalid.actions.clear();
        let saved = store
            .save_draft(invalid, active.draft_revision, 3_000)
            .unwrap();
        assert!(
            store
                .activate(
                    "rule-1",
                    "owner-1",
                    saved.active_revision,
                    saved.draft_revision,
                    4_000,
                )
                .is_err()
        );

        drop(store);
        let reopened = Store::open(&config_path).unwrap();
        let persisted = reopened.rule("rule-1").unwrap().unwrap();
        assert_eq!(persisted.active_revision, 1);
        assert_eq!(persisted.active.unwrap().actions.len(), 1);
        assert!(persisted.draft.actions.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_draft_write_reports_both_current_revisions() {
        let directory = test_dir("notification-rule-conflict");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let saved = store.save_draft(draft("rule-1"), 0, 1_000).unwrap();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();

        let error = store.save_draft(draft("rule-1"), 0, 3_000).unwrap_err();
        assert_eq!(
            error.downcast_ref::<RuleStoreError>(),
            Some(&RuleStoreError::Conflict {
                active_revision: active.active_revision,
                draft_revision: active.draft_revision,
            })
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rules_persist_in_config_without_a_notification_database() {
        let directory = test_dir("notification-rules-config");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();

        let store = Store::open(&config_path).unwrap();
        let saved = store.save_draft(draft("rule-1"), 0, 1_000).unwrap();
        store
            .activate(
                "rule-1",
                "owner-1",
                saved.active_revision,
                saved.draft_revision,
                2_000,
            )
            .unwrap();
        drop(store);

        let reopened = Store::open(&config_path).unwrap();
        let rules = reopened.rules("owner-1").unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "rule-1");
        assert_eq!(rules[0].active_revision, 1);
        assert_eq!(reopened.metrics.snapshot().configured_rules, 1);
        assert!(!directory.join("notifications.db").exists());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn shared_configuration_lock_preserves_an_external_edit() {
        let directory = test_dir("notification-shared-config-lock");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        let config_update = Arc::new(Mutex::new(()));
        let store = Store::open_with_config_update(&config_path, config_update.clone()).unwrap();
        let guard = config_update.lock().unwrap();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            result_tx
                .send(store.save_draft(draft("rule-1"), 0, 1_000))
                .unwrap();
        });
        let mut root = crate::config::load_configuration_table(&config_path).unwrap();
        root.insert("port".to_owned(), toml::Value::Integer(4_321));
        crate::config::write_configuration_table(&config_path, &root).unwrap();
        drop(guard);

        result_rx.recv().unwrap().unwrap();
        thread.join().unwrap();
        let root = crate::config::load_configuration_table(&config_path).unwrap();
        assert_eq!(
            root.get("port").and_then(toml::Value::as_integer),
            Some(4_321)
        );
        assert!(root.contains_key("notifications"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn provider_destinations_are_stored_as_managed_secret_references() {
        let directory = test_dir("notification-rule-secrets");
        let config_path = directory.join("config.toml");
        let destination = serde_json::json!({
            "application_token": "a23456789012345678901234567890",
            "user_key": "u23456789012345678901234567890",
            "priority": 0
        })
        .to_string();
        let mut rule = draft("rule-1");
        rule.actions[0].channel = Channel::Push;
        rule.actions[0].destination.clone_from(&destination);

        let store = Store::open(&config_path).unwrap();
        let saved = store.save_draft(rule, 0, 1_000).unwrap();
        store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        drop(store);

        let config = std::fs::read_to_string(&config_path).unwrap();
        let secrets = std::fs::read_to_string(crate::config::secrets_path(&config_path)).unwrap();
        assert!(!config.contains("a23456789012345678901234567890"));
        assert!(!config.contains("u23456789012345678901234567890"));
        assert!(config.contains("{secret:KEEPPEEK_NOTIFICATION_"));
        assert!(secrets.contains("a23456789012345678901234567890"));
        assert!(secrets.contains("u23456789012345678901234567890"));

        let reopened = Store::open(&config_path).unwrap();
        let restored = reopened.rules("owner-1").unwrap().pop().unwrap();
        let resolved = reopened
            .resolve_rule_destinations(restored.active.as_ref().unwrap())
            .unwrap();
        assert_eq!(resolved.actions[0].destination, destination);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn draft_destination_changes_do_not_modify_the_active_rule() {
        let directory = test_dir("notification-draft-secret-isolation");
        let config_path = directory.join("config.toml");
        let mut initial = draft("rule-1");
        initial.actions[0].channel = Channel::Webhook;
        initial.actions[0].destination = "https://initial.example/hook".to_owned();
        let store = Store::open(&config_path).unwrap();
        let saved = store.save_draft(initial, 0, 1_000).unwrap();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        let mut changed = active.draft;
        changed.actions[0].destination = "https://changed.example/hook".to_owned();
        let saved = store
            .save_draft(changed, active.draft_revision, 3_000)
            .unwrap();

        let active = store
            .resolve_rule_destinations(saved.active.as_ref().unwrap())
            .unwrap();
        let draft = store.resolve_rule_destinations(&saved.draft).unwrap();
        assert_eq!(
            active.actions[0].destination,
            "https://initial.example/hook"
        );
        assert_eq!(draft.actions[0].destination, "https://changed.example/hook");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn draft_secret_rotation_commits_a_new_reference_before_removing_the_old_one() {
        let directory = test_dir("notification-draft-secret-rotation");
        let config_path = directory.join("config.toml");
        let mut initial = draft("rule-1");
        initial.actions[0].channel = Channel::Webhook;
        initial.actions[0].destination = "https://initial.example/hook".to_owned();
        let store = Store::open(&config_path).unwrap();
        let saved = store.save_draft(initial, 0, 1_000).unwrap();
        let first_draft_reference = store.lock_state().rules["rule-1"].draft.actions[0]
            .destination
            .clone();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        let active_reference = store.lock_state().rules["rule-1"]
            .active
            .as_ref()
            .unwrap()
            .actions[0]
            .destination
            .clone();
        let mut changed = active.draft;
        changed.actions[0].destination = "https://changed.example/hook".to_owned();
        store
            .save_draft(changed, active.draft_revision, 3_000)
            .unwrap();
        let state = store.lock_state();
        let next_draft_reference = state.rules["rule-1"].draft.actions[0].destination.clone();
        let retained_active_reference = state.rules["rule-1"].active.as_ref().unwrap().actions[0]
            .destination
            .clone();
        drop(state);

        assert_ne!(next_draft_reference, first_draft_reference);
        assert_eq!(retained_active_reference, active_reference);
        let secrets: BTreeMap<String, String> = toml::from_str(
            &std::fs::read_to_string(crate::config::secrets_path(&config_path)).unwrap(),
        )
        .unwrap();
        let first_draft_key = managed_secret_key(&first_draft_reference).unwrap();
        let active_key = managed_secret_key(&active_reference).unwrap();
        let next_draft_key = managed_secret_key(&next_draft_reference).unwrap();
        assert!(!secrets.contains_key(first_draft_key));
        assert_eq!(
            secrets.get(active_key).map(String::as_str),
            Some("https://initial.example/hook")
        );
        assert_eq!(
            secrets.get(next_draft_key).map(String::as_str),
            Some("https://changed.example/hook")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn configuration_validation_rejects_malformed_notification_rules() {
        let directory = test_dir("notification-rule-validation");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[notifications]\nrules = \"invalid\"\n").unwrap();
        let root = crate::config::load_configuration_table(&config_path).unwrap();

        assert!(crate::config::validate_configuration_table(&config_path, &root).is_err());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_notification_database_migrates_rules_then_is_removed() {
        let directory = test_dir("notification-rule-migration");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        let legacy_path = directory.join("notifications.db");
        let legacy_path_text = legacy_path.to_str().unwrap();
        let database =
            pollster::block_on(turso::Builder::new_local(legacy_path_text).build()).unwrap();
        let connection = database.connect().unwrap();
        let mut rule = draft("rule-1");
        rule.revision = 1;
        let definition = serde_json::to_string(&rule).unwrap();
        pollster::block_on(async {
            connection
                .execute_batch(
                    "CREATE TABLE notification_rules (
                         id TEXT PRIMARY KEY,
                         owner_id TEXT NOT NULL,
                         name TEXT NOT NULL,
                         active_revision INTEGER,
                         active_json TEXT,
                         draft_revision INTEGER NOT NULL,
                         draft_json TEXT NOT NULL,
                         created_at_ms INTEGER NOT NULL,
                         updated_at_ms INTEGER NOT NULL,
                         last_match_at_ms INTEGER,
                         last_delivery_at_ms INTEGER
                     );",
                )
                .await
                .unwrap();
            connection
                .execute(
                    "INSERT INTO notification_rules (
                         id, owner_id, name, active_revision, active_json,
                         draft_revision, draft_json, created_at_ms, updated_at_ms
                     ) VALUES ('rule-1', 'owner-1', 'rule-1', NULL, NULL, 1, ?1, 1000, 1000)",
                    turso::params![definition],
                )
                .await
                .unwrap();
        });
        drop(connection);
        drop(database);

        let store = Store::open(&config_path).unwrap();
        assert_eq!(store.rules("owner-1").unwrap().len(), 1);
        assert!(!legacy_path.exists());
        assert!(!legacy_path.with_extension("db-wal").exists());
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("[notifications")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runtime_history_pruning_keeps_the_newest_entries() {
        let directory = test_dir("notification-history-pruning");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut state = store.lock_state();
        let last_revision = u64::try_from(MAX_HISTORY_EVENTS).unwrap() + 1;
        for revision in 1..=last_revision {
            state.push_history(PendingHistoryEntry {
                logical_id: "logical-1",
                rule_id: "rule-1",
                revision,
                stage: Stage::Preliminary,
                outcome: "created",
                reason: None,
                occurred_at_ms: i64::try_from(revision).unwrap(),
                next_eligible_at_ms: None,
            });
        }
        assert_eq!(state.history.len(), MAX_HISTORY_EVENTS);
        assert_eq!(state.history.front().unwrap().revision, 2);
        assert_eq!(state.history.back().unwrap().revision, last_revision);
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
