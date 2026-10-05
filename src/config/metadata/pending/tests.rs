use super::*;
use crate::config::{Config, Secrets, write_private_file};
use crate::storage::{RecordingCatalog, catalog::authority::Lease};

fn source_storage(directory: &Path) -> StorageToml {
    toml::from_str(&format!(
        r#"
medium_term_path = '{}'
long_term_path = '{}'
event_thumbnail_path = '{}'
recording_catalog_path = '{}'
[[named_volumes.volumes]]
id = 'metadata-owner'
root = '{}'
roles = ['metadata']
state = 'disabled'
"#,
        directory.join("active").display(),
        directory.join("archive").display(),
        directory.join("thumbnails").display(),
        directory.join("custom/source.db").display(),
        directory.join("metadata").display(),
    ))
    .unwrap()
}

fn active_authority(path: &Path) -> crate::storage::catalog::authority::Authority {
    let mut lease = Lease::acquire(path).unwrap();
    let connection = lease.connect().unwrap();
    let authority = lease.verify(&connection).unwrap();
    drop(connection);
    authority
}

fn fixture() -> (PathBuf, toml::Table, MetadataBinding) {
    let directory = std::env::temp_dir().join(format!(
        "keeppeek-pending-metadata-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    let directory = std::fs::canonicalize(directory).unwrap();
    for child in ["custom", "metadata", "archive/.exports"] {
        std::fs::create_dir_all(directory.join(child)).unwrap();
    }
    let config = Config {
        storage: source_storage(&directory),
        ..Default::default()
    };
    let storage = StorageConfig::from_toml(&config.storage);
    RecordingCatalog::open(&storage.recording_catalog_path)
        .unwrap()
        .shutdown();
    std::fs::write(
        storage.long_term_path.join(".exports/history.json"),
        b"{\"version\":1,\"jobs\":[]}\n",
    )
    .unwrap();
    let owner = active_authority(&storage.recording_catalog_path);
    let handoff = uuid::Uuid::new_v4();
    let root_identity = crate::storage::volumes::root::Root::open(&directory.join("metadata"))
        .unwrap()
        .identity()
        .clone();
    let target = MetadataBinding {
        volume_id: VolumeId::parse("metadata-owner").unwrap(),
        catalog_file: format!("catalog-{handoff}.db"),
        history_file: format!("exports-{handoff}.json"),
        catalog_id: owner.catalog_id,
        generation: owner.generation + 1,
        filesystem: root_identity.filesystem,
        root_identity: root_identity.directory,
    };
    let pending = Pending {
        source_digest: fingerprint(&config.storage).unwrap(),
        target: target.clone(),
    };
    let mut root = toml::Value::try_from(&config)
        .unwrap()
        .as_table()
        .unwrap()
        .clone();
    root["storage"]
        .as_table_mut()
        .unwrap()
        .insert(PENDING.into(), toml::Value::try_from(pending).unwrap());
    let path = directory.join("config.toml");
    write_private_file(&path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
    (path, root, target)
}

fn assert_committed(path: &Path, root: &toml::Table, target: &MetadataBinding) {
    let persisted: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(&persisted, root);
    assert!(root["storage"].get(PENDING).is_none());
    let config = crate::config::config_from_table(root, &Secrets::default()).unwrap();
    assert_eq!(config.storage.metadata.as_ref(), Some(target));
    let (catalog, history) = target.paths(&config.storage).unwrap();
    assert_eq!(active_authority(&catalog), target.authority());
    let directory = path.parent().unwrap();
    assert_eq!(
        std::fs::read(history).unwrap(),
        std::fs::read(directory.join("archive/.exports/history.json")).unwrap()
    );
    let source = directory.join("custom/source.db");
    assert!(source.is_file());
    assert!(RecordingCatalog::open(&source).is_err());
    RecordingCatalog::open_managed(&catalog, &target.authority(), &target.root_identity())
        .unwrap()
        .shutdown();
}

#[test]
fn pending_metadata_handoff_commits_matching_authority_and_preserves_sources() {
    let (path, mut root, target) = fixture();
    apply(&path, &mut root, &Secrets::default()).unwrap();
    assert_committed(&path, &root, &target);
}

#[test]
fn invalid_pending_candidate_rejects_before_fencing_or_writing_configuration() {
    let (path, mut root, target) = fixture();
    let source = path.parent().unwrap().join("custom/source.db");
    let before_owner = active_authority(&source);
    root["storage"][PENDING]["target"]["generation"] = 0.into();
    write_private_file(&path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let pending = root.clone();
    assert!(apply(&path, &mut root, &Secrets::default()).is_err());
    assert_eq!(root, pending);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(active_authority(&source), before_owner);
    assert!(
        !path
            .parent()
            .unwrap()
            .join("metadata")
            .join(target.catalog_file)
            .exists()
    );
    RecordingCatalog::open(&source).unwrap().shutdown();
}

#[test]
fn pending_handoff_rejects_live_source_lease_without_consuming_marker() {
    let (path, mut root, target) = fixture();
    let source = path.parent().unwrap().join("custom/source.db");
    let before = std::fs::read(&path).unwrap();
    let pending = root.clone();
    let lease = Lease::acquire(&source).unwrap();
    assert!(apply(&path, &mut root, &Secrets::default()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(root, pending);
    assert!(
        !path
            .parent()
            .unwrap()
            .join("metadata")
            .join(target.catalog_file)
            .exists()
    );
    drop(lease);
    RecordingCatalog::open(&source).unwrap().shutdown();
}

#[test]
fn pending_handoff_replays_completed_transfer_after_configuration_commit_failure() {
    let (path, mut root, target) = fixture();
    let directory = path.parent().unwrap();
    let blocked = directory.join("blocked-config.toml");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("unrelated"), b"retained").unwrap();
    let before = std::fs::read(&path).unwrap();
    let pending = root.clone();
    assert!(apply(&blocked, &mut root, &Secrets::default()).is_err());
    assert_eq!(root, pending);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::read(blocked.join("unrelated")).unwrap(),
        b"retained"
    );
    let catalog = directory.join("metadata").join(&target.catalog_file);
    let activated = active_authority(&catalog);
    assert_eq!(activated, target.authority());
    assert!(RecordingCatalog::open(&directory.join("custom/source.db")).is_err());
    let mut replay = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    apply(&path, &mut replay, &Secrets::default()).unwrap();
    assert_committed(&path, &replay, &target);
    assert_eq!(active_authority(&catalog), activated);
}
fn repeat_metadata_handoff(path: &Path, destination_id: &str) -> MetadataBinding {
    let current = crate::config::load_config(path).unwrap();
    let owner = current.storage.metadata.as_ref().unwrap();
    let volume = current
        .storage
        .named_volumes
        .as_ref()
        .unwrap()
        .volumes
        .iter()
        .find(|volume| volume.id.as_str() == destination_id)
        .unwrap();
    let identity = crate::storage::volumes::root::Root::open(&volume.root)
        .unwrap()
        .identity()
        .clone();
    let handoff = uuid::Uuid::new_v4();
    let target = MetadataBinding {
        volume_id: volume.id.clone(),
        catalog_file: format!("catalog-{handoff}.db"),
        history_file: format!("exports-{handoff}.json"),
        catalog_id: owner.catalog_id.clone(),
        generation: owner.generation + 1,
        filesystem: identity.filesystem,
        root_identity: identity.directory,
    };
    let pending = Pending {
        source_digest: fingerprint(&current.storage).unwrap(),
        target: target.clone(),
    };
    let mut root: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    root["storage"]
        .as_table_mut()
        .unwrap()
        .insert(PENDING.into(), toml::Value::try_from(pending).unwrap());
    write_private_file(path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
    apply(path, &mut root, &Secrets::default()).unwrap();
    let saved = crate::config::load_config(path).unwrap();
    assert_eq!(saved.storage.metadata.as_ref(), Some(&target));
    let (catalog, _) = target.paths(&saved.storage).unwrap();
    assert_eq!(active_authority(&catalog), target.authority());
    target
}

fn add_disabled_metadata_destination(path: &Path) {
    let mut config = crate::config::load_config(path).unwrap();
    let volumes = &mut config.storage.named_volumes.as_mut().unwrap().volumes;
    let mut next = volumes[0].clone();
    next.id = VolumeId::parse("other-metadata").unwrap();
    next.root = path.parent().unwrap().join("other-metadata");
    next.state = VolumeState::Disabled;
    std::fs::create_dir(&next.root).unwrap();
    volumes.push(next);
    let saved = crate::config::update_settings(path, &config).unwrap();
    assert_eq!(saved.storage.metadata, config.storage.metadata);
}

#[test]
fn repeated_metadata_handoff_and_return_keep_latest_history_and_prior_fences() {
    let (path, mut root, first) = fixture();
    apply(&path, &mut root, &Secrets::default()).unwrap();
    let initial = crate::config::load_config(&path).unwrap();
    let (first_catalog, first_history) = first.paths(&initial.storage).unwrap();
    add_disabled_metadata_destination(&path);
    let history_b = b"{\n  \"version\": 1,\n  \"jobs\": []\n}\n";
    std::fs::write(&first_history, history_b).unwrap();
    let second = repeat_metadata_handoff(&path, "other-metadata");
    assert_eq!(second.catalog_id, first.catalog_id);
    assert_eq!(second.generation, first.generation + 1);
    let config = crate::config::load_config(&path).unwrap();
    let (second_catalog, second_history) = second.paths(&config.storage).unwrap();
    assert_eq!(std::fs::read(&second_history).unwrap(), history_b);
    assert!(RecordingCatalog::open(&first_catalog).is_err());
    let fenced_b = std::fs::read(&first_catalog).unwrap();
    let history_c = b"{ \"jobs\": [], \"version\": 1 }\n";
    std::fs::write(&second_history, history_c).unwrap();
    let returned = repeat_metadata_handoff(&path, "metadata-owner");
    assert_eq!(returned.catalog_id, first.catalog_id);
    assert_eq!(returned.generation, second.generation + 1);
    assert_ne!(returned.catalog_file, first.catalog_file);
    assert_ne!(returned.history_file, first.history_file);
    let final_config = crate::config::load_config(&path).unwrap();
    let (returned_catalog, returned_history) = returned.paths(&final_config.storage).unwrap();
    assert_ne!(returned_catalog, first_catalog);
    assert_eq!(std::fs::read(returned_history).unwrap(), history_c);
    assert_eq!(std::fs::read(&second_history).unwrap(), history_c);
    assert!(RecordingCatalog::open(&second_catalog).is_err());
    assert!(RecordingCatalog::open(&first_catalog).is_err());
    assert_eq!(std::fs::read(&first_catalog).unwrap(), fenced_b);
    assert_eq!(std::fs::read(&first_history).unwrap(), history_b);
    for volume in &final_config.storage.named_volumes.as_ref().unwrap().volumes {
        let expected = if volume.id == returned.volume_id {
            VolumeState::Enabled
        } else {
            VolumeState::Disabled
        };
        assert_eq!(volume.state, expected);
    }
    RecordingCatalog::open_managed(
        &returned_catalog,
        &returned.authority(),
        &returned.root_identity(),
    )
    .unwrap()
    .shutdown();
}
fn stage_other_metadata_root(path: &Path) -> (toml::Table, MetadataBinding) {
    let config = crate::config::load_config(path).unwrap();
    let mut target = config.storage.metadata.as_ref().unwrap().clone();
    let handoff = uuid::Uuid::new_v4();
    let identity =
        crate::storage::volumes::root::Root::open(&path.parent().unwrap().join("other-metadata"))
            .unwrap()
            .identity()
            .clone();
    target.volume_id = VolumeId::parse("other-metadata").unwrap();
    target.catalog_file = format!("catalog-{handoff}.db");
    target.history_file = format!("exports-{handoff}.json");
    target.generation += 1;
    target.filesystem = identity.filesystem;
    target.root_identity = identity.directory;
    let pending = Pending {
        source_digest: fingerprint(&config.storage).unwrap(),
        target: target.clone(),
    };
    let mut root: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    root["storage"]
        .as_table_mut()
        .unwrap()
        .insert(PENDING.into(), toml::Value::try_from(pending).unwrap());
    write_private_file(path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
    (root, target)
}

#[test]
fn pending_metadata_rejects_replaced_destination_root_before_source_fence() {
    let (path, mut root, target) = fixture();
    let directory = path.parent().unwrap();
    let destination = directory.join("metadata");
    let retained = directory.join("original-metadata-root");
    let source = directory.join("custom/source.db");
    let authority = active_authority(&source);
    let original_catalog = std::fs::read(&source).unwrap();
    let original_history = std::fs::read(directory.join("archive/.exports/history.json")).unwrap();
    let before = std::fs::read(&path).unwrap();
    let pending = root.clone();
    std::fs::rename(&destination, &retained).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let replacement = crate::storage::volumes::root::Root::open(&destination).unwrap();
    assert_ne!(*replacement.identity(), target.root_identity());
    drop(replacement);
    assert!(apply(&path, &mut root, &Secrets::default()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(root, pending);
    assert_eq!(active_authority(&source), authority);
    assert_eq!(std::fs::read(&source).unwrap(), original_catalog);
    assert_eq!(
        std::fs::read(directory.join("archive/.exports/history.json")).unwrap(),
        original_history
    );
    assert!(!destination.join(target.catalog_file).exists());
    assert!(!destination.join(target.history_file).exists());
    assert!(retained.is_dir());
    RecordingCatalog::open(&source).unwrap().shutdown();
}

#[test]
fn pending_metadata_rejects_replaced_source_root_even_when_catalog_identity_survives() {
    let (path, mut root, owner) = fixture();
    apply(&path, &mut root, &Secrets::default()).unwrap();
    add_disabled_metadata_destination(&path);
    let config = crate::config::load_config(&path).unwrap();
    let (catalog, history) = owner.paths(&config.storage).unwrap();
    let (mut pending_root, target) = stage_other_metadata_root(&path);
    let before = std::fs::read(&path).unwrap();
    let pending = pending_root.clone();
    let directory = path.parent().unwrap();
    let source_root = directory.join("metadata");
    let retained = directory.join("original-source-root");
    std::fs::rename(&source_root, &retained).unwrap();
    std::fs::create_dir(&source_root).unwrap();
    let entries = std::fs::read_dir(&retained)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(entries.len() <= 32);
    for entry in entries {
        std::fs::rename(entry.path(), source_root.join(entry.file_name())).unwrap();
    }
    let replacement = crate::storage::volumes::root::Root::open(&source_root).unwrap();
    assert_ne!(*replacement.identity(), owner.root_identity());
    drop(replacement);
    assert_eq!(active_authority(&catalog), owner.authority());
    let original_catalog = std::fs::read(&catalog).unwrap();
    let original_history = std::fs::read(&history).unwrap();
    assert!(
        RecordingCatalog::open_managed(&catalog, &owner.authority(), &owner.root_identity())
            .is_err()
    );
    assert!(apply(&path, &mut pending_root, &Secrets::default()).is_err());
    assert_eq!(pending_root, pending);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(active_authority(&catalog), owner.authority());
    assert_eq!(std::fs::read(&catalog).unwrap(), original_catalog);
    assert_eq!(std::fs::read(&history).unwrap(), original_history);
    assert!(
        !directory
            .join("other-metadata")
            .join(target.catalog_file)
            .exists()
    );
    assert!(
        !directory
            .join("other-metadata")
            .join(target.history_file)
            .exists()
    );
    assert!(retained.is_dir());
}
#[test]
fn pending_metadata_rejects_invalid_history_before_fencing_and_retries_after_repair() {
    let (path, mut root, target) = fixture();
    let directory = path.parent().unwrap();
    let source = directory.join("custom/source.db");
    let history = directory.join("archive/.exports/history.json");
    let target_catalog = directory.join("metadata").join(&target.catalog_file);
    let target_history = directory.join("metadata").join(&target.history_file);
    let authority = active_authority(&source);
    let catalog_before = std::fs::read(&source).unwrap();
    let config_before = std::fs::read(&path).unwrap();
    let pending = root.clone();
    for invalid in [
        "{malformed-json",
        r#"{"version":99,"jobs":[]}"#,
        r#"{"version":1,"jobs":[{"requester_id":"owner","artifact_id":"attempt","request":"%%","job":"","created_at_ms":1,"updated_at_ms":1}]}"#,
        r#"{"version":1,"jobs":[{"requester_id":"owner","artifact_id":"attempt","request":"AA","job":"AA","created_at_ms":1,"updated_at_ms":1}]}"#,
    ] {
        std::fs::write(&history, invalid).unwrap();
        assert!(
            apply(&path, &mut root, &Secrets::default()).is_err(),
            "accepted invalid history: {invalid}"
        );
        assert_eq!(root, pending);
        assert_eq!(std::fs::read(&path).unwrap(), config_before);
        assert_eq!(std::fs::read(&history).unwrap(), invalid.as_bytes());
        assert_eq!(active_authority(&source), authority);
        assert_eq!(std::fs::read(&source).unwrap(), catalog_before);
        assert!(!target_catalog.exists());
        assert!(!target_history.exists());
    }
    let repaired = b"{\n  \"version\": 1,\n  \"jobs\": []\n}\n";
    std::fs::write(&history, repaired).unwrap();
    apply(&path, &mut root, &Secrets::default()).unwrap();
    assert_committed(&path, &root, &target);
    assert_eq!(std::fs::read(&history).unwrap(), repaired);
    assert_eq!(std::fs::read(&target_history).unwrap(), repaired);
    assert_eq!(active_authority(&target_catalog), target.authority());
}
fn pending_settings_fixture() -> (PathBuf, toml::Table) {
    let (path, mut root, _) = fixture();
    let volume = &root["storage"]["named_volumes"]["volumes"][0];
    let mut secrets = toml::Table::new();
    secrets.insert("PENDING_TEST_ID".into(), volume["id"].clone());
    secrets.insert("PENDING_TEST_ROOT".into(), volume["root"].clone());
    write_private_file(
        &path.with_file_name("secrets.toml"),
        toml::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    root["storage"]["named_volumes"]["volumes"][0]["id"] = "{secret:PENDING_TEST_ID}".into();
    root["storage"]["named_volumes"]["volumes"][0]["root"] = "{secret:PENDING_TEST_ROOT}".into();
    root["storage"][PENDING]["target"]["volume_id"] = "{secret:PENDING_TEST_ID}".into();
    write_private_file(&path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
    (path, root)
}

#[test]
fn pending_metadata_rejects_effective_storage_settings_changes_before_writing() {
    let (path, _) = pending_settings_fixture();
    let current = crate::config::load_config(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let secrets = std::fs::read(path.with_file_name("secrets.toml")).unwrap();
    for change in 0..3 {
        let mut next = current.clone();
        match change {
            0 => next.storage.short_term_secs += 1,
            1 => {
                next.storage.medium_term_path = Some(
                    path.with_file_name("changed-active")
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            2 => next.storage.named_volumes.as_mut().unwrap().volumes[0].priority += 1,
            _ => unreachable!(),
        }
        assert!(
            crate::config::update_settings_with_volume_draft(&path, &next, None, None).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(path.with_file_name("secrets.toml")).unwrap(),
            secrets
        );
        assert!(!path.with_file_name("changed-active").exists());
    }
}

#[test]
fn pending_metadata_rejects_effective_volume_draft_overrides_even_with_unchanged_settings() {
    let (path, root) = pending_settings_fixture();
    let settings = crate::config::load_config(&path).unwrap();
    let original: crate::storage::volumes::VolumeConfiguration<String> =
        root["storage"]["named_volumes"].clone().try_into().unwrap();
    let before = std::fs::read(&path).unwrap();
    for change in 0..3 {
        let mut draft = original.clone();
        match change {
            0 => draft.volumes[0].priority += 1,
            1 => draft.volumes[0].root = path.with_file_name("changed-metadata"),
            2 => draft.volumes.clear(),
            _ => unreachable!(),
        }
        assert!(
            crate::config::update_settings_with_volume_draft(&path, &settings, None, Some(&draft))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_file_name("changed-metadata").exists());
    }
}

#[test]
fn pending_metadata_allows_unrelated_updates_and_preserves_raw_marker_and_secrets() {
    let (path, root) = pending_settings_fixture();
    let marker = root["storage"][PENDING].clone();
    let raw_volumes = root["storage"]["named_volumes"].clone();
    let draft: crate::storage::volumes::VolumeConfiguration<String> =
        raw_volumes.clone().try_into().unwrap();
    let secrets = std::fs::read(path.with_file_name("secrets.toml")).unwrap();
    let mut settings = crate::config::load_config(&path).unwrap();
    let storage_digest = fingerprint(&settings.storage).unwrap();
    settings.port = 9091;
    let saved =
        crate::config::update_settings_with_volume_draft(&path, &settings, None, Some(&draft))
            .unwrap();
    assert_eq!(fingerprint(&saved.storage).unwrap(), storage_digest);
    settings.port = 9092;
    settings.storage.named_volumes = None;
    let saved =
        crate::config::update_settings_with_volume_draft(&path, &settings, None, None).unwrap();
    assert_eq!(saved.port, 9092);
    assert_eq!(fingerprint(&saved.storage).unwrap(), storage_digest);
    let persisted: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(persisted["storage"][PENDING], marker);
    assert_eq!(persisted["storage"]["named_volumes"], raw_volumes);
    assert_eq!(
        std::fs::read(path.with_file_name("secrets.toml")).unwrap(),
        secrets
    );
    let target: toml::Value = marker["target"].clone();
    let destination = path.parent().unwrap().join("metadata");
    assert!(
        !destination
            .join(target["catalog_file"].as_str().unwrap())
            .exists()
    );
    assert!(
        !destination
            .join(target["history_file"].as_str().unwrap())
            .exists()
    );
}

fn refresh_pending_capacity_confirmation(path: &Path, root: &mut toml::Table) {
    let config = crate::config::config_from_table(root, &Secrets::default()).unwrap();
    root["storage"][PENDING]["source_digest"] =
        toml::Value::try_from(fingerprint(&config.storage).unwrap()).unwrap();
    write_private_file(path, toml::to_string(root).unwrap().as_bytes()).unwrap();
}

#[test]
fn pending_metadata_rechecks_capacity_and_free_reserve_before_fencing_then_allows_retry() {
    for (field, limit) in [("capacity_bytes", 1_i64), ("minimum_free_bytes", i64::MAX)] {
        let (path, mut root, target) = fixture();
        let directory = path.parent().unwrap();
        let source = directory.join("custom/source.db");
        let history = directory.join("archive/.exports/history.json");
        let original_history = std::fs::read(&history).unwrap();
        let authority = active_authority(&source);
        let volume = root["storage"]["named_volumes"]["volumes"][0]
            .as_table_mut()
            .unwrap();
        volume.insert(field.into(), limit.into());
        if field == "minimum_free_bytes" {
            volume.insert("warning_free_bytes".into(), limit.into());
        }
        refresh_pending_capacity_confirmation(&path, &mut root);
        let confirmed = root.clone();
        let before = std::fs::read(&path).unwrap();
        assert!(
            apply(&path, &mut root, &Secrets::default()).is_err(),
            "ignored target {field}"
        );
        assert_eq!(root, confirmed);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(active_authority(&source), authority);
        assert_eq!(std::fs::read(&history).unwrap(), original_history);
        let destination = directory.join("metadata");
        assert!(!destination.join(&target.catalog_file).exists());
        assert!(!destination.join(&target.history_file).exists());
        let volume = root["storage"]["named_volumes"]["volumes"][0]
            .as_table_mut()
            .unwrap();
        volume.remove("capacity_bytes");
        volume.insert("minimum_free_bytes".into(), 0.into());
        volume.insert("warning_free_bytes".into(), 0.into());
        refresh_pending_capacity_confirmation(&path, &mut root);
        assert_eq!(
            root["storage"][PENDING]["target"],
            confirmed["storage"][PENDING]["target"]
        );
        apply(&path, &mut root, &Secrets::default()).unwrap();
        assert_committed(&path, &root, &target);
        assert_eq!(std::fs::read(&history).unwrap(), original_history);
        assert_eq!(
            std::fs::read(destination.join(&target.history_file)).unwrap(),
            original_history
        );
    }
}
#[test]
fn pending_metadata_respects_critical_free_space_before_fencing() {
    let (path, mut root, target) = fixture();
    let directory = path.parent().unwrap();
    let source = directory.join("custom/source.db");
    let history = directory.join("archive/.exports/history.json");
    let original = std::fs::read(&history).unwrap();
    let authority = active_authority(&source);
    let volume = root["storage"]["named_volumes"]["volumes"][0]
        .as_table_mut()
        .unwrap();
    volume.insert("minimum_free_bytes".into(), 0.into());
    volume.insert("critical_free_bytes".into(), i64::MAX.into());
    volume.insert("warning_free_bytes".into(), i64::MAX.into());
    refresh_pending_capacity_confirmation(&path, &mut root);
    let confirmed = root.clone();
    let before = std::fs::read(&path).unwrap();
    assert!(apply(&path, &mut root, &Secrets::default()).is_err());
    assert_eq!(root, confirmed);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(active_authority(&source), authority);
    assert_eq!(std::fs::read(&history).unwrap(), original);
    let destination = directory.join("metadata");
    assert!(!destination.join(&target.catalog_file).exists());
    assert!(!destination.join(&target.history_file).exists());
    let volume = root["storage"]["named_volumes"]["volumes"][0]
        .as_table_mut()
        .unwrap();
    volume.insert("critical_free_bytes".into(), 0.into());
    volume.insert("warning_free_bytes".into(), 0.into());
    refresh_pending_capacity_confirmation(&path, &mut root);
    apply(&path, &mut root, &Secrets::default()).unwrap();
    assert_committed(&path, &root, &target);
}
