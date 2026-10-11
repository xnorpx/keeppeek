use std::collections::BTreeMap;

use crate::cameras::CameraRecordingMode;

use super::{
    BaseCameraPolicy, CameraProfileOverride, FieldSource, ProfileRegistry, RuntimeProfile,
    resolve_camera_policy,
};

fn profile(overrides: BTreeMap<String, CameraProfileOverride>) -> RuntimeProfile {
    RuntimeProfile {
        id: "away".to_owned(),
        name: "Away".to_owned(),
        revision: 1,
        cameras: overrides,
    }
}

#[test]
fn sparse_profile_overrides_preserve_base_values_and_provenance() {
    let base = BaseCameraPolicy {
        available: true,
        recording_mode: CameraRecordingMode::Both,
        privacy_enforced: false,
        notifications_enabled: true,
    };
    let overlay = CameraProfileOverride {
        availability: None,
        recording_mode: Some(CameraRecordingMode::EventOnly),
        privacy_enforced: None,
        notifications_suppressed: Some(true),
    };

    let effective = resolve_camera_policy(&base, Some(&overlay));

    assert!(effective.available);
    assert_eq!(effective.recording_mode, CameraRecordingMode::EventOnly);
    assert!(!effective.privacy_enforced);
    assert!(!effective.notifications_enabled);
    assert_eq!(effective.available_source, FieldSource::Base);
    assert_eq!(effective.recording_mode_source, FieldSource::Profile);
    assert_eq!(effective.privacy_source, FieldSource::Base);
    assert_eq!(effective.notifications_source, FieldSource::Profile);
}

#[test]
fn profile_cannot_enable_configured_disabled_recording_or_camera() {
    let base = BaseCameraPolicy {
        available: false,
        recording_mode: CameraRecordingMode::Off,
        privacy_enforced: false,
        notifications_enabled: true,
    };
    let overlay = CameraProfileOverride {
        availability: Some(true),
        recording_mode: Some(CameraRecordingMode::Main),
        privacy_enforced: None,
        notifications_suppressed: None,
    };

    let effective = resolve_camera_policy(&base, Some(&overlay));

    assert!(!effective.available);
    assert_eq!(effective.recording_mode, CameraRecordingMode::Off);
    assert_eq!(effective.available_source, FieldSource::Base);
    assert_eq!(effective.recording_mode_source, FieldSource::Base);
}

#[test]
fn profile_cannot_disable_an_active_privacy_boundary() {
    let base = BaseCameraPolicy {
        available: true,
        recording_mode: CameraRecordingMode::Both,
        privacy_enforced: true,
        notifications_enabled: true,
    };
    let overlay = CameraProfileOverride {
        availability: None,
        recording_mode: None,
        privacy_enforced: Some(false),
        notifications_suppressed: None,
    };

    let effective = resolve_camera_policy(&base, Some(&overlay));

    assert!(effective.privacy_enforced);
    assert_eq!(effective.privacy_source, FieldSource::Base);
    assert_eq!(effective.recording_mode, CameraRecordingMode::Off);
    assert_eq!(effective.recording_mode_source, FieldSource::Base);
}

#[test]
fn profile_privacy_disables_recording_until_the_profile_is_deactivated() {
    let base = BaseCameraPolicy {
        available: true,
        recording_mode: CameraRecordingMode::Both,
        privacy_enforced: false,
        notifications_enabled: true,
    };
    let overlay = CameraProfileOverride {
        availability: None,
        recording_mode: Some(CameraRecordingMode::Main),
        privacy_enforced: Some(true),
        notifications_suppressed: None,
    };

    let effective = resolve_camera_policy(&base, Some(&overlay));

    assert!(effective.privacy_enforced);
    assert_eq!(effective.recording_mode, CameraRecordingMode::Off);
    assert_eq!(effective.recording_mode_source, FieldSource::Profile);
}

#[test]
fn registry_rejects_active_profile_deletion_and_duplicate_ids() {
    let active = profile(BTreeMap::new());
    let mut registry = ProfileRegistry {
        revision: 1,
        active_profile_id: Some("away".to_owned()),
        profiles: BTreeMap::from([("away".to_owned(), active.clone())]),
    };

    assert!(registry.validate().is_ok());
    registry.profiles.remove("away");
    assert!(registry.validate().is_err());

    registry.profiles.insert("away".to_owned(), active.clone());
    registry.profiles.insert("home".to_owned(), active);
    assert!(registry.validate().is_err());
}

#[test]
fn registry_enforces_profile_and_target_bounds() {
    let too_many = (0..=32)
        .map(|index| {
            let id = format!("p{index}");
            (
                id.clone(),
                RuntimeProfile {
                    id,
                    name: "Profile".to_owned(),
                    revision: 1,
                    cameras: BTreeMap::new(),
                },
            )
        })
        .collect();
    let registry = ProfileRegistry {
        revision: 1,
        active_profile_id: None,
        profiles: too_many,
    };

    assert!(registry.validate().is_err());

    let too_many_cameras = (0..=127)
        .map(|index| (format!("camera-{index}"), CameraProfileOverride::default()))
        .collect();
    let registry = ProfileRegistry {
        revision: 1,
        active_profile_id: None,
        profiles: BTreeMap::from([("away".to_owned(), profile(too_many_cameras))]),
    };

    assert!(registry.validate().is_err());
}

#[test]
fn profile_fields_reject_unknown_configuration() {
    let decoded = serde_json::from_str::<CameraProfileOverride>(
        r#"{"availability":true,"password":"secret"}"#,
    );
    assert!(decoded.is_err());
}

#[test]
fn only_the_active_profile_contributes_privacy_overrides() {
    let active = profile(BTreeMap::from([(
        "front".to_owned(),
        CameraProfileOverride {
            privacy_enforced: Some(true),
            ..CameraProfileOverride::default()
        },
    )]));
    let inactive = RuntimeProfile {
        id: "home".to_owned(),
        name: "Home".to_owned(),
        revision: 1,
        cameras: BTreeMap::from([(
            "back".to_owned(),
            CameraProfileOverride {
                privacy_enforced: Some(true),
                ..CameraProfileOverride::default()
            },
        )]),
    };
    let registry = ProfileRegistry {
        revision: 2,
        active_profile_id: Some("away".to_owned()),
        profiles: BTreeMap::from([
            ("away".to_owned(), active.clone()),
            ("home".to_owned(), inactive),
        ]),
    };

    assert_eq!(
        registry.active_privacy_overrides(),
        BTreeMap::from([("front".to_owned(), true)])
    );
}
