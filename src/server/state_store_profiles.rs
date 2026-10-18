use super::*;
use crate::{access::ClientClassificationReason, config, config::Config};
use prost_types::{Value, value::Kind};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
#[path = "state_store_profiles_identity.rs"]
mod identity;
use identity::{
    camera_profile_configuration, camera_profile_ids, cameras_entry, configured_profile_ids,
};

#[cfg(test)]
#[path = "state_store_profiles_lifecycle_tests.rs"]
mod lifecycle_tests;

pub(in crate::server) const NAMESPACE: &str = "service/runtime-profiles/";
pub(super) const ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
pub(super) const REGISTRY_SCHEMA: &str = "keeppeek.runtime-profiles.registry.v2";
const PROFILE_PART_SCHEMA: &str = "keeppeek.runtime-profiles.part.v1";
const PROFILE_DRAFT_PART_SCHEMA: &str = "keeppeek.runtime-profiles.draft-part.v1";
pub(in crate::server) const STATUS_SCHEMA: &str = "keeppeek.runtime-profiles.status.v1";
const ACTIVE_KEY: &str = "active";
const REGISTRY_KEY: &str = "registry";
const CAMERAS_KEY: &str = "cameras";
const CAMERAS_SCHEMA: &str = "keeppeek.runtime-profiles.cameras.v1";
pub(in crate::server) const STATUS_KEY: &str = "status";
const PROFILE_PART_TARGETS: usize = 16;
const PROFILE_DOCUMENT_BYTES_MAX: usize = 16 * 1_024;
const PROFILE_DRAFT_TTL: Duration = Duration::from_secs(120);
const PROFILE_DRAFT_COUNT_MAX: usize = 4;
const PROFILE_DRAFT_PARTS_MAX: usize = crate::profiles::MAX_PROFILES
    * crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS);
const PROFILE_DRAFT_BYTES_MAX: usize = 8 * 1_024 * 1_024;

#[derive(Debug, Default)]
pub(in crate::server) struct RuntimeProfileDrafts {
    drafts: HashMap<String, RuntimeProfileDraft>,
    bytes: usize,
}

#[derive(Debug)]
struct RuntimeProfileDraft {
    owner_id: String,
    created: Instant,
    parts: BTreeMap<(String, usize), RuntimeProfileDraftPart>,
    bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct RuntimeProfileDraftPart {
    profile_id: String,
    generation: u64,
    part_index: usize,
    part_count: usize,
    digest: String,
    cameras: BTreeMap<String, crate::profiles::CameraProfileOverride>,
    bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProfileDraftPartWire {
    draft_id: String,
    profile_id: String,
    generation: String,
    part_index: usize,
    part_count: usize,
    digest: String,
    cameras: BTreeMap<String, crate::profiles::CameraProfileOverride>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProfileDraftManifest {
    draft_id: String,
    revision: u64,
    active_profile_id: Option<String>,
    profiles: BTreeMap<String, RuntimeProfileDraftManifestProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProfileDraftManifestProfile {
    id: String,
    name: String,
    revision: u64,
    target_count: usize,
    part_count: usize,
    digest: String,
}

struct StagedProfileRegistry {
    candidate: crate::profiles::ProfileRegistry,
    draft_id: String,
    expected_profiles: BTreeMap<String, (u64, String)>,
}

pub(super) fn is_adapter_namespace(namespace: &str) -> bool {
    namespace == NAMESPACE
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Err(registry_error(
            Error::Storage("runtime profile settings are unavailable".to_owned()),
            NAMESPACE,
            ACTIVE_KEY,
        ));
    };
    let admin = principal.role == AccessRole::Administrator;
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => {
            if request.namespace != NAMESPACE {
                return Err(registry_error(
                    Error::Invalid(Invalid::Namespace),
                    &request.namespace,
                    &request.key,
                ));
            }
            if (matches!(request.key.as_str(), REGISTRY_KEY | CAMERAS_KEY)
                || request.key.starts_with("profiles/"))
                && !admin
            {
                return Err(registry_error(
                    Error::NotAuthorized,
                    NAMESPACE,
                    &request.key,
                ));
            }
            let registry = config::load_config(config_path)
                .map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, &request.key)
                })?
                .runtime_profiles;
            let entry = match request.key.as_str() {
                ACTIVE_KEY => active_entry(registry),
                REGISTRY_KEY => registry_entry(registry)
                    .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?,
                CAMERAS_KEY => cameras_entry(state)
                    .map_err(|error| registry_error(error, NAMESPACE, CAMERAS_KEY))?,
                key if key.starts_with("profiles/") => profile_part_entry(&registry, key)
                    .map_err(|error| registry_error(error, NAMESPACE, key))?,
                STATUS_KEY => status_entry(visible_status(state, principal)).map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, STATUS_KEY)
                })?,
                _ => {
                    return Err(registry_error(
                        Error::Invalid(Invalid::Key),
                        NAMESPACE,
                        &request.key,
                    ));
                }
            };
            entry_result(entry)
        }
        Some(state_store_command::Action::Put(request))
            if request.namespace == NAMESPACE && request.key == REGISTRY_KEY =>
        {
            return put_registry(state, session_id, principal, request);
        }
        Some(state_store_command::Action::Put(request))
            if request.namespace == NAMESPACE && request.key.starts_with("drafts/") =>
        {
            return put_profile_draft(state, principal, request);
        }
        Some(state_store_command::Action::Put(request)) => {
            if request.namespace != NAMESPACE || request.key != ACTIVE_KEY {
                return Err(registry_error(
                    Error::Invalid(Invalid::Key),
                    &request.namespace,
                    &request.key,
                ));
            }
            if request.schema != ACTIVE_SCHEMA {
                return Err(registry_error(
                    Error::Invalid(Invalid::Schema),
                    NAMESPACE,
                    ACTIVE_KEY,
                ));
            }
            let layout = validate_namespace(NAMESPACE)
                .map_err(|error| registry_error(error, NAMESPACE, ACTIVE_KEY))?;
            authorize_write(&layout, &principal.id(), admin)
                .map_err(|error| registry_error(error, NAMESPACE, ACTIVE_KEY))?;
            if request.ttl.is_some() {
                return Err(registry_error(
                    Error::Invalid(Invalid::Ttl),
                    NAMESPACE,
                    ACTIVE_KEY,
                ));
            }
            if request.expected_revision.is_none() {
                return Err(registry_error(
                    Error::Invalid(Invalid::Schema),
                    NAMESPACE,
                    ACTIVE_KEY,
                ));
            }
            let value = request
                .value
                .ok_or_else(|| invalid("profile activation value is required"))?;
            let profile_id = parse_active_profile_id(&value)
                .map_err(|error| registry_error(error, NAMESPACE, ACTIVE_KEY))?;
            let _guard = super::super::state_store_settings::lock_config(&state.config_update)
                .map_err(|error| registry_error(error, NAMESPACE, ACTIVE_KEY))?;
            let mut current: Config = config::load_config(config_path).map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
            })?;
            let mut root = current.source.clone();
            current.runtime_profiles.validate().map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
            })?;
            if request.expected_revision != Some(current.runtime_profiles.revision) {
                return Err(registry_error(
                    Error::Conflict {
                        current_revision: current.runtime_profiles.revision,
                    },
                    NAMESPACE,
                    ACTIVE_KEY,
                ));
            }
            if profile_id.as_ref().is_some_and(|profile_id| {
                !current.runtime_profiles.profiles.contains_key(profile_id)
            }) {
                return Err(registry_error(Error::NotFound, NAMESPACE, ACTIVE_KEY));
            }
            let camera_ids = current.camera_profile_identities().map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
            })?;
            let identities =
                crate::keeppeek::CameraProfileIds::new(camera_ids.clone()).map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                })?;
            if let Some(profile_id) = profile_id.as_ref() {
                let profile = current
                    .runtime_profiles
                    .profiles
                    .get(profile_id)
                    .expect("profile existence was checked");
                let configured_ids = configured_profile_ids(&identities);
                if profile
                    .cameras
                    .keys()
                    .any(|camera_id| !configured_ids.contains(camera_id))
                {
                    return Err(registry_error(
                        Error::Invalid(Invalid::Schema),
                        NAMESPACE,
                        ACTIVE_KEY,
                    ));
                }
            }
            let next_revision = current
                .runtime_profiles
                .revision
                .checked_add(1)
                .filter(|revision| *revision <= i64::MAX as u64)
                .ok_or_else(|| {
                    registry_error(
                        Error::Storage("runtime profile revision is exhausted".to_owned()),
                        NAMESPACE,
                        ACTIVE_KEY,
                    )
                })?;
            current.runtime_profiles.revision = next_revision;
            current.runtime_profiles.active_profile_id = profile_id;
            current.runtime_profiles.validate().map_err(|_error| {
                registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, ACTIVE_KEY)
            })?;
            let previous_privacy = state.privacy.profile_overrides_snapshot();
            let next_privacy = identities.privacy_overrides(&current.runtime_profiles);
            let mut restrictive_privacy = previous_privacy.clone();
            for (camera_id, enforced) in &next_privacy {
                restrictive_privacy
                    .entry(camera_id.clone())
                    .and_modify(|previous| *previous |= *enforced)
                    .or_insert(*enforced);
            }
            state
                .privacy
                .replace_profile_overrides(restrictive_privacy)
                .map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                })?;
            root.insert(
                "runtime_profiles".to_owned(),
                config::runtime_profile_configuration_value(&current.runtime_profiles).map_err(
                    |error| {
                        registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                    },
                )?,
            );
            if let Err(error) = config::write_configuration_table(config_path, &root) {
                state
                    .privacy
                    .replace_profile_overrides(previous_privacy)
                    .ok();
                return Err(registry_error(
                    Error::Storage(error.to_string()),
                    NAMESPACE,
                    ACTIVE_KEY,
                ));
            }
            *state
                .runtime_profiles
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                current.runtime_profiles.clone();
            mark_runtime_status_pending(state, &current.runtime_profiles);
            publish_status_event(state, now_ms());
            let runtime_result = state.camera_runtime.as_ref().map(|runtime| {
                runtime.set_runtime_profiles_with_camera_ids(
                    current.runtime_profiles.clone(),
                    camera_ids,
                )
            });
            let applications = runtime_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();
            if runtime_result.as_ref().is_some_and(|result| result.is_ok()) {
                state
                    .privacy
                    .replace_profile_overrides(next_privacy)
                    .map_err(|error| {
                        registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                    })?;
            }
            let runtime_error = runtime_result
                .as_ref()
                .and_then(|result| result.as_ref().err().map(|error| error.to_string()));
            update_runtime_status(
                state,
                &current.runtime_profiles,
                applications,
                runtime_error,
                runtime_result.is_none(),
            );
            let audit_target = current.runtime_profiles.active_profile_id.clone();
            let entry = active_entry(current.runtime_profiles);
            let classification = state
                .api_session_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session_id)
                .map_or(ClientClassificationReason::UnknownSession, |session| {
                    session.classification.reason
                });
            super::super::record_access_audit(
                state,
                i64::try_from(now_ms()).unwrap_or(i64::MAX),
                Some(&principal.id()),
                Some(principal.role),
                "runtime_profile_activated",
                Some(audit_target.as_deref().unwrap_or("base")),
                "accepted",
                classification,
            );
            publish_events(
                state,
                Some(WatchEvent {
                    namespace: NAMESPACE.to_owned(),
                    key: ACTIVE_KEY.to_owned(),
                    revision: entry.revision,
                    kind: WatchEventKind::Put(entry.clone()),
                }),
                Vec::new(),
                false,
                now_ms(),
            );
            publish_status_event(state, now_ms());
            entry_result(entry)
        }
        Some(state_store_command::Action::Watch(request)) => {
            watch_profiles(state, session_id, principal, request)?
        }
        Some(state_store_command::Action::Delete(request)) => Err(registry_error(
            Error::NotAuthorized,
            &request.namespace,
            &request.key,
        ))?,
        Some(state_store_command::Action::Unwatch(request)) => unwatch(state, session_id, request)?,
        Some(state_store_command::Action::WatchAck(request)) => {
            acknowledge(state, session_id, request)?
        }
        None => return Err(invalid("state store command has no action")),
    };
    Ok(control_ok::Result::StateStoreResult(result))
}

fn watch_profiles(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::WatchState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    if request.namespace != NAMESPACE {
        return Err(registry_error(
            Error::Invalid(Invalid::Namespace),
            &request.namespace,
            &request.watch_id,
        ));
    }
    super::super::validate_client_id(&request.watch_id, "watch ID")?;
    let admin = principal.role == AccessRole::Administrator;
    let key_prefix = if request.key_prefix.is_empty() && admin {
        ACTIVE_KEY
    } else {
        request.key_prefix.as_str()
    };
    if key_prefix.is_empty() {
        return Err(registry_error(Error::NotAuthorized, NAMESPACE, ACTIVE_KEY));
    }
    if key_prefix != ACTIVE_KEY
        && key_prefix != STATUS_KEY
        && (key_prefix != REGISTRY_KEY || !admin)
    {
        return Err(registry_error(
            Error::Invalid(Invalid::Key),
            NAMESPACE,
            key_prefix,
        ));
    }
    let _config_guard = super::super::state_store_settings::lock_config(&state.config_update)
        .map_err(|error| registry_error(error, NAMESPACE, &request.watch_id))?;
    let config_path = state
        .camera_config_path
        .as_deref()
        .expect("profile dispatch checked configuration path");
    let registry = config::load_config(config_path)
        .map_err(|error| registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY))?
        .runtime_profiles;
    if key_prefix == STATUS_KEY {
        let identities = camera_profile_ids(state).ok();
        let status = state
            .runtime_profile_status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = status_entry(project_status(
            state,
            principal,
            status.clone(),
            identities.as_ref(),
        ))
        .map_err(|error| {
            registry_error(Error::Storage(error.to_string()), NAMESPACE, STATUS_KEY)
        })?;
        // Registration must precede any writer that could invalidate this snapshot.
        #[cfg(test)]
        tests::pause_status_registration(&request.watch_id);
        let result = register_profile_watch(state, session_id, request, entry);
        drop(status);
        return result;
    }
    let entry = if key_prefix == REGISTRY_KEY {
        registry_entry(registry).map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?
    } else {
        active_entry(registry)
    };
    register_profile_watch(state, session_id, request, entry)
}

fn register_profile_watch(
    state: &ServerState,
    session_id: SessionId,
    request: proto::WatchState,
    entry: StoredEntry,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let message = proto::StateWatchSnapshot {
        watch_id: request.watch_id.clone(),
        namespace: NAMESPACE.to_owned(),
        key_prefix: request.key_prefix.clone(),
        snapshot_revision: entry.revision,
        entries: vec![proto_entry(&entry)],
    };
    check_snapshot_bounds(
        message.entries.len(),
        message.encoded_len(),
        NAMESPACE,
        &request.watch_id,
    )?;
    state
        .state_store_watches
        .register(
            session_id,
            NAMESPACE.to_owned(),
            request.key_prefix,
            request.watch_id.clone(),
        )
        .map_err(|error| registry_error(error, NAMESPACE, &request.watch_id))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Watch(message)),
    })
}

fn parse_active_profile_id(value: &Struct) -> Result<Option<String>, Error> {
    if value.fields.len() != 1 {
        return Err(Error::Invalid(Invalid::Schema));
    }
    let profile_id = match value
        .fields
        .get("profile_id")
        .and_then(|value| value.kind.as_ref())
    {
        Some(Kind::StringValue(profile_id)) => profile_id,
        _ => return Err(Error::Invalid(Invalid::Schema)),
    };
    if profile_id.is_empty() {
        Ok(None)
    } else {
        Ok(Some(profile_id.clone()))
    }
}

fn put_profile_draft(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::PutState,
) -> Result<control_ok::Result, ControlCommandError> {
    let (draft_id, profile_id, part_index) = parse_draft_part_key(&request.key)
        .map_err(|error| registry_error(error, NAMESPACE, &request.key))?;
    if request.schema != PROFILE_DRAFT_PART_SCHEMA {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            &request.key,
        ));
    }
    if request.ttl.is_some() {
        return Err(registry_error(
            Error::Invalid(Invalid::Ttl),
            NAMESPACE,
            &request.key,
        ));
    }
    let layout = validate_namespace(NAMESPACE)
        .map_err(|error| registry_error(error, NAMESPACE, &request.key))?;
    authorize_write(
        &layout,
        &principal.id(),
        principal.role == AccessRole::Administrator,
    )
    .map_err(|error| registry_error(error, NAMESPACE, &request.key))?;
    let value = request
        .value
        .ok_or_else(|| invalid("runtime profile draft part is required"))?;
    let bytes = value.encoded_len();
    if bytes > PROFILE_DOCUMENT_BYTES_MAX {
        return Err(registry_error(
            Error::Invalid(Invalid::ValueTooLarge),
            NAMESPACE,
            &request.key,
        ));
    }
    let mut json = super::super::peek_layouts::struct_to_json(value);
    normalize_draft_integer_fields(&mut json)
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, &request.key))?;
    let wire: RuntimeProfileDraftPartWire = serde_json::from_value(json)
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, &request.key))?;
    let generation = wire
        .generation
        .parse::<u64>()
        .ok()
        .filter(|generation| *generation > 0 && *generation <= i64::MAX as u64)
        .ok_or_else(|| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, &request.key))?;
    if wire.draft_id != draft_id
        || wire.profile_id != profile_id
        || wire.part_index != part_index
        || wire.part_count == 0
        || wire.part_count > crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS)
        || wire.part_index >= wire.part_count
        || wire.cameras.is_empty()
        || wire.cameras.len() > PROFILE_PART_TARGETS
        || !is_profile_digest(&wire.digest)
    {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            &request.key,
        ));
    }
    let part = RuntimeProfileDraftPart {
        profile_id,
        generation,
        part_index,
        part_count: wire.part_count,
        digest: wire.digest,
        cameras: wire.cameras,
        bytes,
    };
    let now = Instant::now();
    let mut drafts = state
        .runtime_profile_drafts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    purge_profile_drafts(&mut drafts, now);
    let existing = drafts.drafts.get(&draft_id);
    if existing.is_some_and(|draft| draft.owner_id != principal.id()) {
        return Err(registry_error(
            Error::NotAuthorized,
            NAMESPACE,
            &request.key,
        ));
    }
    let existing_part =
        existing.and_then(|draft| draft.parts.get(&(part.profile_id.clone(), part.part_index)));
    if let Some(existing_part) = existing_part {
        if existing_part == &part {
            return Ok(control_ok::Result::StateStoreResult(entry_result(
                profile_draft_entry(&draft_id, &part, &principal.id()),
            )));
        }
        return Err(registry_error(
            Error::Conflict {
                current_revision: 1,
            },
            NAMESPACE,
            &request.key,
        ));
    }
    if existing.is_none() && drafts.drafts.len() >= PROFILE_DRAFT_COUNT_MAX {
        return Err(registry_error(
            Error::Invalid(Invalid::StoreFull),
            NAMESPACE,
            &request.key,
        ));
    }
    if drafts.bytes.saturating_add(bytes) > PROFILE_DRAFT_BYTES_MAX
        || existing.is_some_and(|draft| draft.parts.len() >= PROFILE_DRAFT_PARTS_MAX)
    {
        return Err(registry_error(
            Error::Invalid(Invalid::StoreFull),
            NAMESPACE,
            &request.key,
        ));
    }
    let draft = drafts
        .drafts
        .entry(draft_id.clone())
        .or_insert_with(|| RuntimeProfileDraft {
            owner_id: principal.id(),
            created: now,
            parts: BTreeMap::new(),
            bytes: 0,
        });
    draft.bytes += bytes;
    draft
        .parts
        .insert((part.profile_id.clone(), part.part_index), part.clone());
    drafts.bytes += bytes;
    Ok(control_ok::Result::StateStoreResult(entry_result(
        profile_draft_entry(&draft_id, &part, &principal.id()),
    )))
}

fn parse_draft_part_key(key: &str) -> Result<(String, String, usize), Error> {
    let mut segments = key.split('/');
    if segments.next() != Some("drafts") {
        return Err(Error::Invalid(Invalid::Key));
    }
    let draft_id = segments.next().ok_or(Error::Invalid(Invalid::Key))?;
    let profile_id = segments.next().ok_or(Error::Invalid(Invalid::Key))?;
    let part_index = segments
        .next()
        .ok_or(Error::Invalid(Invalid::Key))?
        .parse::<usize>()
        .map_err(|_| Error::Invalid(Invalid::Key))?;
    if segments.next().is_some()
        || uuid::Uuid::parse_str(draft_id).is_err()
        || draft_id.len() != 36
        || profile_id.is_empty()
        || profile_id.len() > 128
    {
        return Err(Error::Invalid(Invalid::Key));
    }
    super::validate_key(key)?;
    Ok((draft_id.to_owned(), profile_id.to_owned(), part_index))
}

fn is_profile_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn purge_profile_drafts(drafts: &mut RuntimeProfileDrafts, now: Instant) {
    drafts.drafts.retain(|_, draft| {
        if now.saturating_duration_since(draft.created) < PROFILE_DRAFT_TTL {
            return true;
        }
        drafts.bytes = drafts.bytes.saturating_sub(draft.bytes);
        false
    });
}

fn profile_draft_entry(
    draft_id: &str,
    part: &RuntimeProfileDraftPart,
    owner_id: &str,
) -> StoredEntry {
    let value = super::super::peek_layouts::json_to_struct(serde_json::json!({
        "draft_id": draft_id,
        "profile_id": part.profile_id,
        "generation": part.generation.to_string(),
        "part_index": part.part_index,
        "part_count": part.part_count,
        "digest": part.digest,
        "cameras": part.cameras,
    }))
    .expect("profile draft part was already encoded");
    StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: format!("drafts/{draft_id}/{}/{:}", part.profile_id, part.part_index),
        schema: PROFILE_DRAFT_PART_SCHEMA.to_owned(),
        value,
        revision: 1,
        updated_ms: 0,
        expires_ms: None,
        owner_id: owner_id.to_owned(),
    }
}

fn staged_registry_candidate(
    state: &ServerState,
    owner_id: &str,
    mut value: serde_json::Value,
) -> Result<StagedProfileRegistry, Error> {
    normalize_revision_fields(&mut value, false).map_err(|_| Error::Invalid(Invalid::Schema))?;
    normalize_draft_integer_fields(&mut value).map_err(|_| Error::Invalid(Invalid::Schema))?;
    let manifest: RuntimeProfileDraftManifest =
        serde_json::from_value(value).map_err(|_| Error::Invalid(Invalid::Schema))?;
    if uuid::Uuid::parse_str(&manifest.draft_id).is_err()
        || manifest.draft_id.len() != 36
        || manifest.profiles.len() > crate::profiles::MAX_PROFILES
    {
        return Err(Error::Invalid(Invalid::Schema));
    }
    let expected_part_count = manifest
        .profiles
        .values()
        .try_fold(0usize, |count, profile| {
            if profile.target_count > crate::profiles::MAX_PROFILE_TARGETS
                || profile.part_count != profile.target_count.div_ceil(PROFILE_PART_TARGETS)
                || !is_profile_digest(&profile.digest)
            {
                return None;
            }
            count.checked_add(profile.part_count)
        })
        .ok_or(Error::Invalid(Invalid::Schema))?;
    if expected_part_count > PROFILE_DRAFT_PARTS_MAX {
        return Err(Error::Invalid(Invalid::Schema));
    }
    let now = Instant::now();
    let mut drafts = state
        .runtime_profile_drafts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    purge_profile_drafts(&mut drafts, now);
    let draft = drafts.drafts.get(&manifest.draft_id);
    if let Some(draft) = draft {
        if draft.owner_id != owner_id {
            return Err(Error::NotAuthorized);
        }
        if draft.parts.len() != expected_part_count {
            return Err(Error::Invalid(Invalid::Schema));
        }
    } else if expected_part_count != 0 {
        return Err(Error::NotFound);
    }
    let mut profiles = BTreeMap::new();
    let mut expected_profiles = BTreeMap::new();
    for (key, metadata) in manifest.profiles {
        if key != metadata.id {
            return Err(Error::Invalid(Invalid::Schema));
        }
        let mut cameras = BTreeMap::new();
        for part_index in 0..metadata.part_count {
            let part = draft
                .and_then(|draft| draft.parts.get(&(metadata.id.clone(), part_index)))
                .ok_or(Error::Invalid(Invalid::Schema))?;
            if part.profile_id != metadata.id
                || part.generation != metadata.revision
                || part.part_index != part_index
                || part.part_count != metadata.part_count
                || part.digest != metadata.digest
            {
                return Err(Error::Invalid(Invalid::Schema));
            }
            for (camera_id, override_) in &part.cameras {
                if cameras
                    .insert(camera_id.clone(), override_.clone())
                    .is_some()
                {
                    return Err(Error::Invalid(Invalid::Schema));
                }
            }
        }
        if cameras.len() != metadata.target_count {
            return Err(Error::Invalid(Invalid::Schema));
        }
        profiles.insert(
            key.clone(),
            crate::profiles::RuntimeProfile {
                id: metadata.id,
                name: metadata.name,
                revision: metadata.revision,
                cameras,
            },
        );
        expected_profiles.insert(key, (metadata.revision, metadata.digest));
    }
    Ok(StagedProfileRegistry {
        candidate: crate::profiles::ProfileRegistry {
            revision: manifest.revision,
            active_profile_id: manifest.active_profile_id,
            profiles,
        },
        draft_id: manifest.draft_id,
        expected_profiles,
    })
}

fn normalize_draft_integer_fields(value: &mut serde_json::Value) -> anyhow::Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, field) in fields {
                if matches!(name.as_str(), "part_index" | "part_count" | "target_count") {
                    let maximum = match name.as_str() {
                        "part_index" => PROFILE_PART_TARGETS - 1,
                        "part_count" => {
                            crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS)
                        }
                        "target_count" => crate::profiles::MAX_PROFILE_TARGETS,
                        _ => unreachable!(),
                    };
                    let number = field
                        .as_f64()
                        .filter(|number| {
                            number.is_finite()
                                && *number >= 0.0
                                && *number <= maximum as f64
                                && number.fract() == 0.0
                        })
                        .ok_or_else(|| anyhow::anyhow!("invalid profile draft count"))?;
                    *field = serde_json::Value::Number((number as u64).into());
                } else {
                    normalize_draft_integer_fields(field)?;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_draft_integer_fields(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn remove_profile_draft(state: &ServerState, draft_id: &str) {
    let mut drafts = state
        .runtime_profile_drafts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(draft) = drafts.drafts.remove(draft_id) {
        drafts.bytes = drafts.bytes.saturating_sub(draft.bytes);
    }
}

fn put_registry(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::PutState,
) -> Result<control_ok::Result, ControlCommandError> {
    if request.schema != REGISTRY_SCHEMA {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let layout = validate_namespace(NAMESPACE)
        .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;
    authorize_write(
        &layout,
        &principal.id(),
        principal.role == AccessRole::Administrator,
    )
    .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;
    if request.ttl.is_some() || request.expected_revision.is_none() {
        return Err(registry_error(
            Error::Invalid(if request.ttl.is_some() {
                Invalid::Ttl
            } else {
                Invalid::Schema
            }),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let value = request
        .value
        .ok_or_else(|| invalid("runtime profile registry is required"))?;
    if value.encoded_len() > MAX_VALUE_BYTES {
        return Err(registry_error(
            Error::Invalid(Invalid::ValueTooLarge),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Err(registry_error(
            Error::Storage("runtime profile settings are unavailable".to_owned()),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    };
    let _guard = super::super::state_store_settings::lock_config(&state.config_update)
        .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;
    let current: Config = config::load_config(config_path).map_err(|error| {
        registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
    })?;
    let camera_ids = current.camera_profile_identities().map_err(|error| {
        registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
    })?;
    let mut root = current.source;
    let previous_registry = current.runtime_profiles;
    if request.expected_revision != Some(previous_registry.revision) {
        return Err(registry_error(
            Error::Conflict {
                current_revision: previous_registry.revision,
            },
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let mut registry_value = super::super::peek_layouts::struct_to_json(value);
    let (mut candidate, draft_id, draft_profiles) = if registry_value.get("draft_id").is_some() {
        let staged = staged_registry_candidate(state, &principal.id(), registry_value)
            .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;
        (
            staged.candidate,
            Some(staged.draft_id),
            Some(staged.expected_profiles),
        )
    } else {
        normalize_revision_fields(&mut registry_value, false).map_err(|_| {
            registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY)
        })?;
        let candidate = serde_json::from_value(registry_value).map_err(|_| {
            registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY)
        })?;
        (candidate, None, None)
    };
    if candidate.revision != previous_registry.revision {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    if candidate.active_profile_id != previous_registry.active_profile_id {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let identities =
        crate::keeppeek::CameraProfileIds::new(camera_ids.clone()).map_err(|error| {
            registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
        })?;
    let configured_ids = configured_profile_ids(&identities);
    candidate
        .validate_targets(&configured_ids, &previous_registry)
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY))?;
    for (profile_id, profile) in &mut candidate.profiles {
        profile.revision = match previous_registry.profiles.get(profile_id) {
            Some(previous) if previous == profile => previous.revision,
            Some(previous) => previous.revision.checked_add(1).ok_or_else(|| {
                registry_error(
                    Error::Storage("runtime profile revision is exhausted".to_owned()),
                    NAMESPACE,
                    REGISTRY_KEY,
                )
            })?,
            None => 1,
        };
    }
    candidate.revision = previous_registry
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= i64::MAX as u64)
        .ok_or_else(|| {
            registry_error(
                Error::Storage("runtime profile revision is exhausted".to_owned()),
                NAMESPACE,
                REGISTRY_KEY,
            )
        })?;
    if let Some(draft_profiles) = draft_profiles.as_ref()
        && (candidate.profiles.len() != draft_profiles.len()
            || candidate.profiles.iter().any(|(id, profile)| {
                draft_profiles.get(id).is_none_or(|(revision, digest)| {
                    *revision != profile.revision
                        || profile_digest(profile).as_deref() != Ok(digest.as_str())
                })
            }))
    {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    candidate
        .validate()
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY))?;
    let registry_entry = registry_entry(candidate.clone())
        .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;

    let previous_privacy = state.privacy.profile_overrides_snapshot();
    let next_privacy = identities.privacy_overrides(&candidate);
    let mut restrictive_privacy = previous_privacy.clone();
    for (camera_id, enforced) in &next_privacy {
        restrictive_privacy
            .entry(camera_id.clone())
            .and_modify(|previous| *previous |= *enforced)
            .or_insert(*enforced);
    }
    state
        .privacy
        .replace_profile_overrides(restrictive_privacy)
        .map_err(|error| {
            registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
        })?;
    root.insert(
        "runtime_profiles".to_owned(),
        config::runtime_profile_configuration_value(&candidate).map_err(|error| {
            registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
        })?,
    );
    if let Err(error) = config::write_configuration_table(config_path, &root) {
        state
            .privacy
            .replace_profile_overrides(previous_privacy)
            .ok();
        return Err(registry_error(
            Error::Storage(error.to_string()),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    if let Some(draft_id) = draft_id {
        remove_profile_draft(state, &draft_id);
    }
    *state
        .runtime_profiles
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = candidate.clone();
    mark_runtime_status_pending(state, &candidate);
    publish_status_event(state, now_ms());
    let runtime_result = state
        .camera_runtime
        .as_ref()
        .map(|runtime| runtime.set_runtime_profiles_with_camera_ids(candidate.clone(), camera_ids));
    let applications = runtime_result
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .cloned();
    if runtime_result.as_ref().is_some_and(|result| result.is_ok()) {
        state
            .privacy
            .replace_profile_overrides(next_privacy)
            .map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
            })?;
    }
    let runtime_error = runtime_result
        .as_ref()
        .and_then(|result| result.as_ref().err().map(|error| error.to_string()));
    update_runtime_status(
        state,
        &candidate,
        applications,
        runtime_error,
        runtime_result.is_none(),
    );

    let classification = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .map_or(ClientClassificationReason::UnknownSession, |session| {
            session.classification.reason
        });
    super::super::record_access_audit(
        state,
        i64::try_from(now_ms()).unwrap_or(i64::MAX),
        Some(&principal.id()),
        Some(principal.role),
        "runtime_profile_registry_updated",
        None,
        "accepted",
        classification,
    );
    let active_entry = active_entry(candidate);
    publish_events(
        state,
        Some(WatchEvent {
            namespace: NAMESPACE.to_owned(),
            key: REGISTRY_KEY.to_owned(),
            revision: registry_entry.revision,
            kind: WatchEventKind::Put(registry_entry.clone()),
        }),
        Vec::new(),
        false,
        now_ms(),
    );
    publish_status_event(state, now_ms());
    publish_events(
        state,
        Some(WatchEvent {
            namespace: NAMESPACE.to_owned(),
            key: ACTIVE_KEY.to_owned(),
            revision: active_entry.revision,
            kind: WatchEventKind::Put(active_entry),
        }),
        Vec::new(),
        false,
        now_ms(),
    );
    Ok(control_ok::Result::StateStoreResult(entry_result(
        registry_entry,
    )))
}

fn active_entry(registry: crate::profiles::ProfileRegistry) -> StoredEntry {
    let profile_id = registry.active_profile_id.unwrap_or_default();
    StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: ACTIVE_KEY.to_owned(),
        schema: ACTIVE_SCHEMA.to_owned(),
        value: Struct {
            fields: BTreeMap::from([(
                "profile_id".to_owned(),
                Value {
                    kind: Some(Kind::StringValue(profile_id)),
                },
            )]),
        },
        revision: registry.revision,
        updated_ms: 0,
        expires_ms: None,
        owner_id: "server".to_owned(),
    }
}

fn update_runtime_status(
    state: &ServerState,
    profiles: &crate::profiles::ProfileRegistry,
    applications: Option<Vec<crate::profiles::CameraRuntimeApplication>>,
    runtime_error: Option<String>,
    runtime_unavailable: bool,
) {
    let (configuration, identities) = camera_profile_configuration(state);
    let configs = runtime_profile_camera_configs(configuration.as_ref(), identities.as_ref());
    let configured_camera_ids: BTreeSet<String> = configs.as_ref().map_or_else(
        || {
            state
                .camera_entries()
                .into_iter()
                .map(|camera| camera.info.id)
                .collect()
        },
        |configs| configs.keys().cloned().collect(),
    );
    let mut cameras = applications
        .unwrap_or_default()
        .into_iter()
        .filter(|application| configured_camera_ids.contains(&application.camera_id))
        .map(|application| (application.camera_id.clone(), application))
        .collect::<BTreeMap<_, _>>();
    if runtime_error.is_some() || runtime_unavailable {
        let reason = runtime_error
            .as_deref()
            .unwrap_or("camera runtime is unavailable");
        for application in degraded_runtime_applications(
            profiles,
            &configured_camera_ids,
            configs.as_ref(),
            reason,
        ) {
            cameras
                .entry(application.camera_id.clone())
                .or_insert(application);
        }
    }
    for application in profiles.missing_camera_applications(&configured_camera_ids) {
        cameras
            .entry(application.camera_id.clone())
            .or_insert(application);
    }
    let mut status = state
        .runtime_profile_status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if status.revision > profiles.revision {
        return;
    }
    if status.revision == profiles.revision
        && status.active_profile_id == profiles.active_profile_id
    {
        preserve_runtime_failures(&status.cameras, &mut cameras);
    }
    project_runtime_privacy(
        state,
        profiles,
        &configured_camera_ids,
        &mut cameras,
        identities.as_ref(),
    );
    *status = crate::profiles::RuntimeProfileStatus {
        revision: profiles.revision,
        active_profile_id: profiles.active_profile_id.clone(),
        cameras,
    };
}

fn preserve_runtime_failures(
    observed: &BTreeMap<String, crate::profiles::CameraRuntimeApplication>,
    returned: &mut BTreeMap<String, crate::profiles::CameraRuntimeApplication>,
) {
    for application in returned.values_mut() {
        if application.state == crate::profiles::RuntimeApplicationState::Applied
            && let Some(current) = observed.get(&application.camera_id)
            && current.state == crate::profiles::RuntimeApplicationState::Degraded
            && current.policy == application.policy
        {
            // The loop can observe a failure after sending the activation reply.
            application.state = current.state.clone();
            application.reason = current.reason.clone();
        }
    }
}

fn runtime_profile_camera_configs(
    configuration: Option<&Config>,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
) -> Option<BTreeMap<String, crate::cameras::CameraConfig>> {
    let configuration = configuration?;
    let identities = identities?;
    configuration.camera_configurations().map_or_else(
        |_| {
            tracing::warn!(
                "Unable to load configured cameras for runtime profile status; using catalog"
            );
            None
        },
        |configs| {
            Some(
                configs
                    .into_values()
                    .flatten()
                    .map(|camera| (identities.camera_id(camera.ip), camera))
                    .collect(),
            )
        },
    )
}

fn degraded_runtime_applications(
    profiles: &crate::profiles::ProfileRegistry,
    configured_ids: &BTreeSet<String>,
    configs: Option<&BTreeMap<String, crate::cameras::CameraConfig>>,
    reason: &str,
) -> Vec<crate::profiles::CameraRuntimeApplication> {
    configured_ids
        .iter()
        .map(|camera_id| {
            let base = crate::profiles::BaseCameraPolicy {
                available: true,
                recording_mode: configs
                    .and_then(|configs| configs.get(camera_id))
                    .map_or_else(crate::cameras::CameraRecordingMode::default, |camera| {
                        camera.recording_mode
                    }),
                privacy_enforced: false,
                notifications_enabled: true,
            };
            let profile_override = profiles
                .active_profile_id
                .as_ref()
                .and_then(|profile_id| profiles.profiles.get(profile_id))
                .and_then(|profile| profile.cameras.get(camera_id));
            crate::profiles::CameraRuntimeApplication {
                camera_id: camera_id.clone(),
                state: crate::profiles::RuntimeApplicationState::Degraded,
                policy: crate::profiles::resolve_camera_policy(&base, profile_override),
                reason: Some(reason.chars().take(256).collect()),
            }
        })
        .collect()
}

fn project_runtime_privacy(
    state: &ServerState,
    profiles: &crate::profiles::ProfileRegistry,
    configured_camera_ids: &BTreeSet<String>,
    cameras: &mut BTreeMap<String, crate::profiles::CameraRuntimeApplication>,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
) {
    for application in cameras.values_mut() {
        if !configured_camera_ids.contains(&application.camera_id) {
            continue;
        }
        let privacy_active = identities
            .and_then(|identities| identities.source(&application.camera_id))
            .ok_or_else(|| anyhow::anyhow!("camera profile source is unavailable"))
            .and_then(|ip| state.privacy.decision(&ip.to_string(), chrono::Utc::now()));
        match privacy_active {
            Ok((active, _)) => {
                application.policy.privacy_enforced = active;
                application.policy.privacy_source = if active
                    && profiles
                        .active_profile_id
                        .as_ref()
                        .and_then(|profile_id| profiles.profiles.get(profile_id))
                        .and_then(|profile| profile.cameras.get(&application.camera_id))
                        .is_some_and(|camera| camera.privacy_enforced == Some(true))
                {
                    crate::profiles::FieldSource::Profile
                } else {
                    crate::profiles::FieldSource::Base
                };
            }
            Err(error) => {
                application.policy.privacy_enforced = true;
                if application.state != crate::profiles::RuntimeApplicationState::Degraded {
                    application.reason = Some(error.to_string().chars().take(256).collect());
                }
                application.state = crate::profiles::RuntimeApplicationState::Degraded;
            }
        }
    }
}

fn mark_runtime_status_pending(state: &ServerState, profiles: &crate::profiles::ProfileRegistry) {
    let mut status = state
        .runtime_profile_status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    status.revision = profiles.revision;
    status.active_profile_id = profiles.active_profile_id.clone();
    for application in status.cameras.values_mut() {
        application.state = crate::profiles::RuntimeApplicationState::Pending;
        application.reason = None;
    }
}

pub(in crate::server) fn publish_status_event(state: &ServerState, updated_ms: u64) {
    let status = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Keep newer status writes behind this event's queue admission.
    if let Ok(entry) = status_entry(status.clone()) {
        publish_events(
            state,
            Some(WatchEvent {
                namespace: NAMESPACE.to_owned(),
                key: STATUS_KEY.to_owned(),
                revision: entry.revision,
                kind: WatchEventKind::Put(entry),
            }),
            Vec::new(),
            false,
            updated_ms,
        );
    }
    drop(status);
}

fn status_entry(status: crate::profiles::RuntimeProfileStatus) -> anyhow::Result<StoredEntry> {
    let revision = status.revision;
    let mut value = serde_json::to_value(status)?;
    normalize_revision_fields(&mut value, true)?;
    let value = super::super::peek_layouts::json_to_struct(value)?;
    Ok(StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: STATUS_KEY.to_owned(),
        schema: STATUS_SCHEMA.to_owned(),
        value,
        revision,
        updated_ms: 0,
        expires_ms: None,
        owner_id: "server".to_owned(),
    })
}

fn visible_status(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> crate::profiles::RuntimeProfileStatus {
    let identities = camera_profile_ids(state).ok();
    let status = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    project_status(state, principal, status, identities.as_ref())
}

fn project_status(
    state: &ServerState,
    principal: &ApiPrincipal,
    mut status: crate::profiles::RuntimeProfileStatus,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
) -> crate::profiles::RuntimeProfileStatus {
    if principal.role == AccessRole::Administrator {
        return status;
    }
    let identities = match identities {
        Some(identities) => identities,
        None => {
            status.cameras.clear();
            return status;
        }
    };
    let visible_camera_ids = super::super::camera_access::for_principal(state, principal)
        .ok()
        .and_then(|policy| super::super::camera_access::query_cameras(state, &policy, &[]).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|camera| {
            camera
                .info
                .ip
                .parse()
                .ok()
                .map(|ip| identities.camera_id(ip))
        })
        .collect::<BTreeSet<_>>();
    status
        .cameras
        .retain(|camera_id, _| visible_camera_ids.contains(camera_id));
    status
}

fn registry_entry(registry: crate::profiles::ProfileRegistry) -> Result<StoredEntry, Error> {
    let profiles = registry
        .profiles
        .iter()
        .map(|(id, profile)| {
            Ok((
                id.clone(),
                serde_json::json!({
                    "id": profile.id,
                    "name": profile.name,
                    "revision": profile.revision.to_string(),
                    "target_count": profile.cameras.len(),
                    "part_count": profile.cameras.len().div_ceil(PROFILE_PART_TARGETS),
                    "digest": profile_digest(profile)?,
                }),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let value = super::super::peek_layouts::json_to_struct(serde_json::json!({
        "revision": registry.revision.to_string(),
        "active_profile_id": registry.active_profile_id,
        "profiles": profiles,
    }))
    .map_err(|error| Error::Storage(error.to_string()))?;
    if value.encoded_len() > PROFILE_DOCUMENT_BYTES_MAX {
        return Err(Error::Invalid(Invalid::ValueTooLarge));
    }
    Ok(StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: REGISTRY_KEY.to_owned(),
        schema: REGISTRY_SCHEMA.to_owned(),
        value,
        revision: registry.revision,
        updated_ms: 0,
        expires_ms: None,
        owner_id: "server".to_owned(),
    })
}

fn profile_part_entry(
    registry: &crate::profiles::ProfileRegistry,
    key: &str,
) -> Result<StoredEntry, Error> {
    let mut segments = key.split('/');
    if segments.next() != Some("profiles") {
        return Err(Error::Invalid(Invalid::Key));
    }
    let profile_id = segments.next().ok_or(Error::Invalid(Invalid::Key))?;
    let generation = segments
        .next()
        .ok_or(Error::Invalid(Invalid::Key))?
        .parse::<u64>()
        .map_err(|_| Error::Invalid(Invalid::Key))?;
    let part_index = segments
        .next()
        .ok_or(Error::Invalid(Invalid::Key))?
        .parse::<usize>()
        .map_err(|_| Error::Invalid(Invalid::Key))?;
    if segments.next().is_some() {
        return Err(Error::Invalid(Invalid::Key));
    }
    super::validate_key(key)?;
    let profile = registry.profiles.get(profile_id).ok_or(Error::NotFound)?;
    if profile.revision != generation {
        return Err(Error::Conflict {
            current_revision: profile.revision,
        });
    }
    let part_count = profile.cameras.len().div_ceil(PROFILE_PART_TARGETS);
    if part_index >= part_count {
        return Err(Error::NotFound);
    }
    let cameras = profile
        .cameras
        .iter()
        .skip(part_index * PROFILE_PART_TARGETS)
        .take(PROFILE_PART_TARGETS)
        .map(|(camera_id, override_)| (camera_id.clone(), override_))
        .collect::<BTreeMap<_, _>>();
    let value = super::super::peek_layouts::json_to_struct(serde_json::json!({
        "profile_id": profile.id,
        "generation": profile.revision.to_string(),
        "part_index": part_index,
        "part_count": part_count,
        "digest": profile_digest(profile)?,
        "cameras": cameras,
    }))
    .map_err(|error| Error::Storage(error.to_string()))?;
    if value.encoded_len() > PROFILE_DOCUMENT_BYTES_MAX {
        return Err(Error::Invalid(Invalid::ValueTooLarge));
    }
    Ok(StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: key.to_owned(),
        schema: PROFILE_PART_SCHEMA.to_owned(),
        value,
        revision: profile.revision,
        updated_ms: 0,
        expires_ms: None,
        owner_id: "server".to_owned(),
    })
}

fn profile_digest(profile: &crate::profiles::RuntimeProfile) -> Result<String, Error> {
    let mut value =
        serde_json::to_value(profile).map_err(|error| Error::Storage(error.to_string()))?;
    normalize_revision_fields(&mut value, true)
        .map_err(|error| Error::Storage(error.to_string()))?;
    let bytes = serde_json::to_vec(&value).map_err(|error| Error::Storage(error.to_string()))?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").map_err(|error| Error::Storage(error.to_string()))?;
    }
    Ok(encoded)
}

fn normalize_revision_fields(value: &mut serde_json::Value, encode: bool) -> anyhow::Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, field) in fields {
                if name == "revision" {
                    *field = if encode {
                        let revision = field
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("invalid profile revision"))?;
                        serde_json::Value::String(revision.to_string())
                    } else {
                        let revision = field
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("invalid profile revision"))?
                            .parse::<u64>()?;
                        serde_json::Value::Number(revision.into())
                    };
                } else {
                    normalize_revision_fields(field, encode)?;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_revision_fields(value, encode)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::RuntimeProfile;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(0);

    type StatusWatchPause = (
        String,
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    static STATUS_WATCH_PAUSE: std::sync::Mutex<Option<StatusWatchPause>> =
        std::sync::Mutex::new(None);

    pub(super) fn pause_status_registration(watch_id: &str) {
        let pause = {
            let mut slot = STATUS_WATCH_PAUSE.lock().unwrap();
            if slot.as_ref().is_some_and(|pause| pause.0 == watch_id) {
                slot.take()
            } else {
                None
            }
        };
        if let Some((_, ready, resume)) = pause {
            ready.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    struct Fixture {
        directory: std::path::PathBuf,
        state: ServerState,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "keeppeek-state-store-profiles-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("config.toml");
            let mut config = Config::default();
            config.runtime_profiles.profiles.insert(
                "away".to_owned(),
                RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 1,
                    cameras: Default::default(),
                },
            );
            let root: toml::Table = toml::Value::try_from(&config)
                .unwrap()
                .as_table()
                .unwrap()
                .clone();
            config::write_configuration_table(&path, &root).unwrap();
            let state = ServerState::empty().with_camera_config_path(path);
            Self { directory, state }
        }

        fn config_path(&self) -> std::path::PathBuf {
            self.directory.join("config.toml")
        }

        fn add_camera(&self, camera: &crate::cameras::CameraConfig) -> String {
            config::upsert_camera(&self.config_path(), camera).unwrap();
            self.state
                .upsert_camera(super::super::super::camera_entry(camera, None));
            config::load_camera_profile_identities(&self.config_path()).unwrap()[&camera.ip].clone()
        }

        fn add_profile_cameras(&self, identities: &[String]) {
            let mut root = config::load_configuration_table(&self.config_path()).unwrap();
            let mut cameras = toml::Table::new();
            for (index, identity) in identities.iter().enumerate() {
                let ip = format!("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ff{index:02x}")
                    .parse()
                    .unwrap();
                let configuration = camera_config(ip);
                let mut table = toml::Value::try_from(&configuration)
                    .unwrap()
                    .as_table()
                    .unwrap()
                    .clone();
                table.insert(
                    "profile_identity".to_owned(),
                    toml::Value::String(identity.clone()),
                );
                cameras.insert(format!("target-{index}"), toml::Value::Table(table));
                self.state
                    .upsert_camera(super::super::super::camera_entry(&configuration, None));
            }
            root.insert("cameras".to_owned(), toml::Value::Table(cameras));
            config::write_configuration_table(&self.config_path(), &root).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn put_active(profile_id: &str, expected_revision: u64) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: ACTIVE_KEY.to_owned(),
                schema: ACTIVE_SCHEMA.to_owned(),
                value: Some(Struct {
                    fields: BTreeMap::from([(
                        "profile_id".to_owned(),
                        Value {
                            kind: Some(Kind::StringValue(profile_id.to_owned())),
                        },
                    )]),
                }),
                expected_revision: Some(expected_revision),
                ttl: None,
            })),
        }
    }

    fn put_registry(
        registry: &crate::profiles::ProfileRegistry,
        expected_revision: u64,
    ) -> proto::StateStoreCommand {
        let mut value = serde_json::to_value(registry).unwrap();
        normalize_revision_fields(&mut value, true).unwrap();
        let value = super::super::super::peek_layouts::json_to_struct(value).unwrap();
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(value),
                expected_revision: Some(expected_revision),
                ttl: None,
            })),
        }
    }

    fn watch_active(watch_id: &str) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Watch(proto::WatchState {
                watch_id: watch_id.to_owned(),
                namespace: NAMESPACE.to_owned(),
                key_prefix: ACTIVE_KEY.to_owned(),
            })),
        }
    }

    fn watch_status(watch_id: &str) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Watch(proto::WatchState {
                watch_id: watch_id.to_owned(),
                namespace: NAMESPACE.to_owned(),
                key_prefix: STATUS_KEY.to_owned(),
            })),
        }
    }

    fn get_status() -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: STATUS_KEY.to_owned(),
            })),
        }
    }

    fn get_key(key: &str) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: key.to_owned(),
            })),
        }
    }

    fn full_registry() -> (crate::profiles::ProfileRegistry, Vec<String>) {
        let camera_ids = (1..=127)
            .map(|last| format!("00000000-0000-4000-8000-{last:012x}"))
            .collect::<Vec<_>>();
        let mut profiles = BTreeMap::new();
        for profile_index in 0..32 {
            let id = format!("profile-{profile_index:02}");
            let cameras = camera_ids
                .iter()
                .map(|camera_id| {
                    (
                        camera_id.clone(),
                        crate::profiles::CameraProfileOverride {
                            availability: Some(true),
                            recording_mode: Some(crate::cameras::CameraRecordingMode::Both),
                            privacy_enforced: Some(true),
                            notifications_suppressed: Some(false),
                        },
                    )
                })
                .collect();
            profiles.insert(
                id.clone(),
                RuntimeProfile {
                    id,
                    name: format!("Profile {profile_index:02}"),
                    revision: 1,
                    cameras,
                },
            );
        }
        (
            crate::profiles::ProfileRegistry {
                revision: 0,
                active_profile_id: None,
                profiles,
            },
            camera_ids,
        )
    }

    fn camera_config(ip: std::net::IpAddr) -> crate::cameras::CameraConfig {
        crate::cameras::CameraConfig {
            ip,
            name: None,
            display_name: None,
            manufacturer: None,
            username: String::new(),
            password: String::new(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: Default::default(),
            transport: Default::default(),
            events: Default::default(),
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
            event_pre_recording_duration_secs: 0,
            event_recording_stream: Default::default(),
        }
    }

    fn profile_registry_put(
        registry: &crate::profiles::ProfileRegistry,
    ) -> proto::StateStoreCommand {
        let mut value = serde_json::to_value(registry).unwrap();
        normalize_revision_fields(&mut value, true).unwrap();
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(super::super::super::peek_layouts::json_to_struct(value).unwrap()),
                expected_revision: Some(registry.revision),
                ttl: None,
            })),
        }
    }

    fn profile_draft_part_put(
        draft_id: &str,
        profile: &crate::profiles::RuntimeProfile,
        part_index: usize,
    ) -> proto::StateStoreCommand {
        let cameras = profile
            .cameras
            .iter()
            .skip(part_index * PROFILE_PART_TARGETS)
            .take(PROFILE_PART_TARGETS)
            .map(|(camera_id, override_)| (camera_id.clone(), override_))
            .collect::<BTreeMap<_, _>>();
        let value = super::super::super::peek_layouts::json_to_struct(serde_json::json!({
            "draft_id": draft_id,
            "profile_id": profile.id,
            "generation": profile.revision.to_string(),
            "part_index": part_index,
            "part_count": profile.cameras.len().div_ceil(PROFILE_PART_TARGETS),
            "digest": profile_digest(profile).unwrap(),
            "cameras": cameras,
        }))
        .unwrap();
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: format!("drafts/{draft_id}/{}/{part_index}", profile.id),
                schema: PROFILE_DRAFT_PART_SCHEMA.to_owned(),
                value: Some(value),
                expected_revision: None,
                ttl: None,
            })),
        }
    }

    fn staged_profile_registry_put(
        registry: &crate::profiles::ProfileRegistry,
        draft_id: &str,
        expected_revision: u64,
    ) -> proto::StateStoreCommand {
        let profiles = registry
            .profiles
            .iter()
            .map(|(id, profile)| {
                (
                    id.clone(),
                    serde_json::json!({
                        "id": profile.id,
                        "name": profile.name,
                        "revision": profile.revision.to_string(),
                        "target_count": profile.cameras.len(),
                        "part_count": profile.cameras.len().div_ceil(PROFILE_PART_TARGETS),
                        "digest": profile_digest(profile).unwrap(),
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let value = super::super::super::peek_layouts::json_to_struct(serde_json::json!({
            "draft_id": draft_id,
            "revision": registry.revision.to_string(),
            "active_profile_id": registry.active_profile_id,
            "profiles": profiles,
        }))
        .unwrap();
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(value),
                expected_revision: Some(expected_revision),
                ttl: None,
            })),
        }
    }

    #[test]
    fn runtime_completion_notifies_connected_profile_status_watchers() {
        use prost::Message as _;
        let fixture = Fixture::new();
        let shutdown = crate::shutdown::Shutdown::new();
        let mut runtime = crate::keeppeek::KeepPeekLoop::new(shutdown.clone(), None);
        let control = runtime.control();
        let state = fixture.state.clone().with_camera_runtime(control.clone());
        let publisher = state.clone();
        runtime.set_runtime_profile_status_publisher(move || {
            publisher.publish_runtime_profile_status();
        });
        let mut first = connected_profile_status_watch(&state, 20_221);
        let mut second = connected_profile_status_watch(&state, 20_222);
        let worker = std::thread::spawn(move || runtime.run());
        let started = Instant::now();
        let result = control.set_runtime_profiles(crate::profiles::ProfileRegistry {
            revision: 1,
            ..Default::default()
        });
        let elapsed_us = started.elapsed().as_micros();
        shutdown.cancel();
        worker.join().unwrap();
        assert!(result.is_ok());
        let mut bytes = 0;
        for frames in [first(), second()] {
            assert_eq!(
                frames.len(),
                1,
                "loop completion must reach each connected status watcher"
            );
            bytes += frames[0].len();
            let envelope = proto::ControlEnvelope::decode(frames[0].as_slice()).unwrap();
            let Some(proto::control_envelope::Message::Notification(notification)) =
                envelope.message
            else {
                panic!("completion must be a notification");
            };
            let Some(proto::notification::Event::StateStoreWatchUpdate(update)) =
                notification.event
            else {
                panic!("completion must be a watch update");
            };
            assert_eq!(update.watch_sequence, 1);
            assert_eq!(update.revision, 1);
            let entry = update.entry.unwrap();
            assert_eq!(entry.key, STATUS_KEY);
            assert_eq!(entry.schema, STATUS_SCHEMA);
        }
        eprintln!("profile completion: elapsed_us={elapsed_us} frames=2 bytes={bytes}");
    }

    fn connected_profile_status_watch(
        state: &ServerState,
        id: u64,
    ) -> impl FnMut() -> Vec<Vec<u8>> {
        let session = SessionId::from_u64(id);
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(session, super::super::super::tests::local_test_session());
        let pump = state.webrtc.stage_api_session_pump(session);
        let result = super::super::dispatch(
            state,
            session,
            &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
            watch_status("runtime-status"),
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(result) = result else {
            panic!("watch result expected")
        };
        let Some(state_store_result::Result::Watch(snapshot)) = result.result else {
            panic!("snapshot expected")
        };
        assert_eq!(snapshot.entries[0].key, STATUS_KEY);
        pump
    }

    #[test]
    fn profile_status_snapshot_blocks_updates_until_watch_registration() {
        let fixture = Fixture::new();
        let state = fixture.state.clone();
        let registration_state = state.clone();
        let watch_id = "profile-status-snapshot-fence";
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        *STATUS_WATCH_PAUSE.lock().unwrap() = Some((watch_id.to_owned(), ready_tx, resume_rx));
        let registration = std::thread::spawn(move || {
            super::super::dispatch(
                &registration_state,
                SessionId::from_u64(20_223),
                &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
                watch_status(watch_id),
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let fenced = matches!(
            state.runtime_profile_status.try_write(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        resume_tx.send(()).unwrap();
        let snapshot = registration.join().unwrap().unwrap();
        assert!(
            fenced,
            "status writes must remain blocked between snapshot and registration"
        );
        assert!(matches!(snapshot, control_ok::Result::StateStoreResult(_)));
        assert!(
            state
                .state_store_watches
                .owns_watch(SessionId::from_u64(20_223), watch_id)
        );
        assert!(state.runtime_profile_status.try_write().is_ok());
    }

    #[test]
    fn activation_uses_compare_and_set_and_persists_before_returning() {
        let fixture = Fixture::new();
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let session = SessionId::from_u64(20_202);

        let result =
            super::super::dispatch(&fixture.state, session, &principal, put_active("away", 0))
                .unwrap();

        let control_ok::Result::StateStoreResult(result) = result else {
            panic!("profile activation must return a state-store result");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("profile activation must return the active entry");
        };
        assert_eq!(entry.revision, 1);
        assert!(
            fixture
                .state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| {
                    event.action == "runtime_profile_activated"
                        && event.target_id.as_deref() == Some("away")
                        && event.result == "accepted"
                })
        );
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .active_profile_id
                .as_deref(),
            Some("away")
        );

        let stale = super::super::dispatch(&fixture.state, session, &principal, put_active("", 0))
            .expect_err("stale activation must fail");
        assert_eq!(stale.code, proto::ErrorCode::Rejected);
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .active_profile_id
                .as_deref(),
            Some("away")
        );
    }

    #[test]
    fn runtime_failure_after_durable_activation_keeps_desired_revision() {
        let fixture = Fixture::new();
        let runtime = crate::keeppeek::KeepPeekLoop::new(crate::shutdown::Shutdown::new(), None);
        let control = runtime.control();
        drop(runtime);
        let state = fixture.state.clone().with_camera_runtime(control);
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());

        let result = super::super::dispatch(
            &state,
            SessionId::from_u64(20_210),
            &principal,
            put_active("away", 0),
        )
        .expect("durable activation should succeed despite a post-commit runtime failure");
        let control_ok::Result::StateStoreResult(result) = result else {
            panic!("activation must return a state-store result");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("activation must return the desired active entry");
        };

        assert_eq!(entry.revision, 1);
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .active_profile_id
                .as_deref(),
            Some("away")
        );
        assert_eq!(
            state
                .runtime_profile_status
                .read()
                .unwrap()
                .active_profile_id
                .as_deref(),
            Some("away")
        );
    }

    #[test]
    fn failed_profile_updates_preserve_actual_held_privacy() {
        for update_registry in [false, true] {
            let fixture = Fixture::new();
            let runtime =
                crate::keeppeek::KeepPeekLoop::new(crate::shutdown::Shutdown::new(), None);
            let state = fixture.state.clone().with_camera_runtime(runtime.control());
            drop(runtime);
            let held = BTreeMap::from([("192.0.2.99".to_owned(), true)]);
            state
                .privacy
                .replace_profile_overrides(held.clone())
                .unwrap();
            let command = if update_registry {
                let mut registry = config::load_config(&fixture.config_path())
                    .unwrap()
                    .runtime_profiles;
                registry.profiles.get_mut("away").unwrap().name = "Away changed".to_owned();
                put_registry(&registry, 0)
            } else {
                put_active("away", 0)
            };
            super::super::dispatch(
                &state,
                SessionId::from_u64(20_257),
                &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
                command,
            )
            .expect("desired update must persist despite missing runtime acknowledgement");
            assert_eq!(state.privacy.profile_overrides_snapshot(), held);
            assert!(
                state
                    .privacy
                    .decision("192.0.2.99", chrono::Utc::now())
                    .unwrap()
                    .0
            );
            assert_eq!(
                config::load_config(&fixture.config_path())
                    .unwrap()
                    .runtime_profiles
                    .revision,
                1
            );
        }
    }

    #[test]
    fn missing_profile_targets_ignore_deleted_catalog_cameras_during_runtime_failure() {
        let fixture = Fixture::new();
        let camera: crate::cameras::CameraConfig =
            toml::from_str("ip='192.0.2.1'\nusername='fixture'\npassword='fixture'").unwrap();
        fixture
            .state
            .upsert_camera(super::super::super::camera_entry(&camera, None));
        let mut profiles = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        profiles.revision = 1;
        profiles.active_profile_id = Some("away".to_owned());
        profiles.profiles.get_mut("away").unwrap().cameras.insert(
            camera.ip.to_string(),
            crate::profiles::CameraProfileOverride {
                privacy_enforced: Some(true),
                ..Default::default()
            },
        );
        update_runtime_status(
            &fixture.state,
            &profiles,
            None,
            Some("runtime timed out".to_owned()),
            false,
        );
        let mut stale = fixture.state.runtime_profile_status.read().unwrap().cameras
            [&camera.ip.to_string()]
            .clone();
        stale.state = crate::profiles::RuntimeApplicationState::Applied;
        stale.reason = None;
        update_runtime_status(&fixture.state, &profiles, Some(vec![stale]), None, false);
        let status = fixture.state.runtime_profile_status.read().unwrap();
        let application = &status.cameras[&camera.ip.to_string()];
        assert_eq!(
            application.state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(
            application.reason.as_deref(),
            Some("configured camera is missing")
        );
        assert!(application.policy.privacy_enforced);
    }

    #[test]
    fn profile_status_uses_catalog_conservatively_when_configuration_is_unreadable() {
        let fixture = Fixture::new();
        let camera: crate::cameras::CameraConfig =
            toml::from_str("ip='192.0.2.1'\nusername='fixture'\npassword='fixture'").unwrap();
        fixture
            .state
            .upsert_camera(super::super::super::camera_entry(&camera, None));
        let mut profiles = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        profiles.active_profile_id = Some("away".to_owned());
        profiles.profiles.get_mut("away").unwrap().cameras.insert(
            camera.ip.to_string(),
            crate::profiles::CameraProfileOverride {
                availability: Some(false),
                ..Default::default()
            },
        );
        std::fs::remove_file(fixture.config_path()).unwrap();
        update_runtime_status(
            &fixture.state,
            &profiles,
            None,
            Some("runtime timed out".to_owned()),
            false,
        );
        let status = fixture.state.runtime_profile_status.read().unwrap();
        let application = &status.cameras[&camera.ip.to_string()];
        assert_eq!(
            application.state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(application.reason.as_deref(), Some("runtime timed out"));
        assert!(!application.policy.available);
    }

    #[test]
    fn runtime_status_projection_cannot_replace_newer_pending_revision() {
        let fixture = Fixture::new();
        let profiles = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        fixture
            .state
            .runtime_profile_status
            .write()
            .unwrap()
            .revision = 2;
        let expected = fixture.state.runtime_profile_status.read().unwrap().clone();
        update_runtime_status(
            &fixture.state,
            &profiles,
            None,
            Some("late failure".to_owned()),
            false,
        );
        assert_eq!(
            *fixture.state.runtime_profile_status.read().unwrap(),
            expected
        );
    }

    fn delayed_runtime_completion_fixture() -> (
        Fixture,
        crate::profiles::ProfileRegistry,
        crate::profiles::CameraRuntimeApplication,
    ) {
        let fixture = Fixture::new();
        let camera: crate::cameras::CameraConfig =
            toml::from_str("ip='192.0.2.1'\nusername='fixture'\npassword='fixture'").unwrap();
        config::upsert_camera(&fixture.config_path(), &camera).unwrap();
        let identity = config::load_camera_profile_identities(&fixture.config_path()).unwrap()
            [&camera.ip]
            .clone();
        let mut profiles = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        profiles.revision = 1;
        profiles.active_profile_id = Some("away".to_owned());
        let returned = crate::profiles::CameraRuntimeApplication {
            camera_id: identity,
            state: crate::profiles::RuntimeApplicationState::Applied,
            policy: crate::profiles::resolve_camera_policy(
                &crate::profiles::BaseCameraPolicy {
                    available: true,
                    recording_mode: crate::cameras::CameraRecordingMode::Sub,
                    privacy_enforced: true,
                    notifications_enabled: true,
                },
                None,
            ),
            reason: None,
        };
        let mut live = returned.clone();
        live.state = crate::profiles::RuntimeApplicationState::Degraded;
        live.reason = Some("recording writer reported an active write failure".to_owned());
        *fixture.state.runtime_profile_status.write().unwrap() =
            crate::profiles::RuntimeProfileStatus {
                revision: profiles.revision,
                active_profile_id: profiles.active_profile_id.clone(),
                cameras: BTreeMap::from([(live.camera_id.clone(), live)]),
            };
        (fixture, profiles, returned)
    }

    #[test]
    fn delayed_profile_completion_preserves_live_failure_before_privacy_projection() {
        let (fixture, profiles, returned) = delayed_runtime_completion_fixture();
        update_runtime_status(
            &fixture.state,
            &profiles,
            Some(vec![returned.clone()]),
            None,
            false,
        );
        let status = fixture.state.runtime_profile_status.read().unwrap();
        let application = &status.cameras[&returned.camera_id];
        assert_eq!(
            application.state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(
            application.reason.as_deref(),
            Some("recording writer reported an active write failure")
        );
        assert!(
            !application.policy.privacy_enforced,
            "preserved live failure must still receive current privacy projection"
        );
    }

    #[test]
    fn delayed_profile_completion_does_not_inherit_unrelated_or_pending_observations() {
        let (fixture, profiles, returned) = delayed_runtime_completion_fixture();
        let accepted = fixture.state.runtime_profile_status.read().unwrap().clone();
        for scenario in 0..4 {
            {
                let mut status = fixture.state.runtime_profile_status.write().unwrap();
                *status = accepted.clone();
                match scenario {
                    0 => {
                        status.cameras.get_mut(&returned.camera_id).unwrap().state =
                            crate::profiles::RuntimeApplicationState::Pending;
                    }
                    1 => {
                        status
                            .cameras
                            .get_mut(&returned.camera_id)
                            .unwrap()
                            .policy
                            .recording_mode = crate::cameras::CameraRecordingMode::Main;
                    }
                    2 => status.active_profile_id = None,
                    3 => status.revision = 0,
                    _ => unreachable!("four observation boundaries are tested"),
                }
            }
            update_runtime_status(
                &fixture.state,
                &profiles,
                Some(vec![returned.clone()]),
                None,
                false,
            );
            let status = fixture.state.runtime_profile_status.read().unwrap();
            assert_eq!(
                status.cameras[&returned.camera_id].state,
                crate::profiles::RuntimeApplicationState::Applied
            );
            assert!(status.cameras[&returned.camera_id].reason.is_none());
        }
    }

    #[test]
    fn delayed_profile_completion_keeps_the_returned_independent_failure() {
        let (fixture, profiles, mut returned) = delayed_runtime_completion_fixture();
        returned.state = crate::profiles::RuntimeApplicationState::Degraded;
        returned.reason = Some("notification runtime is unavailable".to_owned());
        update_runtime_status(
            &fixture.state,
            &profiles,
            Some(vec![returned.clone()]),
            None,
            false,
        );
        let status = fixture.state.runtime_profile_status.read().unwrap();
        assert_eq!(
            status.cameras[&returned.camera_id].state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(status.cameras[&returned.camera_id].reason, returned.reason);
    }

    #[test]
    fn active_profile_status_reports_removed_camera_targets_as_degraded() {
        let fixture = Fixture::new();
        let mut profiles = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        profiles.active_profile_id = Some("away".to_owned());
        profiles.profiles.get_mut("away").unwrap().cameras.insert(
            "removed-camera".to_owned(),
            crate::profiles::CameraProfileOverride {
                availability: Some(false),
                privacy_enforced: Some(true),
                ..Default::default()
            },
        );

        update_runtime_status(&fixture.state, &profiles, Some(Vec::new()), None, false);

        let status = fixture.state.runtime_profile_status.read().unwrap();
        let removed = status
            .cameras
            .get("removed-camera")
            .expect("active profile's removed target must remain visible");
        assert_eq!(
            removed.state,
            crate::profiles::RuntimeApplicationState::Degraded
        );
        assert_eq!(
            removed.reason.as_deref(),
            Some("configured camera is missing")
        );
        assert!(!removed.policy.available);
        assert!(removed.policy.privacy_enforced);
    }

    #[test]
    fn activation_is_administrator_only() {
        let fixture = Fixture::new();
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        principal.role = AccessRole::User;

        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_203),
            &principal,
            put_active("away", 0),
        )
        .expect_err("User profile activation must be denied");

        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .active_profile_id,
            None
        );
    }

    #[test]
    fn registry_put_updates_definitions_with_server_revisions() {
        let fixture = Fixture::new();
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let session = SessionId::from_u64(20_205);
        let mut registry = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        registry.profiles.get_mut("away").unwrap().name = "Away from home".to_owned();

        let result = super::super::dispatch(
            &fixture.state,
            session,
            &principal,
            put_registry(&registry, 0),
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(result) = result else {
            panic!("profile registry update must return a state-store result");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("profile registry update must return an entry");
        };
        assert_eq!(entry.key, REGISTRY_KEY);
        assert_eq!(entry.revision, 1);

        let saved = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.profiles["away"].revision, 2);
        assert_eq!(saved.profiles["away"].name, "Away from home");
        assert!(
            fixture
                .state
                .access_manager
                .list_audit(10)
                .iter()
                .any(|event| {
                    event.action == "runtime_profile_registry_updated" && event.result == "accepted"
                })
        );

        let stale = super::super::dispatch(
            &fixture.state,
            session,
            &principal,
            put_registry(&registry, 0),
        )
        .expect_err("stale registry writes must be rejected");
        assert_eq!(stale.code, proto::ErrorCode::Rejected);
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .revision,
            1
        );
    }

    fn assert_persisted_profile_identity_schema(path: &std::path::Path) {
        let root = config::load_configuration_table(path).unwrap();
        assert_eq!(
            root["runtime_profiles"]["identity_schema"].as_integer(),
            Some(1)
        );
    }

    #[test]
    fn runtime_profile_rollback_guard_survives_activation_and_removing_all_cameras() {
        let fixture = Fixture::new();
        let camera_ip = "192.0.2.10".parse().unwrap();
        let identity = fixture.add_camera(&camera_config(camera_ip));
        let path = fixture.config_path();
        let mut configuration = config::load_config(&path).unwrap();
        configuration
            .runtime_profiles
            .profiles
            .get_mut("away")
            .unwrap()
            .cameras
            .insert(
                identity.clone(),
                crate::profiles::CameraProfileOverride {
                    recording_mode: Some(crate::cameras::CameraRecordingMode::Main),
                    ..Default::default()
                },
            );
        let mut root = configuration.source;
        root.insert(
            "runtime_profiles".to_owned(),
            toml::Value::try_from(&configuration.runtime_profiles).unwrap(),
        );
        config::write_configuration_table(&path, &root).unwrap();
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let session = SessionId::from_u64(20_258);
        super::super::dispatch(&fixture.state, session, &principal, put_active("away", 0)).unwrap();
        assert_persisted_profile_identity_schema(&path);
        config::remove_camera(&path, camera_ip).unwrap();
        assert!(
            config::load_camera_profile_identities(&path)
                .unwrap()
                .is_empty()
        );
        for (revision, clear_targets) in [(1, false), (2, true)] {
            let mut registry = config::load_config(&path).unwrap().runtime_profiles;
            let profile = registry.profiles.get_mut("away").unwrap();
            profile.name = format!("Away revision {revision}");
            assert!(profile.cameras.contains_key(&identity));
            if clear_targets {
                profile.cameras.clear();
            }
            let wire = serde_json::to_value(&registry).unwrap();
            assert!(wire.get("identity_schema").is_none());
            let mut persisted_only_wire = wire;
            persisted_only_wire["identity_schema"] = serde_json::json!(1);
            assert!(
                serde_json::from_value::<crate::profiles::ProfileRegistry>(persisted_only_wire)
                    .is_err()
            );
            super::super::dispatch(
                &fixture.state,
                session,
                &principal,
                put_registry(&registry, revision),
            )
            .unwrap();
            assert_persisted_profile_identity_schema(&path);
            let saved = config::load_config(&path).unwrap().runtime_profiles;
            assert_eq!(saved.revision, revision + 1);
            assert_eq!(saved.active_profile_id.as_deref(), Some("away"));
            assert_eq!(saved.profiles["away"].cameras.is_empty(), clear_targets);
        }
    }

    #[test]
    fn registry_edits_preserve_removed_camera_targets_without_accepting_new_unknown_targets() {
        let fixture = Fixture::new();
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let path = fixture.config_path();
        let mut config_value = config::load_config(&path).unwrap();
        config_value
            .runtime_profiles
            .profiles
            .get_mut("away")
            .unwrap()
            .cameras
            .insert(
                "192.0.2.10".to_owned(),
                crate::profiles::CameraProfileOverride {
                    availability: Some(false),
                    ..crate::profiles::CameraProfileOverride::default()
                },
            );
        let root: toml::Table = toml::Value::try_from(&config_value)
            .unwrap()
            .as_table()
            .unwrap()
            .clone();
        config::write_configuration_table(&path, &root).unwrap();

        let mut registry = config_value.runtime_profiles;
        registry.profiles.get_mut("away").unwrap().name = "Away renamed".to_owned();
        super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_207),
            &principal,
            put_registry(&registry, 0),
        )
        .expect("a pre-existing target must not block later profile edits");

        let mut registry = config::load_config(&path).unwrap().runtime_profiles;
        registry.profiles.get_mut("away").unwrap().cameras.insert(
            "192.0.2.11".to_owned(),
            crate::profiles::CameraProfileOverride {
                availability: Some(false),
                ..crate::profiles::CameraProfileOverride::default()
            },
        );
        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_207),
            &principal,
            put_registry(&registry, 1),
        )
        .expect_err("new targets must still identify configured cameras");
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert_eq!(
            config::load_config(&path)
                .unwrap()
                .runtime_profiles
                .revision,
            1
        );
    }

    #[test]
    fn registry_put_rejects_stale_and_active_profile_deletion() {
        let fixture = Fixture::new();
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let session = SessionId::from_u64(20_206);
        super::super::dispatch(&fixture.state, session, &principal, put_active("away", 0))
            .expect("activation must use the active profile path");
        let active = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        assert_eq!(active.active_profile_id.as_deref(), Some("away"));

        let mut remove_active = active;
        remove_active.profiles.remove("away");
        let error = super::super::dispatch(
            &fixture.state,
            session,
            &principal,
            put_registry(&remove_active, 0),
        )
        .expect_err("active profile deletion must be rejected");
        assert_eq!(error.code, proto::ErrorCode::Rejected);

        let current = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        assert_eq!(current.active_profile_id.as_deref(), Some("away"));
        assert_eq!(current.revision, 1);
    }

    #[test]
    fn users_cannot_read_or_update_the_profile_registry() {
        let fixture = Fixture::new();
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        principal.role = AccessRole::User;
        let registry = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;

        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_207),
            &principal,
            put_registry(&registry, 0),
        )
        .expect_err("User profile registry updates must be denied");
        assert_eq!(error.code, proto::ErrorCode::Rejected);
    }

    #[test]
    fn profile_overrides_reject_every_camera_configuration_field_except_recording_mode() {
        let camera: crate::cameras::CameraConfig =
            serde_json::from_value(serde_json::json!({"ip": "192.0.2.8"}))
                .expect("minimal camera configuration should deserialize");
        // ponytail: derive the candidate fields so new camera settings stay outside profile input.
        let mut candidate_fields = serde_json::to_value(camera)
            .expect("camera configuration should serialize")
            .as_object()
            .expect("camera configuration should be an object")
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        candidate_fields.insert("events".to_owned());

        for field in candidate_fields {
            if field == "recording_mode" {
                continue;
            }
            let value = serde_json::json!({field.clone(): null});
            assert!(
                serde_json::from_value::<crate::profiles::CameraProfileOverride>(value).is_err(),
                "camera configuration field {field} must not be a profile override"
            );
        }
    }

    #[test]
    fn users_can_watch_only_the_active_profile_projection() {
        let fixture = Fixture::new();
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        principal.role = AccessRole::User;
        let session = SessionId::from_u64(20_204);

        let response =
            super::super::dispatch(&fixture.state, session, &principal, watch_active("active"))
                .unwrap();

        let control_ok::Result::StateStoreResult(response) = response else {
            panic!("active watch must return a state-store result");
        };
        let Some(state_store_result::Result::Watch(snapshot)) = response.result else {
            panic!("active watch must return a snapshot");
        };
        assert_eq!(snapshot.snapshot_revision, 0);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].key, ACTIVE_KEY);
        assert!(
            fixture
                .state
                .state_store_watches
                .owns_watch(session, "active")
        );
    }

    #[test]
    fn users_can_read_and_watch_runtime_application_status() {
        let fixture = Fixture::new();
        let session = SessionId::from_u64(20_209);
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        principal.role = AccessRole::User;

        let get =
            super::super::dispatch(&fixture.state, session, &principal, get_status()).unwrap();
        let control_ok::Result::StateStoreResult(get) = get else {
            panic!("status get must return a state-store result");
        };
        let Some(state_store_result::Result::Entry(entry)) = get.result else {
            panic!("status get must return an entry");
        };
        assert_eq!(entry.schema, STATUS_SCHEMA);

        let watch =
            super::super::dispatch(&fixture.state, session, &principal, watch_status("status"))
                .unwrap();
        let control_ok::Result::StateStoreResult(watch) = watch else {
            panic!("status watch must return a state-store result");
        };
        let Some(state_store_result::Result::Watch(snapshot)) = watch.result else {
            panic!("status watch must return a snapshot");
        };
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].schema, STATUS_SCHEMA);
    }

    #[test]
    fn profile_camera_catalog_is_admin_only_and_fits_the_127_target_bound() {
        let fixture = Fixture::new();
        let (_, identities) = full_registry();
        fixture.add_profile_cameras(&identities);
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let result = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_254),
            &principal,
            get_key(CAMERAS_KEY),
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(proto::StateStoreResult {
            result: Some(state_store_result::Result::Entry(entry)),
        }) = result
        else {
            panic!("camera catalog entry");
        };
        assert_eq!(entry.schema, CAMERAS_SCHEMA);
        let value = entry.value.unwrap();
        assert!(value.encoded_len() <= PROFILE_DOCUMENT_BYTES_MAX);
        let Some(Kind::StructValue(cameras)) = value.fields["cameras"].kind.as_ref() else {
            panic!("camera catalog map");
        };
        assert_eq!(cameras.fields.len(), 127);
        for (identity, source) in &cameras.fields {
            assert!(identities.contains(identity));
            let Some(Kind::StringValue(source)) = source.kind.as_ref() else {
                panic!("source IP");
            };
            assert_eq!(source.len(), 39);
            assert!(source.parse::<std::net::IpAddr>().is_ok());
        }
        principal.role = AccessRole::User;
        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_254),
            &principal,
            get_key(CAMERAS_KEY),
        )
        .expect_err("Users cannot read profile identity metadata");
        assert_eq!(error.code, proto::ErrorCode::Rejected);
    }

    #[test]
    fn profile_camera_catalog_rejects_a_response_above_its_document_bound() {
        let fixture = Fixture::new();
        let identities = (1..=254)
            .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
            .collect::<Vec<_>>();
        fixture.add_profile_cameras(&identities);
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_255),
            &principal,
            get_key(CAMERAS_KEY),
        )
        .expect_err("oversized identity catalog must report its capacity failure");
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert!(error.message.contains("too large"));
    }

    #[test]
    fn profile_target_validation_never_uses_a_deleted_camera_catalog_entry() {
        let fixture = Fixture::new();
        let identity = fixture.add_camera(&camera_config("192.0.2.1".parse().unwrap()));
        config::remove_camera(&fixture.config_path(), "192.0.2.1".parse().unwrap()).unwrap();
        assert!(!fixture.state.camera_entries().is_empty());
        assert!(configured_profile_ids(&camera_profile_ids(&fixture.state).unwrap()).is_empty());
        let mut registry = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        registry
            .profiles
            .get_mut("away")
            .unwrap()
            .cameras
            .insert(identity, Default::default());
        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_256),
            &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
            put_registry(&registry, 0),
        )
        .expect_err("deleted targets must not be introduced from stale discovery state");
        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles
                .profiles["away"]
                .cameras
                .is_empty()
        );
    }

    #[test]
    fn maximum_registry_put_rejects_before_persisting_an_untransportable_value() {
        let fixture = Fixture::new();
        let (registry, camera_ids) = full_registry();
        fixture.add_profile_cameras(&camera_ids);
        let header = registry_entry(registry.clone()).unwrap();
        assert!(header.value.encoded_len() <= PROFILE_DOCUMENT_BYTES_MAX);
        for profile in registry.profiles.values() {
            let part_count = profile.cameras.len().div_ceil(PROFILE_PART_TARGETS);
            for part_index in 0..part_count {
                let key = format!("profiles/{}/{}/{part_index}", profile.id, profile.revision);
                let part = profile_part_entry(&registry, &key).unwrap();
                assert!(
                    part.value.encoded_len() <= PROFILE_DOCUMENT_BYTES_MAX,
                    "profile part exceeds the bound: {key}"
                );
            }
        }

        let before = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        let result = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_210),
            &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
            profile_registry_put(&registry),
        );

        assert!(
            result
                .err()
                .is_some_and(|error| error.code == proto::ErrorCode::InvalidRequest),
            "oversized registry must be rejected before durable mutation"
        );
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles,
            before
        );
    }

    #[test]
    fn administrator_commits_a_large_profile_registry_from_bounded_parts() {
        let fixture = Fixture::new();
        let (registry, camera_ids) = full_registry();
        fixture.add_profile_cameras(&camera_ids);
        let direct = profile_registry_put(&registry);
        let Some(state_store_command::Action::Put(direct)) = direct.action else {
            unreachable!();
        };
        assert!(direct.value.unwrap().encoded_len() > super::MAX_VALUE_BYTES);

        let draft_id = "debdac58-2e3f-4330-b408-acf9090cdf93";
        let admin = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        for profile in registry.profiles.values() {
            let part_count = profile.cameras.len().div_ceil(PROFILE_PART_TARGETS);
            for part_index in 0..part_count {
                let result = super::super::dispatch(
                    &fixture.state,
                    SessionId::from_u64(20_211),
                    &admin,
                    profile_draft_part_put(draft_id, profile, part_index),
                )
                .unwrap();
                assert!(matches!(
                    result,
                    control_ok::Result::StateStoreResult(proto::StateStoreResult {
                        result: Some(state_store_result::Result::Entry(_))
                    })
                ));
            }
        }

        let result = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_211),
            &admin,
            staged_profile_registry_put(&registry, draft_id, 0),
        )
        .unwrap();
        assert!(matches!(
            result,
            control_ok::Result::StateStoreResult(proto::StateStoreResult {
                result: Some(state_store_result::Result::Entry(_))
            })
        ));
        let mut expected = registry;
        expected.revision = 1;
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles,
            expected
        );
        let drafts = fixture
            .state
            .runtime_profile_drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(drafts.drafts.is_empty());
        assert_eq!(drafts.bytes, 0);
    }

    #[test]
    fn stale_staged_registry_commit_reports_conflict_after_its_draft_was_consumed() {
        let fixture = Fixture::new();
        let camera_id = fixture.add_camera(&camera_config("192.0.2.10".parse().unwrap()));

        let mut registry = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        registry.profiles.insert(
            "new-profile".to_owned(),
            RuntimeProfile {
                id: "new-profile".to_owned(),
                name: "New profile".to_owned(),
                revision: 1,
                cameras: BTreeMap::from([(
                    camera_id,
                    crate::profiles::CameraProfileOverride {
                        availability: Some(false),
                        ..Default::default()
                    },
                )]),
            },
        );
        let profile = registry.profiles.get("new-profile").unwrap();
        let draft_id = "debdac58-2e3f-4330-b408-acf9090cdf93";
        let session = SessionId::from_u64(20_213);
        let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        super::super::dispatch(
            &fixture.state,
            session,
            &principal,
            profile_draft_part_put(draft_id, profile, 0),
        )
        .unwrap();

        let commit = staged_profile_registry_put(&registry, draft_id, 0);
        super::super::dispatch(&fixture.state, session, &principal, commit.clone())
            .expect("first staged commit should persist");
        let accepted = config::load_config(&fixture.config_path())
            .unwrap()
            .runtime_profiles;
        assert_eq!(accepted.revision, 1);

        let stale = super::super::dispatch(&fixture.state, session, &principal, commit)
            .expect_err("a retry of the consumed stale draft must report a CAS conflict");
        assert_eq!(stale.code, proto::ErrorCode::Rejected);
        assert_eq!(
            config::load_config(&fixture.config_path())
                .unwrap()
                .runtime_profiles,
            accepted
        );
    }

    #[test]
    fn users_cannot_create_profile_drafts() {
        let fixture = Fixture::new();
        let (registry, _) = full_registry();
        let profile = registry.profiles.get("profile-00").unwrap();
        let mut principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        principal.role = AccessRole::User;

        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_212),
            &principal,
            profile_draft_part_put("debdac58-2e3f-4330-b408-acf9090cdf93", profile, 0),
        )
        .expect_err("Users must not stage runtime profile definitions");

        assert_eq!(error.code, proto::ErrorCode::Rejected);
        let drafts = fixture
            .state
            .runtime_profile_drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(drafts.drafts.is_empty());
        assert_eq!(drafts.bytes, 0);
    }

    #[test]
    fn administrator_reads_bounded_profile_parts_and_users_cannot_read_definitions() {
        let fixture = Fixture::new();
        let registry = crate::profiles::ProfileRegistry {
            revision: 5,
            active_profile_id: Some("away".to_owned()),
            profiles: BTreeMap::from([(
                "away".to_owned(),
                RuntimeProfile {
                    id: "away".to_owned(),
                    name: "Away".to_owned(),
                    revision: 3,
                    cameras: BTreeMap::from([(
                        "192.0.2.10".to_owned(),
                        crate::profiles::CameraProfileOverride {
                            availability: Some(false),
                            ..Default::default()
                        },
                    )]),
                },
            )]),
        };
        let mut root = config::load_configuration_table(&fixture.config_path()).unwrap();
        root.insert(
            "runtime_profiles".to_owned(),
            toml::Value::try_from(&registry).unwrap(),
        );
        config::write_configuration_table(&fixture.config_path(), &root).unwrap();

        let admin = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        let summary = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_211),
            &admin,
            get_key(REGISTRY_KEY),
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(summary) = summary else {
            panic!("registry Get must return a StateStore result");
        };
        let Some(state_store_result::Result::Entry(summary)) = summary.result else {
            panic!("registry Get must return a summary entry");
        };
        assert_eq!(summary.schema, REGISTRY_SCHEMA);
        let mut summary_value = summary.value.unwrap();
        let profiles = summary_value.fields.remove("profiles").unwrap();
        let Some(Kind::StructValue(profiles)) = profiles.kind else {
            panic!("profile summary must be a struct");
        };
        let Some(Kind::StructValue(away)) = profiles.fields["away"].kind.as_ref() else {
            panic!("profile summary must not be a camera definition");
        };
        assert!(!away.fields.contains_key("cameras"));

        let part = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_211),
            &admin,
            get_key("profiles/away/3/0"),
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(part) = part else {
            panic!("profile part Get must return a StateStore result");
        };
        let Some(state_store_result::Result::Entry(part)) = part.result else {
            panic!("profile part Get must return an entry");
        };
        assert_eq!(part.schema, PROFILE_PART_SCHEMA);
        assert!(part.value.unwrap().fields.contains_key("cameras"));

        let mut user = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
        user.role = AccessRole::User;
        let error = super::super::dispatch(
            &fixture.state,
            SessionId::from_u64(20_212),
            &user,
            get_key("profiles/away/3/0"),
        )
        .expect_err("Users must not read profile definitions");
        assert_eq!(error.code, proto::ErrorCode::Rejected);
    }
}
