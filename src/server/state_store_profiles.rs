use super::*;
use crate::{access::ClientClassificationReason, config, config::Config};
use prost_types::{Value, value::Kind};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(in crate::server) const NAMESPACE: &str = "service/runtime-profiles/";
pub(super) const ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
pub(super) const REGISTRY_SCHEMA: &str = "keeppeek.runtime-profiles.registry.v2";
const PROFILE_PART_SCHEMA: &str = "keeppeek.runtime-profiles.part.v1";
pub(in crate::server) const STATUS_SCHEMA: &str = "keeppeek.runtime-profiles.status.v1";
const ACTIVE_KEY: &str = "active";
const REGISTRY_KEY: &str = "registry";
pub(in crate::server) const STATUS_KEY: &str = "status";
const PROFILE_PART_TARGETS: usize = 16;
const PROFILE_DOCUMENT_BYTES_MAX: usize = 16 * 1_024;

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
            if (request.key == REGISTRY_KEY || request.key.starts_with("profiles/")) && !admin {
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
            let mut root = config::load_configuration_table(config_path).map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
            })?;
            let mut current: Config = config::load_config(config_path).map_err(|error| {
                registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
            })?;
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
            if let Some(profile_id) = profile_id.as_ref() {
                let profile = current
                    .runtime_profiles
                    .profiles
                    .get(profile_id)
                    .expect("profile existence was checked");
                let camera_ids = state
                    .camera_entries()
                    .into_iter()
                    .map(|camera| camera.info.id)
                    .collect::<BTreeSet<_>>();
                if profile
                    .cameras
                    .keys()
                    .any(|camera_id| !camera_ids.contains(camera_id))
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
            let previous_privacy = state
                .runtime_profiles
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active_privacy_overrides();
            let next_privacy = current.runtime_profiles.active_privacy_overrides();
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
                toml::Value::try_from(&current.runtime_profiles).map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
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
            let runtime_result = state
                .camera_runtime
                .as_ref()
                .map(|runtime| runtime.set_runtime_profiles(current.runtime_profiles.clone()));
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
            if request.namespace != NAMESPACE {
                return Err(registry_error(
                    Error::Invalid(Invalid::Namespace),
                    &request.namespace,
                    &request.watch_id,
                ));
            }
            super::super::validate_client_id(&request.watch_id, "watch ID")?;
            let key_prefix = if request.key_prefix.is_empty() {
                if admin {
                    ACTIVE_KEY
                } else {
                    return Err(registry_error(Error::NotAuthorized, NAMESPACE, ACTIVE_KEY));
                }
            } else {
                request.key_prefix.as_str()
            };
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
            let _guard = super::super::state_store_settings::lock_config(&state.config_update)
                .map_err(|error| registry_error(error, NAMESPACE, &request.watch_id))?;
            let registry = config::load_config(config_path)
                .map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                })?
                .runtime_profiles;
            let entry = if key_prefix == REGISTRY_KEY {
                registry_entry(registry.clone())
                    .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?
            } else if key_prefix == STATUS_KEY {
                status_entry(visible_status(state, principal)).map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, STATUS_KEY)
                })?
            } else {
                active_entry(registry.clone())
            };
            let message = proto::StateWatchSnapshot {
                watch_id: request.watch_id.clone(),
                namespace: NAMESPACE.to_owned(),
                key_prefix: request.key_prefix.clone(),
                snapshot_revision: registry.revision,
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
            proto::StateStoreResult {
                result: Some(state_store_result::Result::Watch(message)),
            }
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
    let mut registry_value = super::super::peek_layouts::struct_to_json(value);
    normalize_revision_fields(&mut registry_value, false)
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY))?;
    let mut candidate: crate::profiles::ProfileRegistry = serde_json::from_value(registry_value)
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY))?;

    let Some(config_path) = state.camera_config_path.as_deref() else {
        return Err(registry_error(
            Error::Storage("runtime profile settings are unavailable".to_owned()),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    };
    let _guard = super::super::state_store_settings::lock_config(&state.config_update)
        .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;
    let mut root = config::load_configuration_table(config_path).map_err(|error| {
        registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
    })?;
    let current: Config = config::load_config(config_path).map_err(|error| {
        registry_error(Error::Storage(error.to_string()), NAMESPACE, REGISTRY_KEY)
    })?;
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
    if candidate.active_profile_id != previous_registry.active_profile_id {
        return Err(registry_error(
            Error::Invalid(Invalid::Schema),
            NAMESPACE,
            REGISTRY_KEY,
        ));
    }
    let camera_ids = state
        .camera_entries()
        .into_iter()
        .map(|camera| camera.info.id)
        .collect::<BTreeSet<_>>();
    candidate
        .validate_targets(&camera_ids)
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
    candidate
        .validate()
        .map_err(|_| registry_error(Error::Invalid(Invalid::Schema), NAMESPACE, REGISTRY_KEY))?;
    let registry_entry = registry_entry(candidate.clone())
        .map_err(|error| registry_error(error, NAMESPACE, REGISTRY_KEY))?;

    let previous_privacy = previous_registry.active_privacy_overrides();
    let next_privacy = candidate.active_privacy_overrides();
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
        toml::Value::try_from(&candidate).map_err(|error| {
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
    *state
        .runtime_profiles
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = candidate.clone();
    mark_runtime_status_pending(state, &candidate);
    publish_status_event(state, now_ms());
    let runtime_result = state
        .camera_runtime
        .as_ref()
        .map(|runtime| runtime.set_runtime_profiles(candidate.clone()));
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
    let mut cameras = applications
        .unwrap_or_default()
        .into_iter()
        .map(|application| (application.camera_id.clone(), application))
        .collect::<BTreeMap<_, _>>();
    if runtime_error.is_some() || runtime_unavailable {
        let configs = state
            .camera_config_path
            .as_deref()
            .and_then(|path| config::load_cameras(path).ok())
            .map(|configs| configs.into_values().flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        for entry in state.camera_entries() {
            let camera_id = entry.info.id.clone();
            if cameras.contains_key(&camera_id) {
                continue;
            }
            let base_config = configs
                .iter()
                .find(|camera| camera.ip.to_string() == camera_id);
            let base = crate::profiles::BaseCameraPolicy {
                available: true,
                recording_mode: base_config
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
                .and_then(|profile| profile.cameras.get(&camera_id));
            cameras.insert(
                camera_id.clone(),
                crate::profiles::CameraRuntimeApplication {
                    camera_id,
                    state: crate::profiles::RuntimeApplicationState::Degraded,
                    policy: crate::profiles::resolve_camera_policy(&base, profile_override),
                    reason: Some(
                        runtime_error
                            .clone()
                            .unwrap_or_else(|| "camera runtime is unavailable".to_owned())
                            .chars()
                            .take(256)
                            .collect(),
                    ),
                },
            );
        }
    }
    for application in cameras.values_mut() {
        let privacy_active = state
            .privacy
            .decision(&application.camera_id, chrono::Utc::now());
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
                application.state = crate::profiles::RuntimeApplicationState::Degraded;
                application.reason = Some(error.to_string().chars().take(256).collect());
            }
        }
    }
    *state
        .runtime_profile_status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        crate::profiles::RuntimeProfileStatus {
            revision: profiles.revision,
            active_profile_id: profiles.active_profile_id.clone(),
            cameras,
        };
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

fn publish_status_event(state: &ServerState, updated_ms: u64) {
    let status = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Ok(entry) = status_entry(status) {
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
    let mut status = state
        .runtime_profile_status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if principal.role == AccessRole::Administrator {
        return status;
    }
    let visible_camera_ids = super::super::camera_access::for_principal(state, principal)
        .ok()
        .and_then(|policy| super::super::camera_access::query_cameras(state, &policy, &[]).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|camera| camera.info.id)
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
            .map(|last| format!("10.20.0.{last}"))
            .collect::<Vec<_>>();
        let mut profiles = BTreeMap::from([(
            "away".to_owned(),
            RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: BTreeMap::new(),
            },
        )]);
        for profile_index in 0..31 {
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
    fn maximum_registry_put_rejects_before_persisting_an_untransportable_value() {
        let fixture = Fixture::new();
        let (registry, camera_ids) = full_registry();
        {
            let mut cameras = fixture
                .state
                .cameras
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cameras.extend(camera_ids.iter().map(|camera_id| {
                let ip = camera_id.parse().unwrap();
                super::super::super::camera_entry(&camera_config(ip), None)
            }));
        }
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
