use super::proto;
use prost::Message;

#[test]
fn sharing_command_and_result_use_additive_wire_tags() {
    let request = proto::Request {
        request_id: 7,
        command: Some(proto::request::Command::ShareCommand(proto::ShareCommand {
            action: Some(proto::share_command::Action::GetPolicy(
                proto::GetSharePolicy {},
            )),
        })),
    };
    assert_eq!(request.encode_to_vec(), [8, 7, 250, 1, 2, 10, 0]);
    assert_eq!(
        proto::Request::decode(request.encode_to_vec().as_slice()).unwrap(),
        request
    );
    let result = proto::Ok {
        result: Some(proto::ok::Result::ShareResult(proto::ShareResult {
            result: None,
        })),
    };
    assert_eq!(result.encode_to_vec(), [234, 2, 0]);
}

#[test]
fn nested_control_debug_redacts_the_one_time_guest_url() {
    let token = "secret-guest-token-must-never-be-in-debug";
    let response = proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Response(
            proto::Response {
                request_id: 1,
                result: Some(proto::response::Result::Ok(proto::Ok {
                    result: Some(proto::ok::Result::ShareResult(proto::ShareResult {
                        result: Some(proto::share_result::Result::Created(
                            proto::IssuedShareGrant {
                                grant: Some(proto::ShareGrant {
                                    grant_id: "safe-grant-id".into(),
                                    ..proto::ShareGrant::default()
                                }),
                                url: format!("https://review.example/share#{token}"),
                            },
                        )),
                    })),
                })),
            },
        )),
    };
    let debug = format!("{response:?}");
    assert!(!debug.contains(token));
    assert!(!debug.contains("https://review.example"));
    assert!(debug.contains("safe-grant-id"));
    assert!(debug.contains("[redacted]"));
}

#[test]
fn sharing_creation_preserves_optional_quota_presence_on_the_wire() {
    let request = proto::CreateShareGrant {
        request_id: uuid::Uuid::new_v4().to_string(),
        export_job_id: "immutable-export".into(),
        artifact_id: "generation-one".into(),
        expected_sha256: "ab".repeat(32),
        expires_at_ms: 1_000_000,
        display_name: "Front step".into(),
        note: String::new(),
        allow_stream: true,
        allow_download: false,
        max_views: Some(1),
        max_downloads: None,
    };
    let decoded = proto::CreateShareGrant::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(decoded.max_views, Some(1));
    assert_eq!(decoded.max_downloads, None);
    assert!(!decoded.allow_download);
}
