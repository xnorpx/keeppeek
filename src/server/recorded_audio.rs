//! Captures recorded presentation policy independently of transient playback previews.

use super::*;

pub(super) const CAPABILITY_ID: &str = "keeppeek.recorded-audio.v1";

pub(super) fn configured(
    camera: &CameraEntry,
    stream: &str,
) -> Result<proto::RecordedAudioTiming, ControlCommandError> {
    let settings = camera.configuration.recorded_audio;
    let offset = settings.effective_offset_ms(stream).map_err(invalid)?;
    let encoded = serde_json::to_vec(&settings).map_err(invalid)?;
    let mut digest = Sha256::new();
    digest.update(b"keeppeek.recorded-audio.v1\0");
    digest.update(camera.info.id.as_bytes());
    digest.update([0]);
    digest.update(encoded);
    Ok(proto::RecordedAudioTiming {
        configured_offset_ms: offset,
        applied_offset_ms: offset,
        configuration_revision: encode_lower_hex(digest.finalize()),
        preview: false,
        mapping_version: 1,
        first_video_pts_us: None,
        first_audio_pts_us: None,
        stored_skew_us: None,
        audio_present: None,
        clipped_audio_samples: None,
    })
}

pub(super) fn preview(
    configured: &proto::RecordedAudioTiming,
    requested: Option<&proto::RecordedAudioPreview>,
) -> Result<proto::RecordedAudioTiming, ControlCommandError> {
    let mut timing = configured.clone();
    timing.applied_offset_ms = timing.configured_offset_ms;
    timing.preview = false;
    match requested.and_then(|preview| preview.selection.as_ref()) {
        None if requested.is_none() => {}
        Some(proto::recorded_audio_preview::Selection::UseConfigured(true)) => {}
        Some(proto::recorded_audio_preview::Selection::OffsetMs(offset)) => {
            validate_offset(*offset)?;
            timing.applied_offset_ms = *offset;
            timing.preview = true;
        }
        _ => {
            return Err(invalid(
                "recorded audio preview requires an offset or use_configured=true",
            ));
        }
    }
    Ok(timing)
}

pub(super) fn validate_snapshot(
    timing: Option<&proto::RecordedAudioTiming>,
) -> Result<i32, ControlCommandError> {
    let Some(timing) = timing else { return Ok(0) };
    validate_offset(timing.configured_offset_ms)?;
    validate_offset(timing.applied_offset_ms)?;
    if timing.preview
        || timing.mapping_version != 1
        || timing.configured_offset_ms != timing.applied_offset_ms
        || timing.configuration_revision.len() != 64
        || !timing
            .configuration_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid(
            "export recorded audio timing snapshot is unsupported",
        ));
    }
    Ok(timing.applied_offset_ms)
}

pub(super) fn same_snapshot(
    left: Option<&proto::RecordedAudioTiming>,
    right: Option<&proto::RecordedAudioTiming>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.configured_offset_ms == right.configured_offset_ms
                && left.applied_offset_ms == right.applied_offset_ms
                && left.configuration_revision == right.configuration_revision
                && left.preview == right.preview
                && left.mapping_version == right.mapping_version
        }
        _ => false,
    }
}

pub(super) fn observe(
    timing: &mut proto::RecordedAudioTiming,
    evidence: crate::storage::playback::AudioTimingEvidence,
) {
    timing.first_video_pts_us = evidence.first_video_pts_us;
    timing.first_audio_pts_us = evidence.first_audio_pts_us;
    timing.stored_skew_us = evidence.stored_skew_us();
    timing.audio_present = Some(evidence.audio_present);
    timing.clipped_audio_samples = Some(evidence.clipped_audio_samples);
}

pub(super) fn validate_offset(offset: i32) -> Result<(), ControlCommandError> {
    if !(-crate::cameras::MAX_RECORDED_AUDIO_OFFSET_MS
        ..=crate::cameras::MAX_RECORDED_AUDIO_OFFSET_MS)
        .contains(&offset)
    {
        return Err(invalid(
            "recorded audio offset must be between -5000 and 5000 milliseconds",
        ));
    }
    Ok(())
}

fn invalid(error: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_audio_capability_requires_a_recording_catalog() {
        let state = ServerState::empty();
        assert!(
            !server_capability_ids(&state)
                .iter()
                .any(|id| id == CAPABILITY_ID)
        );
        let root = std::env::temp_dir().join(format!("audio-capability-{}", Uuid::new_v4()));
        let catalog = crate::storage::RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let state = state.with_recording_catalog(catalog.handle());
        assert!(
            server_capability_ids(&state)
                .iter()
                .any(|id| id == CAPABILITY_ID)
        );
        drop(state);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn snapshot() -> proto::RecordedAudioTiming {
        proto::RecordedAudioTiming {
            configured_offset_ms: 200,
            applied_offset_ms: 200,
            configuration_revision: "a".repeat(64),
            mapping_version: 1,
            ..Default::default()
        }
    }

    #[test]
    fn recorded_audio_preview_is_transient_and_reset_uses_the_captured_configured_value() {
        let configured = snapshot();
        let override_value = proto::RecordedAudioPreview {
            selection: Some(proto::recorded_audio_preview::Selection::OffsetMs(-100)),
        };
        let applied = preview(&configured, Some(&override_value)).unwrap();
        assert_eq!(applied.applied_offset_ms, -100);
        assert_eq!(applied.configured_offset_ms, 200);
        assert!(applied.preview);
        assert_eq!(configured, snapshot());
        let reset = proto::RecordedAudioPreview {
            selection: Some(proto::recorded_audio_preview::Selection::UseConfigured(
                true,
            )),
        };
        let reset = preview(&applied, Some(&reset)).unwrap();
        assert_eq!(reset.applied_offset_ms, 200);
        assert!(!reset.preview);
    }

    #[test]
    fn recorded_audio_preview_rejects_empty_false_and_out_of_range_values() {
        let configured = snapshot();
        for selection in [
            None,
            Some(proto::recorded_audio_preview::Selection::UseConfigured(
                false,
            )),
            Some(proto::recorded_audio_preview::Selection::OffsetMs(-5001)),
            Some(proto::recorded_audio_preview::Selection::OffsetMs(5001)),
        ] {
            assert!(
                preview(
                    &configured,
                    Some(&proto::RecordedAudioPreview { selection })
                )
                .is_err()
            );
            assert_eq!(configured, snapshot());
        }
        for offset in [-5000, 0, 5000] {
            assert!(validate_offset(offset).is_ok());
        }
    }

    #[test]
    fn recorded_audio_export_snapshots_reject_previews_and_unknown_mappings_but_preserve_legacy_zero()
     {
        assert_eq!(validate_snapshot(None).unwrap(), 0);
        let captured = snapshot();
        assert_eq!(validate_snapshot(Some(&captured)).unwrap(), 200);
        for changed in [
            proto::RecordedAudioTiming {
                preview: true,
                ..captured.clone()
            },
            proto::RecordedAudioTiming {
                mapping_version: 2,
                ..captured.clone()
            },
            proto::RecordedAudioTiming {
                applied_offset_ms: 0,
                ..captured.clone()
            },
            proto::RecordedAudioTiming {
                configuration_revision: String::new(),
                ..captured
            },
        ] {
            assert!(validate_snapshot(Some(&changed)).is_err());
            assert!(!same_snapshot(Some(&captured), Some(&changed)));
        }
        assert!(!same_snapshot(None, Some(&captured)));
        assert!(same_snapshot(Some(&captured), Some(&captured)));
    }
}
