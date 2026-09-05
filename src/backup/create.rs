use super::BackupSection;
use crate::{api::backup_proto, config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Seek, Write},
    path::Path,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const SECTION_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_FILE_SECTION_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const CONFIG_MEMBER_PATH: &str = "config.toml";
pub(super) const SECRETS_MEMBER_PATH: &str = "secrets.toml";

/// Inputs for one deterministic two-file configuration backup bundle.
#[derive(Clone, Copy)]
pub struct CreateBundleOptions<'a> {
    /// The source `config.toml` path.
    pub config_path: &'a Path,
    /// The configuration-backed sections to include. An empty slice selects all available sections.
    pub sections: &'a [BackupSection],
    /// The manifest creation time as Unix milliseconds.
    pub created_at_unix_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ConfigurationSectionDocument {
    pub schema_version: u32,
    pub revision: String,
    pub values: serde_json::Value,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EventMetadataDocument {
    pub events: u64,
    pub operational_events: u64,
    pub keyframe_links: u64,
    pub search_terms: u64,
    pub embeddings: u64,
    pub catalog_revision: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EventThumbnailInventory {
    pub policy: String,
    pub catalog_revision: String,
    pub entries: Vec<crate::storage::catalog::CatalogEventThumbnailBackupEntry>,
}

struct PreparedSection {
    kind: BackupSection,
    bytes: Vec<u8>,
    revision: String,
}

/// Creates a deterministic format-2 ZIP from configuration-backed sections.
///
/// # Errors
///
/// Returns an error when the configuration is invalid, a selected section is unsupported, a
/// dependency is absent, a secret reference is malformed, or the ZIP cannot be written.
pub fn create_bundle<W: Write + Seek>(
    writer: W,
    options: CreateBundleOptions<'_>,
) -> anyhow::Result<(W, backup_proto::BackupManifest)> {
    if options.created_at_unix_ms == 0 {
        anyhow::bail!("backup creation time must be nonzero");
    }
    let raw_config = read_bounded_file(options.config_path, MAXIMUM_FILE_SECTION_BYTES)?;
    toml::from_str::<toml::Table>(std::str::from_utf8(&raw_config)?)?;
    let raw_secrets = read_bounded_file(
        &config::secrets_path(options.config_path),
        MAXIMUM_FILE_SECTION_BYTES,
    )?;
    toml::from_str::<BTreeMap<String, String>>(std::str::from_utf8(&raw_secrets)?)?;
    let config_revision = super::encode_lower_hex(Sha256::digest(&raw_config));
    let selected = selected_sections(options)?;
    let prepared = vec![PreparedSection {
        kind: selected[0],
        bytes: raw_config,
        revision: config_revision,
    }];
    let manifest = build_manifest(options, &prepared, &raw_secrets)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let writer = write_archive(writer, &manifest_bytes, &prepared[0].bytes, &raw_secrets)?;
    Ok((writer, manifest))
}

fn selected_sections(options: CreateBundleOptions<'_>) -> anyhow::Result<Vec<BackupSection>> {
    if !options.sections.is_empty() && options.sections != [BackupSection::RuntimeConfig] {
        anyhow::bail!("configuration backups do not support section selection");
    }
    Ok(vec![BackupSection::RuntimeConfig])
}

fn read_bounded_file(path: &Path, maximum_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let size = std::fs::metadata(path)?.len();
    if size > maximum_bytes {
        anyhow::bail!("backup section {} exceeds the size limit", path.display());
    }
    std::fs::read(path).map_err(Into::into)
}

fn build_manifest(
    options: CreateBundleOptions<'_>,
    sections: &[PreparedSection],
    secrets: &[u8],
) -> anyhow::Result<backup_proto::BackupManifest> {
    let config_directory = options
        .config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let section_descriptors = sections
        .iter()
        .map(|section| {
            Ok(backup_proto::BackupSectionDescriptor {
                section: section.kind.to_proto() as i32,
                path: section.kind.current_path().to_owned(),
                schema_version: SECTION_SCHEMA_VERSION,
                bytes: u64::try_from(section.bytes.len())?,
                sha256: super::encode_lower_hex(Sha256::digest(&section.bytes)),
                revision: section.revision.clone(),
                dependencies: section
                    .kind
                    .required_dependencies()
                    .iter()
                    .map(|dependency| dependency.to_proto() as i32)
                    .collect(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let source_paths = vec![backup_proto::BackupPath {
        kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
        path: config_directory.to_string_lossy().into_owned(),
    }];
    Ok(backup_proto::BackupManifest {
        format_version: super::FORMAT_VERSION,
        created_at_unix_ms: options.created_at_unix_ms,
        keeppeek_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: Some(backup_proto::BackupSource {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }),
        feature_capabilities: vec!["keeppeek.backup.v1".to_owned()],
        secret_policy: backup_proto::BackupSecretPolicy::Unspecified as i32,
        sections: section_descriptors,
        omitted_data: vec![
            "recording_media".to_owned(),
            "recording_catalog".to_owned(),
            "event_thumbnails".to_owned(),
            "sessions".to_owned(),
            "derived_caches".to_owned(),
        ],
        required_secret_references: Vec::new(),
        source_paths,
        snapshot_revision: super::native_configuration_revision(&sections[0].bytes, secrets),
    })
}

fn write_archive<W: Write + Seek>(
    writer: W,
    manifest: &[u8],
    config: &[u8],
    secrets: &[u8],
) -> anyhow::Result<W> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut archive = ZipWriter::new(writer);
    if manifest.len() > usize::from(u16::MAX) {
        anyhow::bail!("backup manifest exceeds the ZIP comment limit");
    }
    archive.set_comment(std::str::from_utf8(manifest)?);
    archive.start_file(CONFIG_MEMBER_PATH, options)?;
    archive.write_all(config)?;
    archive.start_file(SECRETS_MEMBER_PATH, options)?;
    archive.write_all(secrets)?;
    archive.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read as _};

    #[test]
    fn creates_deterministic_native_configuration_bundle() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        let config = r#"access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[storage]
long_term_max_gb = 10

[camera_defaults]
username = "{secret:CAMERA_USERNAME}"
password = "{secret:CAMERA_PASSWORD}"

[cameras.front]
ip = "192.0.2.10"

[event_forwarder.mqtt]
host = "mqtt.local"
username = "events"
password = "{secret:MQTT_PASSWORD}"
"#;
        let secrets = r#"KEEPPEEK_ACCESS_KEY = "550e8400-e29b-41d4-a716-446655440000"
CAMERA_USERNAME = "operator"
CAMERA_PASSWORD = "camera-password"
MQTT_PASSWORD = "mqtt-password"
"#;
        std::fs::write(&config_path, config).unwrap();
        std::fs::write(config::secrets_path(&config_path), secrets).unwrap();
        std::fs::write(
            directory.join("recordings.db"),
            b"not part of configuration",
        )
        .unwrap();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[],
            created_at_unix_ms: 1_788_000_000_000,
        };

        let (first, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();
        let (second, _) = create_bundle(Cursor::new(Vec::new()), options).unwrap();

        assert_eq!(first.get_ref(), second.get_ref());
        assert_eq!(manifest.sections.len(), 1);
        let mut archive = zip::ZipArchive::new(Cursor::new(first.into_inner())).unwrap();
        assert_eq!(archive.len(), 2);
        let mut archived_config = String::new();
        archive
            .by_name("config.toml")
            .unwrap()
            .read_to_string(&mut archived_config)
            .unwrap();
        let mut archived_secrets = String::new();
        archive
            .by_name("secrets.toml")
            .unwrap()
            .read_to_string(&mut archived_secrets)
            .unwrap();
        assert_eq!(archived_config, config);
        assert_eq!(archived_secrets, secrets);
        assert!(archive.by_name("recordings.db").is_err());
        super::super::inspect_bundle(Cursor::new(second.into_inner())).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_database_section_selection() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        std::fs::write(config::secrets_path(&config_path), "").unwrap();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[
                BackupSection::RuntimeConfig,
                BackupSection::RecordingCatalog,
            ],
            created_at_unix_ms: 1_788_000_000_000,
        };

        let error = create_bundle(Cursor::new(Vec::new()), options).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("do not support section selection")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }
}
