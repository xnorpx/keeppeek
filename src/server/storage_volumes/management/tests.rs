use super::*;
use crate::server::{create_export_job, export_storage_tests};
use std::{sync::Arc, time::Duration};

#[test]
fn volume_operations_require_administrator_before_reading_storage() {
    use proto::storage_volume_command::Action;
    let state = ServerState::empty();
    let mut principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    principal.role = crate::access::AccessRole::User;
    let actions = [
        Action::List(Default::default()),
        Action::Probe(Default::default()),
        Action::Placement(Default::default()),
        Action::Objects(Default::default()),
        Action::PreviewMove(Default::default()),
        Action::ConfirmMove(Default::default()),
        Action::Moves(Default::default()),
        Action::GetMove(Default::default()),
        Action::CancelMove(Default::default()),
        Action::SetDraining(Default::default()),
    ];
    for action in actions {
        let command = proto::StorageVolumeCommand {
            action: Some(action),
        };
        let request = proto::Request {
            request_id: 129,
            command: Some(proto::request::Command::StorageVolumeCommand(
                command.clone(),
            )),
        };
        let bytes = request.encode_to_vec();
        assert_eq!(proto::Request::decode(bytes.as_slice()).unwrap(), request);
        let rejected = dispatch(&state, &principal, command).unwrap_err();
        assert_eq!(rejected._http_status, 403);
    }
}

#[test]
fn operator_drain_requires_current_revision_and_reports_independent_state() {
    let (root, catalog, manager, mut state, _) = export_storage_tests::setup(16 * 1024 * 1024);
    state.storage_config.named_volumes = Some(manager.configuration().clone());
    state.config.storage.named_volumes = Some(
        toml::Value::try_from(manager.configuration())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let command = |revision: String, draining| proto::StorageVolumeCommand {
        action: Some(proto::storage_volume_command::Action::SetDraining(
            proto::SetStorageVolumeDraining {
                volume_id: "primary".into(),
                draining,
                expected_configuration_revision: revision,
            },
        )),
    };
    let before = catalog.handle().volume_ledger_revision().unwrap();
    assert_eq!(
        dispatch(&state, &principal, command("stale".into(), true))
            .unwrap_err()
            ._http_status,
        409
    );
    assert_eq!(catalog.handle().volume_ledger_revision().unwrap(), before);
    for draining in [true, true, false] {
        let proto::ok::Result::StorageVolumeResult(result) = dispatch(
            &state,
            &principal,
            command(camera_configuration_revision(&state).unwrap(), draining),
        )
        .unwrap() else {
            panic!("missing volume result")
        };
        let Some(proto::storage_volume_result::Result::Volumes(result)) = result.result else {
            panic!("missing volume status")
        };
        let volume = result
            .volumes
            .iter()
            .find(|volume| volume.volume_id == "primary")
            .unwrap();
        assert_eq!(volume.operator_draining, draining);
        assert!(!volume.configured_draining);
        assert!(volume.online);
    }
    drop(state);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn preview_request(state: &ServerState) -> proto::PreviewStorageMove {
    let jobs = state.export_jobs.lock().unwrap();
    proto::PreviewStorageMove {
        object: Some(proto::StorageObject {
            kind: proto::StorageObjectKind::Export as i32,
            id: jobs["named-export"].artifact_id.clone(),
        }),
        destination_volume_id: "secondary".into(),
        role: proto::StorageVolumeRole::Export as i32,
    }
}

#[test]
fn volume_status_and_probe_preserve_secret_identifiers() {
    use proto::storage_volume_command::Action;
    let root = std::env::temp_dir().join(format!("volume-api-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    crate::config::write_private_file(&path, b"[[storage.named_volumes.volumes]]\nid = '{secret:ID}'\nroot = '{secret:ROOT}'\nroles = ['export']\nstate = 'disabled'\n").unwrap();
    let mut secrets = toml::Table::new();
    secrets.insert("ID".into(), "private-volume-identity".into());
    secrets.insert(
        "ROOT".into(),
        root.join("private-missing-root")
            .to_string_lossy()
            .as_ref()
            .into(),
    );
    crate::config::write_private_file(
        &root.join("secrets.toml"),
        toml::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    let mut state = ServerState::empty();
    state.camera_config_path = Some(path);
    let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    for action in [
        Action::List(Default::default()),
        Action::Probe(proto::ProbeStorageVolume {
            volume_id: "{secret:ID}".into(),
        }),
    ] {
        let proto::ok::Result::StorageVolumeResult(result) = dispatch(
            &state,
            &principal,
            proto::StorageVolumeCommand {
                action: Some(action),
            },
        )
        .unwrap() else {
            panic!("volume response missing")
        };
        let bytes = result.encode_to_vec();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("{secret:ID}"));
        assert!(!text.contains("private-volume-identity"));
        assert!(!text.contains("private-missing-root"));
    }
    assert!(!root.join("private-missing-root").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn confirmation_is_actor_bound_and_uses_the_existing_durable_worker() {
    let (root, catalog, initial, mut state, request) =
        export_storage_tests::setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        export_storage_tests::completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let manager = export_storage_tests::move_manager(&root, catalog.handle());
    let worker = crate::storage::volumes::runtime::worker::Worker::start(manager.clone()).unwrap();
    state.storage_config.volume_runtime = Some(Arc::new(manager));
    state.storage_config.volume_mover = Some(worker.handle());
    let preview = moves::preview(&state, "administrator", preview_request(&state)).unwrap();
    let confirm = proto::ConfirmStorageMove {
        preview_token: preview.preview_token,
        expected_configuration_revision: preview.configuration_revision,
    };
    assert_eq!(
        moves::confirm(&state, "other-administrator", confirm.clone())
            .unwrap_err()
            ._http_status,
        409
    );
    let mut stale = confirm.clone();
    stale.expected_configuration_revision = "stale".into();
    assert_eq!(
        moves::confirm(&state, "administrator", stale)
            .unwrap_err()
            ._http_status,
        409
    );
    let admitted = moves::confirm(&state, "administrator", confirm.clone()).unwrap();
    assert_eq!(admitted.job_id, preview.job_id);
    wait_for_move_completion(&state, &preview.job_id);
    assert_eq!(
        moves::confirm(&state, "administrator", confirm)
            .unwrap()
            .phase,
        "complete"
    );
    let objects = objects(
        &state,
        proto::ListStorageObjects {
            volume_id: "secondary".into(),
            after: None,
        },
    )
    .unwrap();
    assert_eq!(objects.objects.len(), 1);
    assert_eq!(objects.objects[0].object, preview.source.unwrap().object);
    worker.shutdown().unwrap();
    drop(state);
    drop(initial);
    catalog.shutdown();
}

fn wait_for_move_completion(state: &ServerState, job_id: &str) {
    for _ in 0..100 {
        if moves::get(state, job_id).unwrap().phase == "complete" {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(moves::get(state, job_id).unwrap().phase, "complete");
}

#[test]
fn preview_rejects_configuration_changes_and_in_progress_edits() {
    let (root, catalog, initial, mut state, request) =
        export_storage_tests::setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        export_storage_tests::completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let manager = export_storage_tests::move_manager(&root, catalog.handle());
    state.storage_config.volume_runtime = Some(Arc::new(manager.clone()));
    let guard = state.config_update.lock().unwrap();
    assert_eq!(
        moves::preview(&state, "administrator", preview_request(&state))
            .unwrap_err()
            ._http_status,
        409
    );
    drop(guard);
    let path = root.join("config.toml");
    let mut config = crate::config::Config::default();
    let mut volumes = manager.configuration().clone();
    for volume in &mut volumes.volumes {
        volume.state = crate::storage::volumes::VolumeState::Disabled;
    }
    config.storage.named_volumes = Some(volumes);
    crate::config::write_private_file(&path, toml::to_string(&config).unwrap().as_bytes()).unwrap();
    state.camera_config_path = Some(path);
    let rejected = moves::preview(&state, "administrator", preview_request(&state)).unwrap_err();
    assert_eq!(rejected._http_status, 409);
    assert!(rejected.message.contains("pending changes"));
    drop(manager);
    drop(state);
    drop(initial);
    catalog.shutdown();
}

fn metadata_api_config(path: &std::path::Path, config: &crate::config::Config) {
    let volume = &config.storage.named_volumes.as_ref().unwrap().volumes[0];
    let mut secrets = toml::Table::new();
    secrets.insert("METADATA_API_ID".into(), volume.id.as_str().into());
    secrets.insert(
        "METADATA_API_ROOT".into(),
        volume.root.to_str().unwrap().into(),
    );
    crate::config::write_private_file(
        &path.with_file_name("secrets.toml"),
        toml::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    let mut raw = toml::Value::try_from(config).unwrap();
    raw["storage"]["named_volumes"]["volumes"][0]["id"] = "{secret:METADATA_API_ID}".into();
    raw["storage"]["named_volumes"]["volumes"][0]["root"] = "{secret:METADATA_API_ROOT}".into();
    crate::config::write_private_file(path, toml::to_string(&raw).unwrap().as_bytes()).unwrap();
}

fn metadata_api_root(path: &std::path::Path) {
    std::fs::create_dir(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(windows)]
    assert!(
        std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/.github/scripts/protect-test-directory.ps1"
            ))
            .arg("-Directory")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

fn metadata_api_fixture() -> (
    std::path::PathBuf,
    crate::storage::RecordingCatalog,
    ServerState,
) {
    let (root, catalog, manager, mut state, _) = export_storage_tests::setup(16 * 1024 * 1024);
    let mut volumes = manager.configuration().clone();
    volumes.volumes.truncate(1);
    volumes.placement.clear();
    volumes.volumes[0].root = root.join("metadata-target");
    metadata_api_root(&volumes.volumes[0].root);
    volumes.volumes[0].id =
        crate::storage::volumes::VolumeId::parse("private-metadata-owner").unwrap();
    volumes.volumes[0].roles = vec![crate::storage::volumes::VolumeRole::Metadata];
    volumes.volumes[0].state = crate::storage::volumes::VolumeState::Disabled;
    volumes.volumes[0].capacity_bytes = None;
    let mut config = crate::config::Config::default();
    config.storage.medium_term_path = Some(root.join("recordings").to_str().unwrap().into());
    config.storage.long_term_path = Some(root.join("legacy").to_str().unwrap().into());
    config.storage.event_thumbnail_path = Some(root.join("thumbnails").to_str().unwrap().into());
    config.storage.recording_catalog_path = Some(root.join("catalog.db").to_str().unwrap().into());
    config.storage.named_volumes = Some(volumes);
    let path = root.join("config.toml");
    metadata_api_config(&path, &config);
    let config = crate::config::load_config(&path).unwrap();
    state.storage_config = crate::storage::StorageConfig::from_toml(&config.storage);
    state.storage_config.named_volumes = None;
    state.config = crate::server::sanitized_config(&config, &state.storage_config, 0, &[]);
    state.camera_config_path = Some(path);
    let history = root.join("legacy/.exports/history.json");
    std::fs::create_dir_all(history.parent().unwrap()).unwrap();
    std::fs::write(&history, b"{\"version\":1,\"jobs\":[]}\n").unwrap();
    state.export_history_path = Some(Arc::new(history));
    let paths =
        crate::storage::catalog::locations::legacy::LegacyPaths::effective(&state.storage_config)
            .unwrap();
    catalog
        .handle()
        .volume_location(Request::RegisterLegacyPaths(Box::new(paths)))
        .unwrap();
    drop(manager);
    (root, catalog, state)
}

fn metadata_api_admin() -> ApiPrincipal {
    let mut principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    principal.identity = crate::server::ApiPrincipalIdentity::Credential {
        id: uuid::Uuid::new_v4(),
        revision: 1,
    };
    principal
}

fn metadata_api_call(
    state: &ServerState,
    principal: &ApiPrincipal,
    action: proto::storage_volume_command::Action,
) -> Result<proto::storage_volume_result::Result> {
    let proto::ok::Result::StorageVolumeResult(result) = dispatch(
        state,
        principal,
        proto::StorageVolumeCommand {
            action: Some(action),
        },
    )?
    else {
        panic!("missing storage volume response");
    };
    Ok(result.result.expect("missing metadata response"))
}

fn metadata_api_preview(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> proto::StorageMetadataPreview {
    let proto::storage_volume_result::Result::MetadataPreview(preview) = metadata_api_call(
        state,
        principal,
        proto::storage_volume_command::Action::PreviewMetadata(proto::PreviewStorageMetadata {
            destination_volume_id: "{secret:METADATA_API_ID}".into(),
        }),
    )
    .unwrap() else {
        panic!("missing metadata preview")
    };
    preview
}

fn metadata_api_confirm(
    preview: &proto::StorageMetadataPreview,
) -> proto::storage_volume_command::Action {
    proto::storage_volume_command::Action::ConfirmMetadata(proto::ConfirmStorageMetadata {
        preview_token: preview.preview_token.clone(),
        expected_configuration_revision: preview.configuration_revision.clone(),
    })
}

fn metadata_api_status(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> proto::StorageMetadataStatus {
    let proto::storage_volume_result::Result::Metadata(status) = metadata_api_call(
        state,
        principal,
        proto::storage_volume_command::Action::Metadata(proto::GetStorageMetadata {}),
    )
    .unwrap() else {
        panic!("missing metadata status")
    };
    status
}

#[test]
fn metadata_api_actions_require_administrator_before_accessing_storage() {
    use proto::storage_volume_command::Action;
    let state = ServerState::empty();
    let mut principal = metadata_api_admin();
    principal.role = crate::access::AccessRole::User;
    for action in [
        Action::PreviewMetadata(Default::default()),
        Action::ConfirmMetadata(Default::default()),
        Action::Metadata(Default::default()),
        Action::CancelMetadata(Default::default()),
    ] {
        assert_eq!(
            metadata_api_call(&state, &principal, action)
                .unwrap_err()
                ._http_status,
            403
        );
    }
}

#[test]
fn metadata_confirmation_is_principal_bound_and_requires_current_revision() {
    let (root, catalog, state) = metadata_api_fixture();
    let administrator = metadata_api_admin();
    let preview = metadata_api_preview(&state, &administrator);
    let before = std::fs::read(root.join("config.toml")).unwrap();
    let authority = catalog.handle().metadata_info().unwrap().authority;
    let other = metadata_api_admin();
    assert_ne!(administrator.id(), other.id());
    assert_eq!(
        metadata_api_call(&state, &other, metadata_api_confirm(&preview))
            .unwrap_err()
            ._http_status,
        409
    );
    assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), before);
    for revision in [String::new(), "stale".to_owned()] {
        let mut stale = preview.clone();
        stale.configuration_revision = revision;
        assert_eq!(
            metadata_api_call(&state, &administrator, metadata_api_confirm(&stale))
                .unwrap_err()
                ._http_status,
            409
        );
        assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), before);
    }
    assert_eq!(
        catalog.handle().metadata_info().unwrap().authority,
        authority
    );
    assert!(
        metadata_api_status(&state, &administrator)
            .pending_volume_id
            .is_none()
    );
    drop(state);
    catalog.shutdown();
}

#[test]
fn metadata_confirmation_only_stages_restart_and_preserves_secret_identifiers() {
    let (root, catalog, state) = metadata_api_fixture();
    let administrator = metadata_api_admin();
    let before = std::fs::read(root.join("config.toml")).unwrap();
    let source_history = std::fs::read(root.join("legacy/.exports/history.json")).unwrap();
    let info = catalog.handle().metadata_info().unwrap();
    let preview = metadata_api_preview(&state, &administrator);
    assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), before);
    assert_eq!(preview.destination_volume_id, "{secret:METADATA_API_ID}");
    assert!(preview.required_bytes >= info.snapshot_bytes);
    assert!(
        preview.expires_in_seconds > 0 && preview.restart_required && preview.requires_downtime
    );
    metadata_api_call(&state, &administrator, metadata_api_confirm(&preview)).unwrap();
    let status = metadata_api_status(&state, &administrator);
    assert_ne!(
        status.configuration_revision,
        preview.configuration_revision
    );
    assert_eq!(status.current_volume_id, None);
    assert_eq!(
        status.pending_volume_id.as_deref(),
        Some("{secret:METADATA_API_ID}")
    );
    assert!(status.restart_required);
    assert_eq!(
        catalog.handle().metadata_info().unwrap().authority,
        info.authority
    );
    assert!(catalog.handle().stats().is_ok());
    assert!(root.join("catalog.db").is_file());
    assert_eq!(
        std::fs::read(root.join("legacy/.exports/history.json")).unwrap(),
        source_history
    );
    assert!(
        std::fs::read_dir(root.join("metadata-target"))
            .unwrap()
            .next()
            .is_none()
    );
    let text = std::fs::read_to_string(root.join("config.toml")).unwrap();
    let saved: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        saved["storage"]["metadata_pending"]["target"]["volume_id"].as_str(),
        Some("{secret:METADATA_API_ID}")
    );
    assert!(!text.contains("private-metadata-owner"));
    for bytes in [preview.encode_to_vec(), status.encode_to_vec()] {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("private-metadata-owner"));
        assert!(!text.contains(root.to_str().unwrap()));
    }
    drop(state);
    catalog.shutdown();
}

#[test]
fn durable_metadata_pending_survives_registry_loss_and_cancel_requires_current_revision() {
    use proto::storage_volume_command::Action;
    let (root, catalog, state) = metadata_api_fixture();
    let administrator = metadata_api_admin();
    let authority = catalog.handle().metadata_info().unwrap().authority;
    let preview = metadata_api_preview(&state, &administrator);
    metadata_api_call(&state, &administrator, metadata_api_confirm(&preview)).unwrap();
    let mut fresh = crate::server::tests::media_test_state();
    fresh.storage_config = state.storage_config.clone();
    fresh.config = state.config.clone();
    fresh.camera_config_path = state.camera_config_path.clone();
    fresh.export_history_path = state.export_history_path.clone();
    fresh.catalog = Some(catalog.handle());
    let status = metadata_api_status(&fresh, &administrator);
    assert_ne!(
        status.configuration_revision,
        preview.configuration_revision
    );
    let pending_revision = status.configuration_revision.clone();
    assert_eq!(
        status.pending_volume_id.as_deref(),
        Some("{secret:METADATA_API_ID}")
    );
    assert!(status.restart_required);
    let before = std::fs::read(root.join("config.toml")).unwrap();
    let stale = Action::CancelMetadata(proto::CancelStorageMetadata {
        expected_configuration_revision: preview.configuration_revision,
    });
    assert_eq!(
        metadata_api_call(&fresh, &administrator, stale)
            .unwrap_err()
            ._http_status,
        409
    );
    assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), before);
    metadata_api_call(
        &fresh,
        &administrator,
        Action::CancelMetadata(proto::CancelStorageMetadata {
            expected_configuration_revision: status.configuration_revision,
        }),
    )
    .unwrap();
    let cancelled = metadata_api_status(&fresh, &administrator);
    assert_ne!(cancelled.configuration_revision, pending_revision);
    assert!(cancelled.pending_volume_id.is_none() && !cancelled.restart_required);
    assert_eq!(cancelled.current_volume_id, status.current_volume_id);
    let saved: toml::Table =
        toml::from_str(&std::fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
    assert!(saved["storage"].get("metadata_pending").is_none());
    assert_eq!(
        catalog.handle().metadata_info().unwrap().authority,
        authority
    );
    assert!(
        std::fs::read_dir(root.join("metadata-target"))
            .unwrap()
            .next()
            .is_none()
    );
    drop(fresh);
    drop(state);
    catalog.shutdown();
}

#[test]
fn metadata_preview_refuses_unhealthy_history_and_existing_legacy_migration() {
    for unhealthy_history in [true, false] {
        let (root, catalog, mut state) = metadata_api_fixture();
        let path = root.join("config.toml");
        if unhealthy_history {
            state.export_history_error = Some(Arc::from("history recovery failed"));
        } else {
            let mut config: toml::Table =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            config.insert(
                "storage_migration".into(),
                toml::Value::Table(toml::Table::new()),
            );
            crate::config::write_private_file(&path, toml::to_string(&config).unwrap().as_bytes())
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let authority = catalog.handle().metadata_info().unwrap().authority;
        let action =
            proto::storage_volume_command::Action::PreviewMetadata(proto::PreviewStorageMetadata {
                destination_volume_id: "{secret:METADATA_API_ID}".into(),
            });
        assert!(metadata_api_call(&state, &metadata_api_admin(), action).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            catalog.handle().metadata_info().unwrap().authority,
            authority
        );
        assert!(
            std::fs::read_dir(root.join("metadata-target"))
                .unwrap()
                .next()
                .is_none()
        );
        drop(state);
        catalog.shutdown();
    }
}

#[test]
fn metadata_api_confirmation_activates_on_restart_with_latest_history_and_online_owner() {
    let (root, catalog, state) = metadata_api_fixture();
    let administrator = metadata_api_admin();
    let before = catalog.handle().metadata_info().unwrap().authority;
    let recording_files = catalog.handle().stats().unwrap().recording_files;
    let preview = metadata_api_preview(&state, &administrator);
    metadata_api_call(&state, &administrator, metadata_api_confirm(&preview)).unwrap();
    let source = state.storage_config.recording_catalog_path.clone();
    let source_history = state.export_history_path.as_ref().unwrap().as_ref().clone();
    let latest = b"{\n  \"jobs\": [],\n  \"version\": 1\n}\n";
    crate::config::write_private_file_atomically(&source_history, latest).unwrap();
    let path = root.join("config.toml");
    drop(state);
    catalog.shutdown();
    let mut raw: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let secrets = crate::config::load_secrets(&path).unwrap();
    crate::config::metadata::pending::apply(&path, &mut raw, &secrets).unwrap();
    assert!(raw["storage"].get("metadata_pending").is_none());
    let config = crate::config::load_config(&path).unwrap();
    let binding = config.storage.metadata.as_ref().unwrap();
    assert_eq!(binding.volume_id.as_str(), "private-metadata-owner");
    assert_eq!(binding.catalog_id, before.catalog_id);
    assert_eq!(binding.generation, before.generation + 1);
    let (destination, history) = binding.paths(&config.storage).unwrap();
    assert_ne!(destination, source);
    assert_eq!(std::fs::read(&source_history).unwrap(), latest);
    assert_eq!(std::fs::read(&history).unwrap(), latest);
    assert!(source.is_file());
    assert!(crate::storage::RecordingCatalog::open(&source).is_err());
    let reopened = crate::storage::RecordingCatalog::open_managed(
        &destination,
        &binding.authority(),
        &binding.root_identity(),
    )
    .unwrap();
    assert_eq!(
        reopened.handle().metadata_info().unwrap().authority,
        binding.authority()
    );
    assert_eq!(
        reopened.handle().stats().unwrap().recording_files,
        recording_files
    );
    let manager = Manager::new(
        config.storage.named_volumes.clone().unwrap(),
        reopened.handle(),
    )
    .unwrap();
    let observations = manager.observations().unwrap();
    let owner = observations
        .iter()
        .find(|volume| volume.id == binding.volume_id)
        .unwrap();
    assert_eq!(owner.health, crate::storage::volumes::VolumeHealth::Online);
    drop(manager);
    reopened.shutdown();
}

#[test]
fn cancelled_metadata_preview_token_cannot_restage_the_old_handoff() {
    let (root, catalog, state) = metadata_api_fixture();
    let administrator = metadata_api_admin();
    let authority = catalog.handle().metadata_info().unwrap().authority;
    let preview = metadata_api_preview(&state, &administrator);
    metadata_api_call(&state, &administrator, metadata_api_confirm(&preview)).unwrap();
    let pending = metadata_api_status(&state, &administrator);
    metadata_api_call(
        &state,
        &administrator,
        proto::storage_volume_command::Action::CancelMetadata(proto::CancelStorageMetadata {
            expected_configuration_revision: pending.configuration_revision,
        }),
    )
    .unwrap();
    let before = std::fs::read(root.join("config.toml")).unwrap();
    assert_eq!(
        metadata_api_call(&state, &administrator, metadata_api_confirm(&preview))
            .unwrap_err()
            ._http_status,
        409
    );
    assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), before);
    assert!(
        metadata_api_status(&state, &administrator)
            .pending_volume_id
            .is_none()
    );
    assert_eq!(
        catalog.handle().metadata_info().unwrap().authority,
        authority
    );
    assert!(
        std::fs::read_dir(root.join("metadata-target"))
            .unwrap()
            .next()
            .is_none()
    );
    let fresh = metadata_api_preview(&state, &administrator);
    assert_ne!(fresh.preview_token, preview.preview_token);
    drop(state);
    catalog.shutdown();
}

mod legacy_export_api {
    use super::*;
    use crate::storage::RecordingCatalog;
    use crate::storage::catalog::locations::{Kind, Object, legacy::LegacyPaths};
    use std::path::PathBuf;

    struct LegacyExportFixture {
        root: PathBuf,
        catalog: RecordingCatalog,
        state: ServerState,
        original: PathBuf,
        bytes: Vec<u8>,
    }

    fn legacy_export_fixture() -> LegacyExportFixture {
        let (root, catalog, initial, mut state, request) =
            export_storage_tests::setup(16 * 1024 * 1024);
        state.storage_config.volume_runtime = None;
        state.storage_config.medium_term_path = root.join("recordings");
        state.storage_config.recording_catalog_path = root.join("catalog.db");
        state.storage_config.event_thumbnail_path = root.join("legacy/.event-thumbnails");
        std::fs::create_dir_all(&state.storage_config.long_term_path).unwrap();
        // ponytail: protect the actual export root before the existing exporter creates its children.
        metadata_api_root(&state.storage_config.long_term_path.join(".exports"));
        create_export_job(&state, "owner", request).unwrap();
        assert_eq!(
            export_storage_tests::completed(&state).status,
            proto::ExportJobStatus::Ready as i32
        );
        let original = state.export_jobs.lock().unwrap()["named-export"]
            .path
            .clone()
            .unwrap();
        let bytes = std::fs::read(&original).unwrap();
        catalog
            .handle()
            .volume_location(Request::RegisterLegacyPaths(Box::new(
                LegacyPaths::effective(&state.storage_config).unwrap(),
            )))
            .unwrap();
        crate::storage::volumes::legacy::capture_roots(
            &catalog.handle(),
            &LegacyPaths::effective(&state.storage_config).unwrap(),
        )
        .unwrap();
        let manager = export_storage_tests::move_manager(&root, catalog.handle());
        state.storage_config.volume_runtime = Some(Arc::new(manager));
        drop(initial);
        LegacyExportFixture {
            root,
            catalog,
            state,
            original,
            bytes,
        }
    }

    fn legacy_export_object(state: &ServerState) -> Object {
        Object {
            kind: Kind::Export,
            id: state.export_jobs.lock().unwrap()["named-export"]
                .artifact_id
                .clone(),
        }
    }

    fn assert_export_unowned(fixture: &LegacyExportFixture) {
        assert!(matches!(
            fixture
                .catalog
                .handle()
                .volume_location(Request::Lookup(legacy_export_object(&fixture.state),))
                .unwrap(),
            Reply::Location(None)
        ));
        let Reply::Usage(usage) = fixture
            .catalog
            .handle()
            .volume_location(Request::Usage)
            .unwrap()
        else {
            panic!("missing usage")
        };
        assert!(
            usage
                .iter()
                .all(|volume| volume.allocated_bytes == 0 && volume.reserved_bytes == 0)
        );
        assert_eq!(std::fs::read(&fixture.original).unwrap(), fixture.bytes);
    }

    fn export_confirmation(preview: &proto::StorageMovePreview) -> proto::ConfirmStorageMove {
        proto::ConfirmStorageMove {
            preview_token: preview.preview_token.clone(),
            expected_configuration_revision: preview.configuration_revision.clone(),
        }
    }

    fn assert_export_download(state: &ServerState, expected: &[u8]) {
        let (_, messages) = crate::server::download_export(
            state,
            "owner",
            proto::DownloadExport {
                job_id: "named-export".into(),
                channel: proto::DataChannelKind::ReliableData as i32,
            },
        )
        .unwrap();
        let mut bytes = Vec::new();
        for message in messages {
            let Some(proto::message::Message::Export(export)) = message.message.message else {
                panic!("missing export message")
            };
            let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
                panic!("missing file chunk")
            };
            bytes.extend_from_slice(&chunk.payload);
        }
        assert_eq!(bytes, expected);
    }

    fn finish_legacy_export_fixture(fixture: LegacyExportFixture) {
        drop(fixture.state);
        fixture.catalog.shutdown();
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn legacy_export_confirmation_adopts_and_downloads_the_same_artifact() {
        let mut fixture = legacy_export_fixture();
        assert_export_unowned(&fixture);
        let manager = fixture
            .state
            .storage_config
            .volume_runtime
            .as_ref()
            .unwrap();
        let worker =
            crate::storage::volumes::runtime::worker::Worker::start((**manager).clone()).unwrap();
        fixture.state.storage_config.volume_mover = Some(worker.handle());
        let preview = moves::preview(
            &fixture.state,
            "administrator",
            preview_request(&fixture.state),
        )
        .unwrap();
        assert!(preview.adopts_legacy);
        assert_eq!(
            preview.source.as_ref().unwrap().bytes,
            fixture.bytes.len() as u64
        );
        assert_export_unowned(&fixture);
        let admitted = moves::confirm(
            &fixture.state,
            "administrator",
            export_confirmation(&preview),
        )
        .unwrap();
        assert_eq!(admitted.job_id, preview.job_id);
        wait_for_move_completion(&fixture.state, &preview.job_id);
        assert_export_download(&fixture.state, &fixture.bytes);
        assert!(!fixture.original.exists());
        worker.shutdown().unwrap();
        finish_legacy_export_fixture(fixture);
    }

    #[test]
    fn changed_ready_export_owner_rejects_confirmation_without_adoption() {
        for change_checksum in [true, false] {
            let mut fixture = legacy_export_fixture();
            let manager = fixture
                .state
                .storage_config
                .volume_runtime
                .as_ref()
                .unwrap();
            let worker =
                crate::storage::volumes::runtime::worker::Worker::start((**manager).clone())
                    .unwrap();
            fixture.state.storage_config.volume_mover = Some(worker.handle());
            let preview = moves::preview(
                &fixture.state,
                "administrator",
                preview_request(&fixture.state),
            )
            .unwrap();
            assert!(preview.adopts_legacy);
            {
                let mut jobs = fixture.state.export_jobs.lock().unwrap();
                let record = jobs.get_mut("named-export").unwrap();
                if change_checksum {
                    record.job.sha256 = Some("00".repeat(32));
                } else {
                    record.job.status = proto::ExportJobStatus::Failed as i32;
                }
            }
            assert!(
                moves::confirm(
                    &fixture.state,
                    "administrator",
                    export_confirmation(&preview)
                )
                .is_err()
            );
            assert_export_unowned(&fixture);
            worker.shutdown().unwrap();
            finish_legacy_export_fixture(fixture);
        }
    }

    #[test]
    fn legacy_export_preview_ignores_cached_path_and_keeps_decoy_untouched() {
        let mut fixture = legacy_export_fixture();
        let decoy = fixture.root.join("unrelated.mp4");
        std::fs::write(&decoy, b"unrelated private content").unwrap();
        fixture
            .state
            .export_jobs
            .lock()
            .unwrap()
            .get_mut("named-export")
            .unwrap()
            .path = Some(decoy.clone());
        let manager = fixture
            .state
            .storage_config
            .volume_runtime
            .as_ref()
            .unwrap();
        let worker =
            crate::storage::volumes::runtime::worker::Worker::start((**manager).clone()).unwrap();
        fixture.state.storage_config.volume_mover = Some(worker.handle());
        let preview = moves::preview(
            &fixture.state,
            "administrator",
            preview_request(&fixture.state),
        )
        .unwrap();
        assert!(preview.adopts_legacy);
        assert_eq!(
            preview.source.as_ref().unwrap().bytes,
            fixture.bytes.len() as u64
        );
        moves::confirm(
            &fixture.state,
            "administrator",
            export_confirmation(&preview),
        )
        .unwrap();
        wait_for_move_completion(&fixture.state, &preview.job_id);
        assert_export_download(&fixture.state, &fixture.bytes);
        assert_eq!(std::fs::read(&decoy).unwrap(), b"unrelated private content");
        worker.shutdown().unwrap();
        finish_legacy_export_fixture(fixture);
    }

    #[test]
    fn expired_ready_legacy_export_cannot_be_previewed_or_adopted() {
        let fixture = legacy_export_fixture();
        fixture
            .state
            .export_jobs
            .lock()
            .unwrap()
            .get_mut("named-export")
            .unwrap()
            .job
            .expires_at = Some(crate::server::millis_timestamp(1));
        assert!(
            moves::preview(
                &fixture.state,
                "administrator",
                preview_request(&fixture.state)
            )
            .is_err()
        );
        assert_export_unowned(&fixture);
        finish_legacy_export_fixture(fixture);
    }
    #[test]
    fn admitted_legacy_export_confirmation_retries_after_owner_expiry() {
        let mut fixture = legacy_export_fixture();
        let manager = fixture
            .state
            .storage_config
            .volume_runtime
            .as_ref()
            .unwrap();
        let worker =
            crate::storage::volumes::runtime::worker::Worker::start((**manager).clone()).unwrap();
        fixture.state.storage_config.volume_mover = Some(worker.handle());
        let preview = moves::preview(
            &fixture.state,
            "administrator",
            preview_request(&fixture.state),
        )
        .unwrap();
        assert!(preview.adopts_legacy);
        let request = export_confirmation(&preview);
        let admitted = moves::confirm(&fixture.state, "administrator", request.clone()).unwrap();
        assert_eq!(admitted.job_id, preview.job_id);
        wait_for_move_completion(&fixture.state, &admitted.job_id);
        let durable = moves::get(&fixture.state, &admitted.job_id).unwrap();
        let before = fixture.catalog.handle().volume_ledger_revision().unwrap();
        fixture
            .state
            .export_jobs
            .lock()
            .unwrap()
            .get_mut("named-export")
            .unwrap()
            .job
            .expires_at = Some(crate::server::millis_timestamp(1));
        // ponytail: an expired owner separates durable retry from fresh admission validation.
        for _ in 0..2 {
            let retried = moves::confirm(&fixture.state, "administrator", request.clone()).unwrap();
            assert_eq!(retried, durable);
            assert_eq!(retried.job_id, admitted.job_id);
            assert_eq!(
                fixture.catalog.handle().volume_ledger_revision().unwrap(),
                before
            );
        }
        worker.shutdown().unwrap();
        finish_legacy_export_fixture(fixture);
    }
}
