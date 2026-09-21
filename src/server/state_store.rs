use super::state_store_durable::DurableStore;
use super::state_store_watch::{WatchEvent, WatchEventKind};
use super::{ApiPrincipal, ControlCommandError, ServerState};
use crate::{
    access::AccessRole,
    api::proto::{self, ok as control_ok, state_store_command, state_store_result},
    webrtc::SessionId,
};
use prost::Message as _;
use prost_types::{Duration, Struct, Timestamp};
use std::{
    collections::{HashMap, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const CAPABILITY_ID: &str = "keeppeek.state-store.v1";
pub(super) const MAX_NAMESPACE_CHARS: usize = 128;
pub(super) const MAX_KEY_CHARS: usize = 256;
pub(super) const MAX_SCHEMA_CHARS: usize = 128;
pub(super) const MAX_VALUE_BYTES: usize = 64 * 1_024;
pub(super) const MAX_ENTRIES_PER_NAMESPACE: usize = 1_024;
const MAX_PENDING_EXPIRIES: usize = 4_096;
pub(super) const MAX_NAMESPACES: usize = 256;
pub(super) const MAX_TOTAL_VALUE_BYTES: u64 = 64 * 1_024 * 1_024;
pub(super) const MAX_WATCH_SNAPSHOT_ENTRIES: usize = 64;
pub(super) const MAX_WATCH_SNAPSHOT_BYTES: usize = 64 * 1_024;
pub(super) const MIN_TTL_MS: u64 = 1_000;
pub(super) const MAX_TTL_MS: u64 = 86_400_000;

#[cfg(test)]
pub(in crate::server) mod commit_fault {
    use std::sync::{Mutex, PoisonError};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::server) enum Path {
        Durable,
        Settings,
    }

    static ARMED: Mutex<Option<(Path, String, String)>> = Mutex::new(None);

    pub(in crate::server) fn arm(path: Path, namespace: &str, key: &str) {
        *ARMED.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((path, namespace.to_owned(), key.to_owned()));
    }

    pub(in crate::server) fn take(path: Path, namespace: &str, key: &str) -> bool {
        let mut armed = ARMED.lock().unwrap_or_else(PoisonError::into_inner);
        let matches = matches!(&*armed, Some((armed_path, armed_ns, armed_key))
            if *armed_path == path && armed_ns == namespace && armed_key == key);
        if matches {
            *armed = None;
        }
        matches
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredEntry {
    pub namespace: String,
    pub key: String,
    pub schema: String,
    pub value: Struct,
    pub revision: u64,
    pub updated_ms: u64,
    pub expires_ms: Option<u64>,
    pub owner_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExpiredEntry {
    pub(super) namespace: String,
    pub(super) key: String,
    pub(super) revision: u64,
    pub(super) schema: String,
    pub(super) value: Struct,
    pub(super) owner_id: String,
    pub(super) expires_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    Namespace,
    Key,
    Schema,
    ValueMissing,
    ValueTooLarge,
    NamespaceFull,
    StoreFull,
    Ttl,
    WatchNotFound,
    WatchLimitExceeded,
    AckAhead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Conflict { current_revision: u64 },
    NotAuthorized,
    Invalid(Invalid),
    Storage(String),
}

#[derive(Debug, Default)]
pub(super) struct Registry {
    namespaces: HashMap<String, NamespaceState>,
    pending: VecDeque<ExpiredEntry>,
    pending_overflowed: bool,
    stored_bytes: u64,
}

#[derive(Debug, Default)]
struct NamespaceState {
    revision: u64,
    entries: HashMap<String, StoredRecord>,
}

#[derive(Clone, Debug)]
struct StoredRecord {
    schema: String,
    value: Struct,
    revision: u64,
    updated_ms: u64,
    expires_ms: Option<u64>,
    owner_id: String,
}

impl Registry {
    #[allow(
        clippy::too_many_arguments,
        reason = "put mirrors the wire, auth, and time-injection inputs one-to-one"
    )]
    pub(super) fn put(
        &mut self,
        namespace: &str,
        key: &str,
        schema: &str,
        value: Option<Struct>,
        expected_revision: Option<u64>,
        ttl: Option<Duration>,
        owner_id: &str,
        writer_admin: bool,
        now_ms: u64,
    ) -> Result<StoredEntry, Error> {
        let layout = validate_namespace(namespace)?;
        validate_key(key)?;
        validate_schema(schema)?;
        let value = value.ok_or(Error::Invalid(Invalid::ValueMissing))?;
        if value.encoded_len() > MAX_VALUE_BYTES {
            return Err(Error::Invalid(Invalid::ValueTooLarge));
        }
        let expires_ms = ttl
            .as_ref()
            .map(|ttl| ttl_expiry_ms(ttl, now_ms))
            .transpose()?;
        authorize_write(&layout, owner_id, writer_admin)?;
        super::state_store_schema::validate_value(schema, &value)?;
        self.expire_key_if_due(namespace, key, now_ms);
        let current = self
            .namespaces
            .get(namespace)
            .and_then(|state| state.entries.get(key))
            .map(|record| record.revision);
        check_expected(expected_revision, current)?;
        if current.is_none() {
            if !self.namespaces.contains_key(namespace) {
                if self.namespaces.len() >= MAX_NAMESPACES {
                    return Err(Error::Invalid(Invalid::StoreFull));
                }
            } else {
                self.reclaim_namespace(namespace, now_ms);
                if self.namespaces[namespace].entries.len() >= MAX_ENTRIES_PER_NAMESPACE {
                    return Err(Error::Invalid(Invalid::NamespaceFull));
                }
            }
        }
        let old_bytes = self
            .namespaces
            .get(namespace)
            .and_then(|state| state.entries.get(key))
            .map(|record| value_bytes(&record.value))
            .unwrap_or(0);
        let total_bytes = self
            .stored_bytes
            .saturating_add(value_bytes(&value))
            .saturating_sub(old_bytes);
        if total_bytes > MAX_TOTAL_VALUE_BYTES {
            return Err(Error::Invalid(Invalid::StoreFull));
        }
        let state = self.namespaces.entry(namespace.to_owned()).or_default();
        state.revision = state
            .revision
            .checked_add(1)
            .expect("namespace revision overflow");
        let revision = state.revision;
        state.entries.insert(
            key.to_owned(),
            StoredRecord {
                schema: schema.to_owned(),
                value: value.clone(),
                revision,
                updated_ms: now_ms,
                expires_ms,
                owner_id: owner_id.to_owned(),
            },
        );
        self.stored_bytes = total_bytes;
        Ok(StoredEntry {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            schema: schema.to_owned(),
            value,
            revision,
            updated_ms: now_ms,
            expires_ms,
            owner_id: owner_id.to_owned(),
        })
    }

    pub(super) fn get(
        &mut self,
        namespace: &str,
        key: &str,
        owner_id: &str,
        reader_admin: bool,
        now_ms: u64,
    ) -> Result<StoredEntry, Error> {
        let layout = validate_namespace(namespace)?;
        authorize_read(&layout, owner_id, reader_admin)?;
        validate_key(key)?;
        self.expire_key_if_due(namespace, key, now_ms);
        let state = self.namespaces.get(namespace).ok_or(Error::NotFound)?;
        let record = state.entries.get(key).ok_or(Error::NotFound)?;
        Ok(StoredEntry {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            schema: record.schema.clone(),
            value: record.value.clone(),
            revision: record.revision,
            updated_ms: record.updated_ms,
            expires_ms: record.expires_ms,
            owner_id: record.owner_id.clone(),
        })
    }

    pub(super) fn delete(
        &mut self,
        namespace: &str,
        key: &str,
        expected_revision: Option<u64>,
        owner_id: &str,
        writer_admin: bool,
        now_ms: u64,
    ) -> Result<u64, Error> {
        let layout = validate_namespace(namespace)?;
        validate_key(key)?;
        authorize_write(&layout, owner_id, writer_admin)?;
        self.expire_key_if_due(namespace, key, now_ms);
        let state = self.namespaces.get_mut(namespace).ok_or(Error::NotFound)?;
        let current = state.entries.get(key).map(|record| record.revision);
        check_expected(expected_revision, current)?;
        current.ok_or(Error::NotFound)?;
        let removed = state.entries.remove(key).expect("checked key presence");
        self.stored_bytes = self
            .stored_bytes
            .saturating_sub(value_bytes(&removed.value));
        state.revision = state
            .revision
            .checked_add(1)
            .expect("namespace revision overflow");
        Ok(state.revision)
    }

    pub(super) fn expire_due(&mut self, now_ms: u64) -> ExpiredBatch {
        let mut due = Vec::new();
        for (namespace, state) in &self.namespaces {
            for (key, record) in &state.entries {
                if is_expired(record, now_ms) {
                    due.push((namespace.clone(), key.clone()));
                }
            }
        }
        due.sort_unstable();
        for (namespace, key) in due {
            self.expire_key_if_due(&namespace, &key, now_ms);
        }
        ExpiredBatch {
            entries: self.pending.drain(..).collect(),
            overflowed: std::mem::replace(&mut self.pending_overflowed, false),
        }
    }

    fn expire_key_if_due(&mut self, namespace: &str, key: &str, now_ms: u64) {
        let due = self.namespaces.get(namespace).is_some_and(|state| {
            state
                .entries
                .get(key)
                .is_some_and(|record| is_expired(record, now_ms))
        });
        if !due {
            return;
        }
        if let Some(state) = self.namespaces.get_mut(namespace)
            && let Some(record) = state.entries.remove(key)
        {
            state.revision = state
                .revision
                .checked_add(1)
                .expect("namespace revision overflow");
            self.stored_bytes = self.stored_bytes.saturating_sub(value_bytes(&record.value));
            let expired = ExpiredEntry {
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                revision: state.revision,
                schema: record.schema,
                value: record.value,
                owner_id: record.owner_id,
                expires_ms: record.expires_ms.unwrap_or(now_ms),
            };
            self.push_pending(expired);
        }
    }

    fn push_pending(&mut self, expired: ExpiredEntry) {
        if self.pending.len() >= MAX_PENDING_EXPIRIES {
            self.pending.pop_front();
            self.pending_overflowed = true;
        }
        self.pending.push_back(expired);
    }

    pub(super) fn drain_pending(&mut self) -> (Vec<ExpiredEntry>, bool) {
        (
            self.pending.drain(..).collect(),
            std::mem::replace(&mut self.pending_overflowed, false),
        )
    }

    pub(super) fn due_keys(&self, namespace: &str, key_prefix: &str, now_ms: u64) -> Vec<String> {
        let mut due: Vec<String> = self
            .namespaces
            .get(namespace)
            .map(|state| {
                state
                    .entries
                    .iter()
                    .filter(|(key, record)| {
                        key.starts_with(key_prefix)
                            && record
                                .expires_ms
                                .is_some_and(|expires_ms| expires_ms <= now_ms)
                    })
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default();
        due.sort_unstable();
        due
    }

    pub(super) fn apply_expirations(&mut self, entries: Vec<ExpiredEntry>) {
        for expired in entries {
            let removed_bytes = {
                let state = self
                    .namespaces
                    .entry(expired.namespace.clone())
                    .or_default();
                state.revision = state.revision.max(expired.revision);
                state
                    .entries
                    .remove(&expired.key)
                    .map(|record| value_bytes(&record.value))
            };
            if let Some(freed) = removed_bytes {
                self.stored_bytes = self.stored_bytes.saturating_sub(freed);
            }
            self.push_pending(expired);
        }
    }

    pub(super) fn import_namespace(
        &mut self,
        namespace: String,
        revision: u64,
        entries: Vec<StoredEntry>,
    ) {
        let state = self.namespaces.entry(namespace).or_default();
        state.revision = state.revision.max(revision);
        for entry in entries {
            self.stored_bytes = self.stored_bytes.saturating_add(value_bytes(&entry.value));
            state.entries.insert(
                entry.key.clone(),
                StoredRecord {
                    schema: entry.schema,
                    value: entry.value,
                    revision: entry.revision,
                    updated_ms: entry.updated_ms,
                    expires_ms: entry.expires_ms,
                    owner_id: entry.owner_id,
                },
            );
        }
    }

    pub(super) fn snapshot(
        &mut self,
        namespace: &str,
        key_prefix: &str,
        now_ms: u64,
    ) -> (u64, Vec<StoredEntry>) {
        let mut keys: Vec<String> = self
            .namespaces
            .get(namespace)
            .map(|state| {
                state
                    .entries
                    .keys()
                    .filter(|key| key.starts_with(key_prefix))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        keys.sort_unstable();
        for key in &keys {
            self.expire_key_if_due(namespace, key, now_ms);
        }
        let revision = self
            .namespaces
            .get(namespace)
            .map(|state| state.revision)
            .unwrap_or(0);
        let entries = keys
            .into_iter()
            .filter_map(|key| {
                self.namespaces
                    .get(namespace)
                    .and_then(|state| state.entries.get(&key))
                    .map(|record| StoredEntry {
                        namespace: namespace.to_owned(),
                        key,
                        schema: record.schema.clone(),
                        value: record.value.clone(),
                        revision: record.revision,
                        updated_ms: record.updated_ms,
                        expires_ms: record.expires_ms,
                        owner_id: record.owner_id.clone(),
                    })
            })
            .collect();
        (revision, entries)
    }

    fn reclaim_namespace(&mut self, namespace: &str, now_ms: u64) {
        let expired: Vec<String> = self
            .namespaces
            .get(namespace)
            .map(|state| {
                state
                    .entries
                    .iter()
                    .filter(|(_, record)| is_expired(record, now_ms))
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default();
        for key in expired {
            self.expire_key_if_due(namespace, &key, now_ms);
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ExpiredBatch {
    pub(super) entries: Vec<ExpiredEntry>,
    pub(super) overflowed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NamespaceLayout {
    System,
    Service,
    Group,
    User { owner: String },
}

pub(super) fn validate_namespace(namespace: &str) -> Result<NamespaceLayout, Error> {
    if namespace.len() > MAX_NAMESPACE_CHARS {
        return Err(Error::Invalid(Invalid::Namespace));
    }
    let inner = namespace
        .strip_suffix('/')
        .ok_or(Error::Invalid(Invalid::Namespace))?;
    let mut segments = inner.split('/');
    let top = segments.next().ok_or(Error::Invalid(Invalid::Namespace))?;
    let Some(scope) = segments.next() else {
        if top == "system" {
            return Ok(NamespaceLayout::System);
        }
        return Err(Error::Invalid(Invalid::Namespace));
    };
    if segments.next().is_some() {
        return Err(Error::Invalid(Invalid::Namespace));
    }
    if !is_namespace_segment(top) || !is_namespace_segment(scope) {
        return Err(Error::Invalid(Invalid::Namespace));
    }
    match top {
        "service" => Ok(NamespaceLayout::Service),
        "group" => Ok(NamespaceLayout::Group),
        "user" => Ok(NamespaceLayout::User {
            owner: scope.to_owned(),
        }),
        _ => Err(Error::Invalid(Invalid::Namespace)),
    }
}

fn is_namespace_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 64
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

pub(super) fn validate_key(key: &str) -> Result<(), Error> {
    if key.is_empty() || key.len() > MAX_KEY_CHARS {
        return Err(Error::Invalid(Invalid::Key));
    }
    let mut segments = key.split('/');
    let first = segments.next().ok_or(Error::Invalid(Invalid::Key))?;
    if !is_key_segment(first) {
        return Err(Error::Invalid(Invalid::Key));
    }
    for segment in segments {
        if !is_key_segment(segment) {
            return Err(Error::Invalid(Invalid::Key));
        }
    }
    Ok(())
}

fn is_key_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > 128 || segment == "." || segment == ".." {
        return false;
    }
    segment
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn validate_schema(schema: &str) -> Result<(), Error> {
    if schema.is_empty() || schema.len() > MAX_SCHEMA_CHARS {
        return Err(Error::Invalid(Invalid::Schema));
    }
    let valid = schema
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid(Invalid::Schema))
    }
}

pub(super) fn ttl_expiry_ms(ttl: &Duration, now_ms: u64) -> Result<u64, Error> {
    if ttl.nanos < 0 || ttl.nanos > 999_999_999 {
        return Err(Error::Invalid(Invalid::Ttl));
    }
    let seconds = u64::try_from(ttl.seconds).map_err(|_| Error::Invalid(Invalid::Ttl))?;
    let millis = seconds
        .checked_mul(1_000)
        .and_then(|base| {
            u64::try_from(ttl.nanos)
                .ok()
                .and_then(|nanos| base.checked_add(nanos / 1_000_000))
        })
        .ok_or(Error::Invalid(Invalid::Ttl))?;
    if millis < MIN_TTL_MS {
        return Err(Error::Invalid(Invalid::Ttl));
    }
    let clamped = millis.min(MAX_TTL_MS);
    now_ms
        .checked_add(clamped)
        .ok_or(Error::Invalid(Invalid::Ttl))
}

fn is_expired(record: &StoredRecord, now_ms: u64) -> bool {
    record
        .expires_ms
        .is_some_and(|expires_ms| expires_ms <= now_ms)
}

fn value_bytes(value: &Struct) -> u64 {
    u64::try_from(value.encoded_len()).unwrap_or(u64::MAX)
}

pub(super) fn authorize_write(
    layout: &NamespaceLayout,
    owner_id: &str,
    writer_admin: bool,
) -> Result<(), Error> {
    match layout {
        NamespaceLayout::System => Err(Error::NotAuthorized),
        NamespaceLayout::Service | NamespaceLayout::Group => {
            if writer_admin {
                Ok(())
            } else {
                Err(Error::NotAuthorized)
            }
        }
        NamespaceLayout::User { owner } => {
            if owner_id == owner || writer_admin {
                Ok(())
            } else {
                Err(Error::NotAuthorized)
            }
        }
    }
}

pub(super) fn authorize_read(
    layout: &NamespaceLayout,
    owner_id: &str,
    reader_admin: bool,
) -> Result<(), Error> {
    match layout {
        NamespaceLayout::System | NamespaceLayout::Service | NamespaceLayout::Group => {
            if reader_admin {
                Ok(())
            } else {
                Err(Error::NotAuthorized)
            }
        }
        NamespaceLayout::User { owner } => {
            if owner_id == owner || reader_admin {
                Ok(())
            } else {
                Err(Error::NotAuthorized)
            }
        }
    }
}

pub(super) fn handles(command: &proto::StateStoreCommand) -> bool {
    let namespace = match &command.action {
        Some(state_store_command::Action::Get(request)) => &request.namespace,
        Some(state_store_command::Action::Put(request)) => &request.namespace,
        Some(state_store_command::Action::Delete(request)) => &request.namespace,
        Some(state_store_command::Action::Watch(request)) => &request.namespace,
        Some(state_store_command::Action::Unwatch(_))
        | Some(state_store_command::Action::WatchAck(_))
        | None => return false,
    };
    validate_namespace(namespace).is_ok()
}

pub(super) const fn is_watch_lifecycle(command: &proto::StateStoreCommand) -> bool {
    matches!(
        command.action,
        Some(state_store_command::Action::Unwatch(_))
            | Some(state_store_command::Action::WatchAck(_))
    )
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if let Some(namespace) = command_namespace(&command)
        && super::state_store_settings::is_adapter_namespace(namespace)
    {
        return adapter_dispatch(state, session_id, principal, command);
    }
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => get(state, principal, request)?,
        Some(state_store_command::Action::Put(request)) => put(state, principal, request)?,
        Some(state_store_command::Action::Delete(request)) => delete(state, principal, request)?,
        Some(state_store_command::Action::Watch(request)) => {
            watch(state, session_id, principal, request)?
        }
        Some(state_store_command::Action::Unwatch(request)) => unwatch(state, session_id, request)?,
        Some(state_store_command::Action::WatchAck(request)) => {
            acknowledge(state, session_id, request)?
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "state store command has no action",
            ));
        }
    };
    Ok(control_ok::Result::StateStoreResult(result))
}

fn command_namespace(command: &proto::StateStoreCommand) -> Option<&str> {
    match &command.action {
        Some(state_store_command::Action::Get(request)) => Some(&request.namespace),
        Some(state_store_command::Action::Put(request)) => Some(&request.namespace),
        Some(state_store_command::Action::Delete(request)) => Some(&request.namespace),
        Some(state_store_command::Action::Watch(request)) => Some(&request.namespace),
        Some(state_store_command::Action::Unwatch(_))
        | Some(state_store_command::Action::WatchAck(_))
        | None => None,
    }
}

fn adapter_dispatch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    use super::state_store_settings as adapter;
    let Some(config_path) = state.camera_config_path.clone() else {
        return Err(registry_error(
            Error::Storage("state store settings are unavailable".to_owned()),
            "",
            "",
        ));
    };
    let admin = principal.role == AccessRole::Administrator;
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => {
            let entry = adapter::get(
                &state.config_update,
                &config_path,
                &request.namespace,
                &request.key,
                &principal.id(),
                admin,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
            entry_result(entry)
        }
        Some(state_store_command::Action::Put(request)) => {
            let value = request
                .value
                .ok_or_else(|| invalid("state value is required"))?;
            let now = now_ms();
            let _guard = adapter::lock_config(&state.config_update)
                .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
            let entry = adapter::put_locked(
                &config_path,
                &request.namespace,
                &request.key,
                &request.schema,
                Some(value),
                request.expected_revision,
                request.ttl,
                &principal.id(),
                admin,
                now,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
            let direct = WatchEvent {
                namespace: entry.namespace.clone(),
                key: entry.key.clone(),
                revision: entry.revision,
                kind: WatchEventKind::Put(entry.clone()),
            };
            publish_events(state, Some(direct), Vec::new(), false, now);
            entry_result(entry)
        }
        Some(state_store_command::Action::Delete(request)) => {
            let _guard = adapter::lock_config(&state.config_update)
                .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
            let revision = adapter::delete_locked(
                &config_path,
                &request.namespace,
                &request.key,
                request.expected_revision,
                &principal.id(),
                admin,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
            let direct = WatchEvent {
                namespace: request.namespace.clone(),
                key: request.key.clone(),
                revision,
                kind: WatchEventKind::Delete,
            };
            publish_events(state, Some(direct), Vec::new(), false, now_ms());
            deleted_result(request.namespace, request.key, revision)
        }
        Some(state_store_command::Action::Watch(request)) => {
            super::validate_client_id(&request.watch_id, "watch ID")?;
            if !request.key_prefix.is_empty() {
                validate_key_prefix(&request.key_prefix)
                    .map_err(|error| registry_error(error, &request.namespace, ""))?;
            }
            let admin = principal.role == AccessRole::Administrator;
            let _guard = adapter::lock_config(&state.config_update)
                .map_err(|error| registry_error(error, &request.namespace, &request.watch_id))?;
            let (revision, entries) = adapter::snapshot(
                &config_path,
                &request.namespace,
                &request.key_prefix,
                &principal.id(),
                admin,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.watch_id))?;
            let message = proto::StateWatchSnapshot {
                watch_id: request.watch_id.clone(),
                namespace: request.namespace.clone(),
                key_prefix: request.key_prefix.clone(),
                snapshot_revision: revision,
                entries: entries.iter().map(proto_entry).collect(),
            };
            check_snapshot_bounds(
                message.entries.len(),
                message.encoded_len(),
                &request.namespace,
                &request.watch_id,
            )?;
            state
                .state_store_watches
                .register(
                    session_id,
                    request.namespace.clone(),
                    request.key_prefix.clone(),
                    request.watch_id.clone(),
                )
                .map_err(|error| registry_error(error, &request.namespace, &request.watch_id))?;
            proto::StateStoreResult {
                result: Some(state_store_result::Result::Watch(message)),
            }
        }
        Some(state_store_command::Action::Unwatch(request)) => unwatch(state, session_id, request)?,
        Some(state_store_command::Action::WatchAck(request)) => {
            acknowledge(state, session_id, request)?
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "state store command has no action",
            ));
        }
    };
    Ok(control_ok::Result::StateStoreResult(result))
}

fn get(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::GetState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let now = now_ms();
    let mut registry = lock_registry(state);
    if let Some(mut durable) = lock_durable(state) {
        durable
            .get(
                &request.namespace,
                &request.key,
                &principal.id(),
                principal.role == AccessRole::Administrator,
                now,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    }
    let outcome = registry
        .get(
            &request.namespace,
            &request.key,
            &principal.id(),
            principal.role == AccessRole::Administrator,
            now,
        )
        .map_err(|error| registry_error(error, &request.namespace, &request.key));
    let (expired, overflowed) = registry.drain_pending();
    publish_events(state, None, expired, overflowed, now);
    Ok(entry_result(outcome?))
}

fn put(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::PutState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let value = request
        .value
        .ok_or_else(|| invalid("state value is required"))?;
    let now = now_ms();
    let mut registry = lock_registry(state);
    if let Some(mut durable) = lock_durable(state) {
        durable
            .put(
                &request.namespace,
                &request.key,
                &request.schema,
                Some(value.clone()),
                request.expected_revision,
                request.ttl,
                &principal.id(),
                principal.role == AccessRole::Administrator,
                now,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    }
    let outcome = registry
        .put(
            &request.namespace,
            &request.key,
            &request.schema,
            Some(value),
            request.expected_revision,
            request.ttl,
            &principal.id(),
            principal.role == AccessRole::Administrator,
            now,
        )
        .map_err(|error| registry_error(error, &request.namespace, &request.key));
    let (expired, overflowed) = registry.drain_pending();
    let direct = outcome.as_ref().ok().map(|entry| WatchEvent {
        namespace: entry.namespace.clone(),
        key: entry.key.clone(),
        revision: entry.revision,
        kind: WatchEventKind::Put(entry.clone()),
    });
    publish_events(state, direct, expired, overflowed, now);
    Ok(entry_result(outcome?))
}

fn delete(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::DeleteState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let now = now_ms();
    let mut registry = lock_registry(state);
    if let Some(mut durable) = lock_durable(state) {
        durable
            .delete(
                &request.namespace,
                &request.key,
                request.expected_revision,
                &principal.id(),
                principal.role == AccessRole::Administrator,
                now,
            )
            .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    }
    let outcome = registry
        .delete(
            &request.namespace,
            &request.key,
            request.expected_revision,
            &principal.id(),
            principal.role == AccessRole::Administrator,
            now,
        )
        .map_err(|error| registry_error(error, &request.namespace, &request.key));
    let (expired, overflowed) = registry.drain_pending();
    let direct = outcome.as_ref().ok().map(|revision| WatchEvent {
        namespace: request.namespace.clone(),
        key: request.key.clone(),
        revision: *revision,
        kind: WatchEventKind::Delete,
    });
    publish_events(state, direct, expired, overflowed, now);
    let revision = outcome?;
    Ok(deleted_result(request.namespace, request.key, revision))
}

fn watch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::WatchState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    super::validate_client_id(&request.watch_id, "watch ID")?;
    let layout = validate_namespace(&request.namespace)
        .map_err(|error| registry_error(error, &request.namespace, ""))?;
    if !request.key_prefix.is_empty() {
        validate_key_prefix(&request.key_prefix)
            .map_err(|error| registry_error(error, &request.namespace, ""))?;
    }
    let admin = principal.role == AccessRole::Administrator;
    authorize_read(&layout, &principal.id(), admin)
        .map_err(|error| registry_error(error, &request.namespace, ""))?;
    let proto::WatchState {
        namespace,
        key_prefix,
        watch_id,
    } = request;
    let now = now_ms();
    let result = {
        let mut registry = lock_registry(state);
        let mut durable = lock_durable(state);
        let (stale, stale_overflowed) = registry.drain_pending();
        publish_events(state, None, stale, stale_overflowed, now);
        if let Some(durable) = durable.as_mut() {
            let due = registry.due_keys(&namespace, &key_prefix, now);
            let committed = durable
                .expire_keys(&namespace, &due, now)
                .map_err(|error| registry_error(error, &namespace, &watch_id))?;
            registry.apply_expirations(committed);
        }
        let (fresh, fresh_overflowed) = registry.drain_pending();
        publish_events(state, None, fresh, fresh_overflowed, now);
        let (revision, entries) = registry.snapshot(&namespace, &key_prefix, now);
        let message = proto::StateWatchSnapshot {
            watch_id: watch_id.clone(),
            namespace: namespace.clone(),
            key_prefix: key_prefix.clone(),
            snapshot_revision: revision,
            entries: entries.iter().map(proto_entry).collect(),
        };
        check_snapshot_bounds(
            message.entries.len(),
            message.encoded_len(),
            &namespace,
            &watch_id,
        )?;
        state
            .state_store_watches
            .register(session_id, namespace.clone(), key_prefix, watch_id.clone())
            .map_err(|error| registry_error(error, &namespace, &watch_id))?;
        proto::StateStoreResult {
            result: Some(state_store_result::Result::Watch(message)),
        }
    };
    Ok(result)
}

fn check_snapshot_bounds(
    entry_count: usize,
    encoded_bytes: usize,
    namespace: &str,
    watch_id: &str,
) -> Result<(), ControlCommandError> {
    if entry_count > MAX_WATCH_SNAPSHOT_ENTRIES || encoded_bytes > MAX_WATCH_SNAPSHOT_BYTES {
        return Err(registry_error(
            Error::Invalid(Invalid::ValueTooLarge),
            namespace,
            watch_id,
        ));
    }
    Ok(())
}

fn unwatch(
    state: &ServerState,
    session_id: SessionId,
    request: proto::UnwatchState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    state
        .state_store_watches
        .unregister(session_id, &request.watch_id)
        .map_err(|error| registry_error(error, "", &request.watch_id))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Unwatched(
            proto::StateUnwatchResult {
                watch_id: request.watch_id,
            },
        )),
    })
}

fn acknowledge(
    state: &ServerState,
    session_id: SessionId,
    request: proto::WatchStateAck,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let accepted = state
        .state_store_watches
        .acknowledge(
            session_id,
            &request.watch_id,
            request.applied_sequence,
            now_ms(),
        )
        .map_err(|(namespace, error)| registry_error(error, &namespace, &request.watch_id))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::WatchAck(
            proto::StateWatchAckResult {
                watch_id: request.watch_id,
                applied_sequence: accepted,
            },
        )),
    })
}

pub(super) fn validate_key_prefix(prefix: &str) -> Result<(), Error> {
    let stripped = prefix.strip_suffix('/').unwrap_or(prefix);
    if stripped.is_empty() {
        return Err(Error::Invalid(Invalid::Key));
    }
    validate_key(stripped)
}

pub(super) fn expire_leases(state: &ServerState, now_ms: u64) {
    let mut registry = lock_registry(state);
    if let Some(mut durable) = lock_durable(state)
        && let Err(error) = durable.expire_due(now_ms)
    {
        tracing::warn!(
            ?error,
            "state-store durable expiry failed; memory expiry skipped"
        );
        return;
    }
    let batch = registry.expire_due(now_ms);
    publish_events(state, None, batch.entries, batch.overflowed, now_ms);
}

fn publish_events(
    state: &ServerState,
    direct: Option<WatchEvent>,
    expired: Vec<ExpiredEntry>,
    pending_overflowed: bool,
    now_ms: u64,
) {
    let mut events: Vec<WatchEvent> = expired
        .into_iter()
        .map(|entry| WatchEvent {
            namespace: entry.namespace,
            key: entry.key,
            revision: entry.revision,
            kind: WatchEventKind::Expire,
        })
        .collect();
    if let Some(event) = direct {
        events.push(event);
    }
    state.state_store_watches.publish(
        state,
        &events,
        pending_overflowed,
        now_ms,
        |session_id, notification| {
            super::state_store_watch::enqueue_notification(state, session_id, notification)
        },
    );
}

const fn deleted_result(namespace: String, key: String, revision: u64) -> proto::StateStoreResult {
    proto::StateStoreResult {
        result: Some(state_store_result::Result::Deleted(
            proto::StateDeleteResult {
                namespace,
                key,
                revision,
            },
        )),
    }
}

fn lock_registry(state: &ServerState) -> std::sync::MutexGuard<'_, Registry> {
    state
        .state_store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_durable(state: &ServerState) -> Option<std::sync::MutexGuard<'_, DurableStore>> {
    state.durable_state_store.as_ref().map(|store| {
        store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn timestamp_ms(millis: u64) -> Timestamp {
    Timestamp {
        seconds: i64::try_from(millis / 1_000).unwrap_or(i64::MAX),
        nanos: i32::try_from((millis % 1_000) * 1_000_000).unwrap_or_default(),
    }
}

fn entry_result(entry: StoredEntry) -> proto::StateStoreResult {
    proto::StateStoreResult {
        result: Some(state_store_result::Result::Entry(proto_entry(&entry))),
    }
}

pub(super) fn proto_entry(entry: &StoredEntry) -> proto::StateEntry {
    proto::StateEntry {
        namespace: entry.namespace.clone(),
        key: entry.key.clone(),
        schema: entry.schema.clone(),
        value: Some(entry.value.clone()),
        revision: entry.revision,
        updated_at: Some(timestamp_ms(entry.updated_ms)),
        expires_at: entry.expires_ms.map(timestamp_ms),
        owner_id: entry.owner_id.clone(),
    }
}

fn registry_error(error: Error, namespace: &str, key: &str) -> ControlCommandError {
    match error {
        Error::NotFound => state_store_error(
            proto::ErrorCode::NotFound,
            404,
            "state entry was not found",
            namespace,
            key,
            proto::StateStoreErrorCode::NotFound,
            None,
        ),
        Error::Conflict { current_revision } => state_store_error(
            proto::ErrorCode::Rejected,
            409,
            "state entry changed; reload before saving",
            namespace,
            key,
            proto::StateStoreErrorCode::Conflict,
            Some(current_revision),
        ),
        Error::NotAuthorized => state_store_error(
            proto::ErrorCode::Rejected,
            403,
            "state write is not permitted",
            namespace,
            key,
            proto::StateStoreErrorCode::NotAuthorized,
            None,
        ),
        Error::Invalid(Invalid::Namespace) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state namespace is invalid",
            namespace,
            key,
            proto::StateStoreErrorCode::NamespaceInvalid,
            None,
        ),
        Error::Invalid(Invalid::Key) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state key is invalid",
            namespace,
            key,
            proto::StateStoreErrorCode::KeyInvalid,
            None,
        ),
        Error::Invalid(Invalid::Schema) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state schema is invalid",
            namespace,
            key,
            proto::StateStoreErrorCode::SchemaInvalid,
            None,
        ),
        Error::Invalid(Invalid::ValueMissing) => invalid("state value is required"),
        Error::Invalid(Invalid::ValueTooLarge) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state value is too large",
            namespace,
            key,
            proto::StateStoreErrorCode::ValueTooLarge,
            None,
        ),
        Error::Invalid(Invalid::NamespaceFull) => state_store_error(
            proto::ErrorCode::Rejected,
            409,
            "state namespace is full",
            namespace,
            key,
            proto::StateStoreErrorCode::NamespaceInvalid,
            None,
        ),
        Error::Invalid(Invalid::StoreFull) => state_store_error(
            proto::ErrorCode::Rejected,
            409,
            "state store is full",
            namespace,
            key,
            proto::StateStoreErrorCode::NamespaceInvalid,
            None,
        ),
        Error::Storage(message) => {
            ControlCommandError::new(proto::ErrorCode::Internal, 500, message)
        }
        Error::Invalid(Invalid::Ttl) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state TTL is invalid",
            namespace,
            key,
            proto::StateStoreErrorCode::TtlInvalid,
            None,
        ),
        Error::Invalid(Invalid::WatchNotFound) => state_store_error(
            proto::ErrorCode::NotFound,
            404,
            "state watch was not found",
            namespace,
            key,
            proto::StateStoreErrorCode::WatchNotFound,
            None,
        ),
        Error::Invalid(Invalid::WatchLimitExceeded) => state_store_error(
            proto::ErrorCode::Rejected,
            429,
            "state watch limit reached",
            namespace,
            key,
            proto::StateStoreErrorCode::WatchLimitExceeded,
            None,
        ),
        Error::Invalid(Invalid::AckAhead) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "watch acknowledgement is ahead of delivery",
            namespace,
            key,
            proto::StateStoreErrorCode::Unspecified,
            None,
        ),
    }
}

fn state_store_error(
    code: proto::ErrorCode,
    status: u16,
    message: &str,
    namespace: &str,
    key: &str,
    detail_code: proto::StateStoreErrorCode,
    current_revision: Option<u64>,
) -> ControlCommandError {
    ControlCommandError::new(code, status, message).with_detail(prost_types::Any {
        type_url: "type.googleapis.com/keeppeek.webrtc.v1.StateStoreError".to_owned(),
        value: proto::StateStoreError {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            code: detail_code as i32,
            current_revision,
        }
        .encode_to_vec(),
    })
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

pub(super) const fn check_expected(
    expected: Option<u64>,
    current: Option<u64>,
) -> Result<(), Error> {
    match (expected, current) {
        (None, _) => Ok(()),
        (Some(0), None) => Ok(()),
        (Some(0), Some(revision)) => Err(Error::Conflict {
            current_revision: revision,
        }),
        (Some(wanted), Some(have)) if wanted == have => Ok(()),
        (Some(_), Some(revision)) => Err(Error::Conflict {
            current_revision: revision,
        }),
        (Some(_), None) => Err(Error::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::{Value, value::Kind};
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};

    const NOW_MS: u64 = 1_787_000_000_000;

    fn struct_value(fields: &[(&str, &str)]) -> Struct {
        Struct {
            fields: fields
                .iter()
                .map(|(name, text)| {
                    (
                        (*name).to_owned(),
                        Value {
                            kind: Some(Kind::StringValue((*text).to_owned())),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn bool_field(fields: &mut BTreeMap<String, Value>, name: &str, value: bool) {
        fields.insert(
            name.to_owned(),
            Value {
                kind: Some(Kind::BoolValue(value)),
            },
        );
    }

    fn media_intent_value(role: &str) -> Struct {
        let mut fields = BTreeMap::from([
            (
                "role".to_owned(),
                Value {
                    kind: Some(Kind::StringValue(role.to_owned())),
                },
            ),
            (
                "source_id".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("front-door".to_owned())),
                },
            ),
            (
                "media_kind".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("video".to_owned())),
                },
            ),
        ]);
        bool_field(&mut fields, "desired", true);
        if role == "publish" {
            fields.insert(
                "recording_mode".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("disabled".to_owned())),
                },
            );
        }
        Struct { fields }
    }

    fn duration_ms(millis: u64) -> Duration {
        Duration {
            seconds: (millis / 1_000) as i64,
            nanos: ((millis % 1_000) * 1_000_000) as i32,
        }
    }

    fn put_fixture(registry: &mut Registry) -> StoredEntry {
        registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("fixture put must succeed")
    }

    fn put_lease(registry: &mut Registry, key: &str, ttl_ms: u64, now_ms: u64) -> StoredEntry {
        registry
            .put(
                "service/transcoder-a/",
                key,
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(duration_ms(ttl_ms)),
                "transcoder-a",
                true,
                now_ms,
            )
            .expect("fixture lease must succeed")
    }

    #[test]
    fn import_restores_entries_and_revisions_verbatim() {
        let mut registry = Registry::default();
        let entry = put_fixture(&mut registry);
        let exported = vec![entry.clone()];
        let mut restarted = Registry::default();
        restarted.import_namespace("service/transcoder-a/".to_owned(), 7, exported);
        let read = restarted
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("imported entry must serve");
        assert_eq!(read, entry);
        let (revision, entries) = restarted.snapshot("service/transcoder-a/", "", NOW_MS);
        assert_eq!(revision, 7);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn blind_put_creates_entry_at_revision_one() {
        let mut registry = Registry::default();
        let entry = put_fixture(&mut registry);

        assert_eq!(entry.revision, 1);
        assert_eq!(entry.namespace, "service/transcoder-a/");
        assert_eq!(entry.key, "intents/front-door");
        assert_eq!(entry.owner_id, "transcoder-a");
        assert_eq!(entry.updated_ms, NOW_MS);
        assert_eq!(entry.expires_ms, None);
    }

    #[test]
    fn blind_put_replaces_existing_entry_at_next_revision() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let entry = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS + 1,
            )
            .expect("blind replacement must succeed");

        assert_eq!(entry.revision, 2);
        assert_eq!(entry.updated_ms, NOW_MS + 1);
    }

    #[test]
    fn create_only_put_rejects_existing_key_with_conflict() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(0),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("create-only write over an existing key must fail");

        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 1
            }
        );
    }

    #[test]
    fn compare_and_set_rejects_stale_revision_with_current() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let entry = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                Some(1),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("matching revision must succeed");
        assert_eq!(entry.revision, 2);

        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(1),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("stale revision must fail");

        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 2
            }
        );
    }

    #[test]
    fn get_returns_current_entry() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let entry = registry
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("existing key must be readable");

        assert_eq!(entry.revision, 1);
        assert_eq!(entry.schema, "keeppeek.media-intent.v1");
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let mut registry = Registry::default();
        let error = registry
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("missing key must fail");

        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn delete_removes_entry_and_reports_delete_revision() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let deleted_revision = registry
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                Some(1),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("matching delete must succeed");

        assert_eq!(deleted_revision, 2);
        assert_eq!(
            registry.get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS
            ),
            Err(Error::NotFound)
        );
    }

    #[test]
    fn delete_missing_key_returns_not_found() {
        let mut registry = Registry::default();
        let error = registry
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("missing key delete must fail");

        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn delete_with_stale_revision_returns_conflict() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let error = registry
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                Some(7),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("stale delete revision must fail");

        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 1
            }
        );
    }

    #[test]
    fn revisions_are_namespaced_not_global() {
        let mut registry = Registry::default();
        put_fixture(&mut registry);
        let entry = registry
            .put(
                "service/transcoder-b/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-b",
                true,
                NOW_MS,
            )
            .expect("first write in a new namespace must succeed");

        assert_eq!(entry.revision, 1);
    }

    #[test]
    fn malformed_namespaces_are_rejected() {
        let mut registry = Registry::default();
        for namespace in [
            "",
            "service",
            "service/transcoder-a",
            "/",
            "service//",
            "service/../x/",
            "keeppeek.camera-access",
            "SERVICE/transcoder-a/",
            "service/transcoder a/",
        ] {
            let error = registry
                .put(
                    namespace,
                    "intents/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect_err("malformed namespace must fail");
            assert_eq!(error, Error::Invalid(Invalid::Namespace), "{namespace}");
        }
    }

    #[test]
    fn unknown_top_level_namespaces_are_rejected() {
        let mut registry = Registry::default();
        let error = registry
            .put(
                "archive/camera-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "camera-a",
                true,
                NOW_MS,
            )
            .expect_err("unconfigured namespace layout must fail");

        assert_eq!(error, Error::Invalid(Invalid::Namespace));
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let mut registry = Registry::default();
        for key in [
            "",
            "/intents",
            "intents/",
            "intents//front-door",
            "../front-door",
            "intents/../front-door",
            "intents/./front-door",
        ] {
            let error = registry
                .put(
                    "service/transcoder-a/",
                    key,
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect_err("malformed key must fail");
            assert_eq!(error, Error::Invalid(Invalid::Key), "{key}");
        }
    }

    #[test]
    fn missing_value_and_empty_schema_are_rejected() {
        let mut registry = Registry::default();
        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                None,
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("missing value must fail");
        assert_eq!(error, Error::Invalid(Invalid::ValueMissing));

        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("empty schema must fail");
        assert_eq!(error, Error::Invalid(Invalid::Schema));
    }

    #[test]
    fn oversized_values_are_rejected() {
        let mut registry = Registry::default();
        let padding = "p".repeat(MAX_VALUE_BYTES);
        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("padding", &padding)])),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("oversized value must fail");

        assert_eq!(error, Error::Invalid(Invalid::ValueTooLarge));
    }

    #[test]
    fn private_user_writes_require_owner_or_admin() {
        let mut registry = Registry::default();
        let error = registry
            .put(
                "user/viewer-a/",
                "subscriptions/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "viewer-b",
                false,
                NOW_MS,
            )
            .expect_err("cross-owner private write must fail");
        assert_eq!(error, Error::NotAuthorized);

        registry
            .put(
                "user/viewer-a/",
                "subscriptions/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "viewer-a",
                false,
                NOW_MS,
            )
            .expect("owner write must succeed");
        registry
            .put(
                "user/viewer-a/",
                "subscriptions/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "operator",
                true,
                NOW_MS,
            )
            .expect("administrator write must succeed");
    }

    #[test]
    fn system_namespaces_reject_client_writes() {
        let mut registry = Registry::default();
        let error = registry
            .put(
                "system/",
                "health/transcoder-a",
                "keeppeek.worker-health.v1",
                Some(struct_value(&[("alive", "true")])),
                None,
                None,
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect_err("client write to server-owned state must fail");

        assert_eq!(error, Error::NotAuthorized);
    }

    #[test]
    fn lease_within_bounds_sets_expiry() {
        let mut registry = Registry::default();
        let entry = put_lease(&mut registry, "leases/front-door", 60_000, NOW_MS);

        assert_eq!(entry.revision, 1);
        assert_eq!(entry.expires_ms, Some(NOW_MS + 60_000));
        let read = registry
            .get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 59_999,
            )
            .expect("unexpired lease must be readable");
        assert_eq!(read.expires_ms, Some(NOW_MS + 60_000));
    }

    #[test]
    fn lease_above_maximum_is_clamped() {
        let mut registry = Registry::default();
        let entry = put_lease(&mut registry, "leases/front-door", 30 * 86_400_000, NOW_MS);

        assert_eq!(entry.expires_ms, Some(NOW_MS + MAX_TTL_MS));
    }

    #[test]
    fn short_zero_and_malformed_ttls_are_rejected() {
        let mut registry = Registry::default();
        for ttl in [
            duration_ms(999),
            duration_ms(0),
            Duration {
                seconds: -1,
                nanos: 0,
            },
            Duration {
                seconds: 5,
                nanos: 1_500_000_000,
            },
        ] {
            let error = registry
                .put(
                    "service/transcoder-a/",
                    "leases/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(ttl),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect_err("invalid lease must fail");
            assert_eq!(error, Error::Invalid(Invalid::Ttl));
        }
    }

    #[test]
    fn huge_ttl_duration_is_rejected() {
        let mut registry = Registry::default();
        let error = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(Duration {
                    seconds: i64::MAX,
                    nanos: 0,
                }),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("overflowing lease must fail");

        assert_eq!(error, Error::Invalid(Invalid::Ttl));
    }

    #[test]
    fn lease_expiry_boundary_is_inclusive() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        registry
            .get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 999,
            )
            .expect("lease must survive before its deadline");
        let error = registry
            .get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 1_000,
            )
            .expect_err("lease at its exact deadline must read as not found");
        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn expired_leases_read_as_not_found() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        registry
            .get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 999,
            )
            .expect("lease inside its TTL must be readable");
        let error = registry
            .get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 1_000,
            )
            .expect_err("lease at its expiry instant must be gone");
        assert_eq!(error, Error::NotFound);
        assert_eq!(
            registry.get(
                "service/transcoder-a/",
                "leases/front-door",
                "transcoder-a",
                true,
                NOW_MS + 2_000
            ),
            Err(Error::NotFound)
        );
    }

    fn expired_fixture(state: &ServerState) {
        lock_registry(state)
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(duration_ms(1_000)),
                "transcoder-a",
                true,
                1_000,
            )
            .expect("fixture lease must succeed");
    }

    fn local_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "keeppeek-state-store-dispatch-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("test temp dir must be created");
            Self { path }
        }

        fn db_path(&self) -> std::path::PathBuf {
            self.path.join("state.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn attach_durable(state: &mut ServerState, dir: &TempDir) {
        use std::sync::{Arc, Mutex};
        let durable = DurableStore::open(&dir.db_path(), NOW_MS).expect("test database must open");
        state.durable_state_store = Some(Arc::new(Mutex::new(durable)));
    }

    fn admin_session(state: &ServerState, session: SessionId) {
        use super::super::ApiSessionRecord;
        use crate::access::{ClientClassification, ClientClassificationReason};
        use std::time::Instant;
        state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session,
                ApiSessionRecord {
                    principal: local_principal(),
                    classification: ClientClassification {
                        peer_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                        effective_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                        local: true,
                        reason: ClientClassificationReason::DirectLocal,
                    },
                    created_at_ms: 0,
                    last_activity_at_ms: 0,
                    absolute_expires_at_ms: i64::MAX,
                    last_activity: Instant::now(),
                },
            );
    }

    #[test]
    fn failed_get_still_drains_observed_expiry() {
        let state = ServerState::empty();
        expired_fixture(&state);
        get(
            &state,
            &local_principal(),
            proto::GetState {
                namespace: "service/transcoder-a/".to_owned(),
                key: "leases/front-door".to_owned(),
            },
        )
        .expect_err("expired get must still report not found");
        assert!(
            lock_registry(&state).pending.is_empty(),
            "observed expiry must publish even though get failed"
        );
    }

    #[test]
    fn failed_put_still_drains_observed_expiry() {
        let state = ServerState::empty();
        expired_fixture(&state);
        put(
            &state,
            &local_principal(),
            proto::PutState {
                namespace: "service/transcoder-a/".to_owned(),
                key: "leases/front-door".to_owned(),
                schema: "keeppeek.media-intent.v1".to_owned(),
                value: Some(media_intent_value("publish")),
                expected_revision: Some(99),
                ttl: None,
            },
        )
        .expect_err("guarded put on an expired key must fail");
        assert!(
            lock_registry(&state).pending.is_empty(),
            "observed expiry must publish even though put failed"
        );
    }

    #[test]
    fn failed_delete_still_drains_observed_expiry() {
        let state = ServerState::empty();
        expired_fixture(&state);
        delete(
            &state,
            &local_principal(),
            proto::DeleteState {
                namespace: "service/transcoder-a/".to_owned(),
                key: "leases/front-door".to_owned(),
                expected_revision: None,
            },
        )
        .expect_err("delete on an expired key must fail");
        assert!(
            lock_registry(&state).pending.is_empty(),
            "observed expiry must publish even though delete failed"
        );
    }

    #[test]
    fn snapshot_reports_post_expiry_revision() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        let (revision, entries) =
            registry.snapshot("service/transcoder-a/", "leases/", NOW_MS + 1_000);
        assert_eq!(
            revision, 2,
            "expiring the lease advances the namespace revision"
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn watch_registers_after_draining_stale_expiry() {
        let state = ServerState::empty();
        let principal = local_principal();
        let session = SessionId::from_u64(4242);
        {
            let mut registry = lock_registry(&state);
            registry
                .put(
                    "service/transcoder-a/",
                    "leases/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("fixture lease must succeed");
            registry
                .get(
                    "service/transcoder-a/",
                    "leases/front-door",
                    "transcoder-a",
                    true,
                    NOW_MS + 1_000,
                )
                .expect_err("expired get queues the expiry");
        }
        let result = watch(
            &state,
            session,
            &principal,
            proto::WatchState {
                namespace: "service/transcoder-a/".to_owned(),
                key_prefix: "leases/".to_owned(),
                watch_id: "w".to_owned(),
            },
        )
        .expect("watch must succeed");
        let Some(state_store_result::Result::Watch(snapshot)) = result.result else {
            panic!("watch must return a snapshot");
        };
        assert_eq!(snapshot.snapshot_revision, 2);
        assert!(
            lock_registry(&state).pending.is_empty(),
            "stale expiry must publish before the new barrier"
        );
    }

    fn watch_fixture(state: &ServerState, session: SessionId, watch_id: &str) {
        watch(
            state,
            session,
            &local_principal(),
            proto::WatchState {
                namespace: "service/transcoder-a/".to_owned(),
                key_prefix: String::new(),
                watch_id: watch_id.to_owned(),
            },
        )
        .expect("fixture watch must register");
    }

    #[test]
    fn sweeper_publishes_idle_expiry_without_terminating() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(4243);
        lock_registry(&state)
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(duration_ms(1_000)),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("fixture lease must succeed");
        admin_session(&state, session);
        watch_fixture(&state, session, "w");
        expire_leases(&state, NOW_MS + 5_000);
        assert!(
            lock_registry(&state).pending.is_empty(),
            "idle expiry must run without subsequent writes"
        );
        assert!(
            state.state_store_watches.owns_watch(session, "w"),
            "a buffered expiry must not terminate the watch"
        );
    }

    #[test]
    fn durable_write_through_survives_reopen() {
        let dir = TempDir::new();
        let revision = {
            let mut state = ServerState::empty();
            attach_durable(&mut state, &dir);
            let result = put(
                &state,
                &local_principal(),
                proto::PutState {
                    namespace: "service/transcoder-a/".to_owned(),
                    key: "intents/front-door".to_owned(),
                    schema: "keeppeek.media-intent.v1".to_owned(),
                    value: Some(media_intent_value("publish")),
                    expected_revision: None,
                    ttl: None,
                },
            )
            .expect("durable put must succeed");
            let Some(state_store_result::Result::Entry(entry)) = result.result else {
                panic!("put must return the stored entry");
            };
            assert_eq!(entry.revision, 1);
            let durable_entry = lock_durable(&state)
                .expect("durable store must be attached")
                .get(
                    "service/transcoder-a/",
                    "intents/front-door",
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("durable read must see the write");
            assert_eq!(durable_entry.revision, 1);
            entry.revision
        };
        let mut reopened = Registry::default();
        let durable = super::super::state_store_durable::DurableStore::open(&dir.db_path(), NOW_MS)
            .expect("reopen must succeed");
        let exports = durable.export(NOW_MS).expect("export must succeed");
        assert_eq!(exports.len(), 1);
        for export in exports {
            reopened.import_namespace(export.namespace, export.revision, export.entries);
        }
        let read = reopened
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("reopened entry must serve");
        assert_eq!(read.revision, revision);
        let (snapshot_revision, _) = reopened.snapshot("service/transcoder-a/", "", NOW_MS);
        assert_eq!(snapshot_revision, revision);
    }

    #[test]
    fn durable_open_failure_keeps_gate_shut() {
        let dir = TempDir::new();
        let marker = dir.path.join("not-a-directory");
        std::fs::write(&marker, b"blocking file").expect("marker file must exist");
        let mut state = ServerState::empty();
        state.storage_config.long_term_path = marker;
        state.open_state_store();
        assert!(
            state.durable_state_store.is_none(),
            "an unusable path must not attach a store"
        );
        assert!(
            !state.state_store_generic_enabled,
            "the generic gate must stay shut without durability"
        );
    }

    #[test]
    fn watch_triggered_expiry_keeps_disk_and_cache_in_step() {
        let dir = TempDir::new();
        let mut state = ServerState::empty();
        attach_durable(&mut state, &dir);
        {
            let mut registry = lock_registry(&state);
            let mut durable = lock_durable(&state).expect("durable must be attached");
            for (key, ttl) in [
                ("leases/front-door", Some(duration_ms(1_000))),
                ("intents/front-door", None),
            ] {
                registry
                    .put(
                        "service/transcoder-a/",
                        key,
                        "keeppeek.media-intent.v1",
                        Some(media_intent_value("publish")),
                        None,
                        ttl,
                        "transcoder-a",
                        true,
                        1_000,
                    )
                    .expect("memory setup put must succeed");
                durable
                    .put(
                        "service/transcoder-a/",
                        key,
                        "keeppeek.media-intent.v1",
                        Some(media_intent_value("publish")),
                        None,
                        ttl,
                        "transcoder-a",
                        true,
                        1_000,
                    )
                    .expect("durable setup put must succeed");
            }
        }
        let result = watch(
            &state,
            SessionId::from_u64(4245),
            &local_principal(),
            proto::WatchState {
                namespace: "service/transcoder-a/".to_owned(),
                key_prefix: String::new(),
                watch_id: "w".to_owned(),
            },
        )
        .expect("watch must succeed");
        let Some(state_store_result::Result::Watch(snapshot)) = result.result else {
            panic!("watch must return a snapshot");
        };
        assert_eq!(snapshot.snapshot_revision, 3);
        let replaced = put(
            &state,
            &local_principal(),
            proto::PutState {
                namespace: "service/transcoder-a/".to_owned(),
                key: "intents/front-door".to_owned(),
                schema: "keeppeek.media-intent.v1".to_owned(),
                value: Some(media_intent_value("publish")),
                expected_revision: None,
                ttl: None,
            },
        )
        .expect("replacement put must succeed");
        let Some(state_store_result::Result::Entry(replaced)) = replaced.result else {
            panic!("put must return the stored entry");
        };
        assert_eq!(replaced.revision, 4);
        let durable_revision = lock_durable(&state)
            .expect("durable must be attached")
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("durable read must succeed")
            .revision;
        assert_eq!(
            durable_revision, replaced.revision,
            "disk and cache must report the same revision"
        );
        put(
            &state,
            &local_principal(),
            proto::PutState {
                namespace: "service/transcoder-a/".to_owned(),
                key: "intents/front-door".to_owned(),
                schema: "keeppeek.media-intent.v1".to_owned(),
                value: Some(media_intent_value("publish")),
                expected_revision: Some(replaced.revision),
                ttl: None,
            },
        )
        .expect("CAS with the returned revision must succeed");
        drop(state);
        let reopened = DurableStore::open(&dir.db_path(), NOW_MS).expect("reopen must succeed");
        let exports = reopened.export(NOW_MS).expect("export must succeed");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].namespace, "service/transcoder-a/");
        assert_eq!(exports[0].revision, 5);
        assert_eq!(exports[0].entries.len(), 1);
        assert_eq!(exports[0].entries[0].key, "intents/front-door");
        assert_eq!(exports[0].entries[0].revision, 5);
    }

    #[test]
    fn watch_rejects_snapshot_beyond_entry_bound() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(4246);
        {
            let mut registry = lock_registry(&state);
            for index in 0..=MAX_WATCH_SNAPSHOT_ENTRIES {
                registry
                    .put(
                        "service/transcoder-a/",
                        &format!("intents/k-{index:02}"),
                        "keeppeek.media-intent.v1",
                        Some(media_intent_value("publish")),
                        None,
                        None,
                        "transcoder-a",
                        true,
                        NOW_MS,
                    )
                    .expect("fixture document must fit");
            }
        }
        let error = watch(
            &state,
            session,
            &local_principal(),
            proto::WatchState {
                namespace: "service/transcoder-a/".to_owned(),
                key_prefix: String::new(),
                watch_id: "w".to_owned(),
            },
        )
        .expect_err("a 65-document snapshot must be rejected");
        assert!(
            format!("{error:?}").contains("too large"),
            "rejection must name the size bound, got {error:?}"
        );
        assert!(
            !state.state_store_watches.owns_watch(session, "w"),
            "a rejected snapshot must not register the watch"
        );
    }

    #[test]
    fn watch_accepts_full_size_snapshot_with_valid_documents() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(4247);
        {
            let mut registry = lock_registry(&state);
            for index in 0..MAX_WATCH_SNAPSHOT_ENTRIES {
                let mut value = media_intent_value("publish");
                for name in ["source_id", "stream_id", "variant_id", "output_profile"] {
                    value.fields.insert(
                        name.to_owned(),
                        Value {
                            kind: Some(Kind::StringValue("x".repeat(128))),
                        },
                    );
                }
                registry
                    .put(
                        "service/transcoder-a/",
                        &format!("intents/k-{index:02}"),
                        "keeppeek.media-intent.v1",
                        Some(value),
                        None,
                        None,
                        "transcoder-a",
                        true,
                        NOW_MS,
                    )
                    .expect("max-size fixture document must fit");
            }
        }
        let result = watch(
            &state,
            session,
            &local_principal(),
            proto::WatchState {
                namespace: "service/transcoder-a/".to_owned(),
                key_prefix: String::new(),
                watch_id: "w".to_owned(),
            },
        )
        .expect("64 max-size documents must stay within the wire budget");
        let Some(state_store_result::Result::Watch(snapshot)) = result.result else {
            panic!("watch must return a snapshot");
        };
        assert_eq!(snapshot.entries.len(), MAX_WATCH_SNAPSHOT_ENTRIES);
        assert!(state.state_store_watches.owns_watch(session, "w"));
    }

    #[test]
    fn snapshot_bounds_reject_overflowing_counts_and_bytes() {
        assert!(check_snapshot_bounds(64, 0, "service/transcoder-a/", "w").is_ok());
        assert!(check_snapshot_bounds(0, 65_536, "service/transcoder-a/", "w").is_ok());
        assert!(check_snapshot_bounds(65, 0, "service/transcoder-a/", "w").is_err());
        assert!(check_snapshot_bounds(0, 65_537, "service/transcoder-a/", "w").is_err());
    }

    #[test]
    fn sweeper_overflow_terminates_every_watch() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(4244);
        {
            let mut registry = lock_registry(&state);
            for namespace_no in 0..5 {
                let namespace = format!("service/s{namespace_no}/");
                for key_no in 0..820 {
                    registry
                        .put(
                            &namespace,
                            &format!("leases/k-{key_no}"),
                            "keeppeek.media-intent.v1",
                            Some(media_intent_value("publish")),
                            None,
                            Some(duration_ms(1_000)),
                            "transcoder-a",
                            true,
                            NOW_MS,
                        )
                        .expect("overflow lease must fit its namespace");
                }
            }
        }
        for watch_id in ["a", "b"] {
            watch(
                &state,
                session,
                &local_principal(),
                proto::WatchState {
                    namespace: "service/transcoder-a/".to_owned(),
                    key_prefix: String::new(),
                    watch_id: watch_id.to_owned(),
                },
            )
            .expect("fixture watch must register");
        }
        expire_leases(&state, NOW_MS + 5_000);
        assert!(
            !state.state_store_watches.owns_watch(session, "a")
                && !state.state_store_watches.owns_watch(session, "b"),
            "an overflowing sweeper batch must end every watch"
        );
    }

    #[test]
    fn expire_due_reports_each_expiry_once_in_order() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/a-front-door", 1_000, NOW_MS);
        put_lease(&mut registry, "leases/z-back-door", 3_600_000, NOW_MS);

        let expired = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(expired.entries.len(), 1);
        assert_eq!(expired.entries[0].namespace, "service/transcoder-a/");
        assert_eq!(expired.entries[0].key, "leases/a-front-door");
        assert_eq!(expired.entries[0].revision, 3);
        assert_eq!(expired.entries[0].schema, "keeppeek.media-intent.v1");
        assert_eq!(expired.entries[0].owner_id, "transcoder-a");
        assert_eq!(expired.entries[0].expires_ms, NOW_MS + 1_000);

        assert!(registry.expire_due(NOW_MS + 2_000).entries.is_empty());

        let expired = registry.expire_due(NOW_MS + 3_600_000);
        assert_eq!(expired.entries.len(), 1);
        assert_eq!(expired.entries[0].key, "leases/z-back-door");
        assert_eq!(expired.entries[0].revision, 4);
    }

    #[test]
    fn expired_key_accepts_create_only_write() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        assert_eq!(registry.expire_due(NOW_MS + 1_000).entries.len(), 1);

        let entry = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(0),
                None,
                "transcoder-a",
                true,
                NOW_MS + 1_000,
            )
            .expect("create-only write after expiry must succeed");

        assert_eq!(entry.revision, 3);
        assert_eq!(entry.expires_ms, None);
    }

    #[test]
    fn compare_and_set_on_expired_key_returns_not_found() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        assert_eq!(registry.expire_due(NOW_MS + 1_000).entries.len(), 1);

        let error = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(1),
                None,
                "transcoder-a",
                true,
                NOW_MS + 1_000,
            )
            .expect_err("stale write on an expired key must fail");

        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn delete_expired_key_returns_not_found() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        let error = registry
            .delete(
                "service/transcoder-a/",
                "leases/front-door",
                None,
                "transcoder-a",
                true,
                NOW_MS + 2_000,
            )
            .expect_err("delete of an expired key must fail");

        assert_eq!(error, Error::NotFound);
        let expired = registry.expire_due(NOW_MS + 2_000);
        assert_eq!(expired.entries.len(), 1);
        assert_eq!(expired.entries[0].key, "leases/front-door");
    }

    #[test]
    fn lease_refresh_replaces_expiry_without_phantom_event() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        let entry = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(1),
                Some(duration_ms(3_600_000)),
                "transcoder-a",
                true,
                NOW_MS + 500,
            )
            .expect("lease refresh must succeed");

        assert_eq!(entry.revision, 2);
        assert_eq!(entry.expires_ms, Some(NOW_MS + 500 + 3_600_000));
        assert!(registry.expire_due(NOW_MS + 1_000).entries.is_empty());
        let expired = registry.expire_due(NOW_MS + 500 + 3_600_000);
        assert_eq!(expired.entries.len(), 1);
        assert_eq!(expired.entries[0].revision, 3);
    }

    #[test]
    fn full_namespace_rejects_new_keys_but_allows_replacement() {
        let mut registry = Registry::default();
        for index in 0..MAX_ENTRIES_PER_NAMESPACE {
            registry
                .put(
                    "service/transcoder-a/",
                    &format!("keys/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("fill write must succeed");
        }

        let error = registry
            .put(
                "service/transcoder-a/",
                "keys/overflow",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("new key past the namespace bound must fail");
        assert_eq!(error, Error::Invalid(Invalid::NamespaceFull));

        let entry = registry
            .put(
                "service/transcoder-a/",
                "keys/k-0000",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("replacement inside a full namespace must succeed");
        assert_eq!(entry.revision, (MAX_ENTRIES_PER_NAMESPACE + 1) as u64);
    }

    #[test]
    fn pending_overflow_signals_recovery_instead_of_silent_loss() {
        let mut registry = Registry::default();
        for index in 0..=MAX_PENDING_EXPIRIES {
            registry
                .put(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("overflow fill write must succeed");
        }

        let batch = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(batch.entries.len(), MAX_PENDING_EXPIRIES);
        assert!(
            batch.overflowed,
            "queue overflow must be signaled for watch resynchronization"
        );
        assert!(
            batch
                .entries
                .iter()
                .all(|entry| entry.key != "leases/k-0000"),
            "oldest expiry must be evicted"
        );
        assert_eq!(batch.entries[0].namespace, "service/load-0/");
        assert_eq!(batch.entries[0].key, "leases/k-0005");
        assert_eq!(batch.entries[MAX_PENDING_EXPIRIES - 1].key, "leases/k-4094");
        let drained = registry.expire_due(NOW_MS + 1_000);
        assert!(drained.entries.is_empty());
        assert!(!drained.overflowed);
    }

    #[test]
    fn rejected_writes_allocate_no_namespaces() {
        let mut registry = Registry::default();
        for index in 0..10_000 {
            let error = registry
                .put(
                    &format!("service/n-{index}/"),
                    "intents/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    Some(1),
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect_err("compare-and-set on an absent key must fail");
            assert_eq!(error, Error::NotFound);
        }
        assert!(registry.namespaces.is_empty());
        assert_eq!(registry.stored_bytes, 0);
    }

    #[test]
    fn expired_entries_are_reclaimed_before_namespace_full() {
        let mut registry = Registry::default();
        for index in 0..MAX_ENTRIES_PER_NAMESPACE {
            registry
                .put(
                    "service/transcoder-a/",
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("lease fill write must succeed");
        }

        let entry = registry
            .put(
                "service/transcoder-a/",
                "leases/fresh",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS + 1_000,
            )
            .expect("reclamation must admit a fresh key after every lease expires");
        assert_eq!(entry.expires_ms, None);
        assert_eq!(
            registry
                .get(
                    "service/transcoder-a/",
                    "leases/fresh",
                    "transcoder-a",
                    true,
                    NOW_MS + 1_000
                )
                .expect("fresh key must be readable")
                .revision,
            entry.revision
        );
        assert_eq!(
            registry.stored_bytes,
            value_bytes(&media_intent_value("publish")),
            "reclaimed bytes must leave the counter"
        );
    }

    #[test]
    fn stored_byte_counter_tracks_live_values() {
        let mut registry = Registry::default();
        let first = media_intent_value("publish");
        let second = media_intent_value("subscribe");
        registry
            .put(
                "service/transcoder-a/",
                "intents/a",
                "keeppeek.media-intent.v1",
                Some(first.clone()),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("first write must succeed");
        assert_eq!(registry.stored_bytes, value_bytes(&first));
        registry
            .put(
                "service/transcoder-a/",
                "intents/a",
                "keeppeek.media-intent.v1",
                Some(second.clone()),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("replacement must succeed");
        assert_eq!(registry.stored_bytes, value_bytes(&second));
        registry
            .put(
                "service/transcoder-a/",
                "intents/b",
                "keeppeek.media-intent.v1",
                Some(first.clone()),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("second key must succeed");
        assert_eq!(
            registry.stored_bytes,
            value_bytes(&second) + value_bytes(&first)
        );
        registry
            .delete(
                "service/transcoder-a/",
                "intents/a",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("delete must succeed");
        assert_eq!(registry.stored_bytes, value_bytes(&first));
        registry
            .delete(
                "service/transcoder-a/",
                "intents/b",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("delete must succeed");
        assert_eq!(registry.stored_bytes, 0);
    }

    #[test]
    fn lazy_get_overflow_is_signaled_on_drain() {
        let mut registry = Registry::default();
        for index in 0..=MAX_PENDING_EXPIRIES {
            registry
                .put(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("lease fill write must succeed");
        }
        for index in 0..=MAX_PENDING_EXPIRIES {
            let error = registry
                .get(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "transcoder-a",
                    true,
                    NOW_MS + 1_000,
                )
                .expect_err("expired lease must read as not found");
            assert_eq!(error, Error::NotFound);
        }
        let batch = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(batch.entries.len(), MAX_PENDING_EXPIRIES);
        assert!(
            batch.overflowed,
            "lazy-path eviction must be signaled on drain"
        );
    }

    #[test]
    fn lazy_put_overflow_is_signaled_on_drain() {
        let mut registry = Registry::default();
        for index in 0..=MAX_PENDING_EXPIRIES {
            registry
                .put(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("lease fill write must succeed");
        }
        for index in 0..=MAX_PENDING_EXPIRIES {
            registry
                .put(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS + 1_000,
                )
                .expect("lease refresh must succeed");
        }
        let batch = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(batch.entries.len(), MAX_PENDING_EXPIRIES);
        assert!(
            batch.overflowed,
            "lazy-path eviction must be signaled on drain"
        );
    }

    #[test]
    fn unrelated_reads_of_private_entries_are_denied() {
        let mut registry = Registry::default();
        registry
            .put(
                "user/viewer-a/",
                "subscriptions/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "viewer-a",
                false,
                NOW_MS,
            )
            .expect("owner write must succeed");

        let error = registry
            .get(
                "user/viewer-a/",
                "subscriptions/front-door",
                "viewer-b",
                false,
                NOW_MS,
            )
            .expect_err("cross-owner reads must fail");
        assert_eq!(error, Error::NotAuthorized);
        let error = registry
            .get(
                "user/viewer-a/",
                "subscriptions/missing",
                "viewer-b",
                false,
                NOW_MS,
            )
            .expect_err("cross-owner reads of missing keys must fail");
        assert_eq!(error, Error::NotAuthorized);
        registry
            .get(
                "user/viewer-a/",
                "subscriptions/front-door",
                "viewer-a",
                false,
                NOW_MS,
            )
            .expect("owner read must succeed");
        registry
            .get(
                "user/viewer-a/",
                "subscriptions/front-door",
                "operator",
                true,
                NOW_MS,
            )
            .expect("administrator read must succeed");
    }

    #[test]
    fn denied_reads_leave_expiry_state_untouched() {
        let mut registry = Registry::default();
        registry
            .put(
                "user/viewer-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                Some(duration_ms(1_000)),
                "viewer-a",
                false,
                NOW_MS,
            )
            .expect("owner lease must succeed");

        let error = registry
            .get(
                "user/viewer-a/",
                "leases/front-door",
                "viewer-b",
                false,
                NOW_MS + 1_000,
            )
            .expect_err("expired cross-owner reads must fail closed");
        assert_eq!(error, Error::NotAuthorized);
        let batch = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(batch.entries.len(), 1);
        assert!(!batch.overflowed);
    }

    #[test]
    fn service_and_group_operations_require_administrator() {
        let mut registry = Registry::default();
        for (namespace, owner) in [
            ("service/transcoder-a/", "transcoder-a"),
            ("group/operators/", "operator"),
        ] {
            let error = registry
                .put(
                    namespace,
                    "intents/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    owner,
                    false,
                    NOW_MS,
                )
                .expect_err("non-administrator shared writes must fail");
            assert_eq!(error, Error::NotAuthorized, "{namespace}");
        }

        registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("administrator write must succeed");
        let error = registry
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                None,
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect_err("non-administrator shared deletes must fail");
        assert_eq!(error, Error::NotAuthorized);
        registry
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("administrator delete must succeed");
    }

    #[test]
    fn system_reads_require_administrator() {
        let mut registry = Registry::default();
        let error = registry
            .get(
                "system/",
                "health/transcoder-a",
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect_err("non-administrator system reads must fail");
        assert_eq!(error, Error::NotAuthorized);
        let error = registry
            .get(
                "system/",
                "health/transcoder-a",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("missing system entries read as not found");
        assert_eq!(error, Error::NotFound);
    }
}
