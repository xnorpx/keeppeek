use keeppeek::{config, storage::retention::Interval};
use std::{net::IpAddr, path::PathBuf};

struct Fixture(PathBuf);

impl Fixture {
    fn new(settings: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "keeppeek-retention-settings-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("config.toml"),
            format!(
                "[cameras.front]\nip = '192.0.2.8'\n[cameras.back]\nip = '192.0.2.9'\n{settings}"
            ),
        )
        .unwrap();
        Self(path)
    }

    fn load(&self) -> anyhow::Result<config::Config> {
        config::load_config(&self.0.join("config.toml"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

const RULES: &str = r#"
[recording_retention]
enabled = true
rules = [
  { id = "continuous", class = "continuous", duration_ms = 43200000, mode = "all" },
  { id = "alert", class = "alert", duration_ms = 86400000, mode = "all" }
]
"#;

#[test]
fn retention_configuration_bounds_camera_override_count() {
    for count in [4096, 4097] {
        let text = (0..count)
            .map(|index| {
                format!(
                    "[cameras.'10.0.{}.{}']\nenabled=false\n",
                    index / 256,
                    index % 256
                )
            })
            .collect::<String>();
        assert_eq!(
            toml::from_str::<config::retention::Settings>(&text).is_ok(),
            count == 4096
        );
    }
}

#[test]
fn direct_retention_deserialization_rejects_conflicting_and_unbounded_settings() {
    let duplicate = "event_mappings=[{source='camera',kind='Motion',evidence='motion'},{source='camera',kind='Motion',evidence='alert'}]";
    assert!(toml::from_str::<config::retention::Settings>(duplicate).is_err());
    let mappings = (0..17)
        .map(|index| format!("{{source='camera',kind='kind-{index}',evidence='motion'}}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        toml::from_str::<config::retention::Settings>(&format!("event_mappings=[{mappings}]"))
            .is_err()
    );
    assert!(
        toml::from_str::<config::retention::Settings>(
            "[cameras.'192.0.2.8'.rules.unknown]\nduration_ms=0"
        )
        .is_err()
    );
}

#[test]
fn retention_configuration_is_disabled_by_default_and_zero_overrides_inheritance() {
    let ip: IpAddr = "192.0.2.8".parse().unwrap();
    assert!(
        Fixture::new("")
            .load()
            .unwrap()
            .recording_retention
            .policy_for(ip)
            .unwrap()
            .is_none()
    );
    let fixture = Fixture::new(&format!(
        "{RULES}\n[recording_retention.cameras.'192.0.2.8'.rules.continuous]\nduration_ms = 0\n"
    ));
    let loaded = fixture.load().unwrap();
    let media = Interval::new(1_000, 2_000).unwrap();
    let disabled_rule = loaded.recording_retention.policy_for(ip).unwrap().unwrap();
    assert_eq!(
        disabled_rule.resolve(media, &[], None).unwrap().deadline_ms,
        None
    );
    let inherited = loaded
        .recording_retention
        .policy_for("192.0.2.9".parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        inherited.resolve(media, &[], None).unwrap().deadline_ms,
        Some(43_202_000)
    );
}

#[test]
fn retention_rollout_and_camera_disable_are_hard_bounds() {
    let ip = "192.0.2.8".parse().unwrap();
    for settings in [
        format!(
            "{}\n[recording_retention.cameras.'192.0.2.8']\nenabled=true",
            RULES.replace("enabled = true", "enabled = false")
        ),
        format!("{RULES}\n[recording_retention.cameras.'192.0.2.8']\nenabled=false"),
    ] {
        assert!(
            Fixture::new(&settings)
                .load()
                .unwrap()
                .recording_retention
                .policy_for(ip)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn retention_configuration_rejects_invalid_rules_references_and_durations_at_startup() {
    for settings in [
        RULES.replace("43200000", "-1"),
        RULES.replace("43200000", "0.5"),
        RULES.replace("43200000", "18446744073709551615"),
        RULES.replace("id = \"alert\"", "id = \"continuous\""),
        format!("{RULES}\n[recording_retention.cameras.'192.0.2.10']\nenabled=false"),
        format!("{RULES}\n[recording_retention.cameras.'192.0.2.8'.rules.unknown]\nduration_ms=0"),
    ] {
        assert!(Fixture::new(&settings).load().is_err());
    }
}

#[test]
fn removing_camera_removes_only_its_retention_override() {
    let fixture = Fixture::new(&format!(
        "{RULES}\n[recording_retention.cameras.'192.0.2.8']\nenabled=false\n[recording_retention.cameras.'192.0.2.9']\nenabled=false"
    ));
    let path = fixture.0.join("config.toml");
    config::remove_camera(&path, "192.0.2.8".parse().unwrap()).unwrap();
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cameras = table["recording_retention"]["cameras"].as_table().unwrap();
    assert!(!cameras.contains_key("192.0.2.8"));
    assert!(cameras.contains_key("192.0.2.9"));
    fixture.load().unwrap();
}

#[test]
fn retention_configuration_bounds_rule_and_mapping_counts_and_kind_bytes() {
    for count in [16, 17] {
        let rules = (0..count)
            .map(|index| {
                format!("{{id='rule-{index}',class='continuous',duration_ms=1,mode='all'}}")
            })
            .collect::<Vec<_>>()
            .join(",");
        let mappings = (0..count)
            .map(|index| format!("{{source='camera',kind='kind-{index}',evidence='motion'}}"))
            .collect::<Vec<_>>()
            .join(",");
        for settings in [
            format!("[recording_retention]\nrules=[{rules}]"),
            format!("[recording_retention]\nevent_mappings=[{mappings}]"),
        ] {
            assert_eq!(Fixture::new(&settings).load().is_ok(), count == 16);
        }
    }
    for (kind, valid) in [
        ("é".repeat(64), true),
        ("é".repeat(65), false),
        (String::new(), false),
    ] {
        let settings = format!(
            "[recording_retention]\nevent_mappings=[{{source='camera',kind='{kind}',evidence='motion'}}]"
        );
        assert_eq!(Fixture::new(&settings).load().is_ok(), valid);
    }
    let duplicate = "[recording_retention]\nevent_mappings=[{source='camera',kind='Motion',evidence='motion'},{source='camera',kind='Motion',evidence='alert'}]";
    assert!(Fixture::new(duplicate).load().is_err());
}

#[test]
fn retention_configuration_rejects_duplicate_ipv6_spellings() {
    let fixture = Fixture::new(
        "[cameras.ipv6]\nip='2001:db8::1'\n[recording_retention.cameras.'2001:db8::1']\nenabled=false\n[recording_retention.cameras.'2001:0db8:0:0:0:0:0:1']\nenabled=false",
    );
    assert!(fixture.load().is_err());
}

#[test]
fn retention_mapping_is_exact_and_camera_mapping_override_replaces_defaults() {
    use keeppeek::storage::{metadata::EventSource, retention::EvidenceKind};
    let fixture = Fixture::new(
        "[recording_retention]\nevent_mappings=[{source='camera',kind='Motion',evidence='motion'}]\n[recording_retention.cameras.'192.0.2.8']\nevent_mappings=[]",
    );
    let settings = fixture.load().unwrap().recording_retention;
    let front = "192.0.2.8".parse().unwrap();
    let back = "192.0.2.9".parse().unwrap();
    assert_eq!(
        settings.classify(front, EventSource::Camera, "Motion"),
        None
    );
    assert_eq!(
        settings.classify(back, EventSource::Camera, "Motion"),
        Some(EvidenceKind::Motion)
    );
    assert_eq!(settings.classify(back, EventSource::Camera, "motion"), None);
    assert_eq!(
        settings.classify(back, EventSource::KeepPeek, "Motion"),
        None
    );
}

#[test]
fn camera_updates_preserve_sparse_retention_and_secret_references() {
    let fixture = Fixture::new(&format!(
        "{RULES}\n[recording_retention.cameras.'192.0.2.8'.rules.continuous]\nduration_ms=0\n[camera_defaults]\nusername='operator'\npassword='{{secret:CAMERA_PASSWORD}}'"
    ));
    let path = fixture.0.join("config.toml");
    std::fs::write(
        fixture.0.join("secrets.toml"),
        "CAMERA_PASSWORD='synthetic-secret'\n",
    )
    .unwrap();
    let before: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut camera = config::load_cameras(&path).unwrap()["cameras"][0].clone();
    camera.display_name = Some("Updated".into());
    config::upsert_camera(&path, &camera).unwrap();
    let settings = fixture.load().unwrap();
    config::update_settings(&path, &settings).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let after: toml::Table = toml::from_str(&raw).unwrap();
    assert_eq!(before["recording_retention"], after["recording_retention"]);
    assert!(raw.contains("{secret:CAMERA_PASSWORD}"));
    assert!(!raw.contains("synthetic-secret"));
    fixture.load().unwrap();
}
