//! Live and recorded presentation settings preserve explicit zero and per-stream inheritance.

use serde::{Deserialize, Serialize};

/// Maximum absolute recorded-audio correction, in milliseconds.
pub const MAX_RECORDED_AUDIO_OFFSET_MS: i32 = 5_000;

/// Maximum absolute live listener correction, in milliseconds.
pub const MAX_LIVE_AUDIO_OFFSET_MS: i32 = 200;
pub type RecordedAudioSettings = AudioOffsetSettings<MAX_RECORDED_AUDIO_OFFSET_MS>;
pub type LiveAudioSettings = AudioOffsetSettings<MAX_LIVE_AUDIO_OFFSET_MS>;

/// Signed presentation corrections. Absent profile overrides inherit the camera default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOffsetSettings<const LIMIT_MS: i32> {
    /// An absent camera default resolves to zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_offset_ms: Option<i32>,
    /// An absent main-stream override inherits the camera default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_offset_ms: Option<i32>,
    /// An absent sub-stream override inherits the camera default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_offset_ms: Option<i32>,
}

impl<const LIMIT_MS: i32> AudioOffsetSettings<LIMIT_MS> {
    pub const fn is_default(&self) -> bool {
        self.default_offset_ms.is_none()
            && self.main_offset_ms.is_none()
            && self.sub_offset_ms.is_none()
    }

    /// Rejects corrections outside the inclusive supported millisecond range.
    pub fn validate(&self) -> anyhow::Result<()> {
        for value in [
            self.default_offset_ms,
            self.main_offset_ms,
            self.sub_offset_ms,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(
                (-LIMIT_MS..=LIMIT_MS).contains(&value),
                "audio offset must be between {} and {} milliseconds",
                -LIMIT_MS,
                LIMIT_MS
            );
        }
        Ok(())
    }

    /// Resolves a configured main or sub stream and rejects unknown stream names.
    pub fn effective_offset_ms(&self, stream_id: &str) -> anyhow::Result<i32> {
        let stream_offset = match stream_id {
            "main" => self.main_offset_ms,
            "sub" => self.sub_offset_ms,
            _ => anyhow::bail!("audio correction requires a main or sub stream"),
        };
        Ok(stream_offset.or(self.default_offset_ms).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_overrides_preserve_explicit_zero_and_camera_inheritance() {
        let settings: RecordedAudioSettings =
            toml::from_str("default_offset_ms=125\nmain_offset_ms=0").unwrap();
        assert_eq!(settings.effective_offset_ms("main").unwrap(), 0);
        assert_eq!(settings.effective_offset_ms("sub").unwrap(), 125);
        assert!(settings.effective_offset_ms("other").is_err());
        let restored: RecordedAudioSettings =
            toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(
            RecordedAudioSettings::default()
                .effective_offset_ms("sub")
                .unwrap(),
            0
        );
    }

    #[test]
    fn inclusive_bounds_reject_every_invalid_override_without_abs_overflow() {
        for value in [-5_000, 0, 5_000] {
            let settings = RecordedAudioSettings {
                default_offset_ms: Some(value),
                main_offset_ms: Some(-value),
                sub_offset_ms: Some(value),
            };
            settings.validate().unwrap();
        }
        for value in [i32::MIN, -5_001, 5_001, i32::MAX] {
            for field in ["default_offset_ms", "main_offset_ms", "sub_offset_ms"] {
                let settings: RecordedAudioSettings =
                    toml::from_str(&format!("{field}={value}")).unwrap();
                assert!(settings.validate().is_err(), "{field}={value}");
            }
        }
    }

    #[test]
    fn invalid_offsets_report_the_configured_bounds() {
        let live = LiveAudioSettings {
            default_offset_ms: Some(201),
            ..Default::default()
        };
        let recorded = RecordedAudioSettings {
            default_offset_ms: Some(5001),
            ..Default::default()
        };
        assert_eq!(
            live.validate().unwrap_err().to_string(),
            "audio offset must be between -200 and 200 milliseconds"
        );
        assert_eq!(
            recorded.validate().unwrap_err().to_string(),
            "audio offset must be between -5000 and 5000 milliseconds"
        );
    }

    #[test]
    fn empty_settings_omit_values_but_explicit_zero_remains_serialized() {
        assert!(RecordedAudioSettings::default().is_default());
        assert!(
            toml::to_string(&RecordedAudioSettings::default())
                .unwrap()
                .is_empty()
        );
        let settings: RecordedAudioSettings = toml::from_str("sub_offset_ms=0").unwrap();
        assert!(!settings.is_default());
        assert!(
            toml::to_string(&settings)
                .unwrap()
                .contains("sub_offset_ms = 0")
        );
        for invalid in [
            "main_offset_ms=0.5",
            "offset_ms=100",
            "main_offset_ms='100'",
        ] {
            assert!(toml::from_str::<RecordedAudioSettings>(invalid).is_err());
        }
    }

    #[test]
    fn partial_audio_settings_default_absent_overrides() {
        let partial: LiveAudioSettings = toml::from_str("default_offset_ms = 50").unwrap();
        assert_eq!(partial.default_offset_ms, Some(50));
        assert_eq!(partial.main_offset_ms, None);
        assert_eq!(partial.sub_offset_ms, None);
        assert_eq!(partial.effective_offset_ms("main").unwrap(), 50);

        let empty_toml: LiveAudioSettings = toml::from_str("").unwrap();
        assert_eq!(empty_toml, LiveAudioSettings::default());

        let partial_json: LiveAudioSettings =
            serde_json::from_str(r#"{"default_offset_ms": 50}"#).unwrap();
        assert_eq!(partial_json, partial);
        assert_eq!(
            serde_json::from_str::<LiveAudioSettings>("{}").unwrap(),
            LiveAudioSettings::default()
        );
    }
}
