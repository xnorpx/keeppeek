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
                        privacy_enforced: Some(false),
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
            if status.revision == revision
                && status
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
        let status = self.state.runtime_profile_status.read().unwrap();
        assert_eq!(status.revision, revision);
        assert_eq!(status.active_profile_id.as_deref(), profile);
        assert_eq!(
            status.cameras[&self.identity].state,
            RuntimeApplicationState::Applied
        );
        assert_eq!(status.cameras[&self.identity].policy.recording_mode, mode);
    }

    fn send_frames(&self, stream: &str) {
        // ponytail: Inject encoded media at storage ingress; wire decoding has separate tests.
        let started = Instant::now();
        for offset in [0, 40, 80] {
            self.storage.as_ref().unwrap().handle().ingest_stream(
                RecordingStreamIdentity::new(&self.source, stream, "lifecycle"),
                keyframe(started + Duration::from_millis(offset)),
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

fn keyframe(received_at: Instant) -> RecordingFrame {
    RecordingFrame {
        received_at,
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: vec![
                0, 0, 0, 8, 0x67, 0x42, 0, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]
            .into(),
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
    for key in [ACTIVE_KEY, REGISTRY_KEY] {
        let result = super::super::dispatch(
            &lifecycle.state,
            session,
            &admin,
            proto::StateStoreCommand {
                action: Some(state_store_command::Action::Watch(proto::WatchState {
                    watch_id: key.into(),
                    namespace: NAMESPACE.into(),
                    key_prefix: key.into(),
                })),
            },
        )
        .unwrap();
        assert!(response_envelope(result).encoded_len() <= 48 * 1024);
    }
    let broad = super::super::dispatch(
        &lifecycle.state,
        SessionId::from_u64(20_274),
        &admin,
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Watch(proto::WatchState {
                watch_id: "broad".into(),
                namespace: NAMESPACE.into(),
                key_prefix: String::new(),
            })),
        },
    )
    .expect_err("a broad profile snapshot cannot represent the fleet");
    let detail = proto::StateStoreError::decode(broad.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.code(), proto::StateStoreErrorCode::KeyInvalid);
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
            assert!(generations.contains(json["generation"].as_str().unwrap()));
        }
    }
    let mut expected_keys: BTreeSet<_> = (0..8).map(|i| format!("observed/{i}")).collect();
    expected_keys.insert(ACTIVE_KEY.to_owned());
    assert_eq!(keys, expected_keys);
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
