use super::*;
use crate::{
    cameras::{CameraConfig, CameraRecordingMode},
    keeppeek::KeepPeekLoop,
    profiles::{CameraProfileOverride, RuntimeApplicationState, RuntimeProfile},
    shutdown::Shutdown,
    storage::{
        MediaFrame, RecordingFrame, RecordingStreamIdentity, StorageEngine, VideoCodec, VideoFrame,
    },
    webrtc::WebRtc,
};
use std::{
    path::{Path, PathBuf},
    thread::JoinHandle,
};

const SOURCE: &str = "127.0.0.1";

struct Lifecycle {
    directory: PathBuf,
    source: String,
    identity: String,
    state: ServerState,
    storage: Option<StorageEngine>,
    shutdown: Shutdown,
    worker: Option<JoinHandle<()>>,
    private_before_start: bool,
}

impl Lifecycle {
    fn new(private: bool) -> Self {
        Self::with_privacy(private, false)
    }

    fn with_privacy(private: bool, profile_private: bool) -> Self {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-output")
            .join(format!("profile-lifecycle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("medium")).unwrap();
        let mut configuration = Config {
            storage: crate::config::StorageToml {
                medium_term_path: Some(directory.join("medium").to_string_lossy().into_owned()),
                long_term_path: Some(directory.join("archive").to_string_lossy().into_owned()),
                short_term_secs: 0,
                flush_interval_secs: 0,
                minimum_free_gb: 0,
                warning_free_gb: 0,
                critical_free_gb: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        configuration.runtime_profiles.profiles.insert(
            "away".to_owned(),
            RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: BTreeMap::from([(
                    SOURCE.to_owned(),
                    CameraProfileOverride {
                        recording_mode: Some(CameraRecordingMode::Main),
                        privacy_enforced: Some(profile_private),
                        ..Default::default()
                    },
                )]),
            },
        );
        if private {
            configuration.privacy.default = Some(always_private_schedule());
        }
        let root = toml::Value::try_from(&configuration)
            .unwrap()
            .as_table()
            .unwrap()
            .clone();
        let path = directory.join("config.toml");
        config::write_configuration_table(&path, &root).unwrap();
        let camera: CameraConfig = toml::from_str(
            "ip='127.0.0.1'\nname='lifecycle'\nusername='fixture'\npassword='fixture'\nbackend='retina'\nmain_rtsp_url='rtsp://127.0.0.1:9/main'\nsub_rtsp_url='rtsp://127.0.0.1:9/sub'\nrecording_mode='sub'\n[events]\nmode='disabled'\n",
        ).unwrap();
        config::upsert_camera(&path, &camera).unwrap();
        let identities = config::load_camera_profile_identities(&path).unwrap();
        let identity = identities[&camera.ip].clone();
        let mut root = config::load_configuration_table(&path).unwrap();
        let targets = root
            .get_mut("runtime_profiles")
            .unwrap()
            .get_mut("profiles")
            .unwrap()
            .get_mut("away")
            .unwrap()
            .get_mut("cameras")
            .unwrap()
            .as_table_mut()
            .unwrap();
        let overrides = targets.remove(SOURCE).unwrap();
        targets.insert(identity, overrides);
        config::write_configuration_table(&path, &root).unwrap();
        Self::start(directory)
    }

    fn start(directory: PathBuf) -> Self {
        let path = directory.join("config.toml");
        let configuration = config::load_config(&path).unwrap();
        let configured = configuration.camera_configurations().unwrap();
        let cameras = crate::cameras::configured_cameras(&configured);
        let identities = configuration.camera_profile_identities().unwrap();
        let (source_ip, identity) = identities.first_key_value().unwrap();
        let source = source_ip.to_string();
        let identity = identity.clone();
        let storage_config = crate::storage::StorageConfig::from_toml(&configuration.storage);
        let storage = StorageEngine::start(storage_config.clone());
        let shutdown = Shutdown::new();
        let mut runtime = KeepPeekLoop::new(shutdown.clone(), Some(storage.handle()));
        runtime.set_runtime_profile_camera_ids(identities).unwrap();
        runtime.set_runtime_profiles(configuration.runtime_profiles.clone());
        let state = ServerState::new(
            &configuration,
            &configured,
            &cameras,
            &storage_config,
            crate::storage::RecordingDemand::new(Duration::from_secs(1)),
            WebRtc::new(),
        )
        .with_camera_config_path(path)
        .with_camera_runtime(runtime.control())
        .with_recording_health(storage.health());
        let privacy = state.privacy_registry();
        let private_before_start = privacy.decision(&source, chrono::Utc::now()).unwrap().0;
        storage.set_privacy_registry(privacy.clone());
        runtime.set_privacy_registry(privacy);
        let recovery = state.clone();
        runtime.set_recovered_profile_publisher(move |revision| {
            recovery.confirm_recovered_profile(revision);
        });
        let publisher = state.clone();
        runtime.set_runtime_profile_status_publisher(move || {
            publisher.publish_runtime_profile_status();
        });
        assert!(
            state
                .runtime_profile_status
                .read()
                .unwrap()
                .cameras
                .is_empty()
        );
        runtime.add_cameras(&cameras).unwrap();
        let worker = std::thread::spawn(move || runtime.run());
        Self {
            directory,
            source,
            identity,
            state,
            storage: Some(storage),
            shutdown,
            worker: Some(worker),
            private_before_start,
        }
    }

    fn activate(&self, profile: &str, expected_revision: u64) {
        let command = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: ACTIVE_KEY.to_owned(),
                schema: ACTIVE_SCHEMA.to_owned(),
                value: Some(Struct {
                    fields: BTreeMap::from([(
                        "profile_id".to_owned(),
                        Value {
                            kind: Some(Kind::StringValue(profile.to_owned())),
                        },
                    )]),
                }),
                expected_revision: Some(expected_revision),
                ttl: None,
            })),
        };
        let result = super::super::dispatch(
            &self.state,
            SessionId::from_u64(20_250),
            &ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into()),
            command,
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(result) = result else {
            panic!("state-store result")
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("active entry")
        };
        assert_eq!(entry.revision, expected_revision + 1);
        let persisted = config::load_config(&self.directory.join("config.toml")).unwrap();
        assert_eq!(persisted.runtime_profiles.revision, entry.revision);
        assert_eq!(
            persisted.runtime_profiles.active_profile_id.as_deref(),
            (!profile.is_empty()).then_some(profile)
        );
    }

    fn application(&self) -> crate::profiles::CameraRuntimeApplication {
        self.state.runtime_profile_status.read().unwrap().cameras[&self.identity].clone()
    }

    fn assert_applied(&self, revision: u64, profile: Option<&str>, mode: CameraRecordingMode) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = self.state.runtime_profile_status.read().unwrap();
            let observed = super::application::observed_status(&self.state, &status);
            if observed.revision == revision
                && observed
                    .cameras
                    .get(&self.identity)
                    .is_some_and(|camera| camera.state == RuntimeApplicationState::Applied)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "camera did not converge: {status:?}"
            );
            drop(status);
            std::thread::sleep(Duration::from_millis(5));
        }
        let actual = self.state.runtime_profile_status.read().unwrap();
        let status = super::application::observed_status(&self.state, &actual);
        assert_eq!(status.revision, revision);
        assert_eq!(status.active_profile_id.as_deref(), profile);
        assert_eq!(
            status.cameras[&self.identity].state,
            RuntimeApplicationState::Applied
        );
        assert_eq!(status.cameras[&self.identity].policy.recording_mode, mode);
    }

    fn send_frames(&self, stream: &str) {
        self.send_tagged_frames(stream, &[]);
    }

    fn send_tagged_frames(&self, stream: &str, marker: &[u8]) {
        // Exercise storage ingress with a recognizable IDR payload in each sample.
        let started = Instant::now();
        for offset in [0, 40, 80] {
            self.storage.as_ref().unwrap().handle().ingest_stream(
                RecordingStreamIdentity::new(&self.source, stream, "lifecycle"),
                keyframe(started + Duration::from_millis(offset), marker),
            );
        }
    }

    fn progress(&self, stream: &str) -> u64 {
        self.storage
            .as_ref()
            .unwrap()
            .health()
            .snapshot()
            .streams
            .iter()
            .find(|health| health.stream_id == format!("lifecycle/{stream}"))
            .map_or(0, |health| health.recorded_duration_ms)
    }

    fn wait_for_progress(&self, stream: &str, previous: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.progress(stream) <= previous {
            assert!(
                Instant::now() < deadline,
                "recording did not progress for {stream}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn writer_barrier(&self) {
        let configuration = config::load_config(&self.directory.join("config.toml")).unwrap();
        let identities = configuration.camera_profile_identities().unwrap();
        self.state
            .camera_runtime
            .as_ref()
            .unwrap()
            .set_runtime_profiles_with_camera_ids(configuration.runtime_profiles, identities)
            .unwrap();
    }

    fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
        if let Some(storage) = self.storage.take() {
            storage.shutdown();
        }
    }

    fn restart(&mut self) {
        self.stop();
        let mut restarted = Self::start(self.directory.clone());
        std::mem::swap(self, &mut restarted);
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn always_private_schedule() -> crate::privacy::PrivacySchedule {
    crate::privacy::PrivacySchedule {
        enabled: true,
        timezone: "UTC".to_owned(),
        temporary_override: None,
        keep_camera_connected: true,
        windows: [("00:00", "12:00"), ("12:00", "00:00")]
            .into_iter()
            .map(|(start, end)| crate::privacy::PrivacyWindow {
                weekdays: (1..=7).collect(),
                start: start.to_owned(),
                end: end.to_owned(),
            })
            .collect(),
    }
}

fn keyframe(received_at: Instant, marker: &[u8]) -> RecordingFrame {
    let mut data = vec![
        0, 0, 0, 8, 0x67, 0x42, 0, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce, 0x3c, 0x80,
    ];
    data.extend_from_slice(&u32::try_from(1 + marker.len()).unwrap().to_be_bytes());
    data.push(0x65);
    data.extend_from_slice(marker);
    RecordingFrame {
        source_rtp: None,
        received_at,
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: data.into(),
        }),
    }
}

#[test]
fn runtime_profile_lifecycle_retries_after_real_writer_failure_and_returns_to_base() {
    let mut lifecycle = Lifecycle::new(false);
    lifecycle.activate("away", 0);
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Main);
    let medium = lifecycle.directory.join("medium");
    std::fs::remove_dir(&medium).unwrap();
    std::fs::write(&medium, b"blocked recording directory").unwrap();
    lifecycle.send_frames("main");
    let deadline = Instant::now() + Duration::from_secs(3);
    while lifecycle.application().state != RuntimeApplicationState::Degraded {
        assert!(
            Instant::now() < deadline,
            "writer failure was not published"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        lifecycle
            .application()
            .reason
            .unwrap()
            .contains("active write failure")
    );
    std::fs::remove_file(&medium).unwrap();
    std::fs::create_dir(&medium).unwrap();
    lifecycle.activate("away", 1);
    lifecycle.assert_applied(2, Some("away"), CameraRecordingMode::Main);
    lifecycle.send_frames("main");
    lifecycle.wait_for_progress("main", 0);
    lifecycle.send_frames("sub");
    lifecycle.writer_barrier();
    assert_eq!(lifecycle.progress("sub"), 0);
    lifecycle.activate("", 2);
    lifecycle.assert_applied(3, None, CameraRecordingMode::Sub);
    let main_progress = lifecycle.progress("main");
    lifecycle.send_frames("main");
    lifecycle.send_frames("sub");
    lifecycle.wait_for_progress("sub", 0);
    lifecycle.writer_barrier();
    assert_eq!(lifecycle.progress("main"), main_progress);
    lifecycle.activate("away", 3);
    lifecycle.restart();
    lifecycle.assert_applied(4, Some("away"), CameraRecordingMode::Main);
    lifecycle.send_frames("main");
    lifecycle.wait_for_progress("main", 0);
    lifecycle.send_frames("sub");
    lifecycle.writer_barrier();
    assert_eq!(lifecycle.progress("sub"), 0);
}

#[test]
fn runtime_profile_lifecycle_restores_base_privacy_before_runtime_acceptance() {
    let mut lifecycle = Lifecycle::new(true);
    assert!(lifecycle.private_before_start);
    lifecycle.activate("away", 0);
    assert!(lifecycle.application().policy.privacy_enforced);
    assert!(
        lifecycle
            .state
            .privacy_registry()
            .decision(SOURCE, chrono::Utc::now())
            .unwrap()
            .0
    );
    lifecycle.send_frames("main");
    lifecycle.writer_barrier();
    assert!(
        lifecycle
            .storage
            .as_ref()
            .unwrap()
            .health()
            .snapshot()
            .streams
            .is_empty()
    );
    lifecycle.restart();
    assert!(lifecycle.private_before_start);
    let status = lifecycle.state.runtime_profile_status.read().unwrap();
    assert_eq!(status.revision, 1);
    assert_eq!(status.active_profile_id.as_deref(), Some("away"));
    assert_eq!(
        status.cameras[&lifecycle.identity].state,
        RuntimeApplicationState::Applied
    );
    assert!(status.cameras[&lifecycle.identity].policy.privacy_enforced);
    drop(status);
    lifecycle.send_frames("main");
    lifecycle.writer_barrier();
    assert!(
        lifecycle
            .storage
            .as_ref()
            .unwrap()
            .health()
            .snapshot()
            .streams
            .is_empty()
    );
}

#[test]
fn profile_only_privacy_excludes_tagged_recording_bytes_through_restart_and_supersession() {
    const ACCEPTED: &[u8] = b"keeppeek-accepted-base-frame";
    const PRIVATE: &[u8] = b"keeppeek-private-profile-frame";
    const RESTARTED: &[u8] = b"keeppeek-private-restarted-frame";
    const SUPERSEDED: &[u8] = b"keeppeek-private-superseded-frame";
    let mut lifecycle = Lifecycle::with_privacy(false, true);
    assert!(!lifecycle.private_before_start);
    lifecycle.activate("away", 0);
    for stream in ["main", "sub"] {
        lifecycle.send_tagged_frames(stream, PRIVATE);
    }
    lifecycle.writer_barrier();
    lifecycle.restart();
    assert!(lifecycle.private_before_start);
    for stream in ["main", "sub"] {
        lifecycle.send_tagged_frames(stream, RESTARTED);
    }
    lifecycle.writer_barrier();
    lifecycle.activate("", 1);
    lifecycle.activate("away", 2);
    assert!(
        lifecycle
            .state
            .privacy_registry()
            .decision(SOURCE, chrono::Utc::now())
            .unwrap()
            .0
    );
    for stream in ["main", "sub"] {
        lifecycle.send_tagged_frames(stream, SUPERSEDED);
    }
    lifecycle.writer_barrier();
    assert_eq!(
        (lifecycle.progress("main"), lifecycle.progress("sub")),
        (0, 0)
    );
    lifecycle.activate("", 3);
    lifecycle.assert_applied(4, None, CameraRecordingMode::Sub);
    let deadline = Instant::now() + Duration::from_secs(5);
    while lifecycle
        .state
        .privacy_registry()
        .decision(SOURCE, chrono::Utc::now())
        .unwrap()
        .0
    {
        assert!(
            Instant::now() < deadline,
            "Base did not release the profile privacy gate"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    lifecycle.send_tagged_frames("sub", ACCEPTED);
    lifecycle.wait_for_progress("sub", 0);
    lifecycle.stop();
    assert_final_recording_markers(
        &lifecycle.directory,
        ACCEPTED,
        &[PRIVATE, RESTARTED, SUPERSEDED],
    );
}

fn assert_final_recording_markers(
    directory: &std::path::Path,
    accepted: &[u8],
    forbidden: &[&[u8]],
) {
    let mut directories = vec![directory.to_path_buf()];
    let mut accepted_recording = false;
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                let bytes = std::fs::read(entry.path()).unwrap();
                for &private in forbidden {
                    assert!(
                        !bytes.windows(private.len()).any(|sample| sample == private),
                        "private media was written to {:?}",
                        entry.path()
                    );
                }
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "mp4")
                {
                    accepted_recording |= bytes
                        .windows(accepted.len())
                        .any(|sample| sample == accepted);
                }
            }
        }
    }
    assert!(
        accepted_recording,
        "the authorized control sample must be present in a finalized recording"
    );
}

#[test]
fn profile_privacy_blocks_an_existing_export_download_after_activation_and_restart() {
    use crate::server::{
        download_export, encode_lower_hex, millis_timestamp, tests::active_export_fixture,
    };
    const CONTENT: &[u8] = b"KeepPeek synthetic export privacy control";
    let mut lifecycle = Lifecycle::with_privacy(false, true);
    let path = lifecycle.directory.join("privacy-control.mp4");
    std::fs::write(&path, CONTENT).unwrap();
    let request = proto::CreateExportJob {
        job_id: "profile-privacy-control".to_owned(),
        source_id: SOURCE.to_owned(),
        stream_id: "main".to_owned(),
        start_time: Some(millis_timestamp(1_000)),
        end_time: Some(millis_timestamp(2_000)),
        ..Default::default()
    };
    let mut ready = active_export_fixture(&lifecycle.state, &request);
    ready.job.status = proto::ExportJobStatus::Ready as i32;
    ready.job.sha256 = Some(encode_lower_hex(Sha256::digest(CONTENT)));
    ready.path = Some(path);
    let seed = |state: &ServerState| {
        state
            .export_jobs
            .lock()
            .unwrap()
            .insert(request.job_id.clone(), ready.clone());
    };
    let download = |state: &ServerState| {
        download_export(
            state,
            "owner",
            proto::DownloadExport {
                job_id: request.job_id.clone(),
                channel: proto::DataChannelKind::ReliableData as i32,
                artifact: 0,
            },
        )
    };
    seed(&lifecycle.state);
    let (_, messages) = download(&lifecycle.state).unwrap();
    assert_eq!(messages.len(), 1);
    let Some(proto::message::Message::Export(export)) = &messages[0].message.message else {
        panic!("expected export data")
    };
    let Some(proto::export_message::Message::FileChunk(chunk)) = &export.message else {
        panic!("expected export chunk")
    };
    assert_eq!(chunk.payload.as_slice(), CONTENT);
    let permit = messages[0].permit.as_ref().unwrap();
    assert!(permit.deliver(|| ()).is_some());
    lifecycle.activate("away", 0);
    assert!(permit.deliver(|| ()).is_none());
    let denied = |state: &ServerState| {
        let error = download(state).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert!(error.message.contains("privacy is active"));
    };
    denied(&lifecycle.state);
    lifecycle.restart();
    assert!(lifecycle.private_before_start);
    seed(&lifecycle.state);
    denied(&lifecycle.state);
}

#[test]
fn runtime_profile_identity_survives_ip_edit_and_readd_uses_base() {
    let mut lifecycle = Lifecycle::new(false);
    lifecycle.activate("away", 0);
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Main);
    let original_identity = lifecycle.identity.clone();
    lifecycle.send_frames("main");
    lifecycle.wait_for_progress("main", 0);
    lifecycle.stop();
    let path = lifecycle.directory.join("config.toml");
    let mut root = config::load_configuration_table(&path).unwrap();
    root.get_mut("cameras")
        .unwrap()
        .get_mut("lifecycle")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert("ip".to_owned(), toml::Value::String("127.0.0.2".to_owned()));
    config::write_configuration_table(&path, &root).unwrap();
    lifecycle.restart();
    assert_eq!(lifecycle.identity, original_identity);
    assert_eq!(lifecycle.source, "127.0.0.2");
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Main);
    lifecycle.send_frames("main");
    lifecycle.wait_for_progress("main", 0);
    lifecycle.stop();
    let camera = config::load_cameras(&path)
        .unwrap()
        .into_values()
        .flatten()
        .next()
        .unwrap();
    config::remove_camera(&path, camera.ip).unwrap();
    config::upsert_camera(&path, &camera).unwrap();
    lifecycle.restart();
    assert_ne!(lifecycle.identity, original_identity);
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Sub);
    let status = lifecycle.state.runtime_profile_status.read().unwrap();
    assert_eq!(
        status.cameras[&original_identity].state,
        RuntimeApplicationState::Degraded
    );
    assert!(
        status.cameras[&original_identity]
            .reason
            .as_ref()
            .unwrap()
            .contains("missing")
    );
    drop(status);
    lifecycle.send_frames("main");
    lifecycle.send_frames("sub");
    lifecycle.wait_for_progress("sub", 0);
    lifecycle.writer_barrier();
    assert_eq!(lifecycle.progress("main"), 0);
}

fn maximum_degraded_status() -> crate::profiles::RuntimeProfileStatus {
    let base = crate::profiles::BaseCameraPolicy {
        available: true,
        recording_mode: CameraRecordingMode::Both,
        privacy_enforced: false,
        notifications_enabled: true,
    };
    let overrides = CameraProfileOverride {
        availability: Some(true),
        recording_mode: Some(CameraRecordingMode::EventBoost),
        privacy_enforced: Some(true),
        notifications_suppressed: Some(true),
    };
    crate::profiles::RuntimeProfileStatus {
        revision: i64::MAX as u64,
        active_profile_id: Some("a".repeat(128)),
        definition_revision: Some(i64::MAX as u64),
        cameras: (1..=127)
            .map(|index| {
                let camera_id = format!("00000000-0000-4000-8000-{index:012x}");
                assert_eq!(camera_id.len(), 36);
                assert_eq!(
                    uuid::Uuid::parse_str(&camera_id).unwrap().get_version_num(),
                    4
                );
                let application = crate::profiles::CameraRuntimeApplication {
                    camera_id: camera_id.clone(),
                    state: RuntimeApplicationState::Degraded,
                    policy: crate::profiles::resolve_camera_policy(&base, Some(&overrides)),
                    reason: Some("r".repeat(96)),
                };
                (camera_id, application)
            })
            .collect(),
    }
}

#[test]
fn observed_parts_cover_all_degraded_cameras_and_fit_document_and_snapshot_budgets() {
    let lifecycle = Lifecycle::new(false);
    let status = maximum_degraded_status();
    let expected = status.cameras.keys().cloned().collect::<BTreeSet<_>>();
    *lifecycle.state.runtime_profile_status.write().unwrap() = status;
    let admin = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
    let session = SessionId::from_u64(20_274);
    lifecycle
        .state
        .api_session_owners
        .lock()
        .unwrap()
        .insert(session, super::super::super::tests::local_test_session());
    let mut pump = lifecycle.state.webrtc.stage_api_session_pump(session);
    for (watch_id, prefix, expected_key) in [
        (ACTIVE_KEY, ACTIVE_KEY, ACTIVE_KEY),
        (REGISTRY_KEY, REGISTRY_KEY, REGISTRY_KEY),
        ("legacy-active", "", ACTIVE_KEY),
    ] {
        let result = super::super::dispatch(
            &lifecycle.state,
            session,
            &admin,
            proto::StateStoreCommand {
                action: Some(state_store_command::Action::Watch(proto::WatchState {
                    watch_id: watch_id.into(),
                    namespace: NAMESPACE.into(),
                    key_prefix: prefix.into(),
                })),
            },
        )
        .unwrap();
        let control_ok::Result::StateStoreResult(watch) = &result else {
            panic!("expected a runtime profile watch");
        };
        let Some(state_store_result::Result::Watch(snapshot)) = &watch.result else {
            panic!("expected a runtime profile snapshot");
        };
        assert_eq!(snapshot.key_prefix, prefix);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].key, expected_key);
        assert!(response_envelope(result).encoded_len() <= 48 * 1024);
    }
    let mut seen = BTreeSet::new();
    let mut generations = BTreeSet::new();
    let mut max_document = 0;
    let mut max_snapshot = 0;
    for index in 0..8 {
        let key = format!("observed/{index}");
        let control_ok::Result::StateStoreResult(get) = super::super::dispatch(
            &lifecycle.state,
            SessionId::from_u64(20_274),
            &admin,
            proto::StateStoreCommand {
                action: Some(state_store_command::Action::Get(proto::GetState {
                    namespace: NAMESPACE.into(),
                    key: key.clone(),
                })),
            },
        )
        .unwrap() else {
            panic!("expected observed part")
        };
        let Some(state_store_result::Result::Entry(entry)) = get.result else {
            panic!("expected observed part entry")
        };
        assert_eq!(entry.key, key);
        assert_eq!(entry.revision, i64::MAX as u64);
        let value = entry.value.as_ref().unwrap();
        max_document = max_document.max(value.encoded_len());
        assert!(value.encoded_len() <= 16 * 1024);
        let json = super::super::super::peek_layouts::struct_to_json(value.clone());
        generations.insert(json["generation"].as_str().unwrap().to_owned());
        assert_eq!(
            json["definition_revision"].as_str().unwrap(),
            i64::MAX.to_string()
        );
        assert_eq!(json["part_index"].as_f64(), Some(f64::from(index)));
        let cameras = json["cameras"].as_object().unwrap();
        assert_eq!(cameras.len(), if index == 7 { 15 } else { 16 });
        for (id, camera) in cameras {
            assert!(seen.insert(id.clone()));
            assert_eq!(camera["reason"].as_str().unwrap().len(), 96);
        }
        let control_ok::Result::StateStoreResult(watch) = super::super::dispatch(
            &lifecycle.state,
            SessionId::from_u64(20_274),
            &admin,
            proto::StateStoreCommand {
                action: Some(state_store_command::Action::Watch(proto::WatchState {
                    watch_id: format!("status-{index}"),
                    namespace: NAMESPACE.into(),
                    key_prefix: key,
                })),
            },
        )
        .unwrap() else {
            panic!("expected observed part snapshot")
        };
        assert!(
            response_envelope(control_ok::Result::StateStoreResult(watch.clone())).encoded_len()
                <= 48 * 1024
        );
        let Some(state_store_result::Result::Watch(snapshot)) = watch.result else {
            panic!("expected snapshot")
        };
        max_snapshot = max_snapshot.max(snapshot.encoded_len());
        assert!(snapshot.encoded_len() <= 48 * 1024);
        assert_eq!(snapshot.entries, [entry]);
    }
    eprintln!(
        "127-camera Degraded status: max document={max_document}, max watch snapshot={max_snapshot}"
    );
    assert_eq!(seen, expected);
    assert_eq!(generations.len(), 1);
    publish_status_event(&lifecycle.state, 1);
    assert!(pump().is_empty());
    let next_generation = {
        let mut observed_status = lifecycle.state.runtime_profile_status.write().unwrap();
        observed_status.cameras.values_mut().next().unwrap().state =
            RuntimeApplicationState::Applied;
        observed::generation(&observed_status).unwrap()
    };
    publish_status_event(&lifecycle.state, 2);
    let mut keys = BTreeSet::new();
    for bytes in pump() {
        assert!(bytes.len() <= 48 * 1024);
        let message = proto::ControlEnvelope::decode(bytes.as_slice())
            .unwrap()
            .message
            .unwrap();
        let proto::control_envelope::Message::Notification(notification) = message else {
            panic!("notification expected")
        };
        let Some(proto::notification::Event::StateStoreWatchUpdate(update)) = notification.event
        else {
            panic!("watch update expected")
        };
        let entry = update.entry.unwrap();
        assert!(keys.insert(entry.key.clone()));
        if entry.key != ACTIVE_KEY {
            let json = super::super::super::peek_layouts::struct_to_json(entry.value.unwrap());
            assert_eq!(json["generation"].as_str(), Some(next_generation.as_str()));
        }
    }
    let expected_keys: BTreeSet<_> = (0..8).map(|i| format!("observed/{i}")).collect();
    assert_eq!(keys, expected_keys);
}

#[test]
fn observed_parts_cover_configured_and_retained_missing_targets_at_the_union_limit() {
    for extra in [1, 127, 200, 256] {
        let mut status = maximum_degraded_status();
        for index in 0..extra {
            let mut camera = status.cameras.values().next().unwrap().clone();
            camera.camera_id = format!("{}-{index:03}", "m".repeat(252));
            status.cameras.insert(camera.camera_id.clone(), camera);
        }
        let generation = observed::generation(&status).unwrap();
        let mut seen = BTreeSet::new();
        let mut compact_count = 0;
        for index in 0..observed::PART_COUNT {
            let part = observed::part_entry(&status, index, &generation).unwrap();
            assert!(part.value.encoded_len() <= PROFILE_DOCUMENT_BYTES_MAX);
            let value = super::super::super::peek_layouts::struct_to_json(part.value);
            let cameras = value["cameras"].as_object().unwrap();
            assert!(cameras.len() <= 48);
            for (id, payload) in cameras {
                assert!(seen.insert(id.clone()));
                if part.schema.ends_with(".v2") {
                    compact_count += 1;
                    assert_eq!(payload.as_array().unwrap().len(), 4);
                    assert_eq!(payload[0], "degraded");
                    assert_eq!(payload[1], "off");
                    assert_eq!(payload[2].as_f64(), Some(123.0));
                    assert_eq!(
                        payload[3].as_str().unwrap().len(),
                        if cameras.len() > 40 {
                            24
                        } else if cameras.len() > 32 {
                            64
                        } else {
                            96
                        }
                    );
                }
            }
        }
        assert_eq!(seen, status.cameras.keys().cloned().collect());
        if extra >= 127 {
            assert!(compact_count > 0);
        }
    }
}

fn response_envelope(result: control_ok::Result) -> proto::ControlEnvelope {
    proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Response(
            proto::Response {
                request_id: u64::MAX,
                result: Some(proto::response::Result::Ok(proto::Ok {
                    result: Some(result),
                })),
            },
        )),
    }
}

fn status_watch_command() -> proto::StateStoreCommand {
    proto::StateStoreCommand {
        action: Some(state_store_command::Action::Watch(proto::WatchState {
            watch_id: "status-size-qualification".to_owned(),
            namespace: NAMESPACE.to_owned(),
            key_prefix: STATUS_KEY.to_owned(),
        })),
    }
}

#[test]
fn legacy_status_rejects_oversize_get_and_watch_with_typed_error() {
    let lifecycle = Lifecycle::new(false);
    *lifecycle.state.runtime_profile_status.write().unwrap() = maximum_degraded_status();
    let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
    let get = proto::StateStoreCommand {
        action: Some(state_store_command::Action::Get(proto::GetState {
            namespace: NAMESPACE.to_owned(),
            key: STATUS_KEY.to_owned(),
        })),
    };
    for command in [get, status_watch_command()] {
        let error = super::super::dispatch(
            &lifecycle.state,
            SessionId::from_u64(20_251),
            &principal,
            command,
        )
        .unwrap_err();
        let detail = proto::StateStoreError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code(), proto::StateStoreErrorCode::ValueTooLarge);
        assert_eq!(detail.key, STATUS_KEY);
    }
}

pub(super) fn with_applied_base(run: impl FnOnce(&ServerState, &Path, &str)) {
    let lifecycle = Lifecycle::new(false);
    lifecycle.activate("away", 0);
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Main);
    lifecycle.activate("", 1);
    lifecycle.assert_applied(2, None, CameraRecordingMode::Sub);
    run(
        &lifecycle.state,
        &lifecycle.directory.join("config.toml"),
        &lifecycle.source,
    );
}

#[test]
fn repaired_camera_restart_confirms_privacy_against_the_new_observation() {
    let lifecycle = Lifecycle::new(false);
    lifecycle.activate("away", 0);
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Main);
    let state = &lifecycle.state;
    let path = lifecycle.directory.join("config.toml");
    let mut before = crate::server::refresh_runtime_profile_camera_ids(state, &path).unwrap();
    for observation in &mut before.applications {
        observation.state = RuntimeApplicationState::Degraded;
    }
    state
        .privacy
        .replace_profile_overrides(BTreeMap::from([(lifecycle.source.clone(), true)]))
        .unwrap();
    before.apply(state, None).unwrap();
    assert!(
        state
            .privacy
            .decision(&lifecycle.source, chrono::Utc::now())
            .unwrap()
            .0
    );
    let configured = config::load_config(&path)
        .unwrap()
        .camera_configurations()
        .unwrap();
    let camera = crate::cameras::configured_cameras(&configured)
        .into_values()
        .next()
        .unwrap();
    state
        .camera_runtime
        .as_ref()
        .unwrap()
        .restart_camera(camera)
        .unwrap();
    before.confirm_after_start(state, &path).unwrap();
    assert!(
        !state
            .privacy
            .decision(&lifecycle.source, chrono::Utc::now())
            .unwrap()
            .0
    );
    let mut camera = configured.into_values().flatten().next().unwrap();
    camera.username = "subsequent-edit".into();
    config::upsert_camera(&path, &camera).unwrap();
    assert!(before.confirm_after_start(state, &path).is_err());
}

#[test]
fn recovered_camera_confirms_the_current_configuration_before_releasing_privacy() {
    let lifecycle = Lifecycle::new(false);
    lifecycle.activate("away", 0);
    lifecycle.activate("", 1);
    let wait = |condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(6);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "recovery confirmation did not settle"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let state = &lifecycle.state;
    wait(&|| {
        let status = state.runtime_profile_status.read().unwrap();
        status.revision == 2
            && status
                .cameras
                .get(&lifecycle.identity)
                .is_some_and(|a| a.state == RuntimeApplicationState::Applied)
    });
    let hold = || {
        state
            .privacy
            .replace_profile_overrides(BTreeMap::from([(lifecycle.source.clone(), true)]))
            .unwrap();
        let mut status = state.runtime_profile_status.write().unwrap();
        let camera = status.cameras.get_mut(&lifecycle.identity).unwrap();
        camera.state = RuntimeApplicationState::Degraded;
        camera.reason = Some(crate::profiles::PENDING_PRIVACY_CONFIRMATION.into());
    };
    let private = || {
        state
            .privacy
            .decision(&lifecycle.source, chrono::Utc::now())
            .unwrap()
            .0
    };
    hold();
    state.confirm_recovered_profile(2);
    wait(&|| !private() && lifecycle.application().state == RuntimeApplicationState::Applied);
    let path = lifecycle.directory.join("config.toml");
    let mut camera = config::load_cameras(&path)
        .unwrap()
        .into_values()
        .flatten()
        .next()
        .unwrap();
    camera.recorded_audio.default_offset_ms = Some(50);
    config::upsert_camera(&path, &camera).unwrap();
    hold();
    state.confirm_recovered_profile(2);
    wait(&|| !private() && lifecycle.application().state == RuntimeApplicationState::Applied);
}

fn publish_test_live_source(state: &ServerState) -> Option<String> {
    use crate::keeppeek::StreamKind;
    use crate::server::proto_camera_source_session;
    use crate::webrtc::Source;
    use std::net::{IpAddr, Ipv4Addr};

    state.webrtc.live().publish(
        Source {
            camera_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stream: StreamKind::Sub,
        },
        VideoCodec::H264,
        true,
        Instant::now(),
        None,
        bytes::Bytes::from_static(&[0, 0, 0, 1]),
    );
    let camera = state.camera(SOURCE)?;
    proto_camera_source_session(&camera.info, &state.webrtc).map(|source| source.source_session_id)
}

fn bind_privacy_test_sessions(
    state: &ServerState,
) -> (SessionId, SessionId, crate::server::ServerControlHandler) {
    use crate::access::CameraAccess;
    use crate::server::{ServerControlHandler, tests::local_test_session};
    use std::net::IpAddr;

    state.cameras.write().unwrap()[0].groups = vec!["lobby".to_owned()];
    let admin_session = SessionId::from_u64(31_001);
    let group_user_session = SessionId::from_u64(31_002);
    let group_user = state
        .access_manager
        .create_credential("Lobby Viewer", None, AccessRole::User, None, 1_000)
        .unwrap();
    state
        .access_manager
        .set_camera_access(
            group_user.metadata.id,
            group_user.metadata.revision,
            CameraAccess {
                all_cameras: false,
                camera_ids: Vec::new(),
                group_ids: vec!["lobby".to_owned()],
            },
        )
        .unwrap();
    let auth = format!("Bearer {}", group_user.access_key.canonical());
    let remote: IpAddr = "192.0.2.20".parse().unwrap();
    let principal = ApiPrincipal::credential(
        state
            .access_manager
            .authenticate(remote, &[&auth], 1_001, Instant::now())
            .unwrap(),
    );
    let mut user_record = local_test_session();
    user_record.principal = principal;
    let mut owners = state.api_session_owners.lock().unwrap();
    owners.insert(admin_session, local_test_session());
    owners.insert(group_user_session, user_record);
    drop(owners);
    let (_router, router_tx) = crate::runtime::Router::new().unwrap();
    let handler = ServerControlHandler::new(state.clone(), router_tx);
    (admin_session, group_user_session, handler)
}

fn subscribe_test_events(handler: &crate::server::ServerControlHandler, session: SessionId) {
    use crate::webrtc::ControlRequestHandler as _;

    let reply = handler.handle_for_session(
        session,
        proto::Request {
            request_id: 49,
            command: Some(proto::request::Command::SubscribeEvents(
                proto::SubscribeEvents {
                    subscription_id: "privacy-events".to_owned(),
                    attachment_routes: vec![proto::EventAttachmentRoute {
                        attachment_type: "snapshot".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        channel: proto::DataChannelKind::ReliableData as i32,
                    }],
                    ..Default::default()
                },
            )),
        },
    );
    assert!(matches!(
        reply.response.result,
        Some(proto::response::Result::Ok(_))
    ));
}

fn test_subscribe_video(source_session_id: &str) -> proto::SubscribeMedia {
    proto::SubscribeMedia {
        subscription_id: "live-video-privacy".to_owned(),
        kind: proto::MediaKind::Video as i32,
        source_session_id: source_session_id.to_owned(),
        requested_delivery_transport: proto::DeliveryTransport::Rtp as i32,
        video_quality: proto::VideoQuality::Auto as i32,
        ..Default::default()
    }
}

fn sample_privacy_event(event_id: &str, source_session_id: &str) -> proto::Event {
    proto::Event {
        event_id: event_id.to_owned(),
        revision: 1,
        source_id: SOURCE.to_owned(),
        media_kind: Some(proto::MediaKind::Video as i32),
        origin: proto::EventOrigin::Keeppeek as i32,
        source_session_id: Some(source_session_id.to_owned()),
        event_type: "person".to_owned(),
        start_time: Some(crate::server::millis_timestamp(12_345)),
        ..Default::default()
    }
}

fn assert_live_media_blocked(
    state: &ServerState,
    admin_session: SessionId,
    group_user_session: SessionId,
    handler: &crate::server::ServerControlHandler,
    source_session_id: &str,
) {
    use crate::webrtc::ControlRequestHandler as _;

    let subscribe_request = test_subscribe_video(source_session_id);
    let subscribe_envelope = proto::Request {
        request_id: 1,
        command: Some(proto::request::Command::SubscribeMedia(
            subscribe_request.clone(),
        )),
    };
    for session in [admin_session, group_user_session] {
        let principal = state
            .api_session_owners
            .lock()
            .unwrap()
            .get(&session)
            .unwrap()
            .principal
            .clone();
        assert!(
            crate::server::camera_access::authorize_command(state, &principal, &subscribe_envelope)
                .is_ok()
        );
    }
    let Err(media_error) = handler.resolve_media_subscription(&subscribe_request) else {
        panic!("live media subscription must fail while profile privacy is active");
    };
    assert_eq!(media_error.code, proto::ErrorCode::Rejected);
    assert!(media_error.message.contains("privacy is active"));
}

fn assert_privacy_surfaces_blocked(
    state: &ServerState,
    admin_session: SessionId,
    pump: &mut impl FnMut() -> Vec<Vec<u8>>,
    source_session_id: &str,
) {
    use crate::server::{
        camera_control, create_export_job, event_publication, event_subscription, millis_timestamp,
    };

    assert!(state.privacy.media_permit(SOURCE.into()).is_none());
    let event = sample_privacy_event("private-event-attachment", source_session_id);
    event_subscription::publish_images(
        state,
        &event,
        Some(std::sync::Arc::from(b"private-jpeg-bytes".as_slice())),
        &[],
    );
    assert!(pump().is_empty());

    let Err(publication) = event_publication::dispatch(
        state,
        admin_session,
        proto::EventPublicationCommand {
            action: Some(proto::event_publication_command::Action::Start(
                proto::StartEventPublication {
                    publication_id: "svc-pub-1".to_owned(),
                    event: Some(event),
                    attachment_channel: proto::DataChannelKind::ReliableData as i32,
                },
            )),
        },
    ) else {
        panic!("external analysis event publication must fail under profile privacy");
    };
    assert!(publication.message.contains("privacy is active"));

    let export_error = create_export_job(
        state,
        "owner",
        proto::CreateExportJob {
            job_id: "profile-privacy-export".to_owned(),
            source_id: SOURCE.to_owned(),
            stream_id: "main".to_owned(),
            start_time: Some(millis_timestamp(1_000)),
            end_time: Some(millis_timestamp(2_000)),
            ..Default::default()
        },
    )
    .expect_err("export creation must fail under profile privacy");
    assert_eq!(export_error.code, proto::ErrorCode::Rejected);
    assert!(export_error.message.contains("privacy is active"));

    let ptz_error = camera_control::handle_ptz(
        state,
        admin_session,
        proto::PtzCommand {
            source_id: SOURCE.to_owned(),
            action: Some(proto::ptz_command::Action::ListPresets(
                proto::PtzPresetList {},
            )),
        },
    )
    .expect_err("PTZ control must fail under profile privacy");
    assert_eq!(ptz_error.code, proto::ErrorCode::Rejected);
    assert!(ptz_error.message.contains("privacy is active"));
}

#[test]
fn profile_privacy_blocks_live_media_events_services_groups_controls_and_exports() {
    use crate::server::event_subscription;
    use crate::webrtc::ControlRequestHandler as _;

    let mut lifecycle = Lifecycle::with_privacy(false, true);
    let source_session_id =
        publish_test_live_source(&lifecycle.state).expect("Base live video source must advertise");
    let (admin_session, group_user_session, handler) = bind_privacy_test_sessions(&lifecycle.state);
    let mut pump = lifecycle
        .state
        .webrtc
        .stage_api_session_pump(group_user_session);
    subscribe_test_events(&handler, group_user_session);

    // Baseline under Base: live media, permits, and subscribed event delivery all succeed.
    assert!(
        lifecycle
            .state
            .privacy
            .media_permit(SOURCE.into())
            .is_some()
    );
    assert!(
        handler
            .resolve_media_subscription(&test_subscribe_video(&source_session_id))
            .is_ok()
    );
    event_subscription::publish_images(
        &lifecycle.state,
        &sample_privacy_event("base-event-allowed", &source_session_id),
        Some(std::sync::Arc::from(b"base-jpeg-bytes".as_slice())),
        &[],
    );
    assert!(
        !pump().is_empty(),
        "group-scoped event subscription must deliver while privacy is inactive"
    );

    // Activate privacy profile; live media and permits close before async convergence settles.
    lifecycle.activate("away", 0);
    assert_live_media_blocked(
        &lifecycle.state,
        admin_session,
        group_user_session,
        &handler,
        &source_session_id,
    );
    lifecycle.assert_applied(1, Some("away"), CameraRecordingMode::Off);
    assert_privacy_surfaces_blocked(
        &lifecycle.state,
        admin_session,
        &mut pump,
        &source_session_id,
    );

    drop(pump);

    // Restart recorder and confirm live ingress drops and all surfaces remain closed.
    lifecycle.restart();
    assert!(lifecycle.private_before_start);
    assert!(
        publish_test_live_source(&lifecycle.state).is_none(),
        "live video source session must not advertise while restored profile privacy is active"
    );
    let (admin_after, user_after, handler_after) = bind_privacy_test_sessions(&lifecycle.state);
    let mut restarted_pump = lifecycle.state.webrtc.stage_api_session_pump(user_after);
    subscribe_test_events(&handler_after, user_after);
    assert_privacy_surfaces_blocked(
        &lifecycle.state,
        admin_after,
        &mut restarted_pump,
        &source_session_id,
    );

    // Return to Base and confirm live media and subscribed event delivery both resume.
    lifecycle.activate("", 1);
    lifecycle.assert_applied(2, None, CameraRecordingMode::Sub);
    let deadline = Instant::now() + Duration::from_secs(5);
    while lifecycle
        .state
        .privacy
        .media_permit(SOURCE.into())
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "Base restoration did not release profile privacy"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let resumed_source_session = publish_test_live_source(&lifecycle.state)
        .expect("Base restoration must re-advertise live video source");
    assert!(
        handler_after
            .resolve_media_subscription(&test_subscribe_video(&resumed_source_session))
            .is_ok()
    );
    event_subscription::publish_images(
        &lifecycle.state,
        &sample_privacy_event("base-event-resumed", &resumed_source_session),
        Some(std::sync::Arc::from(b"resumed-jpeg-bytes".as_slice())),
        &[],
    );
    assert!(
        !restarted_pump().is_empty(),
        "group-scoped event subscription must resume after Base clears profile privacy"
    );
}
