use super::*;
use crate::api::proto::{LiveQualitySelectionReason, notification};

#[test]
fn live_quality_subscribe_result_does_not_claim_an_unadmitted_stream() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.remove(0);
    fixture.take_stream_states();
    let result = fixture.subscribe(0, StreamQuality::Auto, true);
    let quality = result
        .live_quality
        .expect("RTP subscription omitted live quality diagnostics");
    assert_eq!(result.selected_variant_id, "sub");
    assert!(quality.active_variant_id.is_empty());
    assert!(quality.pending_variant_id.is_empty());
    assert_eq!(quality.session_bandwidth_bps, None);
    assert_eq!(quality.allocated_bitrate_bps, Some(0));
    assert_eq!(
        quality.selection_reason,
        LiveQualitySelectionReason::WaitingKeyframe as i32
    );
    fixture.close();
}

#[test]
fn live_quality_serialized_stream_state_carries_the_current_diagnostics() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    let mut states = Vec::new();
    fixture
        .media
        .flush_control_notifications(|payload| {
            let envelope = ControlEnvelope::decode(payload).unwrap();
            if let Some(control_envelope::Message::Notification(notification)) = envelope.message
                && let Some(notification::Event::SubscriptionStreamState(state)) =
                    notification.event
            {
                states.push(state);
            }
            Ok(true)
        })
        .unwrap();
    assert_eq!(states.len(), 1);
    let state = states.pop().unwrap();
    assert_eq!(state.subscription_id, "camera-0");
    let quality = state
        .live_quality
        .expect("serialized stream state omitted live quality diagnostics");
    assert!(quality.active_variant_id.is_empty());
    assert_eq!(
        quality.selection_reason,
        LiveQualitySelectionReason::WaitingKeyframe as i32
    );
    fixture.close();
}
