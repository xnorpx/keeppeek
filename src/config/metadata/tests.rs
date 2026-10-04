use super::*;

fn fixture() -> StorageToml {
    let root = std::env::temp_dir().join(format!(
        "keeppeek-metadata-binding-{}",
        uuid::Uuid::new_v4()
    ));
    toml::from_str(&format!(
        r#"
[metadata]
volume_id = "metadata-owner"
catalog_file = "catalog-12345678-1234-4234-8234-123456789abc.db"
history_file = "exports-12345678-1234-4234-8234-123456789abc.json"
catalog_id = "87654321-4321-4321-8321-cba987654321"
generation = 2
filesystem = "fixture-filesystem"
root_identity = "fixture-root"

[[named_volumes.volumes]]
id = "metadata-owner"
root = '{}'
roles = ["metadata"]
state = "enabled"
"#,
        root.display()
    ))
    .unwrap()
}

#[test]
fn binding_resolves_canonical_shared_handoff_leaves_under_its_owner_without_io() {
    let storage = fixture();
    let binding = storage.metadata.as_ref().unwrap();
    let root = &storage.named_volumes.as_ref().unwrap().volumes[0].root;
    assert!(!root.exists());
    validate(&storage).unwrap();
    assert_eq!(
        binding.paths(&storage).unwrap(),
        (
            root.join("catalog-12345678-1234-4234-8234-123456789abc.db"),
            root.join("exports-12345678-1234-4234-8234-123456789abc.json"),
        )
    );
    assert_eq!(
        binding.authority(),
        Authority {
            catalog_id: "87654321-4321-4321-8321-cba987654321".to_owned(),
            generation: 2,
        }
    );
    let decoded: StorageToml = toml::from_str(&toml::to_string(&storage).unwrap()).unwrap();
    assert_eq!(decoded.metadata, storage.metadata);
    assert!(!root.exists());
}

#[test]
fn binding_rejects_traversal_noncanonical_and_mismatched_file_ids() {
    let storage = fixture();
    for invalid in [
        "../catalog-12345678-1234-4234-8234-123456789abc.db",
        "..\\catalog-12345678-1234-4234-8234-123456789abc.db",
        "catalog-12345678-1234-4234-8234-123456789ABC.db",
        "catalog-12345678123442348234123456789abc.db",
        "catalog-87654321-4321-4321-8321-cba987654321.db",
    ] {
        let mut next = storage.clone();
        next.metadata.as_mut().unwrap().catalog_file = invalid.to_owned();
        assert!(validate(&next).is_err(), "accepted catalog leaf: {invalid}");
    }
    for invalid in [
        "../exports-12345678-1234-4234-8234-123456789abc.json",
        "..\\exports-12345678-1234-4234-8234-123456789abc.json",
        "exports-12345678-1234-4234-8234-123456789ABC.json",
        "exports-12345678123442348234123456789abc.json",
        "exports-87654321-4321-4321-8321-cba987654321.json",
    ] {
        let mut next = storage.clone();
        next.metadata.as_mut().unwrap().history_file = invalid.to_owned();
        assert!(validate(&next).is_err(), "accepted history leaf: {invalid}");
    }
}

#[test]
fn binding_requires_an_existing_enabled_metadata_owner() {
    let storage = fixture();
    for state in [
        VolumeState::Disabled,
        VolumeState::ReadOnly,
        VolumeState::Draining,
    ] {
        let mut next = storage.clone();
        next.named_volumes.as_mut().unwrap().volumes[0].state = state;
        assert!(validate(&next).is_err());
    }
    let mut next = storage.clone();
    next.named_volumes.as_mut().unwrap().volumes[0].roles = vec![VolumeRole::Export];
    assert!(validate(&next).is_err());
    next = storage.clone();
    next.metadata.as_mut().unwrap().volume_id = VolumeId::parse("missing-owner").unwrap();
    assert!(validate(&next).is_err());
    next = storage;
    next.named_volumes.as_mut().unwrap().volumes.clear();
    assert!(validate(&next).is_err());
    next.named_volumes = None;
    assert!(validate(&next).is_err());
}

#[test]
fn binding_rejects_legacy_override_and_invalid_authority_bounds() {
    let storage = fixture();
    let mut next = storage.clone();
    next.recording_catalog_path = Some("legacy-catalog.db".to_owned());
    assert!(validate(&next).is_err());
    for generation in [0, i64::MAX as u64 + 1] {
        next = storage.clone();
        next.metadata.as_mut().unwrap().generation = generation;
        assert!(validate(&next).is_err());
    }
    next = storage;
    next.metadata.as_mut().unwrap().catalog_id = "not-a-catalog-uuid".to_owned();
    assert!(validate(&next).is_err());
    next.metadata = None;
    validate(&next).unwrap();
}

#[test]
fn settings_cannot_remove_relocate_or_disable_the_metadata_owner() {
    let storage = fixture();
    let mut variants = Vec::new();
    let mut next = storage.clone();
    next.named_volumes.as_mut().unwrap().volumes[0].root =
        std::env::temp_dir().join("other-metadata-root");
    variants.push(next);
    next = storage.clone();
    next.named_volumes.as_mut().unwrap().volumes.clear();
    variants.push(next);
    next = storage.clone();
    next.named_volumes.as_mut().unwrap().volumes[0].roles = vec![VolumeRole::Export];
    variants.push(next);
    next = storage.clone();
    next.named_volumes.as_mut().unwrap().volumes[0].state = VolumeState::Disabled;
    variants.push(next);
    next = storage.clone();
    next.metadata = None;
    variants.push(next);
    next = storage.clone();
    next.metadata.as_mut().unwrap().generation += 1;
    variants.push(next);
    for next in variants {
        assert!(preserve_owner(&storage, &next).is_err());
    }
    let mut next = storage.clone();
    next.short_term_secs += 1;
    next.event_thumbnail_max_mb += 1;
    preserve_owner(&storage, &next).unwrap();
    let mut unowned = storage.clone();
    unowned.metadata = None;
    assert!(preserve_owner(&unowned, &storage).is_err());
    preserve_owner(&unowned, &unowned).unwrap();
}
use crate::config::{Config, load_config, update_settings, write_private_file};

fn managed_config_fixture() -> (PathBuf, Config) {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-managed-config-{}", uuid::Uuid::new_v4()));
    let path = directory.join("config.toml");
    let settings = Config {
        storage: fixture(),
        ..Default::default()
    };
    write_private_file(&path, toml::to_string(&settings).unwrap().as_bytes()).unwrap();
    (path, settings)
}

#[test]
fn managed_metadata_load_and_unrelated_save_preserve_private_references_without_opening_roots() {
    let (path, original) = managed_config_fixture();
    let volume = &original.storage.named_volumes.as_ref().unwrap().volumes[0];
    let mut secrets = toml::Table::new();
    secrets.insert("METADATA_TEST_ID".into(), volume.id.as_str().into());
    secrets.insert(
        "METADATA_TEST_ROOT".into(),
        volume.root.to_str().unwrap().into(),
    );
    write_private_file(
        &path.with_file_name("secrets.toml"),
        toml::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    let mut raw = toml::Value::try_from(&original).unwrap();
    raw["storage"]["named_volumes"]["volumes"][0]["id"] = "{secret:METADATA_TEST_ID}".into();
    raw["storage"]["named_volumes"]["volumes"][0]["root"] = "{secret:METADATA_TEST_ROOT}".into();
    raw["storage"]["metadata"]["volume_id"] = "{secret:METADATA_TEST_ID}".into();
    write_private_file(&path, toml::to_string(&raw).unwrap().as_bytes()).unwrap();
    let mut loaded = load_config(&path).unwrap();
    assert_eq!(loaded.storage.metadata, original.storage.metadata);
    assert_eq!(loaded.storage.named_volumes, original.storage.named_volumes);
    assert!(!volume.root.exists());
    loaded.port = 9091;
    let saved = update_settings(&path, &loaded).unwrap();
    assert_eq!(saved.storage.metadata, original.storage.metadata);
    let persisted: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        persisted["storage"]["metadata"]["volume_id"].as_str(),
        Some("{secret:METADATA_TEST_ID}")
    );
    let raw_volume = &persisted["storage"]["named_volumes"]["volumes"][0];
    assert_eq!(raw_volume["id"].as_str(), Some("{secret:METADATA_TEST_ID}"));
    assert_eq!(
        raw_volume["root"].as_str(),
        Some("{secret:METADATA_TEST_ROOT}")
    );
    let reopened = load_config(&path).unwrap();
    assert_eq!(reopened.port, 9091);
    assert_eq!(reopened.storage.metadata, original.storage.metadata);
    assert_eq!(
        reopened.storage.named_volumes,
        original.storage.named_volumes
    );
    assert!(!volume.root.exists());
}

#[test]
fn managed_metadata_owner_changes_fail_before_configuration_mutation() {
    let (path, _) = managed_config_fixture();
    let original = load_config(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    for change in 0..5 {
        let mut next = original.clone();
        match change {
            0 => next.storage.named_volumes.as_mut().unwrap().volumes.clear(),
            1 => {
                next.storage.named_volumes.as_mut().unwrap().volumes[0].root =
                    path.with_file_name("replacement-root");
            }
            2 => {
                next.storage.named_volumes.as_mut().unwrap().volumes[0].state =
                    VolumeState::Disabled;
            }
            3 => {
                next.storage.named_volumes.as_mut().unwrap().volumes[0].roles =
                    vec![VolumeRole::Export];
            }
            4 => next.storage.metadata = None,
            _ => unreachable!(),
        }
        assert!(
            update_settings(&path, &next).is_err(),
            "accepted owner change {change}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_file_name("replacement-root").exists());
    }
    assert_eq!(
        load_config(&path).unwrap().storage.metadata,
        original.storage.metadata
    );
}

#[test]
fn ordinary_settings_cannot_forge_a_new_metadata_binding() {
    let (path, mut settings) = managed_config_fixture();
    let proposed = settings.storage.metadata.take().unwrap();
    settings.storage.named_volumes.as_mut().unwrap().volumes[0].state = VolumeState::Disabled;
    write_private_file(&path, toml::to_string(&settings).unwrap().as_bytes()).unwrap();
    let mut loaded = load_config(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    loaded.storage.metadata = Some(proposed);
    loaded.storage.named_volumes.as_mut().unwrap().volumes[0].state = VolumeState::Enabled;
    assert!(update_settings(&path, &loaded).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(load_config(&path).unwrap().storage.metadata.is_none());
}

#[test]
fn metadata_exception_does_not_activate_unbound_or_media_writing_volumes() {
    let (path, settings) = managed_config_fixture();
    for change in 0..3 {
        let mut candidate = settings.clone();
        match change {
            0 => candidate.storage.metadata = None,
            1 => candidate.storage.named_volumes.as_mut().unwrap().volumes[0]
                .roles
                .push(VolumeRole::Active),
            2 => {
                let volumes = &mut candidate.storage.named_volumes.as_mut().unwrap().volumes;
                let mut media = volumes[0].clone();
                media.id = VolumeId::parse("other-media").unwrap();
                media.root = path.with_file_name("other-media-root");
                media.roles = vec![VolumeRole::Active];
                volumes.push(media);
            }
            _ => unreachable!(),
        }
        let bytes = toml::to_string(&candidate).unwrap();
        write_private_file(&path, bytes.as_bytes()).unwrap();
        assert!(load_config(&path).is_err(), "accepted activation {change}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        assert!(!path.with_file_name("other-media-root").exists());
    }
}

#[test]
fn atomic_metadata_configuration_write_accepts_relative_leaf() -> anyhow::Result<()> {
    let path = PathBuf::from(format!(".keeppeek-atomic-{}.toml", uuid::Uuid::new_v4()));
    let result = crate::config::write_private_file_atomically(&path, b"first")
        .and_then(|()| crate::config::write_private_file_atomically(&path, b"second"));
    let bytes = std::fs::read(&path);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    result?;
    assert_eq!(bytes?, b"second");
    Ok(())
}
