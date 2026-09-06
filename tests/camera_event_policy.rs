use std::net::{IpAddr, Ipv4Addr};

use keeppeek::cameras::{
    CameraConfig,
    events::{EventConfig, EventMode, MetadataMode},
};

const CAMERA_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));

#[test]
fn camera_event_policy_defaults_are_compatible_with_legacy_configs() {
    let camera: CameraConfig = toml::from_str("ip = '192.0.2.10'").unwrap();
    let empty: EventConfig = toml::from_str("").unwrap();
    assert_eq!(camera.events, empty);
    assert!(empty.is_default());
    assert_eq!(empty.mode, EventMode::Auto);
    assert_eq!(empty.metadata_stream, MetadataMode::Auto);
    assert!(empty.snapshots);
    assert!(empty.event_service_url.is_none());
    assert!(empty.source_tokens.is_empty());
    assert!(empty.include_topics.is_empty());
    assert!(empty.exclude_topics.is_empty());
    empty.validate(CAMERA_IP).unwrap();
    assert!(
        toml::Value::try_from(&camera)
            .unwrap()
            .get("events")
            .is_none()
    );
    let round_trip: EventConfig = toml::from_str(&toml::to_string(&empty).unwrap()).unwrap();
    assert_eq!(round_trip, empty);
}

#[test]
fn camera_event_policy_forced_modes_are_independent_of_video_backend() {
    for backend in ["auto", "retina", "reo-proto"] {
        for (text, expected) in [
            ("auto", EventMode::Auto),
            ("vendor", EventMode::Vendor),
            ("onvif-pullpoint", EventMode::OnvifPullpoint),
            ("rtsp-metadata", EventMode::RtspMetadata),
            ("disabled", EventMode::Disabled),
        ] {
            let camera: CameraConfig = toml::from_str(&format!(
                "ip = '192.0.2.10'\nbackend = '{backend}'\n[events]\nmode = '{text}'"
            ))
            .unwrap();
            assert_eq!(camera.events.mode, expected);
            camera.events.validate(CAMERA_IP).unwrap();
            assert_eq!(serde_json::to_value(expected).unwrap(), text);
        }
    }
    for (text, expected) in [
        ("auto", MetadataMode::Auto),
        ("enabled", MetadataMode::Enabled),
        ("disabled", MetadataMode::Disabled),
    ] {
        let config: EventConfig = toml::from_str(&format!("metadata_stream = '{text}'")).unwrap();
        assert_eq!(config.metadata_stream, expected);
        assert_eq!(serde_json::to_value(expected).unwrap(), text);
    }
}

#[test]
fn camera_event_policy_rejects_unsupported_modes() {
    for text in ["onvif-push", "pullpoint", "onvif_pullpoint", "unknown"] {
        assert!(toml::from_str::<EventConfig>(&format!("mode = '{text}'")).is_err());
    }
    assert!(toml::from_str::<EventConfig>("metadata_stream = 'onvif-push'").is_err());
}

#[test]
fn camera_event_policy_source_tokens_are_bounded_without_normalizing_identity() {
    let mut config = EventConfig {
        source_tokens: vec!["x".repeat(256); 32],
        ..EventConfig::default()
    };
    config.validate(CAMERA_IP).unwrap();
    config.source_tokens.push("extra".to_owned());
    assert!(config.validate(CAMERA_IP).is_err());
    for token in [
        String::new(),
        " ".to_owned(),
        "channel\n1".to_owned(),
        "channel\u{7f}1".to_owned(),
        "channel\u{85}1".to_owned(),
        "x".repeat(257),
        "\u{e9}".repeat(129),
    ] {
        config.source_tokens = vec![token];
        assert!(config.validate(CAMERA_IP).is_err());
    }
    config.source_tokens = vec![" Channel 1 ".to_owned(), "\u{e9}".repeat(128)];
    config.validate(CAMERA_IP).unwrap();
    let round_trip: EventConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(round_trip.source_tokens, config.source_tokens);
}

#[test]
fn camera_event_policy_topic_filters_share_a_checked_count_and_byte_budget() {
    let maximum = format!("{{urn:camera}}{}", "x".repeat(1012));
    let mut config = EventConfig {
        include_topics: vec![maximum.clone(); 16],
        exclude_topics: vec![maximum.clone(); 16],
        ..EventConfig::default()
    };
    config.validate(CAMERA_IP).unwrap();
    config.exclude_topics.push(maximum.clone());
    assert!(config.validate(CAMERA_IP).is_err());
    config.include_topics.clear();
    config.exclude_topics = vec![maximum.clone(); 32];
    config.validate(CAMERA_IP).unwrap();
    config.exclude_topics.clear();
    config.include_topics = vec![maximum; 32];
    config.validate(CAMERA_IP).unwrap();
    config.include_topics = vec![format!("{{urn:camera}}{}", "x".repeat(1013))];
    assert!(config.validate(CAMERA_IP).is_err());
    config.exclude_topics = std::mem::take(&mut config.include_topics);
    assert!(config.validate(CAMERA_IP).is_err());
}

#[test]
fn camera_event_policy_topics_use_expanded_roots_and_unprefixed_paths() {
    let mut config = EventConfig {
        include_topics: vec![
            "{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm".to_owned(),
        ],
        ..EventConfig::default()
    };
    config.validate(CAMERA_IP).unwrap();
    for topic in [
        "",
        " ",
        "tns1:VideoSource/MotionAlarm",
        "{}VideoSource/MotionAlarm",
        "{urn:camera}",
        "{urn:camera}VideoSource/tns1:MotionAlarm",
        "{urn:camera}VideoSource//MotionAlarm",
        "{urn:camera}VideoSource/Motion\nAlarm",
        "{urn:camera}VideoSource/Motion\u{7f}Alarm",
        "{urn:camera}VideoSource/Motion Alarm",
        "{urn:cam era}VideoSource/MotionAlarm",
    ] {
        config.include_topics = vec![topic.to_owned()];
        assert!(
            config.validate(CAMERA_IP).is_err(),
            "accepted topic {topic:?}"
        );
        config.exclude_topics = std::mem::take(&mut config.include_topics);
        assert!(
            config.validate(CAMERA_IP).is_err(),
            "accepted topic {topic:?}"
        );
        config.exclude_topics.clear();
    }
}

#[test]
fn camera_event_policy_urls_require_exact_camera_ips_without_dns() {
    for (camera_ip, url) in [
        ("192.0.2.10", "http://192.0.2.10/onvif/events"),
        ("192.0.2.10", "https://192.0.2.10:8443/onvif/events"),
        ("2001:db8::10", "http://[2001:db8::10]:8080/onvif/events"),
        (
            "2001:db8::10",
            "https://[2001:db8:0:0:0:0:0:10]/onvif/events",
        ),
    ] {
        let config = EventConfig {
            event_service_url: Some(url.to_owned()),
            ..EventConfig::default()
        };
        config.validate(camera_ip.parse().unwrap()).unwrap();
    }
    for url in [
        "",
        "/onvif/events",
        "http://camera.invalid/private",
        "http://localhost/private",
        "http://192.0.2.11/private",
        "http://[2001:db8::10]/private",
        "http://0.0.0.0/private",
        "http://[::]/private",
        "http://224.0.0.1/private",
        "http://255.255.255.255/private",
        "ftp://192.0.2.10/private",
        "http://user@192.0.2.10/private",
        "http://user:password@192.0.2.10/private",
        "http://192.0.2.10/private?token=secret",
        "http://192.0.2.10/private?",
        "http://192.0.2.10/private#fragment",
        "http://192.0.2.10/private#",
        "http://192.0.2.10:0/private",
        "http://192.0.2.10:65536/private",
        " http://192.0.2.10/private",
        "http://192.0.2.10/pri\nvate",
        "http://192.0.2.10\\private",
    ] {
        let config = EventConfig {
            event_service_url: Some(url.to_owned()),
            ..EventConfig::default()
        };
        let error = config.validate(CAMERA_IP).unwrap_err();
        assert!(!format!("{error:#}").contains("private"));
    }
}

#[test]
fn camera_event_policy_debug_redacts_urls_tokens_and_filters() {
    let mut camera: CameraConfig = toml::from_str("ip = '192.0.2.10'").unwrap();
    camera.events = EventConfig {
        event_service_url: Some("http://192.0.2.10/private-event-path".to_owned()),
        source_tokens: vec!["private-source-token".to_owned()],
        include_topics: vec!["{urn:private-topic}Motion".to_owned()],
        exclude_topics: vec!["{urn:private-topic}Audio".to_owned()],
        snapshots: false,
        ..EventConfig::default()
    };
    for output in [format!("{:?}", camera.events), format!("{camera:?}")] {
        assert!(output.contains("event_service_url_configured: true"));
        assert!(output.contains("source_token_count: 1"));
        assert!(output.contains("snapshots: false"));
        assert!(!output.contains("private"));
    }
    let serialized = toml::to_string(&camera).unwrap();
    let round_trip: CameraConfig = toml::from_str(&serialized).unwrap();
    assert_eq!(round_trip.events, camera.events);
}
