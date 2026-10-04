use super::*;
use crate::storage::{
    RecordingCatalog,
    catalog::locations::{Kind, Object, Reply, Request},
    volumes::runtime::{Manager, tests::fixture},
};
use crate::storage::{
    catalog::locations::export_cleanup::Action,
    volumes::{PlacementRequest, VolumeRole},
};

fn persist_history(state: &mut ServerState, root: &Path) {
    let path = root.join("history.json");
    persist_export_jobs(&path, &state.export_jobs.lock().unwrap()).unwrap();
    state.export_history_path = Some(Arc::new(path));
}

fn recover_without_volumes(state: &ServerState, catalog: RecordingCatalogHandle) -> ServerState {
    let mut recovered = super::tests::media_test_state();
    recovered.storage_config.long_term_path = state.storage_config.long_term_path.clone();
    recovered.export_history_path = state.export_history_path.clone();
    assert!(recovered.storage_config.volume_runtime.is_none());
    recovered.with_recording_catalog(catalog)
}

#[test]
fn ready_named_export_survives_recovery_without_its_volume() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    persist_history(&mut state, &root);
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_none());
    cleanup_expired_exports(&recovered);
    assert_eq!(
        export_job(&recovered, "owner", "named-export")
            .unwrap()
            .status,
        proto::ExportJobStatus::Ready as i32
    );
    let error = download_export(
        &recovered,
        "owner",
        proto::DownloadExport {
            job_id: "named-export".to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Unavailable);
    assert!(matches!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object(&state)))
            .unwrap(),
        Reply::Location(Some(_))
    ));
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn interrupted_named_export_retirement_survives_history_pruning() {
    let (root, catalog, manager, mut state, mut request) = setup(16 * 1024 * 1024);
    request.burn_in_timestamp = true;
    create_export_job(&state, "owner", request).unwrap();
    let object = object(&state);
    let reservation = manager
        .reserve(VolumeRole::Export, "127.0.0.1", &[], object.clone(), 64)
        .unwrap()
        .unwrap();
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open().unwrap();
    writer.write_all(b"interrupted mp4").unwrap();
    drop(writer);
    {
        let mut jobs = state.export_jobs.lock().unwrap();
        let record = jobs.get_mut("named-export").unwrap();
        record.job.status = proto::ExportJobStatus::Running as i32;
        record.job.burn_in_timestamp = false;
        record.completed_at_ms = None;
    }
    persist_history(&mut state, &root);
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_none());
    let load = || {
        catalog
            .handle()
            .volume_location(Request::ExportCleanup(Action::Load(object.id.clone())))
            .unwrap()
    };
    let Reply::ExportCleanup(Some(before)) = load() else {
        panic!("recovery must journal retirement before forgetting an interrupted attempt");
    };
    assert!(before.allocation.is_some());
    assert_eq!(
        export_job(&recovered, "owner", "named-export")
            .unwrap()
            .status,
        proto::ExportJobStatus::Failed as i32
    );
    recovered
        .export_jobs
        .lock()
        .unwrap()
        .get_mut("named-export")
        .unwrap()
        .updated_at_ms = 0;
    cleanup_expired_exports(&recovered);
    assert!(export_job(&recovered, "owner", "named-export").is_err());
    let Reply::ExportCleanup(Some(after)) = load() else {
        panic!("history pruning must preserve retirement");
    };
    assert_eq!(before, after);
    assert_eq!(std::fs::read(path).unwrap(), b"interrupted mp4");
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn malformed_export_history_is_preserved_and_disables_creation() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let path = root.join("history.json");
    let original = b"{\"version\":1,\"jobs\":[broken history";
    std::fs::write(&path, original).unwrap();
    state.export_history_path = Some(Arc::new(path.clone()));
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let error = create_export_job(&recovered, "owner", request).unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Unavailable);
    cleanup_expired_exports(&recovered);
    assert_eq!(std::fs::read(path).unwrap(), original);
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

fn write_source(root: &Path, catalog: RecordingCatalogHandle) -> (i64, i64) {
    let started = Instant::now();
    let mut writer = crate::storage::medium_term::MediumTermWriter::create_with_catalog(
        root,
        "front-door/sub",
        started,
        8 * 1024,
        catalog.clone(),
    )
    .unwrap();
    let payload =
        super::tests::fixture_video_keyframe("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
    for offset in [0, 1000] {
        let elapsed = Duration::from_millis(offset);
        writer
            .append_one(crate::storage::RecordingFrame {
                received_at: started + elapsed,
                timestamp: Some(elapsed),
                frame: crate::storage::MediaFrame::Video(crate::storage::VideoFrame {
                    codec: crate::storage::VideoCodec::H264,
                    is_keyframe: true,
                    width: 640,
                    height: 368,
                    data: payload.clone(),
                }),
            })
            .unwrap();
    }
    writer.finalize().unwrap();
    let fragments = catalog
        .media_fragments_in_range("front-door/sub", 0, i64::MAX)
        .unwrap();
    let last = fragments.last().unwrap();
    (
        fragments[0].start_ms,
        last.start_ms + i64::try_from(last.duration_ms).unwrap(),
    )
}

pub(super) fn setup(
    limit: u64,
) -> (
    PathBuf,
    RecordingCatalog,
    Manager,
    ServerState,
    proto::CreateExportJob,
) {
    let (root, catalog, manager) = fixture(limit).unwrap();
    let (start, end) = write_source(&root.join("recordings"), catalog.handle());
    let mut state = super::tests::media_test_state();
    state.catalog = Some(catalog.handle());
    state.storage_config.long_term_path = root.join("legacy");
    state.storage_config.volume_runtime = Some(Arc::new(manager.clone()));
    let request = proto::CreateExportJob {
        job_id: "named-export".to_owned(),
        source_id: "127.0.0.1".to_owned(),
        stream_id: "main".to_owned(),
        start_time: Some(millis_timestamp(start)),
        end_time: Some(millis_timestamp(end)),
        allow_partial: false,
        burn_in_timestamp: false,
        event_seed: None,
    };
    (root, catalog, manager, state, request)
}

pub(super) fn completed(state: &ServerState) -> proto::ExportJob {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let job = export_job(state, "owner", "named-export").unwrap();
        if job.status != proto::ExportJobStatus::Running as i32 {
            return job;
        }
        assert!(Instant::now() < deadline, "export did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn object(state: &ServerState) -> Object {
    Object {
        kind: Kind::Export,
        id: state
            .export_jobs
            .lock()
            .unwrap()
            .get("named-export")
            .unwrap()
            .artifact_id
            .clone(),
    }
}

#[test]
fn named_export_is_published_and_downloads_verified_mp4() {
    let (root, catalog, manager, state, request) = setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    let ready = completed(&state);
    assert_eq!(ready.status, proto::ExportJobStatus::Ready as i32);
    let Reply::Location(Some(location)) = catalog
        .handle()
        .volume_location(Request::Lookup(object(&state)))
        .unwrap()
    else {
        panic!("ready export must have a named catalog location");
    };
    assert_eq!(location.volume, "primary");
    let path = manager.owned_path(&location).unwrap();
    assert!(path.starts_with(root.join("primary").canonicalize().unwrap()));
    let (_, messages) = download_export(
        &state,
        "owner",
        proto::DownloadExport {
            job_id: "named-export".to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    let mut bytes = Vec::new();
    for message in messages {
        let Some(proto::message::Message::Export(export)) = message.message.message else {
            panic!("download must contain export messages");
        };
        let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
            panic!("download must contain file chunks");
        };
        bytes.extend_from_slice(&chunk.payload);
    }
    assert_eq!(bytes, std::fs::read(path).unwrap());
    assert_eq!(
        encode_lower_hex(Sha256::digest(&bytes)),
        ready.sha256.unwrap()
    );
    let length = u64::try_from(bytes.len()).unwrap();
    assert!(mp4::Mp4Reader::read_header(std::io::Cursor::new(bytes), length).is_ok());
    assert!(
        !state
            .storage_config
            .long_term_path
            .join(".exports")
            .exists()
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn full_matched_export_volume_never_falls_back_to_legacy() {
    let (_root, catalog, manager, state, request) = setup(1);
    create_export_job(&state, "owner", request).unwrap();
    let failed = completed(&state);
    assert_eq!(failed.status, proto::ExportJobStatus::Failed as i32);
    assert!(matches!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object(&state)))
            .unwrap(),
        Reply::Location(None)
    ));
    assert!(
        !state
            .storage_config
            .long_term_path
            .join(".exports")
            .exists()
    );
    assert!(
        download_export(
            &state,
            "owner",
            proto::DownloadExport {
                job_id: "named-export".to_owned(),
                channel: proto::DataChannelKind::ReliableData as i32,
            }
        )
        .is_err()
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn obsolete_download_checksum_failure_does_not_retire_retried_export() {
    let (_root, catalog, manager, state, request) = setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let obsolete = state
        .export_jobs
        .lock()
        .unwrap()
        .get("named-export")
        .unwrap()
        .clone();
    export_storage::download::checksum_failure(&state, &obsolete);
    retry_export_job(&state, "owner", "named-export").unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let current = object(&state);
    assert_ne!(current.id, obsolete.artifact_id);
    export_storage::download::checksum_failure(&state, &obsolete);
    assert_eq!(
        export_job(&state, "owner", "named-export").unwrap().status,
        proto::ExportJobStatus::Ready as i32
    );
    assert!(matches!(
        catalog
            .handle()
            .volume_location(Request::ExportCleanup(Action::Load(current.id.clone())))
            .unwrap(),
        Reply::ExportCleanup(None)
    ));
    let Reply::Location(Some(location)) = catalog
        .handle()
        .volume_location(Request::Lookup(current))
        .unwrap()
    else {
        panic!("the current export must remain published");
    };
    assert!(manager.owned_path(&location).unwrap().is_file());
    assert!(
        download_export(
            &state,
            "owner",
            proto::DownloadExport {
                job_id: "named-export".to_owned(),
                channel: proto::DataChannelKind::ReliableData as i32,
            }
        )
        .is_ok()
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

pub(super) fn move_manager(root: &Path, catalog: RecordingCatalogHandle) -> Manager {
    use crate::storage::volumes::{
        PlacementRule, PlacementStrategy, Volume, VolumeConfiguration, VolumeId, VolumeState,
    };
    // ponytail: Reuse the protected-root fixture instead of a second ACL setup helper.
    let (secondary, unused_catalog, unused_manager) = fixture(16 * 1024 * 1024).unwrap();
    drop(unused_manager);
    unused_catalog.shutdown();
    let volumes = [
        ("primary", root.join("primary")),
        ("secondary", secondary.join("primary")),
    ]
    .into_iter()
    .map(|(id, root)| Volume {
        id: VolumeId::parse(id).unwrap(),
        root,
        roles: vec![VolumeRole::Active, VolumeRole::Export],
        state: VolumeState::Enabled,
        priority: 0,
        capacity_bytes: Some(16 * 1024 * 1024),
        minimum_free_bytes: 0,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        sources: vec![],
        groups: vec![],
    })
    .collect();
    Manager::new(
        VolumeConfiguration {
            volumes,
            placement: vec![PlacementRule {
                role: VolumeRole::Export,
                source: None,
                group: None,
                candidates: vec![VolumeId::parse("secondary").unwrap()],
                strategy: PlacementStrategy::Priority,
                allow_fallback: false,
            }],
        },
        catalog,
    )
    .unwrap()
}

#[test]
fn named_export_download_resolves_moved_location_despite_stale_job_path() {
    let (root, catalog, initial, mut state, request) = setup(16 * 1024 * 1024);
    create_export_job(&state, "owner", request).unwrap();
    let ready = completed(&state);
    assert_eq!(ready.status, proto::ExportJobStatus::Ready as i32);
    let stale = state
        .export_jobs
        .lock()
        .unwrap()
        .get("named-export")
        .unwrap()
        .path
        .clone()
        .unwrap();
    let original = std::fs::read(&stale).unwrap();
    let manager = move_manager(&root, catalog.handle());
    state.storage_config.volume_runtime = Some(Arc::new(manager.clone()));
    let move_id = uuid::Uuid::new_v4().to_string();
    assert!(
        manager
            .move_object(
                &move_id,
                object(&state),
                &PlacementRequest {
                    role: VolumeRole::Export,
                    source: "127.0.0.1",
                    group: "",
                    required_bytes: ready.bytes_written,
                },
                &[],
                || false
            )
            .unwrap()
    );
    assert!(manager.retire_move(&move_id).unwrap());
    assert!(!stale.exists());
    let Reply::Location(Some(location)) = catalog
        .handle()
        .volume_location(Request::Lookup(object(&state)))
        .unwrap()
    else {
        panic!("moved export must remain published");
    };
    assert_eq!(location.volume, "secondary");
    let (_, messages) = download_export(
        &state,
        "owner",
        proto::DownloadExport {
            job_id: "named-export".to_owned(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    assert_eq!(downloaded_bytes(messages), original);
    assert_eq!(
        state
            .export_jobs
            .lock()
            .unwrap()
            .get("named-export")
            .unwrap()
            .path
            .as_ref(),
        Some(&stale)
    );
    drop(state);
    drop(initial);
    drop(manager);
    catalog.shutdown();
}

fn downloaded_bytes(messages: Vec<OutboundDataMessage>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for message in messages {
        let Some(proto::message::Message::Export(export)) = message.message.message else {
            panic!("download must contain export messages");
        };
        let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
            panic!("download must contain file chunks");
        };
        bytes.extend_from_slice(&chunk.payload);
    }
    bytes
}

fn captured_legacy_export() -> (PathBuf, RecordingCatalog, Manager, ServerState) {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    state.storage_config.volume_runtime = None;
    state.storage_config.medium_term_path = root.join("recordings");
    state.storage_config.recording_catalog_path = root.join("catalog.db");
    state.storage_config.event_thumbnail_path = root.join("legacy/.event-thumbnails");
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    assert!(matches!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object(&state)))
            .unwrap(),
        Reply::Location(None)
    ));
    let paths =
        crate::storage::catalog::locations::legacy::LegacyPaths::effective(&state.storage_config)
            .unwrap();
    catalog
        .handle()
        .volume_location(Request::RegisterLegacyPaths(Box::new(paths)))
        .unwrap();
    (root, catalog, manager, state)
}

fn legacy_export_record(state: &ServerState) -> ExportJobRecord {
    state
        .export_jobs
        .lock()
        .unwrap()
        .get("named-export")
        .unwrap()
        .clone()
}

fn assert_legacy_export_retained(state: &ServerState, expected: &ExportJobRecord) {
    let actual = legacy_export_record(state);
    assert_eq!(actual.job.status, proto::ExportJobStatus::Ready as i32);
    assert_eq!(actual.job, expected.job);
    assert_eq!(actual.path, expected.path);
    assert_eq!(actual.artifact_id, expected.artifact_id);
    assert_eq!(actual.updated_at_ms, expected.updated_at_ms);
    assert_eq!(actual.completed_at_ms, expected.completed_at_ms);
}

fn legacy_export_download() -> proto::DownloadExport {
    proto::DownloadExport {
        job_id: "named-export".to_owned(),
        channel: proto::DataChannelKind::ReliableData as i32,
    }
}

#[test]
fn captured_offline_legacy_export_survives_recovery_expiry_and_pruning() {
    let (root, catalog, manager, mut state) = captured_legacy_export();
    let expected = legacy_export_record(&state);
    let bytes = std::fs::read(expected.path.as_ref().unwrap()).unwrap();
    persist_history(&mut state, &root);
    let exports = state.storage_config.long_term_path.join(".exports");
    let offline = root.join("offline-exports");
    std::fs::rename(&exports, &offline).unwrap();
    let mut recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_none());
    assert_legacy_export_retained(&recovered, &expected);
    let error = download_export(&recovered, "owner", legacy_export_download()).unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Unavailable);
    assert_legacy_export_retained(&recovered, &expected);
    {
        let mut jobs = recovered.export_jobs.lock().unwrap();
        let record = jobs.get_mut("named-export").unwrap();
        record.updated_at_ms = 0;
        record.completed_at_ms = Some(0);
        record.job.expires_at = Some(millis_timestamp(0));
    }
    let aged = legacy_export_record(&recovered);
    cleanup_expired_exports(&recovered);
    assert_legacy_export_retained(&recovered, &aged);
    persist_history(&mut recovered, &root);
    let restarted = recover_without_volumes(&recovered, catalog.handle());
    assert!(restarted.export_history_error.is_none());
    assert_legacy_export_retained(&restarted, &aged);
    assert!(!exports.exists());
    let artifact = expected
        .path
        .as_ref()
        .unwrap()
        .strip_prefix(&exports)
        .unwrap();
    assert_eq!(std::fs::read(offline.join(artifact)).unwrap(), bytes);
    std::fs::rename(&offline, &exports).unwrap();
    let (_, messages) = download_export(&restarted, "owner", legacy_export_download()).unwrap();
    assert_eq!(downloaded_bytes(messages), bytes);
    drop(restarted);
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn captured_offline_export_history_is_not_recreated_and_recovers_after_return() {
    let (root, catalog, manager, mut state) = captured_legacy_export();
    let expected = legacy_export_record(&state);
    let bytes = std::fs::read(expected.path.as_ref().unwrap()).unwrap();
    let exports = state.storage_config.long_term_path.join(".exports");
    persist_history(&mut state, &exports);
    let history = std::fs::read(exports.join("history.json")).unwrap();
    let offline = root.join("offline-exports");
    std::fs::rename(&exports, &offline).unwrap();
    let unavailable = recover_without_volumes(&state, catalog.handle());
    assert!(unavailable.export_history_error.is_some());
    cleanup_expired_exports(&unavailable);
    assert!(!exports.exists());
    assert_eq!(
        std::fs::read(offline.join("history.json")).unwrap(),
        history
    );
    std::fs::rename(&offline, &exports).unwrap();
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_none());
    assert_legacy_export_retained(&recovered, &expected);
    let (_, messages) = download_export(&recovered, "owner", legacy_export_download()).unwrap();
    assert_eq!(downloaded_bytes(messages), bytes);
    drop(recovered);
    drop(unavailable);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn captured_online_export_root_still_reports_a_missing_artifact_as_failed() {
    let (root, catalog, manager, mut state) = captured_legacy_export();
    let expected = legacy_export_record(&state);
    persist_history(&mut state, &root);
    std::fs::remove_file(expected.path.as_ref().unwrap()).unwrap();
    assert!(
        state
            .storage_config
            .long_term_path
            .join(".exports")
            .is_dir()
    );
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_none());
    let failed = legacy_export_record(&recovered);
    assert_eq!(failed.job.status, proto::ExportJobStatus::Failed as i32);
    assert!(failed.job.retryable);
    assert!(failed.path.is_none());
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn legacy_export_worker_does_not_recreate_a_captured_offline_root() {
    let (root, catalog, manager, state) = captured_legacy_export();
    let original = legacy_export_record(&state);
    let bytes = std::fs::read(original.path.as_ref().unwrap()).unwrap();
    let exports = state.storage_config.long_term_path.join(".exports");
    let offline = root.join("offline-exports");
    std::fs::rename(&exports, &offline).unwrap();
    assert!(state.export_history_error.is_none());
    let mut request = original.request.clone();
    request.job_id = "offline-new".to_owned();
    create_export_job(&state, "owner", request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal = loop {
        let job = export_job(&state, "owner", "offline-new").unwrap();
        if job.status != proto::ExportJobStatus::Running as i32 {
            break job;
        }
        assert!(Instant::now() < deadline, "offline export did not finish");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(terminal.status, proto::ExportJobStatus::Failed as i32);
    assert!(!exports.exists());
    let artifact = original
        .path
        .as_ref()
        .unwrap()
        .strip_prefix(&exports)
        .unwrap();
    assert_eq!(std::fs::read(offline.join(artifact)).unwrap(), bytes);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_creation_does_not_recreate_captured_history_after_startup() {
    let (root, catalog, manager, mut state) = captured_legacy_export();
    let original = legacy_export_record(&state);
    let exports = state.storage_config.long_term_path.join(".exports");
    persist_history(&mut state, &exports);
    let history = std::fs::read(exports.join("history.json")).unwrap();
    let offline = root.join("offline-exports");
    std::fs::rename(&exports, &offline).unwrap();
    let mut request = original.request.clone();
    request.job_id = "offline-new".into();
    let error = create_export_job(&state, "owner", request).unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::Unavailable);
    assert!(!exports.exists());
    assert_eq!(
        std::fs::read(offline.join("history.json")).unwrap(),
        history
    );
    assert_legacy_export_retained(&state, &original);
    assert_eq!(state.export_jobs.lock().unwrap().len(), 1);
    persist_export_jobs_logged(&state, &state.export_jobs.lock().unwrap(), "offline-test");
    assert!(!exports.exists());
    drop(state);
    drop(manager);
    catalog.shutdown();
}

fn managed_history_fixture() -> (PathBuf, RecordingCatalog, Manager, ServerState) {
    let (directory, catalog, manager) = fixture(16 * 1024 * 1024).unwrap();
    let primary = &manager.configuration().volumes[0].root;
    let identity = crate::storage::volumes::root::Root::open(primary)
        .unwrap()
        .identity()
        .clone();
    let handoff = uuid::Uuid::new_v4();
    let binding = crate::config::MetadataBinding {
        volume_id: manager.configuration().volumes[0].id.clone(),
        catalog_file: format!("catalog-{handoff}.db"),
        history_file: format!("exports-{handoff}.json"),
        catalog_id: uuid::Uuid::new_v4().to_string(),
        generation: 1,
        filesystem: identity.filesystem,
        root_identity: identity.directory,
    };
    let history = primary.join(&binding.history_file);
    std::fs::write(&history, b"{\"version\":1,\"jobs\":[]}\n").unwrap();
    let mut state = super::tests::media_test_state();
    state.catalog = None;
    state.storage_config.long_term_path = directory.join("legacy");
    state.storage_config.recording_catalog_path = primary.join(&binding.catalog_file);
    state.storage_config.metadata_history_path = Some(history.clone());
    state.storage_config.metadata = Some(binding);
    state.export_history_path = Some(Arc::new(history));
    let legacy = state
        .storage_config
        .long_term_path
        .join(".exports/history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(legacy, b"unrelated legacy history").unwrap();
    (directory, catalog, manager, state)
}

#[test]
fn managed_export_history_restores_and_persists_only_at_its_bound_path() {
    let (_directory, catalog, manager, mut state) = managed_history_fixture();
    let history = state.storage_config.metadata_history_path.clone().unwrap();
    let legacy = state
        .storage_config
        .long_term_path
        .join(".exports/history.json");
    export_storage::history::restore(&mut state);
    assert!(state.export_history_error.is_none());
    assert!(state.export_jobs.lock().unwrap().is_empty());
    export_storage::history::persist(&state, &state.export_jobs.lock().unwrap()).unwrap();
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&history).unwrap()).unwrap();
    assert_eq!(persisted["version"], 1);
    assert_eq!(persisted["jobs"], serde_json::json!([]));
    assert_eq!(std::fs::read(legacy).unwrap(), b"unrelated legacy history");
    assert_eq!(state.export_history_path.as_deref(), Some(&history));
    assert!(!state.storage_config.recording_catalog_path.exists());
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn missing_managed_export_history_disables_recovery_without_recreating_it() {
    let (_directory, catalog, manager, mut state) = managed_history_fixture();
    let history = state.storage_config.metadata_history_path.clone().unwrap();
    std::fs::remove_file(&history).unwrap();
    export_storage::history::restore(&mut state);
    assert!(state.export_history_error.is_some());
    assert!(!history.exists());
    assert!(export_storage::history::persist(&state, &state.export_jobs.lock().unwrap()).is_err());
    assert!(!history.exists());
    let legacy = state
        .storage_config
        .long_term_path
        .join(".exports/history.json");
    assert_eq!(std::fs::read(legacy).unwrap(), b"unrelated legacy history");
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn replaced_managed_history_root_rejects_restore_and_persist_without_overwriting_either_file() {
    let (directory, catalog, manager, mut state) = managed_history_fixture();
    export_storage::history::restore(&mut state);
    assert!(state.export_history_error.is_none());
    let history = state.storage_config.metadata_history_path.clone().unwrap();
    let original = std::fs::read(&history).unwrap();
    let primary = history.parent().unwrap();
    let retained = directory.join("retained-primary");
    drop(manager);
    std::fs::rename(primary, &retained).unwrap();
    std::fs::create_dir(primary).unwrap();
    let unrelated = b"{ \"jobs\": [], \"version\": 1 }\n";
    std::fs::write(&history, unrelated).unwrap();
    let replacement = crate::storage::volumes::root::Root::open(primary).unwrap();
    assert_ne!(
        *replacement.identity(),
        state
            .storage_config
            .metadata
            .as_ref()
            .unwrap()
            .root_identity()
    );
    drop(replacement);
    export_storage::history::restore(&mut state);
    assert!(state.export_history_error.is_some());
    assert!(export_storage::history::persist(&state, &state.export_jobs.lock().unwrap()).is_err());
    assert_eq!(std::fs::read(&history).unwrap(), unrelated);
    assert_eq!(
        std::fs::read(retained.join(history.file_name().unwrap())).unwrap(),
        original
    );
    let legacy = state
        .storage_config
        .long_term_path
        .join(".exports/history.json");
    assert_eq!(std::fs::read(legacy).unwrap(), b"unrelated legacy history");
    drop(state);
    catalog.shutdown();
}

fn history_with_records(records: &[&ExportJobRecord]) -> Vec<u8> {
    serde_json::to_vec(&PersistedExportHistory {
        version: EXPORT_HISTORY_VERSION,
        jobs: records
            .iter()
            .map(|record| PersistedExportJobRecord::from_record(record))
            .collect(),
    })
    .unwrap()
}

#[test]
fn history_snapshot_rejects_duplicate_job_and_artifact_ids_but_accepts_distinct_records() {
    let (_root, catalog, manager, state) = captured_legacy_export();
    let first = legacy_export_record(&state);
    let mut second = first.clone();
    second.request.job_id = "other-export".to_owned();
    second.job.job_id = "other-export".to_owned();
    second.artifact_id = uuid::Uuid::new_v4().to_string();
    validate_export_history_snapshot(&history_with_records(&[&first, &second])).unwrap();
    let mut duplicate_job = second.clone();
    duplicate_job.request.job_id = first.request.job_id.clone();
    duplicate_job.job.job_id = first.job.job_id.clone();
    assert!(
        validate_export_history_snapshot(&history_with_records(&[&first, &duplicate_job])).is_err()
    );
    let mut duplicate_artifact = second;
    duplicate_artifact.artifact_id = first.artifact_id.clone();
    assert!(
        validate_export_history_snapshot(&history_with_records(&[&first, &duplicate_artifact]))
            .is_err()
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn duplicate_export_history_is_rejected_before_reconciliation_can_remove_an_artifact() {
    let (root, catalog, manager, mut state) = captured_legacy_export();
    let original = legacy_export_record(&state);
    let artifact = original.path.as_ref().unwrap();
    let retained = std::fs::read(artifact).unwrap();
    let mut failed = original.clone();
    failed.job.status = proto::ExportJobStatus::Failed as i32;
    let mut duplicate = original.clone();
    duplicate.artifact_id = uuid::Uuid::new_v4().to_string();
    let bytes = history_with_records(&[&failed, &duplicate]);
    let history = root.join("duplicate-history.json");
    std::fs::write(&history, &bytes).unwrap();
    state.export_history_path = Some(Arc::new(history.clone()));
    let recovered = recover_without_volumes(&state, catalog.handle());
    assert!(recovered.export_history_error.is_some());
    assert_eq!(std::fs::read(artifact).unwrap(), retained);
    assert_eq!(std::fs::read(&history).unwrap(), bytes);
    assert!(recovered.export_jobs.lock().unwrap().is_empty());
    drop(recovered);
    drop(state);
    drop(manager);
    catalog.shutdown();
}
