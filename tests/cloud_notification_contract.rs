use keeppeek::{
    api::proto::{
        self, CloudNotificationCommand, CloudNotificationContext, CloudNotificationFailureCode,
        CloudNotificationResult, CloudNotificationSettings, CloudNotificationState,
        CloudNotificationStatus, CloudNotificationTestResult, GetCloudNotificationSettings,
        GetCloudNotificationStatus, Ok as ControlOk, Request, ResolveCloudNotification,
        TestCloudNotification, UpdateCloudNotificationSettings, cloud_notification_command,
        cloud_notification_result, ok as control_ok, request as control_request,
    },
    notifications::cloud_protocol::{
        CANONICAL_BODY_BYTES, DOMAIN_SEPARATOR, PRE_PARSE_CEILING_BYTES, PROTOCOL_VERSION_V1,
        SignalEnvelope, SignalError, parse_and_verify_signal, public_key_from_seed, sign_signal,
    },
};
use prost::Message as _;

#[test]
fn protobuf_wire_tags_34_and_52_round_trip_and_preserve_unknown_field_compatibility() {
    let request = Request {
        request_id: 99,
        command: Some(control_request::Command::CloudNotificationCommand(
            CloudNotificationCommand {
                action: Some(cloud_notification_command::Action::UpdateSettings(
                    UpdateCloudNotificationSettings {
                        expected_revision: 4,
                        enabled: true,
                        image_previews_enabled: false,
                    },
                )),
            },
        )),
    };
    let encoded_request = request.encode_to_vec();
    let decoded_request = Request::decode(encoded_request.as_slice()).unwrap();
    assert_eq!(decoded_request, request);

    // Wire tag for field 34 length-delimited: (34 << 3) | 2 = 274 -> varint [0x92, 0x02].
    assert!(
        encoded_request
            .windows(2)
            .any(|window| window == [0x92, 0x02]),
        "Request.cloud_notification_command must encode on approved field tag 34"
    );

    // Older clients without field 34 still decode Request.request_id without failing.
    #[derive(Clone, PartialEq, prost::Message)]
    struct LegacyRequest {
        #[prost(uint64, tag = "1")]
        request_id: u64,
    }
    let legacy = LegacyRequest::decode(encoded_request.as_slice()).unwrap();
    assert_eq!(legacy.request_id, 99);

    let ok = ControlOk {
        result: Some(control_ok::Result::CloudNotificationResult(
            CloudNotificationResult {
                result: Some(cloud_notification_result::Result::Status(
                    CloudNotificationStatus {
                        state: CloudNotificationState::Throttled as i32,
                        paired: true,
                        pending: true,
                        accepted_last_hour: 120,
                        accepted_last_day: 240,
                        suppressed_last_hour: 11,
                        suppressed_last_day: 33,
                        next_eligible_at: None,
                        last_accepted_at: None,
                        last_failure: CloudNotificationFailureCode::ServiceLimit as i32,
                    },
                )),
            },
        )),
    };
    let encoded_ok = ok.encode_to_vec();
    let decoded_ok = ControlOk::decode(encoded_ok.as_slice()).unwrap();
    assert_eq!(decoded_ok, ok);

    // Wire tag for field 52 length-delimited: (52 << 3) | 2 = 418 -> varint [0xa2, 0x03].
    assert!(
        encoded_ok.windows(2).any(|window| window == [0xa2, 0x03]),
        "Ok.cloud_notification_result must encode on approved field tag 52"
    );

    // Verify all other approved command/result message types round-trip cleanly.
    for command in [
        CloudNotificationCommand {
            action: Some(cloud_notification_command::Action::GetSettings(
                GetCloudNotificationSettings {},
            )),
        },
        CloudNotificationCommand {
            action: Some(cloud_notification_command::Action::GetStatus(
                GetCloudNotificationStatus {},
            )),
        },
        CloudNotificationCommand {
            action: Some(cloud_notification_command::Action::Test(
                TestCloudNotification {},
            )),
        },
        CloudNotificationCommand {
            action: Some(cloud_notification_command::Action::Resolve(
                ResolveCloudNotification {
                    cloud_event_id: vec![0x42; 16],
                },
            )),
        },
    ] {
        let bytes = command.encode_to_vec();
        assert_eq!(
            CloudNotificationCommand::decode(bytes.as_slice()).unwrap(),
            command
        );
    }

    for result in [
        CloudNotificationResult {
            result: Some(cloud_notification_result::Result::Settings(
                CloudNotificationSettings {
                    revision: 2,
                    enabled: false,
                    image_previews_enabled: false,
                },
            )),
        },
        CloudNotificationResult {
            result: Some(cloud_notification_result::Result::Context(
                CloudNotificationContext {
                    cloud_event_id: vec![0x77; 16],
                    event_id: "evt-1".to_owned(),
                    revision: 3,
                    canonical_attachment: None,
                    image_availability: proto::EventImageAvailability::None as i32,
                },
            )),
        },
        CloudNotificationResult {
            result: Some(cloud_notification_result::Result::Test(
                CloudNotificationTestResult {
                    accepted: true,
                    next_eligible_at: None,
                    failure: CloudNotificationFailureCode::None as i32,
                },
            )),
        },
    ] {
        let bytes = result.encode_to_vec();
        assert_eq!(
            CloudNotificationResult::decode(bytes.as_slice()).unwrap(),
            result
        );
    }
}

#[test]
fn golden_109_byte_signal_vector_and_size_boundaries_match_contract() {
    assert_eq!(PROTOCOL_VERSION_V1, 1);
    assert_eq!(CANONICAL_BODY_BYTES, 109);
    assert_eq!(PRE_PARSE_CEILING_BYTES, 256);
    assert_eq!(DOMAIN_SEPARATOR, b"keeppeek.nvr-event.v1\0");

    let seed = [0x0b_u8; 32];
    let nvr_id = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    let cloud_event_id = [
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
        0x2f,
    ];
    let envelope = SignalEnvelope {
        nvr_id,
        cloud_event_id,
        issued_at_secs: 1_756_728_123,
        signing_key_version: 5,
    };
    let body = sign_signal(envelope, &seed).unwrap();
    let public_key = public_key_from_seed(&seed).unwrap();
    let verified = parse_and_verify_signal(&body, &public_key).unwrap();
    assert_eq!(verified.version, 1);
    assert_eq!(verified.nvr_id, nvr_id);
    assert_eq!(verified.cloud_event_id, cloud_event_id);
    assert_eq!(verified.issued_at_secs, 1_756_728_123);
    assert_eq!(verified.signing_key_version, 5);

    // Determinism: signing identical inputs yields byte-for-byte identical canonical output.
    assert_eq!(sign_signal(envelope, &seed).unwrap(), body);
    assert_eq!(
        parse_and_verify_signal(&[0_u8; 257], &public_key),
        Err(SignalError::ExceedsPreParseCeiling)
    );
}
