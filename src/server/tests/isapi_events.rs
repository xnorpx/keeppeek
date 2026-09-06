use super::*;

mod capabilities;

fn timeline() -> TimelineEvent {
    TimelineEvent {
        id: "isapi-motion".to_owned(),
        revision: 2,
        camera_id: "127.0.0.1".to_owned(),
        stream: None,
        source: EventSource::Camera,
        kind: "motion".to_owned(),
        start_time_ms: 1000,
        end_time_ms: Some(2000),
        confidence: None,
        bbox: None,
        bbox_attachment_id: None,
        zone: None,
        text: None,
        payload: serde_json::json!({"protocol":"isapi"}).as_object().cloned(),
        attachments: Vec::new(),
        canonical_attachment_id: None,
        icon_key: "motion".to_owned(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

#[test]
fn isapi_native_event_routes_to_live_subscribers_without_relabeling_its_origin() {
    let state = media_test_state();
    for camera in state.cameras.write().unwrap().iter_mut() {
        camera.info.is_reolink = false;
        camera.info.capabilities.events = true;
    }
    state.webrtc.live().publish(
        crate::webrtc::Source {
            camera_ip: Ipv4Addr::LOCALHOST.into(),
            stream: StreamKind::Sub,
        },
        crate::storage::VideoCodec::H264,
        true,
        Instant::now(),
        None,
        bytes::Bytes::from_static(&[0, 0, 0, 1]),
    );
    let session_id = SessionId::from_u64(17);
    state
        .event_subscriptions
        .subscribe(
            &state,
            session_id,
            proto::SubscribeEvents {
                subscription_id: "native-motion".to_owned(),
                source_ids: vec!["127.0.0.1".to_owned()],
                event_types: vec!["motion".to_owned()],
                ..Default::default()
            },
        )
        .unwrap();
    let timeline = timeline();
    let message = native_events::message(&timeline, Some("camera:127.0.0.1:0".to_owned()), false);
    assert_eq!(message.origin, proto::EventOrigin::Camera as i32);
    assert_eq!(message.event_type, "motion");
    assert_eq!(message.revision, 2);
    assert_eq!(message.end_time, Some(millis_timestamp(2000)));
    assert_eq!(
        message.image_availability,
        proto::EventImageAvailability::None as i32
    );
    state.publish_camera_event(&timeline);
    let metrics = state.event_subscriptions.metrics_snapshot();
    assert_eq!(metrics.deliveries, 1);
    assert_eq!(metrics.sheds, 1);
    assert_eq!(metrics.active, 0);
}

#[test]
fn isapi_capabilities_keep_camera_snapshots_optional() {
    let state = media_test_state();
    let mut camera = state.camera_entries().remove(0).info;
    camera.is_reolink = false;
    camera.capabilities.events = true;
    let types = native_events::event_types(&camera);
    assert!(types.iter().any(|kind| kind.event_type == "motion"));
    assert!(types.iter().all(|kind| {
        kind.attachments
            .iter()
            .all(|attachment| attachment.minimum_count == 0)
    }));
}

#[test]
fn generic_event_capabilities_and_subscriptions_use_observed_native_kinds() {
    let state = media_test_state();
    let mut camera = state.camera_entries().remove(0).info;
    camera.is_reolink = false;
    camera.capabilities.events = false;
    state.cameras.write().unwrap()[0].info = camera.clone();
    state
        .health
        .events
        .record_kind("127.0.0.1".parse().unwrap(), "digital_input");
    state.webrtc.live().publish(
        crate::webrtc::Source {
            camera_ip: Ipv4Addr::LOCALHOST.into(),
            stream: StreamKind::Sub,
        },
        crate::storage::VideoCodec::H264,
        true,
        Instant::now(),
        None,
        bytes::Bytes::from_static(&[0, 0, 0, 1]),
    );
    let handler = test_control_handler(state.clone());
    let capabilities = handler
        .initial_capabilities(SessionId::from_u64(11))
        .unwrap();
    let native = capabilities
        .source_sessions
        .iter()
        .find(|source| source.source_id == camera.id)
        .unwrap();
    assert!(
        native
            .event_types
            .iter()
            .any(|event| event.event_type == "digital_input")
    );
    assert!(
        !native
            .event_types
            .iter()
            .any(|event| event.event_type == "loitering")
    );
    assert!(
        capabilities.cameras[0]
            .device_capabilities
            .as_ref()
            .unwrap()
            .events
    );
    assert!(
        state
            .event_subscriptions
            .subscribe(
                &state,
                SessionId::from_u64(11),
                proto::SubscribeEvents {
                    subscription_id: "generic-input".to_owned(),
                    source_ids: vec![camera.id],
                    event_types: vec!["digital_input".to_owned()],
                    ..Default::default()
                }
            )
            .is_ok()
    );
}

#[test]
fn native_event_capabilities_do_not_invent_plain_rtsp_event_types() {
    let state = media_test_state();
    let mut camera = state.camera_entries().remove(0).info;
    camera.is_reolink = false;
    camera.capabilities.events = false;
    camera.capabilities.analytics = false;
    assert!(native_events::event_types(&camera).is_empty());
    assert!(native_events::reported_types(&camera, &state.health.events).is_empty());
}

#[test]
fn native_event_capabilities_preserve_observed_isapi_multi_image_support() {
    let state = media_test_state();
    let mut camera = state.camera_entries().remove(0).info;
    camera.is_reolink = false;
    camera.capabilities.events = true;
    state
        .health
        .events
        .record_kind("127.0.0.1".parse().unwrap(), "license_plate");
    let types = native_events::reported_types(&camera, &state.health.events);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].event_type, "license_plate");
    assert_eq!(types[0].attachments[0].minimum_count, 0);
    assert_eq!(types[0].attachments[0].maximum_count, 16);
}

#[test]
fn native_event_capabilities_honor_explicit_disabled_policy() {
    let state = media_test_state();
    let mut entry = state.camera_entries().remove(0);
    entry.configuration.events.mode = crate::cameras::events::EventMode::Disabled;
    entry.info.capabilities.events = true;
    entry.info.capabilities.analytics = true;
    state
        .health
        .events
        .record_kind(entry.configuration.ip, "person");
    state.health.events.configure(
        entry.configuration.ip,
        entry.configuration.events.clone(),
        false,
        Shutdown::new(),
    );
    let info = state.camera_info(&entry);
    assert!(!info.capabilities.events);
    assert!(!info.capabilities.analytics);
    assert!(native_events::reported_types(&info, &state.health.events).is_empty());
}

#[test]
fn camera_settings_update_keeps_saved_native_event_policy() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let directory =
        std::env::temp_dir().join(format!("keeppeek-event-policy-{}", uuid::Uuid::new_v4()));
    let path = directory.join("config.toml");
    crate::config::write_private_file(
        &path,
        format!(
            r#"[cameras.front]
ip="127.0.0.1"
username="test"
password="test"
onvif_port={}
main_rtsp_url="rtsp://{}/main"
[cameras.front.events]
mode="rtsp-metadata"
source_tokens=["source-2"]
snapshots=false
"#,
            fake.address().port(),
            fake.address()
        )
        .as_bytes(),
    )
    .unwrap();
    let shutdown = Shutdown::new();
    let recorder = crate::keeppeek::KeepPeekLoop::new(shutdown.clone(), None);
    let state = ServerState::empty()
        .with_camera_config_path(path.clone())
        .with_camera_runtime(recorder.control());
    let recording = std::thread::spawn(move || recorder.run());
    let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
    let response =
        std::thread::spawn(move || router.wait_and_drain(Some(Duration::from_secs(3))).unwrap());
    let result = save_camera_settings(
        CameraSettingsUpdate {
            display_name: Some(Some("Renamed".to_owned())),
            ..Default::default()
        },
        &router_tx,
        &state,
        "127.0.0.1",
    )
    .unwrap();
    response.join().unwrap();
    let saved = crate::config::load_cameras(&path)
        .unwrap()
        .into_values()
        .flatten()
        .next()
        .unwrap();
    assert_eq!(
        saved.events.mode,
        crate::cameras::events::EventMode::RtspMetadata
    );
    assert!(!result.restart_required);
    assert_eq!(
        state.camera("127.0.0.1").unwrap().configuration.events,
        saved.events
    );
    shutdown.cancel();
    recording.join().unwrap();
    assert!(state.camera_metadata.wait_for_idle(Duration::from_secs(10)));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn hikvision_motion_toggle_uses_shared_control_and_preserves_detection_layout() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!(
        "ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\nmain_rtsp_url='rtsp://127.0.0.1/Streaming/Channels/101'\n",
        fake.address().port()
    )).unwrap();
    let mut cameras = crate::cameras::configured_cameras(&HashMap::from([(
        "test".to_owned(),
        vec![config.clone()],
    )]));
    let camera = cameras.remove(&config.ip).unwrap();
    let entry = camera_entry(&config, Some(&camera));
    let state = ServerState::empty();
    state.upsert_camera(entry.clone());
    let initial = motion_detection_status(&entry);
    assert!(initial.supported && initial.controllable);
    assert_eq!(initial.enabled, Some(true));
    for enabled in [false, true] {
        let actual = set_camera_motion(&state, "127.0.0.1", enabled).unwrap();
        assert_eq!(actual.enabled, Some(enabled));
        assert!(actual.error.is_none());
        let xml = String::from_utf8(
            fake.resource("/ISAPI/System/Video/inputs/channels/1/motionDetection")
                .unwrap(),
        )
        .unwrap();
        assert!(xml.contains("<regionName>retained</regionName>"));
        assert!(xml.contains("<sensitivityLevel>60</sensitivityLevel>"));
    }
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|request| request.authenticated() && request.method() == "PUT")
            .count(),
        2
    );
}

#[test]
fn hikvision_ptz_uses_shared_ownership_presets_and_disconnect_stop() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let owner = SessionId::from_u64(901);
    let other = SessionId::from_u64(902);
    let command = |action| proto::PtzCommand {
        source_id: "127.0.0.1".to_owned(),
        action: Some(action),
    };
    let movement = || {
        proto::ptz_command::Action::Continuous(proto::PtzContinuous {
            pan: 0.5,
            tilt: -0.25,
            zoom: 0.0,
        })
    };
    let ptz = handler.initial_capabilities(owner).unwrap().cameras[0]
        .ptz
        .unwrap();
    assert!(ptz.supported && ptz.continuous && ptz.presets);
    assert!(handler.handle_ptz(owner, command(movement())).is_ok());
    assert!(handler.handle_ptz(other, command(movement())).is_err());
    let xml = String::from_utf8(
        fake.resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .unwrap(),
    )
    .unwrap();
    assert!(xml.contains("<pan>50</pan>"));
    assert!(xml.contains("<tilt>-25</tilt>"));
    let presets = handler
        .handle_ptz(
            owner,
            command(proto::ptz_command::Action::ListPresets(
                proto::PtzPresetList {},
            )),
        )
        .unwrap();
    assert!(matches!(presets, control_ok::Result::PtzResult(_)));
    handler.session_closed(owner);
    let xml = String::from_utf8(
        fake.resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .unwrap(),
    )
    .unwrap();
    assert!(xml.contains("<pan>0</pan>"));
    assert!(xml.contains("<tilt>0</tilt>"));
    assert!(state.ptz_owners.lock().unwrap().is_empty());
}

#[test]
fn hikvision_read_only_probes_report_device_control_capabilities() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    assert!(!camera_control::ptz_capability(&state.camera("127.0.0.1").unwrap()).supported);
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let capabilities = handler
        .initial_capabilities(SessionId::from_u64(951))
        .unwrap();
    let camera = &capabilities.cameras[0];
    assert_eq!(camera.model.as_deref(), Some("FAKE-HIKVISION"));
    let device = camera.device_capabilities.as_ref().unwrap();
    assert!(device.events && device.audio && device.two_way_audio);
    let ptz = camera.ptz.as_ref().unwrap();
    assert!(ptz.supported && ptz.continuous && ptz.zoom && ptz.presets);
    assert!(!ptz.relative);
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.method() == "GET")
    );
    fake.set_resource("/ISAPI/PTZCtrl/channels/1/capabilities", "<PTZChanelCap><maxPresetNum>0</maxPresetNum><homePostionSupport>false</homePostionSupport></PTZChanelCap>").unwrap();
    fake.set_resource("/ISAPI/System/TwoWayAudio/channels/1", "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType><lineOutForbidden>true</lineOutForbidden></TwoWayAudioChannel>").unwrap();
    state.probe_hikvision_capabilities(config.ip);
    let capabilities = handler
        .initial_capabilities(SessionId::from_u64(951))
        .unwrap();
    assert!(
        !capabilities.cameras[0]
            .device_capabilities
            .as_ref()
            .unwrap()
            .two_way_audio
    );
    assert!(!capabilities.cameras[0].ptz.as_ref().unwrap().supported);
}

#[test]
fn hikvision_capability_results_do_not_cross_camera_replacements() {
    let config: CameraConfig = toml::from_str(
        "ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\n",
    )
    .unwrap();
    let state = ServerState::empty();
    let previous = camera_entry(&config, None);
    state.upsert_camera(previous.clone());
    let mut replacement = camera_entry(&config, None);
    replacement.info.capabilities.ptz = true;
    replacement.info.capabilities.two_way_audio = true;
    state.upsert_camera(replacement);
    state.apply_hikvision_capabilities(&previous, 1, camera_control::Report::default());
    let current = state.camera("127.0.0.1").unwrap();
    assert!(current.info.capabilities.ptz);
    assert!(current.info.capabilities.two_way_audio);
    assert!(current.hikvision.is_none());
}

#[test]
fn hikvision_ptz_respects_reported_axis_ranges_and_rejects_missing_axes() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    fake.set_resource("/ISAPI/PTZCtrl/channels/1/capabilities", "<PTZChanelCap><ContinuousPanTiltSpace><XRange><Min>-50</Min><Max>50</Max></XRange><YRange><Min>-20</Min><Max>30</Max></YRange></ContinuousPanTiltSpace><homePostionSupport>false</homePostionSupport><maxPresetNum>0</maxPresetNum></PTZChanelCap>").unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let camera = state.camera("127.0.0.1").unwrap();
    assert!(!camera_control::ptz_capability(&camera).zoom);
    let movement = camera_control::movement(
        &camera,
        &proto::PtzContinuous {
            pan: 0.5,
            tilt: -0.5,
            zoom: 0.0,
        },
    )
    .unwrap();
    movement.send(&camera).unwrap();
    let body = String::from_utf8(
        fake.resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .unwrap(),
    )
    .unwrap();
    assert!(body.contains("<pan>25</pan>"));
    assert!(body.contains("<tilt>-10</tilt>"));
    let requests = fake.requests().len();
    assert!(
        camera_control::movement(
            &camera,
            &proto::PtzContinuous {
                pan: 0.0,
                tilt: 0.0,
                zoom: 0.5
            }
        )
        .is_err()
    );
    assert!(camera_control::presets(&camera).is_err());
    assert_eq!(fake.requests().len(), requests);
    camera_control::stop(&camera).unwrap();
}

#[test]
fn hikvision_ptz_stops_after_an_uncertain_movement_acknowledgement() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let owner = SessionId::from_u64(960);
    let command = || proto::PtzCommand {
        source_id: "127.0.0.1".to_owned(),
        action: Some(proto::ptz_command::Action::Continuous(
            proto::PtzContinuous {
                pan: 0.5,
                tilt: 0.0,
                zoom: 0.0,
            },
        )),
    };
    handler.handle_ptz(owner, command()).unwrap();
    fake.enqueue(test_hikvision::Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: 100\r\nConnection: close\r\n\r\n<".to_vec())).unwrap();
    assert!(handler.handle_ptz(owner, command()).is_err());
    handler.session_closed(owner);
    let body = String::from_utf8(
        fake.resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .unwrap(),
    )
    .unwrap();
    assert!(body.contains("<pan>0</pan>"));
    assert!(state.ptz_owners.lock().unwrap().is_empty());
}

#[test]
fn hikvision_ptz_disconnect_stops_the_original_control_target_after_replacement() {
    let original = test_hikvision::FakeHikvision::builder().start().unwrap();
    let replacement = test_hikvision::FakeHikvision::builder().start().unwrap();
    let mut config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", original.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let owner = SessionId::from_u64(961);
    handler
        .handle_ptz(
            owner,
            proto::PtzCommand {
                source_id: "127.0.0.1".to_owned(),
                action: Some(proto::ptz_command::Action::Continuous(
                    proto::PtzContinuous {
                        pan: 0.5,
                        tilt: 0.0,
                        zoom: 0.0,
                    },
                )),
            },
        )
        .unwrap();
    config.http_port = Some(replacement.address().port());
    state.upsert_camera(camera_entry(&config, None));
    handler.session_closed(owner);
    let body = String::from_utf8(
        original
            .resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .unwrap(),
    )
    .unwrap();
    assert!(body.contains("<pan>0</pan>"));
    assert!(replacement.requests().is_empty());
}

#[test]
fn hikvision_failed_refresh_preserves_verified_capabilities_and_stop() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let owner = SessionId::from_u64(970);
    handler
        .handle_ptz(
            owner,
            proto::PtzCommand {
                source_id: "127.0.0.1".to_owned(),
                action: Some(proto::ptz_command::Action::Continuous(
                    proto::PtzContinuous {
                        pan: 0.5,
                        tilt: 0.0,
                        zoom: 0.0,
                    },
                )),
            },
        )
        .unwrap();
    for resource in [
        "/ISAPI/PTZCtrl/channels/1/capabilities",
        "/ISAPI/System/TwoWayAudio/channels",
    ] {
        fake.set_resource(
            resource,
            "<ResponseStatus><statusCode>2</statusCode></ResponseStatus>",
        )
        .unwrap();
    }
    state.probe_hikvision_capabilities(config.ip);
    let camera = state.camera("127.0.0.1").unwrap();
    assert!(camera.info.capabilities.ptz);
    assert!(camera.info.capabilities.two_way_audio);
    handler
        .handle_ptz(
            owner,
            proto::PtzCommand {
                source_id: camera.info.id,
                action: Some(proto::ptz_command::Action::Stop(proto::PtzStop {})),
            },
        )
        .unwrap();
    assert!(state.ptz_owners.lock().unwrap().is_empty());
}

#[test]
fn hikvision_runtime_camera_activation_queues_capability_rediscovery() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let mut config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\nonvif_port={}\n", fake.address().port(), fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.activate_camera_entry(camera_entry(&config, None));
    assert!(state.camera_metadata.wait_for_idle(Duration::from_secs(10)));
    assert!(camera_control::ptz_capability(&state.camera("127.0.0.1").unwrap()).supported);
    config.display_name = Some("Renamed camera".to_owned());
    state.activate_camera_entry(camera_entry(&config, None));
    assert!(state.camera_metadata.wait_for_idle(Duration::from_secs(10)));
    let camera = state.camera("127.0.0.1").unwrap();
    assert!(camera_control::ptz_capability(&camera).supported);
    assert!(camera.info.capabilities.two_way_audio);
    assert_eq!(camera.info.name.as_deref(), Some("Renamed camera"));
}

#[test]
fn camera_motion_errors_never_echo_camera_response_secrets() {
    let secret = "private-motion-test-password";
    let response = test_hikvision::Reply::http(
        200,
        "application/json",
        format!("malformed login: {secret}"),
    );
    let fake = test_hikvision::FakeHikvision::builder()
        .replies([response.clone(), response])
        .start()
        .unwrap();
    let config: CameraConfig = toml::from_str(&format!(
        "ip='127.0.0.1'\nbackend='reo-proto'\nusername='test'\npassword='{secret}'\nhttp_port={}\n",
        fake.address().port()
    ))
    .unwrap();
    let state = ServerState::empty();
    let camera = camera_entry(&config, None);
    state.upsert_camera(camera.clone());
    let status = camera_control::status(&camera);
    assert!(status.error.is_some());
    assert!(status.enabled.is_none());
    assert!(!serde_json::to_string(&status).unwrap().contains(secret));
    let error = camera_control::set_motion(&state, "127.0.0.1", false).unwrap_err();
    assert!(!error.message.contains(secret));
}

#[test]
fn hikvision_ptz_reboot_required_stop_keeps_cleanup_responsibility() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    let owner = SessionId::from_u64(980);
    handler
        .handle_ptz(
            owner,
            proto::PtzCommand {
                source_id: "127.0.0.1".to_owned(),
                action: Some(proto::ptz_command::Action::Continuous(
                    proto::PtzContinuous {
                        pan: 0.5,
                        tilt: 0.0,
                        zoom: 0.0,
                    },
                )),
            },
        )
        .unwrap();
    fake.enqueue(test_hikvision::Reply::http(200, "application/xml", "<ResponseStatus><statusCode>7</statusCode><subStatusCode>rebootRequired</subStatusCode></ResponseStatus>")).unwrap();
    assert!(
        handler
            .handle_ptz(
                owner,
                proto::PtzCommand {
                    source_id: "127.0.0.1".to_owned(),
                    action: Some(proto::ptz_command::Action::Stop(proto::PtzStop {}))
                }
            )
            .is_err()
    );
    assert_eq!(state.ptz_owners.lock().unwrap().len(), 1);
    handler.session_closed(owner);
    assert!(state.ptz_owners.lock().unwrap().is_empty());
}

#[test]
fn hikvision_ptz_uncertain_preset_acknowledgement_triggers_a_safety_stop() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    state.upsert_camera(camera_entry(&config, None));
    state.probe_hikvision_capabilities(config.ip);
    let handler = test_control_handler(state.clone());
    fake.enqueue(test_hikvision::Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: 100\r\nConnection: close\r\n\r\n<".to_vec())).unwrap();
    assert!(
        handler
            .handle_ptz(
                SessionId::from_u64(981),
                proto::PtzCommand {
                    source_id: "127.0.0.1".to_owned(),
                    action: Some(proto::ptz_command::Action::GotoPreset(
                        proto::PtzPresetGoto { preset_id: 1 }
                    ))
                }
            )
            .is_err()
    );
    let body = String::from_utf8(
        fake.resource("/ISAPI/PTZCtrl/channels/1/continuous")
            .expect("uncertain preset must trigger stop"),
    )
    .unwrap();
    assert!(body.contains("<pan>0</pan>"));
    assert!(state.ptz_owners.lock().unwrap().is_empty());
}

#[test]
fn hikvision_explicit_microphone_loss_preserves_only_independent_audio_evidence() {
    let fake = test_hikvision::FakeHikvision::builder().start().unwrap();
    let config: CameraConfig = toml::from_str(&format!("ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\nhttp_port={}\n", fake.address().port())).unwrap();
    let state = ServerState::empty();
    for independent in [false, true] {
        let mut entry = camera_entry(&config, None);
        entry.info.capabilities.audio = independent;
        state.upsert_camera(entry);
        fake.set_resource("/ISAPI/System/TwoWayAudio/channels/1", "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType></TwoWayAudioChannel>").unwrap();
        state.probe_hikvision_capabilities(config.ip);
        assert!(state.camera("127.0.0.1").unwrap().info.capabilities.audio);
        fake.set_resource("/ISAPI/System/TwoWayAudio/channels/1", "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType><micInForbidden>true</micInForbidden></TwoWayAudioChannel>").unwrap();
        state.probe_hikvision_capabilities(config.ip);
        assert_eq!(
            state.camera("127.0.0.1").unwrap().info.capabilities.audio,
            independent
        );
    }
}

#[test]
fn hikvision_configured_route_does_not_invent_device_capabilities() {
    let config: CameraConfig = toml::from_str(
        "ip='127.0.0.1'\nmanufacturer='Hikvision'\nusername='test'\npassword='test'\n",
    )
    .unwrap();
    let mut cameras = crate::cameras::configured_cameras(&HashMap::from([(
        "test".to_owned(),
        vec![config.clone()],
    )]));
    let camera = cameras.remove(&config.ip).unwrap();
    let entry = camera_entry(&config, Some(&camera));
    assert!(camera_control::available(&entry));
    assert!(!entry.info.capabilities.events);
    assert!(!entry.info.capabilities.audio);
    assert!(!entry.info.capabilities.ptz);
    assert!(!entry.info.capabilities.two_way_audio);
}
