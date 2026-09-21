use super::dispatch;
use crate::{
    api::proto,
    server::{ApiPrincipal, ControlCommandError, ServerState},
    storage::{StorageConfig, StorageEngine},
};
use proto::recording_policy_command::Action;
use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    engine: Option<StorageEngine>,
    state: ServerState,
    actor: ApiPrincipal,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-control-api-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let engine = StorageEngine::start(StorageConfig {
            medium_term_path: root.clone(),
            long_term_path: root.clone(),
            recording_catalog_path: root.join("catalog.db"),
            event_thumbnail_path: root.join("thumbnails"),
            minimum_free_bytes: 0,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            long_term_max_bytes: 0,
            ..StorageConfig::default()
        });
        let mut state = ServerState::empty();
        engine.handle().configure_camera_recording(
            "camera",
            crate::cameras::CameraRecordingMode::Sub,
            Duration::from_secs(60),
        );
        state.recording_control = Some(engine.handle());
        Self {
            root,
            engine: Some(engine),
            state,
            actor: ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        }
    }
    fn request(&self, action: Action) -> Result<proto::RecordingControlState, ControlCommandError> {
        let result = dispatch(
            &self.state,
            &self.actor,
            proto::RecordingPolicyCommand {
                source_id: "camera".into(),
                action: Some(action),
            },
        )?;
        let proto::ok::Result::RecordingControlState(value) = result else {
            panic!("wrong result")
        };
        Ok(value)
    }
    fn get(&self) -> proto::RecordingControlState {
        self.request(Action::Get(proto::GetRecordingControl {}))
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.take().unwrap().shutdown();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn request(revision: String) -> proto::SetRecordingOverride {
    proto::SetRecordingOverride {
        expected_revision: revision,
        enabled: false,
        source: proto::RecordingOverrideSource::Manual as i32,
        reason: "inspection".into(),
        ttl_ms: 60_000,
    }
}

#[test]
fn recording_controls_require_admin_and_live_backend() {
    let mut fixture = Fixture::new();
    let initial = fixture.get();
    fixture.actor.role = crate::access::AccessRole::User;
    for action in [
        Action::Get(proto::GetRecordingControl {}),
        Action::SetOverride(request(initial.revision.clone())),
        Action::ClearOverride(proto::ClearRecordingOverride {
            expected_revision: initial.revision.clone(),
        }),
    ] {
        assert_eq!(fixture.request(action).unwrap_err()._http_status, 403);
    }
    fixture.actor.role = crate::access::AccessRole::Administrator;
    assert_eq!(fixture.get(), initial);
    fixture.state.recording_control = None;
    assert_eq!(
        fixture
            .request(Action::Get(proto::GetRecordingControl {}))
            .unwrap_err()
            ._http_status,
        503
    );
}

#[test]
fn recording_control_api_preserves_revision_and_configured_off_bound() {
    let fixture = Fixture::new();
    let initial = fixture.get();
    let command = request(initial.revision);
    let paused = fixture
        .request(Action::SetOverride(command.clone()))
        .unwrap();
    assert_eq!(
        paused.effective_mode,
        proto::CameraRecordingMode::Off as i32
    );
    assert_eq!(paused.override_state.unwrap().actor, fixture.actor.id());
    assert_eq!(
        fixture
            .request(Action::SetOverride(command.clone()))
            .unwrap_err()
            ._http_status,
        409
    );
    assert_eq!(
        fixture
            .request(Action::ClearOverride(proto::ClearRecordingOverride {
                expected_revision: command.expected_revision
            }))
            .unwrap_err()
            ._http_status,
        409
    );
    let resumed = fixture
        .request(Action::ClearOverride(proto::ClearRecordingOverride {
            expected_revision: paused.revision,
        }))
        .unwrap();
    assert_eq!(
        resumed.effective_mode,
        proto::CameraRecordingMode::Sub as i32
    );
    fixture
        .state
        .recording_control
        .as_ref()
        .unwrap()
        .configure_camera_recording(
            "camera",
            crate::cameras::CameraRecordingMode::Off,
            Duration::from_secs(60),
        );
    let enable = proto::SetRecordingOverride {
        enabled: true,
        ..request(fixture.get().revision)
    };
    assert_eq!(
        fixture
            .request(Action::SetOverride(enable))
            .unwrap_err()
            ._http_status,
        409
    );
}

#[test]
fn invalid_control_input_preserves_state_and_privacy_cannot_be_bypassed() {
    let fixture = Fixture::new();
    let initial = fixture.get();
    let base = request(initial.revision.clone());
    for invalid in [
        proto::SetRecordingOverride {
            expected_revision: "bad".into(),
            ..base.clone()
        },
        proto::SetRecordingOverride {
            source: 0,
            ..base.clone()
        },
        proto::SetRecordingOverride {
            source: 99,
            ..base.clone()
        },
        proto::SetRecordingOverride {
            reason: " ".into(),
            ..base.clone()
        },
        proto::SetRecordingOverride {
            reason: "é".repeat(129),
            ..base.clone()
        },
        proto::SetRecordingOverride {
            ttl_ms: 0,
            ..base.clone()
        },
        proto::SetRecordingOverride {
            ttl_ms: 86_400_001,
            ..base
        },
    ] {
        assert_eq!(
            fixture
                .request(Action::SetOverride(invalid))
                .unwrap_err()
                .code,
            proto::ErrorCode::InvalidRequest
        );
        assert_eq!(fixture.get(), initial);
    }
    for privacy in [Some(true), None] {
        fixture
            .state
            .recording_control
            .as_ref()
            .unwrap()
            .set_recording_privacy("camera", privacy)
            .unwrap();
        let before = fixture.get();
        let enable = proto::SetRecordingOverride {
            enabled: true,
            ..request(before.revision.clone())
        };
        assert_eq!(
            fixture
                .request(Action::SetOverride(enable))
                .unwrap_err()
                ._http_status,
            409
        );
        assert_eq!(fixture.get(), before);
    }
}

#[test]
fn recording_control_capability_requires_backend_and_revisions_are_source_scoped() {
    let mut fixture = Fixture::new();
    assert!(
        crate::server::server_capabilities(&fixture.state, &[])
            .capability_ids
            .contains(&super::CAPABILITY.to_owned())
    );
    let storage = fixture.state.recording_control.as_ref().unwrap();
    storage.configure_camera_recording(
        "other",
        crate::cameras::CameraRecordingMode::Sub,
        Duration::from_secs(60),
    );
    let other = storage.recording_control("other").unwrap();
    let revision = format!("{:032x}:{}", other.revision.epoch, other.revision.sequence);
    assert_eq!(
        fixture
            .request(Action::SetOverride(request(revision)))
            .unwrap_err()
            ._http_status,
        409
    );
    fixture.state.recording_control = None;
    assert!(
        !crate::server::server_capabilities(&fixture.state, &[])
            .capability_ids
            .contains(&super::CAPABILITY.to_owned())
    );
}

#[test]
fn recording_control_builder_registers_offline_configured_sources() {
    let fixture = Fixture::new();
    let camera: crate::cameras::CameraConfig =
        toml::from_str("ip = '192.0.2.8'\nname = 'offline'\nrecording_mode = 'off'").unwrap();
    let cameras = std::collections::HashMap::from([("test".into(), vec![camera])]);
    let state = ServerState::empty()
        .with_recording_control(fixture.engine.as_ref().unwrap().handle(), &cameras);
    assert!(state.cameras.read().unwrap().is_empty());
    let snapshot = state
        .recording_control
        .as_ref()
        .unwrap()
        .recording_control("192.0.2.8")
        .unwrap();
    assert_eq!(snapshot.mode, crate::cameras::CameraRecordingMode::Off);
    assert_eq!(
        snapshot.reason,
        crate::storage::recording_control::Reason::ConfiguredDisabled
    );
}

#[test]
fn saved_offline_camera_mode_immediately_bounds_recording_admission() {
    let mut fixture = Fixture::new();
    let path = fixture.root.join("config.toml");
    crate::config::write_private_file(
        &path,
        br#"
        [camera_defaults]
        username = "operator"
        password = "synthetic-password"
        [cameras.offline]
        ip = "192.0.2.8"
        recording_mode = "sub"
    "#,
    )
    .unwrap();
    fixture.state.camera_config_path = Some(path);
    let storage = fixture.state.recording_control.as_ref().unwrap();
    storage.configure_camera_recording(
        "192.0.2.8",
        crate::cameras::CameraRecordingMode::Sub,
        Duration::from_secs(60),
    );
    let before = storage.recording_control("192.0.2.8").unwrap();
    let (mut router, sender) = crate::runtime::Router::new().unwrap();
    let worker =
        std::thread::spawn(move || router.wait_and_drain(Some(Duration::from_secs(2))).unwrap());
    crate::server::save_camera_settings(
        crate::server::CameraSettingsUpdate {
            recording_mode: Some(crate::cameras::CameraRecordingMode::Off),
            ..Default::default()
        },
        &sender,
        &fixture.state,
        "192.0.2.8",
    )
    .unwrap();
    worker.join().unwrap();
    let after = storage.recording_control("192.0.2.8").unwrap();
    assert_eq!(after.mode, crate::cameras::CameraRecordingMode::Off);
    assert_eq!(
        after.reason,
        crate::storage::recording_control::Reason::ConfiguredDisabled
    );
    assert_ne!(after.revision, before.revision);
}

#[test]
fn configuration_plan_bounds_offline_recording_before_runtime_activation() {
    use proto::configuration_command::Action as ConfigAction;
    use proto::configuration_result::Result as ConfigResult;
    let fixture = Fixture::new();
    let path = fixture.root.join("config.toml");
    crate::config::write_private_file(
        &path,
        br#"
        [camera_defaults]
        username = "operator"
        password = "synthetic-password"
        [cameras.offline]
        ip = "192.0.2.8"
        recording_mode = "sub"
    "#,
    )
    .unwrap();
    let state = fixture.state.clone().with_camera_config_path(path);
    let storage = state.recording_control.as_ref().unwrap();
    storage.configure_camera_recording(
        "192.0.2.8",
        crate::cameras::CameraRecordingMode::Sub,
        Duration::from_secs(60),
    );
    let snapshot = configuration_request(
        &state,
        ConfigAction::Get(proto::GetConfigurationSnapshot::default()),
    );
    let ConfigResult::Snapshot(snapshot) = snapshot else {
        panic!("expected snapshot")
    };
    let revision = snapshot.configuration_revision;
    let plan = configuration_request(&state, ConfigAction::Plan(off_plan(revision.clone())));
    let ConfigResult::Plan(plan) = plan else {
        panic!("expected plan")
    };
    assert!(plan.valid);
    let applied = configuration_request(
        &state,
        ConfigAction::Apply(proto::ApplyConfigurationPlan {
            expected_configuration_revision: revision,
            plan_id: plan.plan_id,
        }),
    );
    let ConfigResult::Applied(applied) = applied else {
        panic!("expected applied")
    };
    assert!(applied.configuration_committed);
    assert_eq!(
        applied.activations[0].status,
        proto::ConfigurationActivationStatus::RestartRequired as i32
    );
    let snapshot = storage.recording_control("192.0.2.8").unwrap();
    assert_eq!(snapshot.mode, crate::cameras::CameraRecordingMode::Off);
}

fn configuration_request(
    state: &ServerState,
    action: proto::configuration_command::Action,
) -> proto::configuration_result::Result {
    let result = crate::server::configuration::dispatch(
        state,
        proto::ConfigurationCommand {
            action: Some(action),
        },
    )
    .unwrap();
    let proto::ok::Result::ConfigurationResult(result) = result else {
        panic!("expected configuration result")
    };
    result.result.unwrap()
}

fn off_plan(revision: String) -> proto::PlanConfigurationChange {
    proto::PlanConfigurationChange {
        expected_configuration_revision: revision,
        targets: Some(proto::ConfigurationTargetSelector {
            selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                proto::CameraIdList {
                    camera_ids: vec!["192.0.2.8".into()],
                },
            )),
        }),
        change: Some(proto::ConfigurationChange {
            change: Some(proto::configuration_change::Change::Patch(
                proto::CameraConfigurationPatch {
                    recording_mode: Some(proto::OptionalCameraRecordingModeUpdate {
                        value: Some(proto::optional_camera_recording_mode_update::Value::Set(
                            proto::CameraRecordingMode::Off as i32,
                        )),
                    }),
                    ..Default::default()
                },
            )),
        }),
    }
}
