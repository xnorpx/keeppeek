use super::{ApiPrincipal, ControlCommandError, ServerState};
use crate::{
    access::AccessRole,
    api::proto::{self, ok as control_ok, state_store_command, state_store_result},
};
use prost::Message as _;
use prost_types::{Duration, Struct, Timestamp};
use std::{
    collections::{HashMap, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const CAPABILITY_ID: &str = "keeppeek.state-store.v1";
const MAX_NAMESPACE_CHARS: usize = 128;
const MAX_KEY_CHARS: usize = 256;
const MAX_SCHEMA_CHARS: usize = 128;
const MAX_VALUE_BYTES: usize = 64 * 1_024;
const MAX_ENTRIES_PER_NAMESPACE: usize = 1_024;
const MAX_PENDING_EXPIRIES: usize = 4_096;
const MIN_TTL_MS: u64 = 1_000;
const MAX_TTL_MS: u64 = 86_400_000;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct StoredEntry {
    pub(super) namespace: String,
    pub(super) key: String,
    pub(super) schema: String,
    pub(super) value: Struct,
    pub(super) revision: u64,
    pub(super) updated_ms: u64,
    pub(super) expires_ms: Option<u64>,
    pub(super) owner_id: String,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code, reason = "expiry fan-out lands with watch delivery")]
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
pub(super) enum Invalid {
    Namespace,
    Key,
    Schema,
    ValueMissing,
    ValueTooLarge,
    NamespaceFull,
    Ttl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Error {
    NotFound,
    Conflict { current_revision: u64 },
    NotAuthorized,
    Invalid(Invalid),
}

#[derive(Debug, Default)]
pub(super) struct Registry {
    namespaces: HashMap<String, NamespaceState>,
    pending: VecDeque<ExpiredEntry>,
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
        expire_key_if_due(
            &mut self.namespaces,
            &mut self.pending,
            namespace,
            key,
            now_ms,
        );
        let state = self.namespaces.entry(namespace.to_owned()).or_default();
        let current = state.entries.get(key).map(|record| record.revision);
        check_expected(expected_revision, current)?;
        if current.is_none() && state.entries.len() >= MAX_ENTRIES_PER_NAMESPACE {
            return Err(Error::Invalid(Invalid::NamespaceFull));
        }
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
        now_ms: u64,
    ) -> Result<StoredEntry, Error> {
        validate_namespace(namespace)?;
        validate_key(key)?;
        expire_key_if_due(
            &mut self.namespaces,
            &mut self.pending,
            namespace,
            key,
            now_ms,
        );
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
        expire_key_if_due(
            &mut self.namespaces,
            &mut self.pending,
            namespace,
            key,
            now_ms,
        );
        let state = self.namespaces.get_mut(namespace).ok_or(Error::NotFound)?;
        let current = state.entries.get(key).map(|record| record.revision);
        check_expected(expected_revision, current)?;
        current.ok_or(Error::NotFound)?;
        state.entries.remove(key);
        state.revision = state
            .revision
            .checked_add(1)
            .expect("namespace revision overflow");
        Ok(state.revision)
    }

    #[allow(dead_code, reason = "expiry fan-out lands with watch delivery")]
    pub(super) fn expire_due(&mut self, now_ms: u64) -> Vec<ExpiredEntry> {
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
            expire_key_if_due(
                &mut self.namespaces,
                &mut self.pending,
                &namespace,
                &key,
                now_ms,
            );
        }
        self.pending.drain(..).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NamespaceLayout {
    System,
    Service,
    Group,
    User { owner: String },
}

fn validate_namespace(namespace: &str) -> Result<NamespaceLayout, Error> {
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

fn validate_key(key: &str) -> Result<(), Error> {
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

fn validate_schema(schema: &str) -> Result<(), Error> {
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

fn ttl_expiry_ms(ttl: &Duration, now_ms: u64) -> Result<u64, Error> {
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

fn expire_key_if_due(
    namespaces: &mut HashMap<String, NamespaceState>,
    pending: &mut VecDeque<ExpiredEntry>,
    namespace: &str,
    key: &str,
    now_ms: u64,
) {
    let due = namespaces.get(namespace).is_some_and(|state| {
        state
            .entries
            .get(key)
            .is_some_and(|record| is_expired(record, now_ms))
    });
    if !due {
        return;
    }
    if let Some(state) = namespaces.get_mut(namespace)
        && let Some(record) = state.entries.remove(key)
    {
        state.revision = state
            .revision
            .checked_add(1)
            .expect("namespace revision overflow");
        push_pending(
            pending,
            ExpiredEntry {
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                revision: state.revision,
                schema: record.schema,
                value: record.value,
                owner_id: record.owner_id,
                expires_ms: record.expires_ms.unwrap_or(now_ms),
            },
        );
    }
}

#[allow(dead_code, reason = "expiry fan-out lands with watch delivery")]
fn push_pending(pending: &mut VecDeque<ExpiredEntry>, expired: ExpiredEntry) {
    if pending.len() >= MAX_PENDING_EXPIRIES {
        pending.pop_front();
    }
    pending.push_back(expired);
}

fn authorize_write(
    layout: &NamespaceLayout,
    owner_id: &str,
    writer_admin: bool,
) -> Result<(), Error> {
    match layout {
        NamespaceLayout::System => Err(Error::NotAuthorized),
        NamespaceLayout::Service | NamespaceLayout::Group => Ok(()),
        NamespaceLayout::User { owner } => {
            if owner_id == owner || writer_admin {
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
        Some(state_store_command::Action::Unwatch(_)) | None => return false,
    };
    validate_namespace(namespace).is_ok()
}

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => get(state, request)?,
        Some(state_store_command::Action::Put(request)) => put(state, principal, request)?,
        Some(state_store_command::Action::Delete(request)) => delete(state, principal, request)?,
        Some(state_store_command::Action::Watch(_) | state_store_command::Action::Unwatch(_)) => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "this state store watch operation is not implemented",
            ));
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
    request: proto::GetState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let entry = lock_registry(state)
        .get(&request.namespace, &request.key, now_ms())
        .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    Ok(entry_result(entry))
}

fn put(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::PutState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let value = request
        .value
        .ok_or_else(|| invalid("state value is required"))?;
    let entry = lock_registry(state)
        .put(
            &request.namespace,
            &request.key,
            &request.schema,
            Some(value),
            request.expected_revision,
            request.ttl,
            &principal.id(),
            principal.role == AccessRole::Administrator,
            now_ms(),
        )
        .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    Ok(entry_result(entry))
}

fn delete(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::DeleteState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let revision = lock_registry(state)
        .delete(
            &request.namespace,
            &request.key,
            request.expected_revision,
            &principal.id(),
            principal.role == AccessRole::Administrator,
            now_ms(),
        )
        .map_err(|error| registry_error(error, &request.namespace, &request.key))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Deleted(
            proto::StateDeleteResult {
                namespace: request.namespace,
                key: request.key,
                revision,
            },
        )),
    })
}

fn lock_registry(state: &ServerState) -> std::sync::MutexGuard<'_, Registry> {
    state
        .state_store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        result: Some(state_store_result::Result::Entry(proto::StateEntry {
            namespace: entry.namespace,
            key: entry.key,
            schema: entry.schema,
            value: Some(entry.value),
            revision: entry.revision,
            updated_at: Some(timestamp_ms(entry.updated_ms)),
            expires_at: entry.expires_ms.map(timestamp_ms),
            owner_id: entry.owner_id,
        })),
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
        Error::Invalid(Invalid::Ttl) => state_store_error(
            proto::ErrorCode::InvalidRequest,
            400,
            "state TTL is invalid",
            namespace,
            key,
            proto::StateStoreErrorCode::TtlInvalid,
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

const fn check_expected(expected: Option<u64>, current: Option<u64>) -> Result<(), Error> {
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
                Some(struct_value(&[("role", "publish")])),
                None,
                None,
                "transcoder-a",
                false,
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
                Some(struct_value(&[("role", "publish")])),
                None,
                Some(duration_ms(ttl_ms)),
                "transcoder-a",
                false,
                now_ms,
            )
            .expect("fixture lease must succeed")
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
                Some(struct_value(&[("role", "subscribe")])),
                None,
                None,
                "transcoder-a",
                false,
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
                Some(struct_value(&[("role", "publish")])),
                Some(0),
                None,
                "transcoder-a",
                false,
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
                Some(struct_value(&[("role", "subscribe")])),
                Some(1),
                None,
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect("matching revision must succeed");
        assert_eq!(entry.revision, 2);

        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("role", "publish")])),
                Some(1),
                None,
                "transcoder-a",
                false,
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
            .get("service/transcoder-a/", "intents/front-door", NOW_MS)
            .expect("existing key must be readable");

        assert_eq!(entry.revision, 1);
        assert_eq!(entry.schema, "keeppeek.media-intent.v1");
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let mut registry = Registry::default();
        let error = registry
            .get("service/transcoder-a/", "intents/front-door", NOW_MS)
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
                false,
                NOW_MS,
            )
            .expect("matching delete must succeed");

        assert_eq!(deleted_revision, 2);
        assert_eq!(
            registry.get("service/transcoder-a/", "intents/front-door", NOW_MS),
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
                false,
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
                false,
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
                Some(struct_value(&[("role", "publish")])),
                None,
                None,
                "transcoder-b",
                false,
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
                    Some(struct_value(&[("role", "publish")])),
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
                Some(struct_value(&[("role", "publish")])),
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
                    Some(struct_value(&[("role", "publish")])),
                    None,
                    None,
                    "transcoder-a",
                    false,
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
                false,
                NOW_MS,
            )
            .expect_err("missing value must fail");
        assert_eq!(error, Error::Invalid(Invalid::ValueMissing));

        let error = registry
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "",
                Some(struct_value(&[("role", "publish")])),
                None,
                None,
                "transcoder-a",
                false,
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
                false,
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
                Some(struct_value(&[("role", "subscribe")])),
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
                Some(struct_value(&[("role", "subscribe")])),
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
                Some(struct_value(&[("role", "subscribe")])),
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
                    Some(struct_value(&[("role", "publish")])),
                    None,
                    Some(ttl),
                    "transcoder-a",
                    false,
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
                Some(struct_value(&[("role", "publish")])),
                None,
                Some(Duration {
                    seconds: i64::MAX,
                    nanos: 0,
                }),
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect_err("overflowing lease must fail");

        assert_eq!(error, Error::Invalid(Invalid::Ttl));
    }

    #[test]
    fn expired_leases_read_as_not_found() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        registry
            .get("service/transcoder-a/", "leases/front-door", NOW_MS + 999)
            .expect("lease inside its TTL must be readable");
        let error = registry
            .get("service/transcoder-a/", "leases/front-door", NOW_MS + 1_000)
            .expect_err("lease at its expiry instant must be gone");
        assert_eq!(error, Error::NotFound);
        assert_eq!(
            registry.get("service/transcoder-a/", "leases/front-door", NOW_MS + 2_000),
            Err(Error::NotFound)
        );
    }

    #[test]
    fn expire_due_reports_each_expiry_once_in_order() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/a-front-door", 1_000, NOW_MS);
        put_lease(&mut registry, "leases/z-back-door", 3_600_000, NOW_MS);

        let expired = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].namespace, "service/transcoder-a/");
        assert_eq!(expired[0].key, "leases/a-front-door");
        assert_eq!(expired[0].revision, 3);
        assert_eq!(expired[0].schema, "keeppeek.media-intent.v1");
        assert_eq!(expired[0].owner_id, "transcoder-a");
        assert_eq!(expired[0].expires_ms, NOW_MS + 1_000);

        assert!(registry.expire_due(NOW_MS + 2_000).is_empty());

        let expired = registry.expire_due(NOW_MS + 3_600_000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].key, "leases/z-back-door");
        assert_eq!(expired[0].revision, 4);
    }

    #[test]
    fn expired_key_accepts_create_only_write() {
        let mut registry = Registry::default();
        put_lease(&mut registry, "leases/front-door", 1_000, NOW_MS);
        assert_eq!(registry.expire_due(NOW_MS + 1_000).len(), 1);

        let entry = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("role", "publish")])),
                Some(0),
                None,
                "transcoder-a",
                false,
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
        assert_eq!(registry.expire_due(NOW_MS + 1_000).len(), 1);

        let error = registry
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("role", "publish")])),
                Some(1),
                None,
                "transcoder-a",
                false,
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
                false,
                NOW_MS + 2_000,
            )
            .expect_err("delete of an expired key must fail");

        assert_eq!(error, Error::NotFound);
        let expired = registry.expire_due(NOW_MS + 2_000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].key, "leases/front-door");
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
                Some(struct_value(&[("role", "publish")])),
                Some(1),
                Some(duration_ms(3_600_000)),
                "transcoder-a",
                false,
                NOW_MS + 500,
            )
            .expect("lease refresh must succeed");

        assert_eq!(entry.revision, 2);
        assert_eq!(entry.expires_ms, Some(NOW_MS + 500 + 3_600_000));
        assert!(registry.expire_due(NOW_MS + 1_000).is_empty());
        let expired = registry.expire_due(NOW_MS + 500 + 3_600_000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].revision, 3);
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
                    Some(struct_value(&[("role", "publish")])),
                    None,
                    None,
                    "transcoder-a",
                    false,
                    NOW_MS,
                )
                .expect("fill write must succeed");
        }

        let error = registry
            .put(
                "service/transcoder-a/",
                "keys/overflow",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("role", "publish")])),
                None,
                None,
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect_err("new key past the namespace bound must fail");
        assert_eq!(error, Error::Invalid(Invalid::NamespaceFull));

        let entry = registry
            .put(
                "service/transcoder-a/",
                "keys/k-0000",
                "keeppeek.media-intent.v1",
                Some(struct_value(&[("role", "subscribe")])),
                None,
                None,
                "transcoder-a",
                false,
                NOW_MS,
            )
            .expect("replacement inside a full namespace must succeed");
        assert_eq!(entry.revision, (MAX_ENTRIES_PER_NAMESPACE + 1) as u64);
    }

    #[test]
    fn pending_overflow_evicts_oldest_expiry() {
        let mut registry = Registry::default();
        for index in 0..=MAX_PENDING_EXPIRIES {
            registry
                .put(
                    &format!("service/load-{}/", index % 5),
                    &format!("leases/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(struct_value(&[("role", "publish")])),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    false,
                    NOW_MS,
                )
                .expect("overflow fill write must succeed");
        }

        let expired = registry.expire_due(NOW_MS + 1_000);
        assert_eq!(expired.len(), MAX_PENDING_EXPIRIES);
        assert!(
            expired.iter().all(|entry| entry.key != "leases/k-0000"),
            "oldest expiry must be evicted"
        );
        assert_eq!(expired[0].namespace, "service/load-0/");
        assert_eq!(expired[0].key, "leases/k-0005");
        assert_eq!(expired[MAX_PENDING_EXPIRIES - 1].key, "leases/k-4094");
        assert!(registry.expire_due(NOW_MS + 1_000).is_empty());
    }
}
