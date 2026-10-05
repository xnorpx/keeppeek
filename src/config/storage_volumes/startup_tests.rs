use crate::config::{Config, StorageToml, load_config, load_from_path, write_private_file};
use crate::storage::{StorageConfig, volumes::bootstrap};
use std::path::PathBuf;

fn directory() -> anyhow::Result<PathBuf> {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let base = std::fs::canonicalize(base)?;
    let path = base.join(format!("keeppeek-startup-volumes-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path)?;
    Ok(path)
}

#[test]
fn startup_persists_named_defaults_and_reloads_the_same_authority() -> anyhow::Result<()> {
    let base = directory()?;
    let path = base.join("config.toml");
    let (created, _) = load_from_path(path.clone())?;
    let storage = StorageConfig::from_toml(&created.storage);
    assert_eq!(storage.medium_term_path, base.join("storage/media"));
    assert_eq!(storage.long_term_path, storage.medium_term_path);
    assert_eq!(storage.event_thumbnail_path, base.join("storage/images"));
    let saved = load_config(&path)?;
    assert_eq!(saved.storage.metadata, created.storage.metadata);
    assert_eq!(saved.storage.named_volumes, created.storage.named_volumes);
    let (restarted, _) = load_from_path(path.clone())?;
    assert_eq!(restarted.storage.metadata, saved.storage.metadata);
    assert_eq!(restarted.storage.named_volumes, saved.storage.named_volumes);
    let raw: toml::Table = toml::from_str(&std::fs::read_to_string(path)?)?;
    assert!(raw["access_key"].as_str().unwrap().starts_with("{secret:"));
    std::fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn startup_preserves_secret_volume_id_when_initializing_metadata() -> anyhow::Result<()> {
    let base = directory()?;
    let path = base.join("config.toml");
    let mut storage = StorageToml::default();
    bootstrap::initialize(&mut storage, &base)?;
    storage.metadata = None;
    let config = Config {
        storage,
        ..Config::default()
    };
    let mut raw = toml::Value::try_from(config)?;
    raw["storage"]["named_volumes"]["volumes"][3]["id"] = "{secret:META_ID}".into();
    raw["storage"]["named_volumes"]["placement"][4]["candidates"] =
        toml::Value::Array(vec!["{secret:META_ID}".into()]);
    write_private_file(&path, toml::to_string(&raw)?.as_bytes())?;
    write_private_file(&base.join("secrets.toml"), b"META_ID='metadata'\n")?;
    let (loaded, _) = load_from_path(path.clone())?;
    assert_eq!(
        loaded.storage.metadata.as_ref().unwrap().volume_id.as_str(),
        "metadata"
    );
    let persisted: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(
        persisted["storage"]["metadata"]["volume_id"].as_str(),
        Some("{secret:META_ID}")
    );
    assert_eq!(
        persisted["storage"]["named_volumes"]["volumes"][3]["id"].as_str(),
        Some("{secret:META_ID}")
    );
    assert_eq!(
        load_config(&path)?.storage.metadata,
        loaded.storage.metadata
    );
    std::fs::remove_dir_all(base)?;
    Ok(())
}
