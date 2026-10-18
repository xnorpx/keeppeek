use super::export_storage_tests::{completed, setup};
use super::*;

pub(super) fn configured_source(state: &mut ServerState, root: &Path, identity: Uuid) -> PathBuf {
    let path = root.join("camera-config.toml");
    config::write_private_file(
        &path,
        format!("[cameras.front]\nip = '127.0.0.1'\nprofile_identity = '{identity}'\n").as_bytes(),
    )
    .unwrap();
    state.camera_config_path = Some(path.clone());
    path
}

fn configured_identity(path: &Path, ip: &str) -> Uuid {
    let configuration = config::load_config(path).unwrap();
    let identities = configuration.camera_profile_identities().unwrap();
    Uuid::parse_str(&identities[&ip.parse::<IpAddr>().unwrap()]).unwrap()
}

pub(super) fn current_record(state: &ServerState) -> ExportJobRecord {
    state.export_jobs.lock().unwrap()["named-export"].clone()
}

pub(super) fn restored_record(
    state: &ServerState,
    catalog: &RecordingCatalogHandle,
) -> ExportJobRecord {
    let bytes = export_history_bytes(&state.export_jobs.lock().unwrap()).unwrap();
    restore_export_history(
        &bytes,
        &state.storage_config.long_term_path.join(".exports"),
        Some(catalog),
    )
    .unwrap()
    .remove("named-export")
    .unwrap()
}

fn replace_camera(path: &Path) -> Uuid {
    let ip = "127.0.0.1".parse().unwrap();
    config::remove_camera(path, ip).unwrap();
    let replacement: CameraConfig = toml::from_str("ip = '127.0.0.1'").unwrap();
    config::upsert_camera(path, &replacement).unwrap();
    configured_identity(path, "127.0.0.1")
}

#[test]
fn export_provenance_keeps_original_attempt_identity_after_camera_replacement() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let original = Uuid::new_v4();
    let path = configured_source(&mut state, &root, original);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let before = current_record(&state);
    assert_eq!(before.source_identity, Some(original));
    let artifact_path = before.path.clone().unwrap();
    let bytes = std::fs::read(&artifact_path).unwrap();
    let replacement = replace_camera(&path);
    assert_ne!(replacement, original);
    let restored = restored_record(&state, &catalog.handle());
    assert_eq!(restored.source_identity, Some(original));
    assert_ne!(restored.source_identity, Some(replacement));
    assert_eq!(restored.artifact_id, before.artifact_id);
    assert_eq!(restored.job.sha256, before.job.sha256);
    assert_eq!(std::fs::read(&artifact_path).unwrap(), bytes);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_provenance_survives_ip_edit_and_history_round_trip() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let identity = Uuid::new_v4();
    let path = configured_source(&mut state, &root, identity);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let before = current_record(&state);
    let source = std::fs::read_to_string(&path)
        .unwrap()
        .replace("127.0.0.1", "127.0.0.2");
    config::write_private_file_atomically(&path, source.as_bytes()).unwrap();
    assert_eq!(configured_identity(&path, "127.0.0.2"), identity);
    let restored = restored_record(&state, &catalog.handle());
    assert_eq!(restored.source_identity, Some(identity));
    assert_eq!(restored.artifact_id, before.artifact_id);
    assert_eq!(restored.job.source_id, "127.0.0.1");
    assert_eq!(restored.job.sha256, before.job.sha256);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn legacy_export_provenance_remains_absent_and_normal_download_still_works() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    assert_eq!(current_record(&state).source_identity, None);
    let bytes = export_history_bytes(&state.export_jobs.lock().unwrap()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["jobs"][0].get("source_identity").is_none());
    assert!(json["jobs"][0].get("owner_key").is_none());
    assert_eq!(current_record(&state).owner_key, None);
    configured_source(&mut state, &root, Uuid::new_v4());
    let restored = restored_record(&state, &catalog.handle());
    assert_eq!(restored.source_identity, None);
    state.export_jobs = Arc::new(Mutex::new(HashMap::from([(
        "named-export".into(),
        restored,
    )])));
    let (_, messages) = download_export(
        &state,
        "owner",
        proto::DownloadExport {
            job_id: "named-export".into(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    assert!(!messages.is_empty());
    assert_eq!(current_record(&state).source_identity, None);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_retry_captures_current_identity_only_for_its_new_artifact_generation() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let original = Uuid::new_v4();
    let path = configured_source(&mut state, &root, original);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let obsolete = current_record(&state);
    export_storage::download::checksum_failure(&state, &obsolete);
    let replacement = replace_camera(&path);
    assert_ne!(replacement, original);
    retry_export_job(&state, "owner", "named-export").unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let current = current_record(&state);
    assert_ne!(current.artifact_id, obsolete.artifact_id);
    assert_ne!(current.path, obsolete.path);
    assert_eq!(obsolete.source_identity, Some(original));
    assert_eq!(current.source_identity, Some(replacement));
    assert_eq!(
        restored_record(&state, &catalog.handle()).source_identity,
        Some(replacement)
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}
