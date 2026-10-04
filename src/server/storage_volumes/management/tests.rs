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
