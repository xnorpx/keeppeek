use super::*;
use crate::api::proto::{LiveQualityDiagnostics, LiveQualitySelectionReason, Notification};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::webrtc) struct QualitySnapshot {
    session_bandwidth_bps: Option<u64>,
    allocated_bitrate_bps: Option<u64>,
    active_variant: Option<StreamKind>,
    pending_variant: Option<StreamKind>,
    reason: LiveQualitySelectionReason,
}

impl QualitySnapshot {
    fn observe(track: &TrackRuntime, session_bandwidth_bps: Option<u64>) -> Self {
        let subscription = &track.subscription;
        let allocated_bitrate_bps = adaptive_auto(subscription).then(|| {
            subscription
                .control
                .as_ref()
                .expect("adaptive Auto has a controller")
                .estimated_bitrate_bps
                .load(Ordering::Acquire)
        });
        Self {
            session_bandwidth_bps,
            allocated_bitrate_bps,
            active_variant: track
                .keyframe_gate
                .has_live_gop()
                .then_some(subscription.active_source.stream),
            pending_variant: subscription.pending_source.map(|source| source.stream),
            reason: selection_reason(track, session_bandwidth_bps, allocated_bitrate_bps),
        }
    }

    fn into_proto(self) -> LiveQualityDiagnostics {
        LiveQualityDiagnostics {
            session_bandwidth_bps: self.session_bandwidth_bps,
            allocated_bitrate_bps: self.allocated_bitrate_bps,
            active_variant_id: self
                .active_variant
                .map(|stream| stream.to_string())
                .unwrap_or_default(),
            pending_variant_id: self
                .pending_variant
                .map(|stream| stream.to_string())
                .unwrap_or_default(),
            selection_reason: self.reason as i32,
        }
    }
}

fn selection_reason(
    track: &TrackRuntime,
    session_bandwidth_bps: Option<u64>,
    allocated_bitrate_bps: Option<u64>,
) -> LiveQualitySelectionReason {
    let subscription = &track.subscription;
    let Some(quality) = subscription.requested_quality() else {
        return LiveQualitySelectionReason::FixedDelivery;
    };
    if subscription.variant_unavailable {
        return LiveQualitySelectionReason::VariantUnavailable;
    }
    if quality != StreamQuality::Auto {
        return LiveQualitySelectionReason::Manual;
    }
    if !track.keyframe_gate.has_live_gop() || subscription.pending_source.is_some() {
        return LiveQualitySelectionReason::WaitingKeyframe;
    }
    if subscription.low_source.is_none() {
        return LiveQualitySelectionReason::SingleVariant;
    }
    if session_bandwidth_bps.is_none() {
        return LiveQualitySelectionReason::BandwidthUnknown;
    }
    if subscription.upgrade_since.is_some() {
        return LiveQualitySelectionReason::UpgradeHold;
    }
    if subscription.downgrade_since.is_some() {
        return LiveQualitySelectionReason::DowngradeHold;
    }
    let threshold_percent = if subscription.active_source == subscription.high_source {
        105
    } else {
        125
    };
    let required_bps = subscription
        .source_bitrate(subscription.high_source)
        .as_u64();
    if allocated_bitrate_bps.unwrap_or(0).saturating_mul(100)
        < required_bps.saturating_mul(threshold_percent)
    {
        LiveQualitySelectionReason::InsufficientBandwidth
    } else {
        LiveQualitySelectionReason::Stable
    }
}

impl ApiMediaRuntime {
    pub(in crate::webrtc) fn live_quality(
        &self,
        track_id: &TrackId,
    ) -> Option<LiveQualityDiagnostics> {
        self.tracks
            .iter()
            .find(|track| &track.runtime.track_id == track_id)
            .map(|track| {
                QualitySnapshot::observe(&track.runtime, self.bandwidth.transport_estimate_bps)
                    .into_proto()
            })
    }

    pub(in crate::webrtc) fn enqueue_live_state(&mut self, track_id: &TrackId, stream: StreamKind) {
        let index = self
            .tracks
            .iter()
            .position(|track| &track.runtime.track_id == track_id);
        let result = if let Some(index) = index {
            self.enqueue_quality_state_at(index)
        } else {
            self.enqueue_control_notification(state_notification(track_id, stream, None))
        };
        if let Err(error) = result {
            tracing::warn!(%error, "dropping subscription stream-state notification");
        }
    }

    pub(in crate::webrtc) fn discard_queued_stream_states(&mut self, track_id: &TrackId) {
        let mut removed_bytes = 0usize;
        self.control_notifications.retain(|notification| {
            let remove = matches!(
                notification.event.as_ref(),
                Some(crate::api::proto::notification::Event::SubscriptionStreamState(state))
                    if state.subscription_id == track_id.0
            );
            if remove {
                removed_bytes += control_notification_encoded_len(notification);
            }
            !remove
        });
        self.control_notification_bytes = self
            .control_notification_bytes
            .checked_sub(removed_bytes)
            .expect("notification byte count includes queued stream states");
    }

    pub(super) fn report_live_quality(&mut self, now: Instant) {
        if self
            .bandwidth
            .quality_report_due
            .is_some_and(|due| now < due)
        {
            return;
        }
        self.bandwidth.quality_report_due = Some(now + DESIRED_BITRATE_REFRESH);
        for index in 0..self.tracks.len() {
            let runtime = &self.tracks[index].runtime;
            let snapshot = QualitySnapshot::observe(runtime, self.bandwidth.transport_estimate_bps);
            if runtime.reported_quality == Some(snapshot) {
                continue;
            }
            if self.enqueue_quality_state_at(index).is_err() {
                break;
            }
        }
    }

    fn enqueue_quality_state_at(&mut self, index: usize) -> anyhow::Result<()> {
        let runtime = &self.tracks[index].runtime;
        let snapshot = QualitySnapshot::observe(runtime, self.bandwidth.transport_estimate_bps);
        let notification = state_notification(
            &runtime.track_id,
            runtime.subscription.active_source.stream,
            Some(snapshot.into_proto()),
        );
        self.enqueue_control_notification(notification)?;
        self.tracks[index].runtime.reported_quality = Some(snapshot);
        Ok(())
    }
}

fn state_notification(
    track_id: &TrackId,
    stream: StreamKind,
    live_quality: Option<LiveQualityDiagnostics>,
) -> Notification {
    Notification {
        event: Some(
            crate::api::proto::notification::Event::SubscriptionStreamState(
                crate::api::proto::SubscriptionStreamState {
                    subscription_id: track_id.0.clone(),
                    active_variant_id: stream.to_string(),
                    live_quality,
                },
            ),
        ),
    }
}
