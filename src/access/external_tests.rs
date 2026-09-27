use super::{Config, Mapping, Method, map_claims, subject_fingerprint};
use crate::access::{AccessRole, CameraAccess};

fn user_rule() -> Mapping {
    Mapping {
        claim: "groups".into(),
        value: "viewers".into(),
        role: AccessRole::User,
        camera_access: Some(CameraAccess {
            all_cameras: false,
            group_ids: vec!["outside".into()],
            camera_ids: vec![],
        }),
    }
}

#[test]
fn external_mapping_requires_explicit_camera_policy() {
    let mut rule = user_rule();
    assert!(rule.validate().is_ok());
    rule.camera_access = None;
    assert!(rule.validate().is_err());
    rule.camera_access = Some(CameraAccess::default());
    assert!(rule.validate().is_ok());
    rule.role = AccessRole::Administrator;
    assert!(rule.validate().is_err());
    rule.camera_access = None;
    assert!(rule.validate().is_ok());
    rule.role = AccessRole::User;
    rule.camera_access = Some(CameraAccess {
        all_cameras: false,
        camera_ids: (0..128).map(|id| format!("camera-{id}")).collect(),
        group_ids: vec!["outside".into()],
    });
    assert!(rule.validate().is_err());
}

#[test]
fn external_mapping_denies_unmapped_and_ambiguous_claims() {
    let mut rules = vec![user_rule()];
    let claims = serde_json::json!({"groups": ["viewers", "operators"]});
    let grant = map_claims(&rules, &claims).unwrap();
    assert_eq!(grant.role, AccessRole::User);
    assert_eq!(grant.camera_access.group_ids, ["outside"]);
    assert!(!grant.camera_access.all_cameras);
    assert!(map_claims(&rules, &serde_json::json!({})).is_err());
    rules.push(Mapping {
        claim: "groups".into(),
        value: "operators".into(),
        role: AccessRole::Administrator,
        camera_access: None,
    });
    assert!(map_claims(&rules, &claims).is_err());
    assert!(map_claims(&rules, &serde_json::json!({"groups": ["viewers", 5]})).is_err());
}

#[test]
fn external_mapping_bounds_claims_before_matching() {
    let rules = vec![user_rule()];
    let groups = vec!["viewers"; 129];
    assert!(map_claims(&rules, &serde_json::json!({"groups": groups})).is_err());
    let claims = serde_json::json!({"groups": "viewers", "name": "x".repeat(16_385)});
    assert!(map_claims(&rules, &claims).is_err());
}

#[test]
fn external_subject_fingerprint_is_unambiguous_and_stable() {
    assert_ne!(
        subject_fingerprint("ab", "c"),
        subject_fingerprint("a", "bc")
    );
    assert_ne!(
        subject_fingerprint("oidc:a", "user"),
        subject_fingerprint("proxy:a", "user")
    );
    assert_eq!(
        subject_fingerprint("issuer", "user"),
        subject_fingerprint("issuer", "user")
    );
    assert_eq!(subject_fingerprint("issuer", "user").len(), 64);
}

fn oidc_config() -> Config {
    toml::from_str(
        r#"
allowed_origins = ["https://keeppeek.example"]
[[providers]]
id = "office"
name = "Office"
[[providers.mappings]]
claim = "groups"
value = "viewers"
role = "user"
[providers.mappings.camera_access]
all_cameras = false
camera_ids = ["front"]
[providers.method]
kind = "oidc"
issuer = "https://identity.example"
client_id = "keeppeek"
redirect_uri = "https://keeppeek.example/auth/callback"
"#,
    )
    .unwrap()
}

#[test]
fn proxy_providers_require_disjoint_immediate_peer_networks() {
    let mut config = oidc_config();
    let proxy = super::Proxy {
        trusted_peers: vec!["203.0.113.0/24".parse().unwrap()],
        subject_header: "X-Identity-Subject".into(),
        role_header: "X-Identity-Role".into(),
        name_header: None,
        secret_header: None,
        shared_secret: None,
    };
    config.providers[0].method = Method::Proxy(proxy);
    let mut other = config.providers[0].clone();
    other.id = "other".into();
    config.providers.push(other);
    for (first, second, valid) in [
        ("203.0.113.0/24", "203.0.113.0/24", false),
        ("203.0.113.0/24", "203.0.113.42/32", false),
        ("203.0.113.42/32", "203.0.113.0/24", false),
        ("2001:db8::/32", "2001:db8:1::/48", false),
        ("203.0.113.0/24", "203.0.114.0/24", true),
        ("2001:db8::/48", "2001:db8:1::/48", true),
        ("203.0.113.0/24", "2001:db8::/32", true),
    ] {
        for (provider, network) in config.providers.iter_mut().zip([first, second]) {
            let Method::Proxy(proxy) = &mut provider.method else {
                panic!()
            };
            proxy.trusted_peers = vec![network.parse().unwrap()];
        }
        assert_eq!(config.validate().is_ok(), valid, "{first}, {second}");
    }
}

#[test]
fn external_config_validates_origin_issuer_and_mixed_mode() {
    let mut config = oidc_config();
    assert!(config.validate().is_ok());
    config.allowed_origins = vec!["https://keeppeek.example/".into()];
    assert!(config.validate().is_err());
    config.allowed_origins = vec!["https://keeppeek.example".into()];
    config.bearer_enabled = true;
    assert!(config.validate().is_err());
    config.bearer_transition_until_ms = Some(1_900_000_000_000);
    assert!(config.validate().is_ok());
    let Method::Oidc(ref mut oidc) = config.providers[0].method else {
        panic!()
    };
    oidc.issuer = "http://identity.example".into();
    assert!(config.validate().is_err());
}

#[test]
fn external_config_rejects_duplicate_providers_and_unsafe_callback() {
    let mut config = oidc_config();
    config.providers.push(config.providers[0].clone());
    assert!(config.validate().is_err());
    config.providers.pop();
    let Method::Oidc(ref mut oidc) = config.providers[0].method else {
        panic!()
    };
    oidc.redirect_uri = "https://other.example/auth/callback".into();
    assert!(config.validate().is_err());
}

#[test]
fn external_config_never_debugs_secrets_or_mapping_claims() {
    let mut config = oidc_config();
    let Method::Oidc(ref mut oidc) = config.providers[0].method else {
        panic!()
    };
    oidc.client_secret = Some("synthetic-client-secret".into());
    let debug = format!("{config:?}");
    assert!(!debug.contains("synthetic-client-secret"));
    assert!(!debug.contains("viewers"));
}

#[test]
fn external_config_loader_rejects_invalid_policy_without_mutating_file() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-external-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("config.toml");
    let original = "host = '127.0.0.1'\nother_setting = 'preserved'\n";
    std::fs::write(&path, original).unwrap();
    let mut root: toml::Table = toml::from_str(original).unwrap();
    let mut config = oidc_config();
    config.providers[0].mappings[0].camera_access = None;
    root.insert(
        "external_auth".into(),
        toml::Value::try_from(config).unwrap(),
    );
    let invalid_path = directory.join("invalid.toml");
    std::fs::write(&invalid_path, toml::to_string(&root).unwrap()).unwrap();
    let load_failed = crate::config::load_config(&invalid_path).is_err();
    let write_failed = crate::config::write_configuration_table(&path, &root).is_err();
    let saved = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(load_failed);
    assert!(write_failed);
    assert_eq!(saved, original);
}

#[test]
fn external_config_loader_rejects_inline_client_secrets() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-external-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("config.toml");
    let mut config = oidc_config();
    let Method::Oidc(ref mut oidc) = config.providers[0].method else {
        panic!()
    };
    oidc.client_secret = Some("synthetic-inline-secret".into());
    let mut root = toml::Table::new();
    root.insert(
        "external_auth".into(),
        toml::Value::try_from(config).unwrap(),
    );
    std::fs::write(&path, toml::to_string(&root).unwrap()).unwrap();
    let result = crate::config::load_config(&path);
    std::fs::remove_dir_all(directory).unwrap();
    assert!(result.is_err());
}

#[test]
fn external_config_round_trip_preserves_secret_references_and_other_settings() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-external-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("config.toml");
    std::fs::write(
        directory.join("secrets.toml"),
        "OIDC_SECRET = 'synthetic-private-value'\n",
    )
    .unwrap();
    let mut config = oidc_config();
    let Method::Oidc(ref mut oidc) = config.providers[0].method else {
        panic!()
    };
    oidc.client_secret = Some("{secret:OIDC_SECRET}".into());
    let mut root = toml::Table::new();
    root.insert("other_setting".into(), "preserved".into());
    root.insert(
        "external_auth".into(),
        toml::Value::try_from(config).unwrap(),
    );
    crate::config::write_configuration_table(&path, &root).unwrap();
    let loaded = crate::config::load_config(&path).unwrap();
    let external = loaded.external_auth.unwrap();
    let Method::Oidc(oidc) = &external.providers[0].method else {
        panic!()
    };
    assert_eq!(
        oidc.client_secret.as_deref(),
        Some("synthetic-private-value")
    );
    assert!(!format!("{external:?}").contains("synthetic-private-value"));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("{secret:OIDC_SECRET}"));
    assert!(saved.contains("preserved"));
    assert!(!saved.contains("synthetic-private-value"));
    assert!(crate::config::load_cameras(&path).unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn operator_examples_have_valid_explicit_authentication_policies() {
    let guide = include_str!("../../docs/external-authentication-operations.md");
    let examples: Vec<_> = guide.split("```toml\n").skip(1).collect();
    assert_eq!(examples.len(), 2);
    for example in examples {
        let table: toml::Table = toml::from_str(example.split("\n```").next().unwrap()).unwrap();
        let settings: super::Config = table["external_auth"].clone().try_into().unwrap();
        settings.validate().unwrap();
    }
}
