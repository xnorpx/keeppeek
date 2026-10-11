use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::cameras::CameraRecordingMode;

pub const MAX_PROFILES: usize = 32;
pub const MAX_PROFILE_TARGETS: usize = 127;
const MAX_PROFILE_ID_BYTES: usize = 128;
const MAX_PROFILE_NAME_BYTES: usize = 80;
const MAX_CAMERA_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CameraProfileOverride {
    #[serde(default)]
    pub availability: Option<bool>,
    #[serde(default)]
    pub recording_mode: Option<CameraRecordingMode>,
    #[serde(default)]
    pub privacy_enforced: Option<bool>,
    #[serde(default)]
    pub notifications_suppressed: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfile {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub cameras: BTreeMap<String, CameraProfileOverride>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileRegistry {
    pub revision: u64,
    pub active_profile_id: Option<String>,
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseCameraPolicy {
    pub available: bool,
    pub recording_mode: CameraRecordingMode,
    pub privacy_enforced: bool,
    pub notifications_enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldSource {
    Base,
    Profile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveCameraPolicy {
    pub available: bool,
    pub recording_mode: CameraRecordingMode,
    pub privacy_enforced: bool,
    pub notifications_enabled: bool,
    pub available_source: FieldSource,
    pub recording_mode_source: FieldSource,
    pub privacy_source: FieldSource,
    pub notifications_source: FieldSource,
}

impl ProfileRegistry {
    pub fn is_empty(&self) -> bool {
        self.revision == 0 && self.active_profile_id.is_none() && self.profiles.is_empty()
    }

    pub fn active_privacy_overrides(&self) -> BTreeMap<String, bool> {
        self.active_profile_id
            .as_ref()
            .and_then(|profile_id| self.profiles.get(profile_id))
            .map(|profile| {
                profile
                    .cameras
                    .iter()
                    .filter_map(|(camera_id, overrides)| {
                        overrides
                            .privacy_enforced
                            .map(|enforced| (camera_id.clone(), enforced))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.revision <= i64::MAX as u64,
            "profile registry revision exceeds the persisted range"
        );
        ensure!(
            self.profiles.len() <= MAX_PROFILES,
            "profile registry cannot contain more than {MAX_PROFILES} profiles"
        );
        if let Some(active_id) = &self.active_profile_id {
            validate_profile_id(active_id)?;
            ensure!(
                self.profiles.contains_key(active_id),
                "active profile does not exist"
            );
        }
        for (key, profile) in &self.profiles {
            validate_profile(profile)?;
            ensure!(key == &profile.id, "profile key does not match its ID");
        }
        Ok(())
    }

    pub fn validate_targets(&self, camera_ids: &BTreeSet<String>) -> Result<()> {
        self.validate()?;
        for profile in self.profiles.values() {
            for camera_id in profile.cameras.keys() {
                ensure!(
                    camera_ids.contains(camera_id),
                    "profile references an unknown camera"
                );
            }
        }
        Ok(())
    }

    pub fn validate_delete(&self, profile_id: &str) -> Result<()> {
        validate_profile_id(profile_id)?;
        ensure!(
            self.active_profile_id.as_deref() != Some(profile_id),
            "activate Base or another profile before deleting the active profile"
        );
        ensure!(
            self.profiles.contains_key(profile_id),
            "profile does not exist"
        );
        Ok(())
    }
}

pub fn validate_profile(profile: &RuntimeProfile) -> Result<()> {
    validate_profile_id(&profile.id)?;
    validate_profile_name(&profile.name)?;
    ensure!(
        profile.revision > 0 && profile.revision <= i64::MAX as u64,
        "profile revision must be positive and fit the persisted range"
    );
    ensure!(
        profile.cameras.len() <= MAX_PROFILE_TARGETS,
        "profile cannot target more than {MAX_PROFILE_TARGETS} cameras"
    );
    for (camera_id, overrides) in &profile.cameras {
        validate_camera_id(camera_id)?;
        ensure!(
            overrides.availability.is_some()
                || overrides.recording_mode.is_some()
                || overrides.privacy_enforced.is_some()
                || overrides.notifications_suppressed.is_some(),
            "camera override must set at least one eligible field"
        );
    }
    Ok(())
}

pub fn resolve_camera_policy(
    base: &BaseCameraPolicy,
    profile: Option<&CameraProfileOverride>,
) -> EffectiveCameraPolicy {
    let profile_availability = profile.and_then(|overrides| overrides.availability);
    let available = base.available && profile_availability.unwrap_or(true);
    let available_source = if !base.available || profile_availability == Some(false) {
        if base.available {
            FieldSource::Profile
        } else {
            FieldSource::Base
        }
    } else if profile_availability.is_some() {
        FieldSource::Profile
    } else {
        FieldSource::Base
    };

    let profile_privacy = profile.and_then(|overrides| overrides.privacy_enforced);
    let (privacy_enforced, privacy_source) = if base.privacy_enforced {
        (true, FieldSource::Base)
    } else if let Some(enforced) = profile_privacy {
        (enforced, FieldSource::Profile)
    } else {
        (false, FieldSource::Base)
    };

    let profile_recording_mode = profile.and_then(|overrides| overrides.recording_mode);
    let (recording_mode, recording_mode_source) = if base.recording_mode == CameraRecordingMode::Off
    {
        (CameraRecordingMode::Off, FieldSource::Base)
    } else if privacy_enforced {
        (CameraRecordingMode::Off, privacy_source)
    } else if !available {
        (CameraRecordingMode::Off, available_source)
    } else if let Some(mode) = profile_recording_mode {
        (mode, FieldSource::Profile)
    } else {
        (base.recording_mode, FieldSource::Base)
    };

    let profile_suppression = profile.and_then(|overrides| overrides.notifications_suppressed);
    let (notifications_enabled, notifications_source) = if !base.notifications_enabled {
        (false, FieldSource::Base)
    } else if let Some(suppressed) = profile_suppression {
        (!suppressed, FieldSource::Profile)
    } else {
        (true, FieldSource::Base)
    };

    EffectiveCameraPolicy {
        available,
        recording_mode,
        privacy_enforced,
        notifications_enabled,
        available_source,
        recording_mode_source,
        privacy_source,
        notifications_source,
    }
}

fn validate_profile_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= MAX_PROFILE_ID_BYTES
            && id != "."
            && id != ".."
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "profile ID must contain 1 to {MAX_PROFILE_ID_BYTES} ASCII letters, digits, dots, underscores, or hyphens"
    );
    Ok(())
}

fn validate_profile_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty()
            && name.len() <= MAX_PROFILE_NAME_BYTES
            && !name.chars().any(char::is_control),
        "profile name must contain 1 to {MAX_PROFILE_NAME_BYTES} UTF-8 bytes without control characters"
    );
    Ok(())
}

fn validate_camera_id(camera_id: &str) -> Result<()> {
    ensure!(
        !camera_id.trim().is_empty()
            && camera_id.len() <= MAX_CAMERA_ID_BYTES
            && !camera_id.chars().any(char::is_control),
        "camera ID must contain 1 to {MAX_CAMERA_ID_BYTES} UTF-8 bytes without control characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
