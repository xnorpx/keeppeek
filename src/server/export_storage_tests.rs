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
