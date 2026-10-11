use super::*;
use crate::{access::ClientClassificationReason, config, config::Config};
use prost_types::{Value, value::Kind};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const NAMESPACE: &str = "service/runtime-profiles/";
pub(super) const ACTIVE_SCHEMA: &str = "keeppeek.runtime-profiles.active.v1";
const ACTIVE_KEY: &str = "active";

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
            if request.namespace != NAMESPACE || request.key != ACTIVE_KEY {
                return Err(registry_error(
                    Error::Invalid(Invalid::Key),
                    &request.namespace,
                    &request.key,
                ));
            }
            let entry = active_entry(
                config::load_config(config_path)
                    .map_err(|error| {
                        registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                    })?
                    .runtime_profiles,
            );
            entry_result(entry)
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
            state
                .privacy
                .replace_profile_overrides(next_privacy)
                .map_err(|error| {
                    registry_error(Error::Storage(error.to_string()), NAMESPACE, ACTIVE_KEY)
                })?;
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
            if key_prefix != ACTIVE_KEY {
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
            let entry = active_entry(registry.clone());
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
                    fields: [(
                        "profile_id".to_owned(),
                        Value {
                            kind: Some(Kind::StringValue(profile_id.to_owned())),
                        },
                    )]
                    .into_iter()
                    .collect(),
                }),
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
}
