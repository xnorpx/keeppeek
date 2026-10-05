use super::*;

fn fixture() -> VolumeConfiguration {
    let text = r#"
        [[volumes]]
        id = "primary"
        root = "/recordings/primary"
        roles = ["active", "archive"]
        capacity_bytes = 1000
        minimum_free_bytes = 100
        warning_free_bytes = 200
        critical_free_bytes = 100
        [[volumes]]
        id = "secondary"
        root = "/recordings/secondary"
        roles = ["active", "archive"]
        capacity_bytes = 2000
        [[placement]]
        role = "active"
        candidates = ["primary", "secondary"]
        allow_fallback = true
        "#;
    let text = if cfg!(windows) {
        text.replace("/recordings/", "C:/recordings/")
    } else {
        text.to_owned()
    };
    toml::from_str(&text).unwrap()
}

fn request() -> PlacementRequest<'static> {
    PlacementRequest {
        role: VolumeRole::Active,
        source: "front",
        group: "outside",
        required_bytes: 50,
    }
}

fn observations() -> Vec<VolumeObservation> {
    vec![
        VolumeObservation {
            id: VolumeId::parse("primary").unwrap(),
            health: VolumeHealth::Online,
            draining: false,
            total_bytes: 4000,
            available_bytes: 500,
            owned_bytes: 500,
        },
        VolumeObservation {
            id: VolumeId::parse("secondary").unwrap(),
            health: VolumeHealth::Online,
            draining: false,
            total_bytes: 4000,
            available_bytes: 1000,
            owned_bytes: 100,
        },
    ]
}

#[test]
fn operational_drain_requires_explicit_placement_fallback() {
    let mut configuration = fixture();
    let mut samples = observations();
    samples[0].draining = true;
    let decision = configuration.place(&request(), &samples).unwrap();
    assert_eq!(
        decision.selected.as_ref().map(VolumeId::as_str),
        Some("secondary")
    );
    assert!(
        decision
            .rejected
            .iter()
            .any(|item| item.reason == RejectionReason::Draining)
    );
    configuration.placement[0].allow_fallback = false;
    assert!(
        configuration
            .place(&request(), &samples)
            .unwrap()
            .selected
            .is_none()
    );
}

#[test]
fn volume_placement_resolves_multiple_groups_without_configuration_order_dependence() {
    let mut configuration = fixture();
    let base = configuration.placement[0].clone();
    for (group, candidate) in [("zulu", "primary"), ("alpha", "secondary")] {
        let mut rule = base.clone();
        rule.group = Some(group.to_owned());
        rule.candidates = vec![VolumeId::parse(candidate).unwrap()];
        configuration.placement.push(rule);
    }
    configuration.volumes[1].groups = vec!["zulu".into()];
    for groups in [["alpha", "zulu"], ["zulu", "alpha"]] {
        let decision = configuration
            .place_with_groups(&request(), &groups, &observations())
            .unwrap();
        assert_eq!(
            decision.selected.as_ref().map(VolumeId::as_str),
            Some("secondary")
        );
        configuration.placement.reverse();
    }
    let mut source = base;
    source.source = Some(request().source.into());
    source.candidates = vec![VolumeId::parse("primary").unwrap()];
    configuration.placement.push(source);
    let decision = configuration
        .place_with_groups(&request(), &["alpha", "zulu"], &observations())
        .unwrap();
    assert_eq!(
        decision.selected.as_ref().map(VolumeId::as_str),
        Some("primary")
    );
}

#[test]
fn volume_placement_falls_back_only_when_explicitly_allowed() {
    let mut config = fixture();
    config.validate().unwrap();
    let mut observations = observations();
    observations[0].health = VolumeHealth::Offline;
    let decision = config.place(&request(), &observations).unwrap();
    assert_eq!(
        decision.selected.as_ref().map(VolumeId::as_str),
        Some("secondary")
    );
    assert_eq!(decision.rejected[0].reason, RejectionReason::Offline);
    config.placement[0].allow_fallback = false;
    let decision = config.place(&request(), &observations).unwrap();
    assert!(decision.selected.is_none());
}

#[test]
fn volume_placement_honors_reserved_space_and_owned_byte_cap() {
    let config = fixture();
    let mut observations = observations();
    observations[0].available_bytes = 149;
    observations[1].owned_bytes = 1951;
    let decision = config.place(&request(), &observations).unwrap();
    assert!(decision.selected.is_none());
    assert_eq!(
        decision.rejected[0].reason,
        RejectionReason::InsufficientSpace
    );
    assert_eq!(
        decision.rejected[1].reason,
        RejectionReason::CapacityExceeded
    );
    observations[0].available_bytes = 150;
    assert_eq!(
        config
            .place(&request(), &observations)
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "primary"
    );
}

#[test]
fn volume_configuration_rejects_ambiguous_and_unsafe_inputs() {
    let original = fixture();
    let mut config = original.clone();
    config.volumes[1].root = config.volumes[0].root.join("child");
    assert!(config.validate().is_err());
    config = original.clone();
    config.volumes[1].id = config.volumes[0].id.clone();
    assert!(config.validate().is_err());
    config = original.clone();
    config.placement[0].candidates[0] = VolumeId::parse("missing").unwrap();
    assert!(config.validate().is_err());
    config = original;
    config.placement.push(config.placement[0].clone());
    assert!(config.validate().is_err());
    for invalid in ["", "../escape", "upperCase", "white space", "legacy-active"] {
        assert!(VolumeId::parse(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn volume_placement_rejects_unavailable_states_and_missing_evidence() {
    let mut config = fixture();
    for state in [
        VolumeState::Disabled,
        VolumeState::Draining,
        VolumeState::ReadOnly,
    ] {
        config.volumes[0].state = state;
        config.volumes[1].state = state;
        assert!(
            config
                .place(&request(), &observations())
                .unwrap()
                .selected
                .is_none()
        );
    }
    config.volumes[0].state = VolumeState::Enabled;
    assert!(config.place(&request(), &[]).unwrap().selected.is_none());
}

#[test]
fn volume_placement_uses_source_override_and_stable_priority_ties() {
    let mut config = fixture();
    let mut rule = config.placement[0].clone();
    rule.source = Some("front".into());
    rule.candidates.reverse();
    config.placement.push(rule);
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "primary"
    );
    config.placement[1].allow_fallback = false;
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "secondary"
    );
}

#[test]
fn volume_placement_free_space_strategy_accounts_for_cap_and_reserve() {
    let mut config = fixture();
    config.placement[0].strategy = PlacementStrategy::FreeSpace;
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "secondary"
    );
    config.volumes[1].capacity_bytes = Some(400);
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "primary"
    );
}

#[cfg(windows)]
#[test]
fn volume_roots_reject_windows_devices_and_ambiguous_paths() {
    let mut config = fixture();
    for root in [
        r"C:\recordings\CON",
        r"C:\recordings\NUL.txt",
        r"C:\recordings\AUX",
        r"C:\recordings\COM1",
        r"C:\recordings\LPT9.log",
        "C:\\recordings\\COM\u{b9}.txt",
        "C:\\recordings\\LPT\u{b2}",
        r"C:\recordings\bad*name",
        r"C:\recordings\bad?name",
        r"C:\recordings\bad|name",
        r"C:\recordings\bad<name",
        r"C:\recordings\bad>name",
        "C:\\recordings\\bad\"name",
        r"C:\recordings\bad:name",
        r"C:\recordings\bad.",
        r"C:\recordings\bad ",
        r"C:\recordings\..\elsewhere",
        r"\\server\share",
        r"\\?\C:\recordings",
        r"C:recordings",
        r"\recordings",
    ] {
        config.volumes[0].root = root.into();
        assert!(config.validate().is_err(), "accepted {root}");
    }
    config.volumes[0].root = r"C:\recordings\SECONDARY".into();
    assert!(config.validate().is_err());
    config.volumes[0].root = r"C:\recordings\secondary-other".into();
    assert!(config.validate().is_ok());
}

#[test]
fn volume_placement_override_failure_does_not_bypass_source_policy() {
    let mut config = fixture();
    let mut group = config.placement[0].clone();
    group.group = Some("outside".into());
    group.candidates.reverse();
    group.allow_fallback = false;
    config.placement.push(group);
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "secondary"
    );
    let mut source = config.placement[0].clone();
    source.source = Some("front".into());
    source.allow_fallback = false;
    config.placement.push(source);
    let mut observations = observations();
    observations[0].health = VolumeHealth::Offline;
    assert!(
        config
            .place(&request(), &observations)
            .unwrap()
            .selected
            .is_none()
    );
}

#[test]
fn volume_allowlists_accept_either_source_or_group_and_otherwise_deny() {
    let mut config = fixture();
    config.placement[0].allow_fallback = false;
    config.volumes[0].sources = vec!["back".into()];
    config.volumes[0].groups = vec!["inside".into()];
    let decision = config.place(&request(), &observations()).unwrap();
    assert_eq!(decision.rejected[0].reason, RejectionReason::SourceDenied);
    assert!(decision.selected.is_none());
    config.volumes[0].groups[0] = "outside".into();
    assert!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .is_some()
    );
    config.volumes[0].groups[0] = "inside".into();
    config.volumes[0].sources[0] = "front".into();
    assert!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .is_some()
    );
}

#[test]
fn volume_placement_respects_priority_and_critical_reserve() {
    let mut config = fixture();
    config.volumes[1].priority = 1;
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "primary"
    );
    config.volumes[0].priority = 2;
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "secondary"
    );
    config.volumes[1].critical_free_bytes = 951;
    config.volumes[1].warning_free_bytes = 1000;
    assert_eq!(
        config
            .place(&request(), &observations())
            .unwrap()
            .selected
            .unwrap()
            .as_str(),
        "primary"
    );
}

#[test]
fn volume_placement_rejects_duplicate_or_impossible_capacity_evidence() {
    let config = fixture();
    let mut evidence = observations();
    evidence.push(evidence[0].clone());
    assert!(config.place(&request(), &evidence).is_err());
    evidence.pop();
    evidence[0].available_bytes = evidence[0].total_bytes + 1;
    assert!(config.place(&request(), &evidence).is_err());
    let mut request = request();
    request.required_bytes = 0;
    assert!(config.place(&request, &observations()).is_err());
}

#[test]
fn volume_configuration_enforces_collection_boundaries() {
    let mut config = fixture();
    let volume = config.volumes[0].clone();
    config.volumes = (0..33)
        .map(|index| {
            let mut volume = volume.clone();
            volume.id = VolumeId::parse(&format!("volume-{index}")).unwrap();
            volume.root = std::env::temp_dir().join(format!("volume-{index}"));
            volume
        })
        .collect();
    config.placement.clear();
    assert!(config.validate().is_err());
    config.volumes.pop();
    assert!(config.validate().is_ok());
    let mut rule = fixture().placement.remove(0);
    rule.candidates = config.volumes[..9]
        .iter()
        .map(|volume| volume.id.clone())
        .collect();
    config.placement.push(rule.clone());
    assert!(config.validate().is_err());
    config.placement[0].candidates.pop();
    assert!(config.validate().is_ok());
    rule = config.placement.remove(0);
    config.placement = (0..257)
        .map(|index| {
            let mut rule = rule.clone();
            rule.source = Some(format!("camera-{index}"));
            rule
        })
        .collect();
    assert!(config.validate().is_err());
    config.placement.pop();
    assert!(config.validate().is_ok());
    config.volumes[0].sources = (0..257).map(|index| format!("source-{index}")).collect();
    assert!(config.validate().is_err());
    config.volumes[0].sources.pop();
    assert!(config.validate().is_ok());
}

#[test]
fn volume_configuration_rejects_duplicate_roles_candidates_and_invalid_metadata_rules() {
    let original = fixture();
    let mut config = original.clone();
    config.volumes[0].roles.push(VolumeRole::Archive);
    assert!(config.validate().is_err());
    config = original.clone();
    let duplicate = config.placement[0].candidates[0].clone();
    config.placement[0].candidates.push(duplicate);
    assert!(config.validate().is_err());
    config = original.clone();
    config.volumes[0].warning_free_bytes = 99;
    assert!(config.validate().is_err());
    config = original;
    config.volumes[0].roles.push(VolumeRole::Metadata);
    config.volumes[0].sources.push("front".into());
    assert!(config.validate().is_err());
    config.volumes[0].sources.clear();
    config.volumes[1].roles.push(VolumeRole::Metadata);
    assert!(config.validate().is_ok());
    config.volumes[1].roles.pop();
    let mut metadata = config.placement[0].clone();
    metadata.role = VolumeRole::Metadata;
    metadata.candidates.truncate(1);
    config.placement.push(metadata);
    assert!(config.validate().is_err());
    config.placement[1].allow_fallback = false;
    assert!(config.validate().is_ok());
}

#[test]
fn volume_configuration_bounds_root_paths_and_redacts_debug_output() {
    let mut config = fixture();
    let root = config.volumes[0].root.clone();
    config.volumes[0].root = root.join("a".repeat(4096));
    assert!(config.validate().is_err());
    config.volumes[0].root = (0..65).fold(root.clone(), |path, _| path.join("a"));
    assert!(config.validate().is_err());
    config.volumes[0].root = root.join("\0");
    assert!(config.validate().is_err());
    config.volumes[0].root = root;
    let debug = format!("{config:?}");
    assert!(!debug.contains("recordings"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn volume_configuration_roundtrip_preserves_roles_rules_and_unknown_field_rejection() {
    let config = fixture();
    let text = toml::to_string(&config).unwrap();
    assert_eq!(
        toml::from_str::<VolumeConfiguration>(&text).unwrap(),
        config
    );
    assert!(
        toml::from_str::<VolumeConfiguration>(&text.replace("capacity_bytes", "capacity_byte"))
            .is_err()
    );
    let mut config = config;
    config.volumes[0].capacity_bytes = Some(0);
    assert!(config.validate().is_err());
}
