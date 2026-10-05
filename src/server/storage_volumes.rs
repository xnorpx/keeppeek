//! Maps Administrator volume settings without disclosing resolved secrets.

use crate::{api::proto, config::Config, storage::volumes::*};
pub(super) mod management;
pub(super) mod settings;

macro_rules! enum_bridge {
    ($from:ident, $to:ident, $model:ident, $wire:ident, [$($variant:ident),+]) => {
        fn $from(value: i32) -> anyhow::Result<$model> {
            match proto::$wire::try_from(value) {
                $(Ok(proto::$wire::$variant) => Ok($model::$variant),)+
                _ => anyhow::bail!("invalid storage volume enum value"),
            }
        }
        const fn $to(value: $model) -> i32 {
            match value {
                $($model::$variant => proto::$wire::$variant as i32,)+
            }
        }
    };
}

enum_bridge!(
    role_from_wire,
    role_to_wire,
    VolumeRole,
    StorageVolumeRole,
    [Active, Archive, Export, Thumbnail, Metadata]
);
enum_bridge!(
    state_from_wire,
    state_to_wire,
    VolumeState,
    StorageVolumeState,
    [Enabled, ReadOnly, Draining, Disabled]
);
enum_bridge!(
    strategy_from_wire,
    strategy_to_wire,
    PlacementStrategy,
    StoragePlacementStrategy,
    [Priority, FreeSpace]
);

pub(super) fn from_wire(
    value: proto::StorageVolumeConfiguration,
) -> anyhow::Result<VolumeConfiguration<String>> {
    anyhow::ensure!(
        value.volumes.len() <= VOLUMES_MAX && value.placement.len() <= RULES_MAX,
        "named-volume configuration exceeds its collection limits"
    );
    Ok(VolumeConfiguration {
        volumes: value
            .volumes
            .into_iter()
            .map(volume_from_wire)
            .collect::<anyhow::Result<_>>()?,
        placement: value
            .placement
            .into_iter()
            .map(rule_from_wire)
            .collect::<anyhow::Result<_>>()?,
    })
}

fn volume_from_wire(value: proto::StorageVolume) -> anyhow::Result<Volume<String>> {
    anyhow::ensure!(
        value.roles.len() <= 5
            && value.sources.len() <= RULES_MAX
            && value.groups.len() <= RULES_MAX,
        "storage volume exceeds its collection limits"
    );
    Ok(Volume {
        id: value.id,
        root: value.root.into(),
        roles: value
            .roles
            .into_iter()
            .map(role_from_wire)
            .collect::<anyhow::Result<_>>()?,
        state: state_from_wire(value.state)?,
        priority: u16::try_from(value.priority)
            .map_err(|_| anyhow::anyhow!("volume priority exceeds 65535"))?,
        capacity_bytes: value.capacity_bytes,
        minimum_free_bytes: value.minimum_free_bytes,
        warning_free_bytes: value.warning_free_bytes,
        critical_free_bytes: value.critical_free_bytes,
        sources: value.sources,
        groups: value.groups,
    })
}

fn rule_from_wire(value: proto::StoragePlacementRule) -> anyhow::Result<PlacementRule<String>> {
    anyhow::ensure!(
        value.candidates.len() <= CANDIDATES_MAX,
        "placement has too many candidates"
    );
    Ok(PlacementRule {
        role: role_from_wire(value.role)?,
        source: value.source,
        group: value.group,
        candidates: value.candidates,
        strategy: strategy_from_wire(value.strategy)?,
        allow_fallback: value.allow_fallback,
    })
}

pub(super) fn to_wire(value: VolumeConfiguration<String>) -> proto::StorageVolumeConfiguration {
    proto::StorageVolumeConfiguration {
        volumes: value
            .volumes
            .into_iter()
            .map(|volume| proto::StorageVolume {
                id: volume.id,
                root: volume.root.to_string_lossy().into_owned(),
                roles: volume.roles.into_iter().map(role_to_wire).collect(),
                state: state_to_wire(volume.state),
                priority: u32::from(volume.priority),
                capacity_bytes: volume.capacity_bytes,
                minimum_free_bytes: volume.minimum_free_bytes,
                warning_free_bytes: volume.warning_free_bytes,
                critical_free_bytes: volume.critical_free_bytes,
                sources: volume.sources,
                groups: volume.groups,
            })
            .collect(),
        placement: value
            .placement
            .into_iter()
            .map(|rule| proto::StoragePlacementRule {
                role: role_to_wire(rule.role),
                source: rule.source,
                group: rule.group,
                candidates: rule.candidates,
                strategy: strategy_to_wire(rule.strategy),
                allow_fallback: rule.allow_fallback,
            })
            .collect(),
    }
}

pub(super) fn sanitized(config: &Config) -> Option<VolumeConfiguration<String>> {
    let resolved = config.storage.named_volumes.as_ref()?;
    // ponytail: serialization is bounded by 32 volumes and 256 rules; map fields if this grows.
    let mut value =
        toml::Value::try_from(resolved).expect("validated volume configuration serializes");
    if let Some(raw) = config
        .source
        .get("storage")
        .and_then(|value| value.get("named_volumes"))
    {
        restore_string_references(&mut value, raw);
    }
    Some(
        value
            .try_into()
            .expect("sanitized volume strings retain their schema"),
    )
}

fn restore_string_references(value: &mut toml::Value, raw: &toml::Value) {
    // Traverse only the fixed schema serialized above, never arbitrary source structure.
    match (value, raw) {
        (toml::Value::Table(value), toml::Value::Table(raw)) => {
            for (key, value) in value {
                // Enum strings are represented by typed values on the wire.
                if !matches!(key.as_str(), "roles" | "state" | "role" | "strategy")
                    && let Some(raw) = raw.get(key)
                {
                    restore_string_references(value, raw);
                }
            }
        }
        (toml::Value::Array(value), toml::Value::Array(raw)) => {
            for (value, raw) in value.iter_mut().zip(raw) {
                restore_string_references(value, raw);
            }
        }
        (value @ toml::Value::String(_), toml::Value::String(raw))
            if crate::config::contains_secret_reference(raw) =>
        {
            *value = toml::Value::String(raw.clone());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{ServerControlHandler, ServerState};
    use crate::storage::{RecordingDemand, StorageConfig};
    use crate::webrtc::{ControlRequestHandler, WebRtc};
    use prost::Message;
    use std::{collections::HashMap, path::PathBuf, time::Duration};

    fn fixture() -> (
        PathBuf,
        ServerControlHandler,
        crate::storage::RecordingCatalog,
    ) {
        let directory = std::env::temp_dir().join(format!("volume-wire-{}", uuid::Uuid::new_v4()));
        let root = directory.join("recordings").to_string_lossy().into_owned();
        let config = Config {
            storage: crate::config::StorageToml {
                medium_term_path: Some(root.clone()),
                long_term_path: Some(root),
                ..Default::default()
            },
            ..Default::default()
        };
        let path = directory.join("config.toml");
        crate::config::write_private_file(&path, toml::to_string(&config).unwrap().as_bytes())
            .unwrap();
        let storage = StorageConfig::from_toml(&config.storage);
        let catalog =
            crate::storage::RecordingCatalog::open(&directory.join("catalog.db")).unwrap();
        let state = ServerState::new(
            &config,
            &HashMap::new(),
            &HashMap::new(),
            &storage,
            RecordingDemand::new(Duration::ZERO),
            WebRtc::new(),
        )
        .with_camera_config_path(path)
        .with_recording_catalog(catalog.handle());
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        (
            directory,
            ServerControlHandler::new(state, router_tx),
            catalog,
        )
    }

    fn dispatch(
        handler: &ServerControlHandler,
        action: proto::runtime_configuration_command::Action,
    ) -> Result<proto::SanitizedRuntimeConfiguration, proto::Error> {
        let response = handler
            .handle(proto::Request {
                request_id: 129,
                command: Some(proto::request::Command::RuntimeConfigurationCommand(
                    proto::RuntimeConfigurationCommand {
                        action: Some(action),
                    },
                )),
            })
            .response;
        match response.result.unwrap() {
            proto::response::Result::Ok(ok) => match ok.result.unwrap() {
                proto::ok::Result::RuntimeConfigurationResult(result) => Ok(result.config.unwrap()),
                _ => panic!("runtime settings response required"),
            },
            proto::response::Result::Error(error) => Err(error),
        }
    }

    fn update(
        configuration: proto::SanitizedRuntimeConfiguration,
    ) -> proto::runtime_configuration_command::Action {
        proto::runtime_configuration_command::Action::Update(proto::UpdateRuntimeConfiguration {
            host: configuration.host,
            port: configuration.port,
            storage: configuration.storage,
            expected_configuration_revision: configuration.configuration_revision,
            move_existing_recordings: false,
        })
    }

    #[test]
    fn named_volume_wire_update_preserves_references_and_legacy_omission() {
        use proto::runtime_configuration_command::Action;
        let (directory, handler, catalog) = fixture();
        let root = directory.join("private-volume-root");
        let mut secrets = toml::Table::new();
        secrets.insert("VOLUME_ROOT".into(), root.to_string_lossy().as_ref().into());
        secrets.insert("VOLUME_ID".into(), "private-volume-id".into());
        crate::config::write_private_file(
            &directory.join("secrets.toml"),
            toml::to_string(&secrets).unwrap().as_bytes(),
        )
        .unwrap();
        let mut current =
            dispatch(&handler, Action::Get(proto::GetRuntimeConfiguration {})).unwrap();
        current.storage.as_mut().unwrap().named_volumes = Some(proto::StorageVolumeConfiguration {
            volumes: vec![proto::StorageVolume {
                id: "{secret:VOLUME_ID}".into(),
                root: "{secret:VOLUME_ROOT}".into(),
                roles: vec![proto::StorageVolumeRole::Archive as i32],
                state: proto::StorageVolumeState::Disabled as i32,
                sources: vec!["{secret:VOLUME_ROOT}".into()],
                groups: vec!["{secret:VOLUME_ROOT}".into()],
                ..Default::default()
            }],
            placement: vec![proto::StoragePlacementRule {
                role: proto::StorageVolumeRole::Archive as i32,
                source: Some("{secret:VOLUME_ROOT}".into()),
                candidates: vec!["{secret:VOLUME_ID}".into()],
                strategy: proto::StoragePlacementStrategy::Priority as i32,
                ..Default::default()
            }],
        });
        let saved = dispatch(&handler, update(current)).unwrap();
        let volume = &saved
            .storage
            .as_ref()
            .unwrap()
            .named_volumes
            .as_ref()
            .unwrap()
            .volumes[0];
        assert_eq!(volume.root, "{secret:VOLUME_ROOT}");
        assert_eq!(volume.id, "{secret:VOLUME_ID}");
        let fetched = dispatch(&handler, Action::Get(proto::GetRuntimeConfiguration {})).unwrap();
        for response in [&saved, &fetched] {
            let bytes = response.encode_to_vec();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("private-volume-root"));
            assert!(!text.contains("private-volume-id"));
        }
        let text = std::fs::read_to_string(directory.join("config.toml")).unwrap();
        assert!(text.contains("{secret:VOLUME_ROOT}"));
        assert!(!text.contains("private-volume-root"));
        assert!(!root.exists(), "draft update must not probe its root");
        let mut legacy = saved.clone();
        legacy.port = 9099;
        legacy.storage.as_mut().unwrap().named_volumes = None;
        let preserved = dispatch(&handler, update(legacy)).unwrap();
        assert_eq!(
            preserved.storage.as_ref().unwrap().named_volumes,
            saved.storage.as_ref().unwrap().named_volumes
        );
        assert_eq!(
            dispatch(&handler, update(saved)).unwrap_err().code,
            proto::ErrorCode::Rejected as i32
        );
        let mut clear = preserved;
        clear.storage.as_mut().unwrap().named_volumes =
            Some(proto::StorageVolumeConfiguration::default());
        let cleared = dispatch(&handler, update(clear)).unwrap();
        assert!(
            cleared
                .storage
                .unwrap()
                .named_volumes
                .unwrap()
                .volumes
                .is_empty()
        );
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_wire_presence_preserves_legacy_omission_and_explicit_clear() {
        let omitted = proto::RuntimeStorageConfiguration::default();
        assert!(
            proto::RuntimeStorageConfiguration::decode(omitted.encode_to_vec().as_slice())
                .unwrap()
                .named_volumes
                .is_none()
        );
        let clear = proto::RuntimeStorageConfiguration {
            named_volumes: Some(proto::StorageVolumeConfiguration::default()),
            ..omitted
        };
        let bytes = clear.encode_to_vec();
        assert_eq!(bytes, [0x92, 0x01, 0x00]);
        assert!(
            proto::RuntimeStorageConfiguration::decode(bytes.as_slice())
                .unwrap()
                .named_volumes
                .is_some()
        );
    }

    #[test]
    fn named_volume_wire_rejects_invalid_updates_without_mutation() {
        use proto::runtime_configuration_command::Action;
        let (directory, handler, catalog) = fixture();
        let original = std::fs::read(directory.join("config.toml")).unwrap();
        let current = dispatch(&handler, Action::Get(proto::GetRuntimeConfiguration {})).unwrap();
        for state in [0, 99] {
            let mut candidate = current.clone();
            candidate.storage.as_mut().unwrap().named_volumes =
                Some(proto::StorageVolumeConfiguration {
                    volumes: vec![proto::StorageVolume {
                        id: "draft".into(),
                        root: directory.join("unused").to_string_lossy().into_owned(),
                        roles: vec![proto::StorageVolumeRole::Archive as i32],
                        state,
                        ..Default::default()
                    }],
                    ..Default::default()
                });
            assert_eq!(
                dispatch(&handler, update(candidate)).unwrap_err().code,
                proto::ErrorCode::InvalidRequest as i32
            );
            assert_eq!(
                std::fs::read(directory.join("config.toml")).unwrap(),
                original
            );
        }
        let mut unversioned = current;
        unversioned.configuration_revision.clear();
        unversioned.storage.as_mut().unwrap().named_volumes = Some(Default::default());
        assert_eq!(
            dispatch(&handler, update(unversioned)).unwrap_err().code,
            proto::ErrorCode::InvalidRequest as i32
        );
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            original
        );
        assert!(!directory.join("unused").exists());
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_wire_rejects_unknown_and_unspecified_enums() {
        for invalid in [0, -1, 99] {
            assert!(role_from_wire(invalid).is_err());
            assert!(state_from_wire(invalid).is_err());
            assert!(strategy_from_wire(invalid).is_err());
        }
        for role in [
            VolumeRole::Active,
            VolumeRole::Archive,
            VolumeRole::Export,
            VolumeRole::Thumbnail,
            VolumeRole::Metadata,
        ] {
            assert_eq!(role_from_wire(role_to_wire(role)).unwrap(), role);
        }
        for state in [
            VolumeState::Enabled,
            VolumeState::ReadOnly,
            VolumeState::Draining,
            VolumeState::Disabled,
        ] {
            assert_eq!(state_from_wire(state_to_wire(state)).unwrap(), state);
        }
    }
    #[test]
    fn settings_removal_rejects_unbound_enabled_runtime_until_restart() {
        use crate::storage::{
            catalog::locations::{Reply, Request},
            volumes::runtime::Manager,
        };
        let (directory, mut handler, catalog, saved) = bound_secret_volume_fixture(false);
        let configuration = crate::config::load_config(&directory.join("config.toml"))
            .unwrap()
            .storage
            .named_volumes
            .unwrap();
        let mut running = configuration.clone();
        running.volumes[0].state = VolumeState::Enabled;
        handler.state.storage_config.volume_runtime = Some(std::sync::Arc::new(
            Manager::new(running, catalog.handle()).unwrap(),
        ));
        assert_eq!(
            catalog.handle().volume_location(Request::Usage).unwrap(),
            Reply::Usage(vec![])
        );
        let before = std::fs::read(directory.join("config.toml")).unwrap();
        let error = dispatch(&handler, explicit_volume_clear(saved.clone())).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            before
        );
        // A restarted disabled runtime cannot later recover this root for admission.
        handler.state.storage_config.volume_runtime = Some(std::sync::Arc::new(
            Manager::new(configuration, catalog.handle()).unwrap(),
        ));
        dispatch(&handler, explicit_volume_clear(saved)).unwrap();
        assert!(!directory.join("unopened-volume").exists());
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn bound_secret_volume_fixture(
        bind: bool,
    ) -> (
        PathBuf,
        ServerControlHandler,
        crate::storage::RecordingCatalog,
        proto::SanitizedRuntimeConfiguration,
    ) {
        use crate::storage::catalog::locations::{Binding, Request};
        use proto::runtime_configuration_command::Action;
        let (directory, handler, catalog) = fixture();
        crate::config::write_private_file(
            &directory.join("secrets.toml"),
            b"REMOVAL_VOLUME_ID='resolved-volume'\n",
        )
        .unwrap();
        let root = directory.join("unopened-volume");
        let mut current =
            dispatch(&handler, Action::Get(proto::GetRuntimeConfiguration {})).unwrap();
        current.storage.as_mut().unwrap().named_volumes = Some(proto::StorageVolumeConfiguration {
            volumes: vec![proto::StorageVolume {
                id: "{secret:REMOVAL_VOLUME_ID}".into(),
                root: root.to_string_lossy().into_owned(),
                roles: vec![proto::StorageVolumeRole::Export as i32],
                state: proto::StorageVolumeState::Disabled as i32,
                ..Default::default()
            }],
            placement: vec![],
        });
        let saved = dispatch(&handler, update(current)).unwrap();
        assert_eq!(
            saved
                .storage
                .as_ref()
                .unwrap()
                .named_volumes
                .as_ref()
                .unwrap()
                .volumes[0]
                .id,
            "{secret:REMOVAL_VOLUME_ID}"
        );
        if bind {
            catalog
                .handle()
                .volume_location(Request::Bind(Binding {
                    id: "resolved-volume".into(),
                    generation: 1,
                    root: root.clone(),
                    filesystem: "disk".into(),
                    root_identity: "bound-root".into(),
                    writable: true,
                    draining: false,
                    limit_bytes: Some(100),
                    minimum_free_bytes: 0,
                }))
                .unwrap();
        }
        assert!(!root.exists());
        (directory, handler, catalog, saved)
    }

    fn explicit_volume_clear(
        mut config: proto::SanitizedRuntimeConfiguration,
    ) -> proto::runtime_configuration_command::Action {
        config.storage.as_mut().unwrap().named_volumes = Some(Default::default());
        update(config)
    }

    fn drain_secret_volume(catalog: &crate::storage::RecordingCatalog) {
        use crate::storage::catalog::locations::{Reply, Request};
        assert_eq!(
            catalog
                .handle()
                .volume_location(Request::SetDraining {
                    volume: "resolved-volume".into(),
                    generation: 1,
                    draining: true,
                })
                .unwrap(),
            Reply::Bound
        );
    }

    #[test]
    fn settings_removal_resolves_secret_ids_requires_drain_and_retains_binding() {
        use crate::storage::catalog::locations::{Reply, Request};
        let (directory, handler, catalog, saved) = bound_secret_volume_fixture(true);
        let mut omitted = saved.clone();
        omitted.port = 9099;
        omitted.storage.as_mut().unwrap().named_volumes = None;
        let preserved = dispatch(&handler, update(omitted)).unwrap();
        assert_eq!(
            preserved.storage.as_ref().unwrap().named_volumes,
            saved.storage.as_ref().unwrap().named_volumes
        );
        let before = std::fs::read(directory.join("config.toml")).unwrap();
        let revision = catalog.handle().volume_ledger_revision().unwrap();
        let error = dispatch(&handler, explicit_volume_clear(preserved.clone())).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            before
        );
        assert_eq!(catalog.handle().volume_ledger_revision().unwrap(), revision);
        drain_secret_volume(&catalog);
        let usage = catalog.handle().volume_location(Request::Usage).unwrap();
        let cleared = dispatch(&handler, explicit_volume_clear(preserved)).unwrap();
        assert!(
            cleared
                .storage
                .unwrap()
                .named_volumes
                .unwrap()
                .volumes
                .is_empty()
        );
        assert_eq!(
            catalog.handle().volume_location(Request::Usage).unwrap(),
            usage
        );
        let Reply::Usage(usage) = usage else {
            panic!("volume usage missing")
        };
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].volume, "resolved-volume");
        assert!(usage[0].operator_draining);
        assert!(!directory.join("unopened-volume").exists());
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_removal_rejects_drained_volume_with_reserved_export() {
        use crate::storage::catalog::locations::{Allocation, Capacity, Kind, Object, Request};
        let (directory, handler, catalog, saved) = bound_secret_volume_fixture(true);
        let handle = catalog.handle();
        handle
            .volume_location(Request::Reserve(Allocation {
                operation: "pending-export".into(),
                object: Object {
                    kind: Kind::Export,
                    id: "pending-export".into(),
                },
                volume: "resolved-volume".into(),
                generation: 1,
                relative_key: "pending-export.mp4".into(),
                bytes: 10,
                capacity: Capacity {
                    ledger_revision: handle.volume_ledger_revision().unwrap(),
                    observed_at: std::time::Instant::now(),
                    available_bytes: 1000,
                    filesystem: "disk".into(),
                    root_identity: "bound-root".into(),
                },
            }))
            .unwrap();
        drain_secret_volume(&catalog);
        let before = std::fs::read(directory.join("config.toml")).unwrap();
        let usage = handle.volume_location(Request::Usage).unwrap();
        let revision = handle.volume_ledger_revision().unwrap();
        let error = dispatch(&handler, explicit_volume_clear(saved)).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            before
        );
        assert_eq!(handle.volume_location(Request::Usage).unwrap(), usage);
        assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
        assert!(!directory.join("unopened-volume").exists());
        drop(handle);
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn settings_removal_rejects_disabled_unbound_candidate_in_running_archive_rule() {
        use crate::storage::{
            catalog::locations::{Reply, Request},
            volumes::{
                PlacementRule, PlacementStrategy, VolumeRole, VolumeState, runtime::Manager,
            },
        };
        let (directory, mut handler, catalog, saved) = bound_secret_volume_fixture(false);
        let configuration = crate::config::load_config(&directory.join("config.toml"))
            .unwrap()
            .storage
            .named_volumes
            .unwrap();
        let mut running = configuration.clone();
        let candidate = running.volumes[0].id.clone();
        assert_eq!(running.volumes[0].state, VolumeState::Disabled);
        running.volumes[0].roles = vec![VolumeRole::Archive];
        running.placement.push(PlacementRule {
            role: VolumeRole::Archive,
            source: None,
            group: None,
            candidates: vec![candidate],
            strategy: PlacementStrategy::Priority,
            allow_fallback: false,
        });
        handler.state.storage_config.volume_runtime = Some(std::sync::Arc::new(
            Manager::new(running, catalog.handle()).unwrap(),
        ));
        assert_eq!(
            catalog.handle().volume_location(Request::Usage).unwrap(),
            Reply::Usage(vec![])
        );
        let before = std::fs::read(directory.join("config.toml")).unwrap();
        let revision = catalog.handle().volume_ledger_revision().unwrap();
        let error = dispatch(&handler, explicit_volume_clear(saved.clone())).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            before
        );
        assert_eq!(catalog.handle().volume_ledger_revision().unwrap(), revision);
        assert_eq!(
            catalog.handle().volume_location(Request::Usage).unwrap(),
            Reply::Usage(vec![])
        );
        handler.state.storage_config.volume_runtime = Some(std::sync::Arc::new(
            Manager::new(configuration, catalog.handle()).unwrap(),
        ));
        let cleared = dispatch(&handler, explicit_volume_clear(saved)).unwrap();
        assert!(
            cleared
                .storage
                .unwrap()
                .named_volumes
                .unwrap()
                .volumes
                .is_empty()
        );
        assert!(!directory.join("unopened-volume").exists());
        drop(handler);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
