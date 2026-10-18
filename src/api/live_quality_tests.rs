use super::proto;
use prost::Message as _;

#[derive(Clone, PartialEq, prost::Message)]
struct LegacyStreamState {
    #[prost(string, tag = "1")]
    subscription_id: String,
    #[prost(string, tag = "2")]
    active_variant_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct LegacySubscriptionResult {
    #[prost(string, tag = "1")]
    subscription_id: String,
    #[prost(string, tag = "6")]
    selected_variant_id: String,
}

fn diagnostics() -> proto::LiveQualityDiagnostics {
    proto::LiveQualityDiagnostics {
        session_bandwidth_bps: None,
        allocated_bitrate_bps: Some(0),
        active_variant_id: "sub".to_owned(),
        pending_variant_id: "main".to_owned(),
        selection_reason: 999,
    }
}

#[test]
fn live_quality_state_preserves_presence_and_legacy_variant_identity() {
    let current = proto::SubscriptionStreamState {
        subscription_id: "camera-1".to_owned(),
        active_variant_id: "sub".to_owned(),
        live_quality: Some(diagnostics()),
    };
    let wire = current.encode_to_vec();
    assert_eq!(
        proto::SubscriptionStreamState::decode(wire.as_slice()).unwrap(),
        current
    );
    let legacy = LegacyStreamState::decode(wire.as_slice()).unwrap();
    assert_eq!(legacy.subscription_id, "camera-1");
    assert_eq!(legacy.active_variant_id, "sub");
    let old_wire = legacy.encode_to_vec();
    let old_state = proto::SubscriptionStreamState::decode(old_wire.as_slice()).unwrap();
    assert!(old_state.live_quality.is_none());
    assert_eq!(old_state.active_variant_id, "sub");
}

#[test]
fn live_quality_result_keeps_requested_target_separate_from_active_source() {
    let current = proto::SubscriptionResult {
        subscription_id: "camera-1".to_owned(),
        selected_variant_id: "main".to_owned(),
        live_quality: Some(diagnostics()),
        ..Default::default()
    };
    let wire = current.encode_to_vec();
    let decoded = proto::SubscriptionResult::decode(wire.as_slice()).unwrap();
    assert_eq!(decoded, current);
    assert_eq!(decoded.live_quality.unwrap().active_variant_id, "sub");
    let legacy = LegacySubscriptionResult::decode(wire.as_slice()).unwrap();
    assert_eq!(legacy.selected_variant_id, "main");
    let old_wire = legacy.encode_to_vec();
    assert!(
        proto::SubscriptionResult::decode(old_wire.as_slice())
            .unwrap()
            .live_quality
            .is_none()
    );
}
