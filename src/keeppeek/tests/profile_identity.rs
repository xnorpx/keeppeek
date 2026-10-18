use super::*;

const FIRST_ID: &str = "1b37c9e2-b9c3-43f2-a49c-17fcc5ced63a";
const SECOND_ID: &str = "bff503df-aa66-4f2d-b15d-990c43fdcc26";

fn identity_profile() -> crate::profiles::ProfileRegistry {
    crate::profiles::ProfileRegistry {
        revision: 1,
        active_profile_id: Some("away".to_owned()),
        profiles: BTreeMap::from([(
            "away".to_owned(),
            crate::profiles::RuntimeProfile {
                id: "away".to_owned(),
                name: "Away".to_owned(),
                revision: 1,
                cameras: BTreeMap::from([(
                    FIRST_ID.to_owned(),
                    crate::profiles::CameraProfileOverride {
                        recording_mode: Some(crate::cameras::CameraRecordingMode::Main),
                        ..Default::default()
                    },
                )]),
            },
        )]),
    }
}

fn install_identity_camera(recorder: &mut KeepPeekLoop, ip: &str) -> Camera {
    let mut camera = runtime_rtsp_camera("127.0.0.1:9".parse().unwrap());
    camera.config.ip = ip.parse().unwrap();
    camera.config.recording_mode = crate::cameras::CameraRecordingMode::Sub;
    recorder
        .camera_workers
        .insert(camera.config.ip, CameraWorkers::new());
    recorder
        .camera_definitions
        .insert(camera.config.ip, camera.clone());
    camera
}

#[test]
fn runtime_profile_identity_follows_ip_and_does_not_bind_a_replacement() {
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    let original = install_identity_camera(&mut recorder, "192.0.2.1");
    let identities =
        CameraProfileIds::new(BTreeMap::from([(original.config.ip, FIRST_ID.to_owned())])).unwrap();
    recorder
        .apply_runtime_profiles_with_camera_ids(identity_profile(), Some(identities))
        .unwrap();
    assert_eq!(
        recorder.effective_recording_mode(&original.config),
        crate::cameras::CameraRecordingMode::Main
    );
    recorder.camera_definitions.remove(&original.config.ip);
    recorder.camera_workers.remove(&original.config.ip);
    let moved = install_identity_camera(&mut recorder, "192.0.2.2");
    let replacement = install_identity_camera(&mut recorder, "192.0.2.1");
    let identities = CameraProfileIds::new(BTreeMap::from([
        (moved.config.ip, FIRST_ID.to_owned()),
        (replacement.config.ip, SECOND_ID.to_owned()),
    ]))
    .unwrap();
    recorder
        .apply_runtime_profiles_with_camera_ids(identity_profile(), Some(identities))
        .unwrap();
    assert_eq!(
        recorder.effective_recording_mode(&moved.config),
        crate::cameras::CameraRecordingMode::Main
    );
    assert_eq!(
        recorder.effective_recording_mode(&replacement.config),
        crate::cameras::CameraRecordingMode::Sub
    );
    recorder.camera_definitions.remove(&moved.config.ip);
    recorder.camera_workers.remove(&moved.config.ip);
    let identities = CameraProfileIds::new(BTreeMap::from([(
        replacement.config.ip,
        SECOND_ID.to_owned(),
    )]))
    .unwrap();
    recorder
        .apply_runtime_profiles_with_camera_ids(identity_profile(), Some(identities))
        .unwrap();
    let status = recorder.runtime_profile_status.read().unwrap();
    assert_eq!(
        status.cameras[FIRST_ID].state,
        crate::profiles::RuntimeApplicationState::Degraded
    );
    assert_eq!(
        status.cameras[SECOND_ID].state,
        crate::profiles::RuntimeApplicationState::Applied
    );
    assert_eq!(
        status.cameras[SECOND_ID].policy.recording_mode,
        crate::cameras::CameraRecordingMode::Sub
    );
    assert_eq!(status.cameras.len(), 2);
}

#[test]
fn runtime_profile_stale_request_cannot_replace_camera_identity_mapping() {
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    let camera = install_identity_camera(&mut recorder, "192.0.2.1");
    let identities =
        CameraProfileIds::new(BTreeMap::from([(camera.config.ip, FIRST_ID.to_owned())])).unwrap();
    recorder
        .apply_runtime_profiles_with_camera_ids(identity_profile(), Some(identities))
        .unwrap();
    let stale =
        CameraProfileIds::new(BTreeMap::from([(camera.config.ip, SECOND_ID.to_owned())])).unwrap();
    assert!(
        recorder
            .apply_runtime_profiles_with_camera_ids(Default::default(), Some(stale))
            .is_err()
    );
    assert_eq!(
        recorder
            .runtime_profile_camera_ids
            .camera_id(camera.config.ip),
        FIRST_ID
    );
    assert_eq!(
        recorder.effective_recording_mode(&camera.config),
        crate::cameras::CameraRecordingMode::Main
    );
}

#[test]
fn runtime_profile_identity_translates_privacy_and_rejects_duplicate_owners() {
    let ip = "192.0.2.1".parse().unwrap();
    let identities = CameraProfileIds::new(BTreeMap::from([(ip, FIRST_ID.to_owned())])).unwrap();
    let mut profiles = identity_profile();
    profiles
        .profiles
        .get_mut("away")
        .unwrap()
        .cameras
        .get_mut(FIRST_ID)
        .unwrap()
        .privacy_enforced = Some(true);
    assert_eq!(
        identities.privacy_overrides(&profiles),
        BTreeMap::from([("192.0.2.1".to_owned(), true)])
    );
    assert!(identities.source("192.0.2.1").is_none());
    assert_eq!(identities.source_id(FIRST_ID), Some("192.0.2.1"));
    assert_eq!(
        CameraProfileIds::default().source_id("192.0.2.1"),
        Some("192.0.2.1")
    );
    assert!(
        CameraProfileIds::new(BTreeMap::from([
            (ip, FIRST_ID.to_owned()),
            ("192.0.2.2".parse().unwrap(), FIRST_ID.to_owned()),
        ]))
        .is_err()
    );
}
