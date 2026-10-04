use crate::storage::volumes::{VolumeConfiguration, VolumeState};

use super::{Secrets, resolve_toml_secret_references};

#[cfg(test)]
mod migration_tests;

pub(super) fn validate(configuration: Option<&VolumeConfiguration>) -> anyhow::Result<()> {
    let Some(configuration) = configuration else {
        return Ok(());
    };
    configuration.validate()?;
    anyhow::ensure!(
        configuration
            .volumes
            .iter()
            .all(|volume| volume.state == VolumeState::Disabled),
        "named storage volumes must remain disabled until durable placement is available"
    );
    Ok(())
}

pub(super) fn persist<I: serde::Serialize>(
    storage: &mut toml::Table,
    configuration: Option<&VolumeConfiguration<I>>,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    let Some(configuration) = configuration else {
        return Ok(());
    };
    validate_with_secrets(configuration, secrets)?;
    let mut next = toml::Value::try_from(configuration)?;
    if let Some(existing) = storage.get("named_volumes")
        && (!configuration.volumes.is_empty() || !configuration.placement.is_empty())
    {
        let mut resolved = existing.clone();
        resolve_toml_secret_references(&mut resolved, secrets)?;
        let mut next_resolved = next.clone();
        resolve_toml_secret_references(&mut next_resolved, secrets)?;
        preserve_references(&mut next, &next_resolved, existing, &resolved);
    }
    storage.insert("named_volumes".to_owned(), next);
    Ok(())
}

pub(super) fn validate_with_secrets<I: serde::Serialize>(
    configuration: &VolumeConfiguration<I>,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    let mut value = toml::Value::try_from(configuration)?;
    resolve_toml_secret_references(&mut value, secrets)?;
    let resolved = value.try_into()?;
    validate(Some(&resolved))
}

fn preserve_references(
    next: &mut toml::Value,
    next_resolved: &toml::Value,
    raw: &toml::Value,
    resolved: &toml::Value,
) {
    if next.is_str() && next == next_resolved && next_resolved == resolved {
        *next = raw.clone();
        return;
    }
    match (next, next_resolved, raw, resolved) {
        (
            toml::Value::Table(next),
            toml::Value::Table(next_resolved),
            toml::Value::Table(raw),
            toml::Value::Table(resolved),
        ) => {
            for (key, value) in next {
                if let (Some(next_resolved), Some(raw), Some(resolved)) =
                    (next_resolved.get(key), raw.get(key), resolved.get(key))
                {
                    preserve_references(value, next_resolved, raw, resolved);
                }
            }
        }
        (
            toml::Value::Array(next),
            toml::Value::Array(next_resolved),
            toml::Value::Array(raw),
            toml::Value::Array(resolved),
        ) => {
            for (value, next_resolved) in next.iter_mut().zip(next_resolved) {
                if let Some(index) = resolved
                    .iter()
                    .position(|old| same_entry(next_resolved, old))
                {
                    preserve_references(value, next_resolved, &raw[index], &resolved[index]);
                }
            }
        }
        _ => {}
    }
}

fn same_entry(next: &toml::Value, old: &toml::Value) -> bool {
    if next.get("id").is_some() {
        return next.get("id") == old.get("id");
    }
    if next.get("role").is_some() {
        return ["role", "source", "group"]
            .iter()
            .all(|key| next.get(key) == old.get(key));
    }
    next == old
}

#[cfg(test)]
mod tests {
    use crate::config::{Config, load_config, update_settings, write_private_file};
    use crate::storage::volumes::{VolumeConfiguration, VolumeState};

    fn reordered_secret_draft(directory: &std::path::Path) -> VolumeConfiguration<String> {
        let text = r#"
            [[storage.named_volumes.volumes]]
            id = "{secret:REORDER_ID_ONE}"
            root = "{secret:REORDER_ROOT_ONE}"
            roles = ["archive"]
            state = "disabled"
            sources = ["{secret:REORDER_SOURCE_ONE}"]
            [[storage.named_volumes.volumes]]
            id = "{secret:REORDER_ID_TWO}"
            root = "{secret:REORDER_ROOT_TWO}"
            roles = ["archive"]
            state = "disabled"
            sources = ["{secret:REORDER_SOURCE_TWO}"]
            [[storage.named_volumes.placement]]
            role = "archive"
            source = "{secret:REORDER_SOURCE_ONE}"
            candidates = ["{secret:REORDER_ID_ONE}", "{secret:REORDER_ID_TWO}"]
            [[storage.named_volumes.placement]]
            role = "archive"
            source = "{secret:REORDER_SOURCE_TWO}"
            candidates = ["{secret:REORDER_ID_ONE}", "{secret:REORDER_ID_TWO}"]
        "#;
        let mut secrets = toml::Table::new();
        for (key, value) in [
            ("REORDER_ID_ONE", "disk-one"),
            ("REORDER_ID_TWO", "disk-two"),
            ("REORDER_SOURCE_ONE", "front"),
            ("REORDER_SOURCE_TWO", "rear"),
        ] {
            secrets.insert(key.into(), value.into());
        }
        for (key, name) in [("REORDER_ROOT_ONE", "one"), ("REORDER_ROOT_TWO", "two")] {
            secrets.insert(
                key.into(),
                directory.join(name).to_string_lossy().as_ref().into(),
            );
        }
        write_private_file(
            &directory.join("secrets.toml"),
            toml::to_string(&secrets).unwrap().as_bytes(),
        )
        .unwrap();
        write_private_file(&directory.join("config.toml"), text.as_bytes()).unwrap();
        let root: toml::Table = toml::from_str(text).unwrap();
        root["storage"]["named_volumes"].clone().try_into().unwrap()
    }

    #[test]
    fn named_volume_reordered_draft_preserves_references_and_explicit_replacements() {
        use crate::config::update_settings_with_volume_draft;

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-volumes-reorder-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        let mut next = reordered_secret_draft(&directory);
        let settings = load_config(&path).unwrap();
        next.volumes.reverse();
        next.placement.reverse();
        for rule in &mut next.placement {
            rule.candidates.reverse();
        }
        next.volumes[0].root = directory.join("replacement");
        next.volumes[0].sources[0] = "rear".into();
        next.volumes[1].root = directory.join("one");
        next.placement[0].source = Some("rear".into());
        next.placement[0].candidates[0] = "disk-two".into();
        next.placement[0].allow_fallback = true;
        update_settings_with_volume_draft(&path, &settings, None, Some(&next)).unwrap();

        let mut expected = next;
        expected.volumes[0].sources[0] = "{secret:REORDER_SOURCE_TWO}".into();
        expected.volumes[1].root = "{secret:REORDER_ROOT_ONE}".into();
        expected.placement[0].source = Some("{secret:REORDER_SOURCE_TWO}".into());
        expected.placement[0].candidates[0] = "{secret:REORDER_ID_TWO}".into();
        let text = std::fs::read_to_string(&path).unwrap();
        let persisted: toml::Table = toml::from_str(&text).unwrap();
        let actual: VolumeConfiguration<String> = persisted["storage"]["named_volumes"]
            .clone()
            .try_into()
            .unwrap();
        assert_eq!(actual, expected);
        let loaded = load_config(&path).unwrap();
        let volumes = loaded.storage.named_volumes.as_ref().unwrap();
        assert_eq!(volumes.volumes[0].id.as_str(), "disk-two");
        assert_eq!(volumes.volumes[0].root, directory.join("replacement"));
        assert_eq!(volumes.volumes[1].root, directory.join("one"));
        assert_eq!(volumes.placement[0].source.as_deref(), Some("rear"));
        assert_eq!(volumes.placement[0].candidates[0].as_str(), "disk-two");

        write_private_file(&directory.join("secrets.toml"), b"").unwrap();
        let empty = VolumeConfiguration::<String>::default();
        update_settings_with_volume_draft(&path, &loaded, None, Some(&empty)).unwrap();
        assert_eq!(
            load_config(&path).unwrap().storage.named_volumes,
            Some(VolumeConfiguration::default())
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn draft() -> VolumeConfiguration {
        let mut root = toml::Table::new();
        let mut volume = toml::Table::new();
        volume.insert("id".into(), "disk-one".into());
        volume.insert(
            "root".into(),
            std::env::temp_dir()
                .join("keeppeek-volume-draft")
                .to_string_lossy()
                .as_ref()
                .into(),
        );
        volume.insert("roles".into(), toml::Value::Array(vec!["archive".into()]));
        volume.insert("state".into(), "disabled".into());
        root.insert(
            "volumes".into(),
            toml::Value::Array(vec![toml::Value::Table(volume)]),
        );
        toml::Value::Table(root).try_into().unwrap()
    }

    #[test]
    fn named_volume_drafts_roundtrip_and_legacy_updates_preserve_them() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-volumes-config-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(&path, b"port=8081\n[storage]\n[custom]\nnote='preserved'\n").unwrap();
        let mut settings = Config::default();
        settings.storage.named_volumes = Some(draft());
        update_settings(&path, &settings).unwrap();
        assert_eq!(
            load_config(&path).unwrap().storage.named_volumes,
            settings.storage.named_volumes
        );
        settings.port = 9090;
        settings.storage.named_volumes = None;
        update_settings(&path, &settings).unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.port, 9090);
        assert_eq!(loaded.storage.named_volumes, Some(draft()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("preserved"));
        settings.storage.named_volumes = Some(VolumeConfiguration::default());
        update_settings(&path, &settings).unwrap();
        assert_eq!(
            load_config(&path).unwrap().storage.named_volumes,
            settings.storage.named_volumes
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_clear_removes_unresolvable_draft_references() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-volumes-clear-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        let text = "[storage.named_volumes]\nvolumes=[{id='draft', root='{secret:MISSING}', roles=['archive'], state='disabled'}]\n";
        write_private_file(&path, text.as_bytes()).unwrap();
        assert!(load_config(&path).is_err());
        let mut settings = Config::default();
        settings.storage.named_volumes = Some(VolumeConfiguration::default());
        update_settings(&path, &settings).unwrap();
        assert_eq!(
            load_config(&path).unwrap().storage.named_volumes,
            Some(VolumeConfiguration::default())
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_activation_fails_before_configuration_mutation() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-volumes-activation-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(&path, b"port=8081\n").unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut settings = Config::default();
        let mut volumes = draft();
        volumes.volumes[0].state = VolumeState::Enabled;
        settings.storage.named_volumes = Some(volumes);
        assert!(update_settings(&path, &settings).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_load_rejects_activation_and_invalid_drafts_without_writing() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-volumes-load-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        let mut settings = Config::default();
        for state in [
            VolumeState::Enabled,
            VolumeState::Draining,
            VolumeState::ReadOnly,
        ] {
            let mut volumes = draft();
            volumes.volumes[0].state = state;
            settings.storage.named_volumes = Some(volumes);
            let text = toml::to_string(&settings).unwrap();
            write_private_file(&path, text.as_bytes()).unwrap();
            assert!(load_config(&path).is_err(), "accepted {state:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        let mut volumes = draft();
        volumes.volumes[0].root = "relative-root".into();
        settings.storage.named_volumes = Some(volumes);
        let text = toml::to_string(&settings).unwrap();
        write_private_file(&path, text.as_bytes()).unwrap();
        assert!(load_config(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn named_volume_update_preserves_root_secret_references() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-volumes-secret-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        let mut volumes = draft();
        let mut secret = toml::Table::new();
        secret.insert(
            "VOLUME_ROOT_TEST".into(),
            volumes.volumes[0].root.to_string_lossy().as_ref().into(),
        );
        secret.insert("VOLUME_ID_TEST".into(), "disk-one".into());
        secret.insert("VOLUME_SOURCE_TEST".into(), "front-camera".into());
        write_private_file(
            &directory.join("secrets.toml"),
            toml::to_string(&secret).unwrap().as_bytes(),
        )
        .unwrap();
        let mut root = toml::Table::new();
        let mut storage = toml::Table::new();
        volumes.volumes[0].root = "{secret:VOLUME_ROOT_TEST}".into();
        let mut raw = toml::Value::try_from(volumes).unwrap();
        raw["volumes"][0]["id"] = "{secret:VOLUME_ID_TEST}".into();
        raw["placement"] = toml::from_str::<toml::Table>(r#"
            rules = [{ role = "archive", source = "{secret:VOLUME_SOURCE_TEST}", candidates = ["{secret:VOLUME_ID_TEST}"] }]
        "#).unwrap().remove("rules").unwrap();
        storage.insert("named_volumes".into(), raw);
        root.insert("storage".into(), toml::Value::Table(storage));
        write_private_file(&path, toml::to_string(&root).unwrap().as_bytes()).unwrap();
        let mut settings = load_config(&path).unwrap();
        settings.storage.named_volumes.as_mut().unwrap().volumes[0].priority = 10;
        settings.storage.named_volumes.as_mut().unwrap().placement[0].allow_fallback = true;
        update_settings(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("{secret:VOLUME_ROOT_TEST}"));
        assert!(text.contains("{secret:VOLUME_ID_TEST}"));
        assert!(text.contains("{secret:VOLUME_SOURCE_TEST}"));
        assert!(!text.contains("disk-one"));
        assert!(!text.contains("front-camera"));
        assert!(!text.contains("keeppeek-volume-draft"));
        assert_eq!(
            load_config(&path)
                .unwrap()
                .storage
                .named_volumes
                .unwrap()
                .volumes[0]
                .priority,
            10
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
