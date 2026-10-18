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
fn runtime_profile_full_degraded_status_fits_get_and_watch_transport() {
    let lifecycle = Lifecycle::new(false);
    let status = maximum_degraded_status();
    let entry = status_entry(status.clone()).unwrap();
    *lifecycle.state.runtime_profile_status.write().unwrap() = status;
    let principal = ApiPrincipal::local(std::net::Ipv4Addr::LOCALHOST.into());
    let get = super::super::dispatch(
        &lifecycle.state,
        SessionId::from_u64(20_251),
        &principal,
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: STATUS_KEY.to_owned(),
            })),
        },
    )
    .unwrap();
    let get_bytes = response_envelope(get).encoded_len();
    let snapshot = proto::StateWatchSnapshot {
        watch_id: "status-size-qualification".to_owned(),
        namespace: NAMESPACE.to_owned(),
        key_prefix: STATUS_KEY.to_owned(),
        snapshot_revision: entry.revision,
        entries: vec![proto_entry(&entry)],
    };
    let snapshot_bytes = snapshot.encoded_len();
    let watch_bytes = response_envelope(control_ok::Result::StateStoreResult(
        proto::StateStoreResult {
            result: Some(state_store_result::Result::Watch(snapshot)),
        },
    ))
    .encoded_len();
    let watch = super::super::dispatch(
        &lifecycle.state,
        SessionId::from_u64(20_252),
        &principal,
        status_watch_command(),
    );
    eprintln!(
        "127-target Degraded status: value={} Get={get_bytes} snapshot={snapshot_bytes} watch={watch_bytes}; watch accepted={}",
        entry.value.encoded_len(),
        watch.is_ok()
    );
    assert!(
        get_bytes <= MAX_VALUE_BYTES,
        "full Get exceeds 64 KiB: {get_bytes}"
    );
    assert!(
        watch_bytes <= MAX_VALUE_BYTES,
        "full watch exceeds 64 KiB: {watch_bytes}"
    );
    assert!(watch.is_ok(), "full Degraded status watch must be admitted");
}
