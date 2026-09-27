use super::authenticate;
use crate::access::{AccessRole, external::Config};

fn config() -> Config {
    toml::from_str(
        r#"
allowed_origins = ["https://keeppeek.example"]
[[providers]]
id = "gateway"
name = "Office gateway"
[[providers.mappings]]
claim = "role"
value = "viewer"
role = "user"
[providers.mappings.camera_access]
all_cameras = false
camera_ids = ["front"]
[providers.method]
kind = "proxy"
trusted_peers = ["192.0.2.2/32"]
subject_header = "X-Identity-Subject"
role_header = "X-Identity-Role"
secret_header = "X-Identity-Secret"
shared_secret = "synthetic-proxy-secret"
"#,
    )
    .unwrap()
}

#[test]
fn proxy_identity_only_trusts_immediate_peer() {
    let config = config();
    let headers = [
        ("X-Identity-Subject", "alice"),
        ("X-Identity-Role", "viewer"),
        ("X-Identity-Secret", "synthetic-proxy-secret"),
        ("X-Forwarded-For", "192.0.2.2"),
    ];
    assert!(
        authenticate(&config, "203.0.113.9".parse().unwrap(), &headers)
            .unwrap()
            .is_none()
    );
    let assertion = authenticate(&config, "192.0.2.2".parse().unwrap(), &headers)
        .unwrap()
        .unwrap();
    assert_eq!(assertion.subject, "alice");
    assert_eq!(assertion.grant.role, AccessRole::User);
    assert!(assertion.grant.camera_access.allows("front"));
    assert!(!assertion.grant.camera_access.allows("back"));
    assert!(!format!("{assertion:?}").contains("alice"));
}

#[test]
fn proxy_identity_rejects_missing_duplicate_and_invalid_assertions() {
    let config = config();
    let peer = "192.0.2.2".parse().unwrap();
    let base = [
        ("X-Identity-Subject", "alice"),
        ("X-Identity-Role", "viewer"),
        ("X-Identity-Secret", "synthetic-proxy-secret"),
    ];
    for missing in 0..base.len() {
        let headers: Vec<_> = base
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, header)| (index != missing).then_some(header))
            .collect();
        assert!(authenticate(&config, peer, &headers).is_err());
    }
    let mut headers = base.to_vec();
    headers.push(("x-identity-subject", "mallory"));
    assert!(authenticate(&config, peer, &headers).is_err());
    for value in ["", "alice\r\nInjected: value", "alice,bob"] {
        let mut headers = base;
        headers[0].1 = value;
        assert!(authenticate(&config, peer, &headers).is_err());
    }
    let mut headers = base;
    headers[2].1 = "wrong-proxy-secret";
    assert!(authenticate(&config, peer, &headers).is_err());
    headers = base;
    headers[1].1 = "administrator";
    assert!(authenticate(&config, peer, &headers).is_err());
}

#[test]
fn proxy_identity_rejects_oversized_subject_and_competing_providers() {
    let mut config = config();
    let subject = "x".repeat(257);
    let mut headers = [
        ("X-Identity-Subject", subject.as_str()),
        ("X-Identity-Role", "viewer"),
        ("X-Identity-Secret", "synthetic-proxy-secret"),
    ];
    let peer = "192.0.2.2".parse().unwrap();
    assert!(authenticate(&config, peer, &headers).is_err());
    headers[0].1 = "alice";
    config.providers.push(config.providers[0].clone());
    assert!(authenticate(&config, peer, &headers).is_err());
}
