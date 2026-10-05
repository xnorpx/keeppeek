use crate::{backup, config};
use std::{io::Cursor, path::Path};

#[test]
fn configuration_restore_preserves_target_named_owners_and_metadata_authority() -> anyhow::Result<()>
{
    let directory = directory()?;
    let source = configuration(&directory, "source", 17)?;
    let target = configuration(&directory, "target", 53)?;
    let before = config::load_config(&target)?;
    let archive = archive(&source, &directory)?;
    let candidate = backup::inspect_configuration_candidate(&archive, &target)?;
    let storage: config::StorageToml = candidate.configuration.source["storage"]
        .clone()
        .try_into()?;
    assert_eq!(storage.short_term_secs, 17);
    assert_eq!(storage.named_volumes, before.storage.named_volumes);
    assert_eq!(storage.metadata, before.storage.metadata);
    assert_ne!(
        storage.metadata,
        config::load_config(&source)?.storage.metadata
    );
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn conflicting_root_secrets_reject_restore_and_matching_references_remain_private()
-> anyhow::Result<()> {
    let directory = directory()?;
    let source = configuration(&directory, "source", 17)?;
    let target = configuration(&directory, "target", 53)?;
    let mut raw = config::load_configuration_table(&target)?;
    let root = raw["storage"]["named_volumes"]["volumes"][0]["root"].clone();
    raw["storage"]["named_volumes"]["volumes"][0]["root"] = "{secret:OWNED_TEST_ROOT}".into();
    config::write_private_file(&target, toml::to_string(&raw)?.as_bytes())?;
    write_secret(&target, root.clone())?;
    write_secret(
        &source,
        directory
            .join("wrong-root")
            .to_string_lossy()
            .into_owned()
            .into(),
    )?;
    let before = std::fs::read(&target)?;
    let archive_path = archive(&source, &directory)?;
    assert!(backup::inspect_configuration_candidate(&archive_path, &target).is_err());
    assert_eq!(std::fs::read(&target)?, before);
    write_secret(&source, root)?;
    let archive_path = archive(&source, &directory)?;
    let candidate = backup::inspect_configuration_candidate(&archive_path, &target)?;
    assert_eq!(
        candidate.configuration.source["storage"]["named_volumes"]["volumes"][0]["root"].as_str(),
        Some("{secret:OWNED_TEST_ROOT}")
    );
    assert_eq!(std::fs::read(&target)?, before);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn directory() -> anyhow::Result<std::path::PathBuf> {
    let parent = std::env::temp_dir();
    #[cfg(unix)]
    let parent = parent.canonicalize()?;
    let path = parent.join(format!("keeppeek-owned-restore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path)?;
    Ok(path)
}

#[test]
fn pending_metadata_handoff_rejects_configuration_restore_before_mutation() -> anyhow::Result<()> {
    let directory = directory()?;
    let source = configuration(&directory, "source", 17)?;
    let target = configuration(&directory, "target", 53)?;
    let mut raw = config::load_configuration_table(&target)?;
    raw["storage"].as_table_mut().unwrap().insert(
        config::metadata::pending::PENDING.into(),
        toml::Value::Table(toml::Table::new()),
    );
    config::write_private_file(&target, toml::to_string(&raw)?.as_bytes())?;
    let before = std::fs::read(&target)?;
    let archive = archive(&source, &directory)?;
    assert!(backup::inspect_configuration_candidate(&archive, &target).is_err());
    assert_eq!(std::fs::read(&target)?, before);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn configuration(
    parent: &Path,
    name: &str,
    short_term_secs: u64,
) -> anyhow::Result<std::path::PathBuf> {
    #[cfg(unix)]
    let parent = parent.canonicalize()?;
    let base = parent.join(name);
    std::fs::create_dir(&base)?;
    let mut configuration = config::Config::default();
    configuration.storage.short_term_secs = short_term_secs;
    crate::storage::volumes::bootstrap::initialize(&mut configuration.storage, &base)?;
    let path = base.join("config.toml");
    config::write_private_file(&path, toml::to_string(&configuration)?.as_bytes())?;
    config::write_private_file(&config::secrets_path(&path), b"")?;
    Ok(path)
}

fn archive(source: &Path, parent: &Path) -> anyhow::Result<std::path::PathBuf> {
    let (bundle, _) = backup::create_bundle(
        Cursor::new(Vec::new()),
        backup::CreateBundleOptions {
            config_path: source,
            sections: &[],
            created_at_unix_ms: 1_788_000_000_000,
        },
    )?;
    let path = parent.join("configuration.zip");
    std::fs::write(&path, bundle.into_inner())?;
    Ok(path)
}

fn write_secret(path: &Path, value: toml::Value) -> anyhow::Result<()> {
    let mut secrets = toml::Table::new();
    secrets.insert("OWNED_TEST_ROOT".into(), value);
    config::write_private_file(
        &config::secrets_path(path),
        toml::to_string(&secrets)?.as_bytes(),
    )?;
    Ok(())
}
