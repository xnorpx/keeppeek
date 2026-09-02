use super::{BackupPathKind, BackupSection, BackupStoragePaths, MANIFEST_PATH};
use crate::{api::backup_proto, config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    io::{Seek, Write},
    path::Path,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const SECTION_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_FILE_SECTION_BYTES: u64 = 16 * 1024 * 1024;

/// Inputs for one deterministic reference-only backup bundle.
#[derive(Clone, Copy)]
pub struct CreateBundleOptions<'a> {
    /// The source `config.toml` path.
    pub config_path: &'a Path,
    /// The configuration-backed sections to include. An empty slice selects all available sections.
    pub sections: &'a [BackupSection],
    /// The manifest creation time as Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// The live recording catalog used when its section is selected.
    pub recording_catalog: Option<&'a crate::storage::RecordingCatalogHandle>,
    /// The live notification store used when its section is selected.
    pub notifications: Option<&'a crate::notifications::Handle>,
    /// Storage roots referenced by catalog metadata but omitted as media bytes.
    pub storage_paths: Option<&'a BackupStoragePaths>,
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
    let raw_config = std::fs::read(options.config_path)?;
    let root: toml::Table = toml::from_str(std::str::from_utf8(&raw_config)?)?;
    let config_revision = super::encode_lower_hex(Sha256::digest(&raw_config));
    let selected = selected_sections(options)?;
    let mut required_secret_references = BTreeSet::new();
    let sanitized = sanitize_table(root, &mut required_secret_references)?;
    let prepared = prepare_sections(
        options,
        &sanitized,
        &selected,
        &config_revision,
        &mut required_secret_references,
    )?;
    let manifest = build_manifest(
        options,
        &prepared,
        required_secret_references.into_iter().collect(),
    )?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let writer = write_archive(writer, &manifest_bytes, &prepared)?;
    Ok((writer, manifest))
}

fn selected_sections(options: CreateBundleOptions<'_>) -> anyhow::Result<Vec<BackupSection>> {
    let mut selected = if options.sections.is_empty() {
        let mut selected = vec![
            BackupSection::RuntimeConfig,
            BackupSection::CameraDatabase,
            BackupSection::Integrations,
        ];
        for (section, file_name) in [
            (BackupSection::Access, "access.toml"),
            (BackupSection::Layouts, "peek-layouts.json"),
            (
                BackupSection::ConfigurationTemplates,
                "configuration-templates.json",
            ),
        ] {
            if options.config_path.with_file_name(file_name).is_file() {
                selected.push(section);
            }
        }
        if options.recording_catalog.is_some() {
            selected.push(BackupSection::RecordingCatalog);
            selected.push(BackupSection::EventMetadata);
            if options.storage_paths.is_some() {
                selected.push(BackupSection::EventThumbnails);
            }
        }
        if options.notifications.is_some() {
            selected.push(BackupSection::Notifications);
        }
        selected
    } else {
        options.sections.to_vec()
    };
    selected.sort_unstable();
    let original_len = selected.len();
    selected.dedup();
    if selected.len() != original_len {
        anyhow::bail!("backup sections must not contain duplicates");
    }
    if selected.iter().any(|section| {
        !matches!(
            section,
            BackupSection::RuntimeConfig
                | BackupSection::CameraDatabase
                | BackupSection::Integrations
                | BackupSection::Access
                | BackupSection::Layouts
                | BackupSection::ConfigurationTemplates
                | BackupSection::RecordingCatalog
                | BackupSection::EventMetadata
                | BackupSection::EventThumbnails
                | BackupSection::Notifications
        )
    }) {
        anyhow::bail!("the selected backup section is not available");
    }
    for section in &selected {
        for dependency in section.required_dependencies() {
            if !selected.contains(dependency) {
                anyhow::bail!(
                    "backup {} section requires {} section",
                    section.as_str(),
                    dependency.as_str()
                );
            }
        }
    }
    Ok(selected)
}

fn sanitize_table(
    table: toml::Table,
    required: &mut BTreeSet<String>,
) -> anyhow::Result<toml::Table> {
    let mut value = toml::Value::Table(table);
    sanitize_value(&mut value, &mut Vec::new(), required)?;
    value
        .as_table()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("configuration root is not a table"))
}

fn sanitize_value(
    value: &mut toml::Value,
    path: &mut Vec<String>,
    required: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                path.push(key.clone());
                sanitize_value(child, path, required)?;
                path.pop();
            }
        }
        toml::Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                path.push(index.to_string());
                sanitize_value(child, path, required)?;
                path.pop();
            }
        }
        toml::Value::String(text) => sanitize_string(text, path, required)?,
        _ => {}
    }
    Ok(())
}

fn sanitize_string(
    value: &mut String,
    path: &[String],
    required: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let references = secret_references(value)?;
    if value.is_empty() || (references.is_empty() && public_string(path, value)) {
        return Ok(());
    }
    if references.len() == 1 && value == &references[0]
        || !references.is_empty() && reference_url_is_safe(value, &references)
    {
        required.extend(references);
    } else {
        let reference = generated_secret_reference(path);
        required.insert(reference.clone());
        *value = reference;
    }
    Ok(())
}

fn reference_url_is_safe(value: &str, references: &[String]) -> bool {
    let mut normalized = value.to_owned();
    for reference in references {
        normalized = normalized.replacen(reference, "BACKUPSECRET", 1);
    }
    let Ok(url) = url::Url::parse(&normalized) else {
        return false;
    };
    if !matches!(
        url.scheme(),
        "http" | "https" | "rtsp" | "rtsps" | "mqtt" | "mqtts"
    ) || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let username_referenced = url.username() == "BACKUPSECRET";
    let password_referenced = url.password() == Some("BACKUPSECRET");
    if (!url.username().is_empty() && !username_referenced)
        || (url.password().is_some() && !password_referenced)
    {
        return false;
    }
    usize::from(username_referenced) + usize::from(password_referenced) == references.len()
}

fn public_string(path: &[String], value: &str) -> bool {
    let key = path
        .iter()
        .rev()
        .find(|segment| !segment.bytes().all(|byte| byte.is_ascii_digit()))
        .map(String::as_str);
    match key {
        Some(
            "host"
            | "medium_term_path"
            | "long_term_path"
            | "recording_catalog_path"
            | "event_thumbnail_path"
            | "service"
            | "bind"
            | "local_networks"
            | "trusted_proxies"
            | "ip"
            | "name"
            | "display_name"
            | "manufacturer"
            | "backend"
            | "transport"
            | "recording_mode"
            | "client_id"
            | "instance_id"
            | "forwarder_id"
            | "topic_prefix"
            | "tls_ca_path",
        ) => true,
        Some("allowed_origins") => url_without_credentials(value, &["http", "https"]),
        Some("broker_url") => url_without_credentials(value, &["mqtt", "mqtts"]),
        _ => false,
    }
}

fn url_without_credentials(value: &str, schemes: &[&str]) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        schemes.contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn secret_references(value: &str) -> anyhow::Result<Vec<String>> {
    const PREFIX: &str = "{secret:";
    let mut references = Vec::new();
    let mut remaining = value;
    while let Some(start) = remaining.find(PREFIX) {
        let candidate = &remaining[start..];
        let Some(end) = candidate.find('}') else {
            anyhow::bail!("configuration contains a malformed secret reference");
        };
        let reference = &candidate[..=end];
        if !config::is_secret_reference(reference) {
            anyhow::bail!("configuration contains an invalid secret reference");
        }
        references.push(reference.to_owned());
        remaining = &candidate[end + 1..];
    }
    Ok(references)
}

fn generated_secret_reference(path: &[String]) -> String {
    let joined = path.join("_");
    let mut key = String::from("BACKUP_");
    for character in joined.chars() {
        if key.len() >= 48 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            key.push(character.to_ascii_uppercase());
        } else {
            key.push('_');
        }
    }
    key.push('_');
    for byte in &Sha256::digest(joined.as_bytes())[..4] {
        write!(&mut key, "{byte:02X}").expect("writing to a String cannot fail");
    }
    format!("{{secret:{key}}}")
}

fn prepare_sections(
    options: CreateBundleOptions<'_>,
    root: &toml::Table,
    selected: &[BackupSection],
    config_revision: &str,
    required_secret_references: &mut BTreeSet<String>,
) -> anyhow::Result<Vec<PreparedSection>> {
    let (runtime, cameras, integrations) = split_configuration(root);
    let mut prepared = Vec::with_capacity(selected.len());
    let mut catalog_sections = selected
        .contains(&BackupSection::RecordingCatalog)
        .then(|| {
            recording_catalog_sections(
                options.config_path,
                options
                    .recording_catalog
                    .ok_or_else(|| anyhow::anyhow!("recording catalog is unavailable"))?,
                selected.contains(&BackupSection::EventMetadata),
                selected.contains(&BackupSection::EventThumbnails),
                options.storage_paths,
            )
        })
        .transpose()?;
    for kind in selected {
        let section = match kind {
            BackupSection::RuntimeConfig => PreparedSection {
                kind: *kind,
                bytes: toml::to_string_pretty(&runtime)?.into_bytes(),
                revision: config_revision.to_owned(),
            },
            BackupSection::CameraDatabase => {
                json_section(*kind, serde_json::to_value(&cameras)?, config_revision)?
            }
            BackupSection::Integrations => {
                json_section(*kind, serde_json::to_value(&integrations)?, config_revision)?
            }
            BackupSection::Access => {
                let bytes = crate::access::backup_catalog_document(options.config_path)?
                    .ok_or_else(|| anyhow::anyhow!("access catalog is unavailable"))?;
                PreparedSection {
                    kind: *kind,
                    revision: super::encode_lower_hex(Sha256::digest(&bytes)),
                    bytes,
                }
            }
            BackupSection::Layouts => json_sidecar(
                *kind,
                options.config_path,
                "peek-layouts.json",
                required_secret_references,
            )?,
            BackupSection::ConfigurationTemplates => json_sidecar(
                *kind,
                options.config_path,
                "configuration-templates.json",
                required_secret_references,
            )?,
            BackupSection::RecordingCatalog => catalog_sections
                .as_mut()
                .and_then(|sections| sections.catalog.take())
                .ok_or_else(|| anyhow::anyhow!("recording catalog snapshot is unavailable"))?,
            BackupSection::EventMetadata => catalog_sections
                .as_mut()
                .and_then(|sections| sections.events.take())
                .ok_or_else(|| anyhow::anyhow!("event metadata snapshot is unavailable"))?,
            BackupSection::EventThumbnails => catalog_sections
                .as_mut()
                .and_then(|sections| sections.thumbnails.take())
                .ok_or_else(|| anyhow::anyhow!("event thumbnail inventory is unavailable"))?,
            BackupSection::Notifications => database_section(
                *kind,
                options.config_path,
                required_secret_references,
                |destination| {
                    let snapshot = options
                        .notifications
                        .ok_or_else(|| anyhow::anyhow!("notification store is unavailable"))?
                        .snapshot_reference_only_to(
                            destination,
                            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
                        )?;
                    if snapshot.bytes == 0
                        || snapshot.bytes > super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes
                    {
                        anyhow::bail!("notification snapshot exceeds its size limit");
                    }
                    Ok(snapshot.required_secret_references)
                },
            )?,
            _ => unreachable!("selected_sections permits only implemented file sections"),
        };
        prepared.push(section);
    }
    Ok(prepared)
}

struct PreparedCatalogSections {
    catalog: Option<PreparedSection>,
    events: Option<PreparedSection>,
    thumbnails: Option<PreparedSection>,
}

fn recording_catalog_sections(
    config_path: &Path,
    catalog: &crate::storage::RecordingCatalogHandle,
    include_events: bool,
    include_thumbnails: bool,
    storage_paths: Option<&BackupStoragePaths>,
) -> anyhow::Result<PreparedCatalogSections> {
    let destination = config_path.with_file_name(format!(
        ".keeppeek-backup-recording-catalog-{}.db",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        catalog.snapshot_to(
            &destination,
            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
        )?;
        if !include_events {
            crate::storage::catalog::strip_event_metadata(&destination)?;
        }
        let summary = include_events
            .then(|| crate::storage::catalog::event_backup_summary(&destination))
            .transpose()?;
        let thumbnails = include_thumbnails
            .then(|| {
                let root = storage_paths
                    .ok_or_else(|| anyhow::anyhow!("event thumbnail root is unavailable"))?
                    .path(BackupPathKind::EventThumbnails)?;
                crate::storage::catalog::event_thumbnail_backup_entries(&destination, root)
            })
            .transpose()?;
        let bytes = read_bounded_file(
            &destination,
            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
        )?;
        let revision = super::encode_lower_hex(Sha256::digest(&bytes));
        let events = summary
            .map(|summary| {
                let values = serde_json::to_value(EventMetadataDocument {
                    events: summary.events,
                    operational_events: summary.operational_events,
                    keyframe_links: summary.keyframe_links,
                    search_terms: summary.search_terms,
                    embeddings: summary.embeddings,
                    catalog_revision: revision.clone(),
                })?;
                json_section(BackupSection::EventMetadata, values, &revision)
            })
            .transpose()?;
        let thumbnails = thumbnails
            .map(|entries| {
                let values = serde_json::to_value(EventThumbnailInventory {
                    policy: "inventory_only".to_owned(),
                    catalog_revision: revision.clone(),
                    entries,
                })?;
                let thumbnail_revision =
                    super::encode_lower_hex(Sha256::digest(serde_json::to_vec(&values)?));
                json_section(BackupSection::EventThumbnails, values, &thumbnail_revision)
            })
            .transpose()?;
        Ok(PreparedCatalogSections {
            catalog: Some(PreparedSection {
                kind: BackupSection::RecordingCatalog,
                bytes,
                revision,
            }),
            events,
            thumbnails,
        })
    })();
    super::database::remove_database_family(&destination);
    result
}

fn database_section(
    kind: BackupSection,
    config_path: &Path,
    required_secret_references: &mut BTreeSet<String>,
    snapshot: impl FnOnce(&Path) -> anyhow::Result<Vec<String>>,
) -> anyhow::Result<PreparedSection> {
    let destination = config_path.with_file_name(format!(
        ".keeppeek-backup-{}-{}.db",
        kind.as_str(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        required_secret_references.extend(snapshot(&destination)?);
        let bytes = read_bounded_file(
            &destination,
            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
        )?;
        let revision = super::encode_lower_hex(Sha256::digest(&bytes));
        Ok(PreparedSection {
            kind,
            bytes,
            revision,
        })
    })();
    super::database::remove_database_family(&destination);
    result
}

fn json_section(
    kind: BackupSection,
    values: serde_json::Value,
    revision: &str,
) -> anyhow::Result<PreparedSection> {
    let document = ConfigurationSectionDocument {
        schema_version: SECTION_SCHEMA_VERSION,
        revision: revision.to_owned(),
        values,
    };
    Ok(PreparedSection {
        kind,
        bytes: serde_json::to_vec_pretty(&document)?,
        revision: revision.to_owned(),
    })
}

fn json_sidecar(
    kind: BackupSection,
    config_path: &Path,
    file_name: &str,
    required_secret_references: &mut BTreeSet<String>,
) -> anyhow::Result<PreparedSection> {
    let path = config_path.with_file_name(file_name);
    let bytes = read_bounded_file(&path, MAXIMUM_FILE_SECTION_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    collect_json_secret_references(&value, required_secret_references)?;
    let revision = super::encode_lower_hex(Sha256::digest(&bytes));
    json_section(kind, value, &revision)
}

fn read_bounded_file(path: &Path, maximum_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let size = std::fs::metadata(path)?.len();
    if size > maximum_bytes {
        anyhow::bail!("backup section {} exceeds the size limit", path.display());
    }
    std::fs::read(path).map_err(Into::into)
}

fn collect_json_secret_references(
    value: &serde_json::Value,
    required: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    match value {
        serde_json::Value::String(value) => required.extend(secret_references(value)?),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_json_secret_references(value, required)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                collect_json_secret_references(value, required)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn split_configuration(root: &toml::Table) -> (toml::Table, toml::Table, toml::Table) {
    let mut runtime = toml::Table::new();
    let mut cameras = toml::Table::new();
    let mut integrations = toml::Table::new();
    for (key, value) in root {
        if key == "storage_migration" {
            continue;
        }
        if key == "event_forwarder" {
            integrations.insert(key.clone(), value.clone());
        } else if key == "camera_defaults" || config::is_camera_namespace(key, value) {
            cameras.insert(key.clone(), value.clone());
        } else {
            runtime.insert(key.clone(), value.clone());
        }
    }
    (runtime, cameras, integrations)
}

fn build_manifest(
    options: CreateBundleOptions<'_>,
    sections: &[PreparedSection],
    required_secret_references: Vec<String>,
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
                path: section.kind.canonical_path().to_owned(),
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
    let mut snapshot_hasher = Sha256::new();
    for section in sections {
        snapshot_hasher.update(section.kind.as_str().as_bytes());
        snapshot_hasher.update([0]);
        snapshot_hasher.update(section.revision.as_bytes());
        snapshot_hasher.update([0]);
    }
    let mut source_paths = vec![backup_proto::BackupPath {
        kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
        path: config_directory.to_string_lossy().into_owned(),
    }];
    if sections
        .iter()
        .any(|section| section.kind == BackupSection::RecordingCatalog)
    {
        let catalog = options
            .recording_catalog
            .ok_or_else(|| anyhow::anyhow!("recording catalog is unavailable"))?;
        source_paths.push(backup_proto::BackupPath {
            kind: backup_proto::BackupPathKind::RecordingCatalog as i32,
            path: catalog.database_path().to_string_lossy().into_owned(),
        });
        if let Some(storage_paths) = options.storage_paths {
            let mut kinds = vec![BackupPathKind::LongTermMedia];
            if sections
                .iter()
                .any(|section| section.kind == BackupSection::EventThumbnails)
            {
                kinds.push(BackupPathKind::EventThumbnails);
            }
            for kind in kinds {
                source_paths.push(backup_proto::BackupPath {
                    kind: kind.to_proto() as i32,
                    path: storage_paths
                        .path(kind)?
                        .canonicalize()?
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
    }
    if sections
        .iter()
        .any(|section| section.kind == BackupSection::Notifications)
    {
        let notifications = options
            .notifications
            .ok_or_else(|| anyhow::anyhow!("notification store is unavailable"))?;
        source_paths.push(backup_proto::BackupPath {
            kind: backup_proto::BackupPathKind::NotificationDatabase as i32,
            path: notifications.database_path().to_string_lossy().into_owned(),
        });
    }
    Ok(backup_proto::BackupManifest {
        format_version: super::FORMAT_VERSION,
        created_at_unix_ms: options.created_at_unix_ms,
        keeppeek_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: Some(backup_proto::BackupSource {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }),
        feature_capabilities: vec!["keeppeek.backup.v1".to_owned()],
        secret_policy: backup_proto::BackupSecretPolicy::ReferencesOnly as i32,
        sections: section_descriptors,
        omitted_data: vec![
            "recording_media".to_owned(),
            "resolved_secrets".to_owned(),
            "sessions".to_owned(),
            "derived_caches".to_owned(),
        ],
        required_secret_references,
        source_paths,
        snapshot_revision: super::encode_lower_hex(snapshot_hasher.finalize()),
    })
}

fn write_archive<W: Write + Seek>(
    writer: W,
    manifest: &[u8],
    sections: &[PreparedSection],
) -> anyhow::Result<W> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut archive = ZipWriter::new(writer);
    archive.start_file(MANIFEST_PATH, options)?;
    archive.write_all(manifest)?;
    for section in sections {
        archive.start_file(section.kind.canonical_path(), options)?;
        archive.write_all(&section.bytes)?;
    }
    archive.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read as _};

    #[test]
    fn creates_deterministic_reference_only_configuration_sections() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(
            &config_path,
            r#"access_key = "inline-access-key"

[storage]
long_term_max_gb = 10

[camera_defaults]
username = "{secret:CAMERA_USERNAME}"
password = "inline-default-password"

[cameras.front]
ip = "192.0.2.10"
username = "inline-camera-user"
password = "inline-camera-password"
main_rtsp_url = "rtsp://inline-user:inline-password@camera.local/main"
sub_rtsp_url = "rtsp://mixed-inline-user:{secret:CAMERA_PASSWORD}@camera.local/sub"
custom_note = "inline-unknown-secret"

[event_forwarder.mqtt]
host = "mqtt.local"
username = "events"
password = "{secret:MQTT_PASSWORD}"
"#,
        )
        .unwrap();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[
                BackupSection::RuntimeConfig,
                BackupSection::CameraDatabase,
                BackupSection::Integrations,
            ],
            created_at_unix_ms: 1_788_000_000_000,
            recording_catalog: None,
            notifications: None,
            storage_paths: None,
        };

        let (first, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();
        let (second, _) = create_bundle(Cursor::new(Vec::new()), options).unwrap();

        assert_eq!(first.get_ref(), second.get_ref());
        assert_eq!(manifest.sections.len(), 3);
        let mut archive = zip::ZipArchive::new(Cursor::new(first.into_inner())).unwrap();
        let mut expanded = String::new();
        for index in 0..archive.len() {
            archive
                .by_index(index)
                .unwrap()
                .read_to_string(&mut expanded)
                .unwrap();
        }
        for secret in [
            "inline-access-key",
            "inline-default-password",
            "inline-camera-user",
            "inline-camera-password",
            "inline-user:inline-password",
            "mixed-inline-user",
            "inline-unknown-secret",
        ] {
            assert!(!expanded.contains(secret), "artifact contains {secret}");
        }
        assert!(expanded.contains("{secret:CAMERA_USERNAME}"));
        assert!(expanded.contains("{secret:MQTT_PASSWORD}"));
        assert!(
            manifest
                .required_secret_references
                .iter()
                .any(|reference| reference.starts_with("{secret:BACKUP_ACCESS_KEY_"))
        );
        let mut runtime = String::new();
        archive
            .by_name("config/runtime.toml")
            .unwrap()
            .read_to_string(&mut runtime)
            .unwrap();
        assert!(runtime.contains("access_key = \"{secret:BACKUP_ACCESS_KEY_"));
        super::super::inspect_bundle(Cursor::new(second.into_inner())).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creates_valid_file_backed_access_layout_and_template_sections() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let access = crate::access::AccessManager::open(
            &config_path,
            crate::access::AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        )
        .unwrap();
        access.record_audit(crate::access::NewAccessAuditEvent {
            timestamp_ms: 1_788_000_000_000,
            principal_id: Some("local-administrator"),
            role: Some(crate::access::AccessRole::Administrator),
            action: "backup_download",
            target_id: Some("private-backup-id"),
            result: "success",
            client_classification: crate::access::ClientClassificationReason::DirectLocal,
        });
        access.flush_audit(true).unwrap();
        std::fs::write(
            directory.join("peek-layouts.json"),
            r#"{"schema_version":1,"revision":1,"shared_layouts":[{"id":"default","name":"All cameras","scope":"shared","owner_id":"server","audience":{"everyone":true,"credential_ids":[]},"activity_focus":true,"tiles":[]}],"users":{}}"#,
        )
        .unwrap();
        std::fs::write(
            directory.join("configuration-templates.json"),
            r#"{"document_version":1,"templates":[]}"#,
        )
        .unwrap();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[
                BackupSection::RuntimeConfig,
                BackupSection::CameraDatabase,
                BackupSection::Access,
                BackupSection::Layouts,
                BackupSection::ConfigurationTemplates,
            ],
            created_at_unix_ms: 1_788_000_000_000,
            recording_catalog: None,
            notifications: None,
            storage_paths: None,
        };

        let (bundle, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();

        assert_eq!(manifest.sections.len(), 5);
        let bytes = bundle.into_inner();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        let mut access_document = String::new();
        archive
            .by_name("access/access.toml")
            .unwrap()
            .read_to_string(&mut access_document)
            .unwrap();
        assert!(access_document.contains("Initial Administrator"));
        assert!(!access_document.contains("backup_download"));
        assert!(!access_document.contains("private-backup-id"));
        super::super::inspect_bundle(Cursor::new(bytes)).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creates_consistent_live_database_sections_with_source_paths() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let catalog_path = directory.join("recordings.db");
        let media_path = directory.join("media");
        let thumbnail_path = directory.join("event-thumbnails");
        std::fs::create_dir_all(&media_path).unwrap();
        std::fs::create_dir_all(&thumbnail_path).unwrap();
        let catalog = crate::storage::RecordingCatalog::open(&catalog_path).unwrap();
        let catalog_handle = catalog.handle();
        catalog_handle
            .upsert_recording(crate::storage::CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front/main".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: media_path
                    .join("front/main/recording-1.mp4")
                    .to_string_lossy()
                    .into_owned(),
                init_offset: 0,
                init_len: 512,
                finalized: true,
            })
            .unwrap();
        catalog_handle
            .insert_event(crate::storage::metadata::TimelineEvent {
                id: "event-1".to_owned(),
                revision: 1,
                camera_id: "front".to_owned(),
                stream: Some("main".to_owned()),
                source: crate::storage::metadata::EventSource::Camera,
                kind: "person".to_owned(),
                start_time_ms: 1_500,
                end_time_ms: Some(1_600),
                confidence: Some(0.9),
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let mut thumbnail = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut thumbnail)
            .encode_image(&image::DynamicImage::new_rgb8(1, 1))
            .unwrap();
        std::fs::write(thumbnail_path.join("event-1.jpg"), &thumbnail).unwrap();
        catalog_handle
            .attach_event_thumbnail(
                "event-1",
                "event-1.jpg",
                u64::try_from(thumbnail.len()).unwrap(),
            )
            .unwrap();
        let notification_path = directory.join("notifications.db");
        let notifications = crate::notifications::Runtime::open(&notification_path).unwrap();
        let notification_handle = notifications.handle();
        let storage_paths = BackupStoragePaths::new(media_path.clone(), thumbnail_path.clone());
        let application_token = "a23456789012345678901234567890";
        let user_key = "u23456789012345678901234567890";
        let saved = notification_handle
            .save_draft(notification_rule(application_token, user_key), 0, 1_000)
            .unwrap();
        notification_handle
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[
                BackupSection::RuntimeConfig,
                BackupSection::CameraDatabase,
                BackupSection::RecordingCatalog,
                BackupSection::EventMetadata,
                BackupSection::EventThumbnails,
                BackupSection::Notifications,
            ],
            created_at_unix_ms: 1_788_000_000_000,
            recording_catalog: Some(&catalog_handle),
            notifications: Some(&notification_handle),
            storage_paths: Some(&storage_paths),
        };

        let (bundle, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();
        let canonical_catalog = std::fs::canonicalize(&catalog_path).unwrap();
        let canonical_notifications = std::fs::canonicalize(&notification_path).unwrap();

        assert!(manifest.source_paths.iter().any(|path| {
            path.kind == backup_proto::BackupPathKind::RecordingCatalog as i32
                && path.path == canonical_catalog.to_string_lossy()
        }));
        assert!(manifest.source_paths.iter().any(|path| {
            path.kind == backup_proto::BackupPathKind::LongTermMedia as i32
                && path.path
                    == std::fs::canonicalize(&media_path)
                        .unwrap()
                        .to_string_lossy()
        }));
        assert!(manifest.source_paths.iter().any(|path| {
            path.kind == backup_proto::BackupPathKind::EventThumbnails as i32
                && path.path
                    == std::fs::canonicalize(&thumbnail_path)
                        .unwrap()
                        .to_string_lossy()
        }));
        assert!(manifest.source_paths.iter().any(|path| {
            path.kind == backup_proto::BackupPathKind::NotificationDatabase as i32
                && path.path == canonical_notifications.to_string_lossy()
        }));
        assert!(!std::fs::read_dir(&directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".keeppeek-backup-")
        }));
        let bytes = bundle.into_inner();
        super::super::inspect_bundle(Cursor::new(bytes.clone())).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut expanded = Vec::new();
        for index in 0..archive.len() {
            archive
                .by_index(index)
                .unwrap()
                .read_to_end(&mut expanded)
                .unwrap();
        }
        for secret in [application_token, user_key] {
            assert!(
                !expanded
                    .windows(secret.len())
                    .any(|candidate| candidate == secret.as_bytes()),
                "artifact contains a notification credential"
            );
        }
        assert_eq!(manifest.required_secret_references.len(), 3);
        assert!(manifest.sections.iter().any(|section| {
            section.section == backup_proto::BackupSection::EventMetadata as i32
        }));
        let thumbnails = manifest
            .sections
            .iter()
            .find(|section| section.section == backup_proto::BackupSection::EventThumbnails as i32)
            .unwrap();
        let inventory: ConfigurationSectionDocument = serde_json::from_slice(
            &super::super::read_member(&mut archive, &thumbnails.path, thumbnails.bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(inventory.values["entries"][0]["eventId"], "event-1");
        assert_eq!(
            inventory.values["entries"][0]["bytes"],
            u64::try_from(thumbnail.len()).unwrap()
        );
        assert_eq!(
            inventory.values["entries"][0]["sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        drop(notification_handle);
        notifications.shutdown();
        drop(catalog_handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn notification_rule(
        application_token: &str,
        user_key: &str,
    ) -> crate::notifications::model::Rule {
        serde_json::from_value(serde_json::json!({
            "id": "rule-1",
            "name": "Front door alert",
            "enabled": true,
            "revision": 0,
            "owner_id": "owner-1",
            "triggers": ["test"],
            "filter": {},
            "schedule": { "timezone": "UTC", "active_windows": [], "quiet_hours": null },
            "critical_bypass": null,
            "enrichment": {
                "deadline_ms": 10000,
                "maximum_revisions": 2,
                "maximum_attempts": 2,
                "maximum_attachment_bytes": 1048576,
                "wake_after_deadline": false
            },
            "actions": [{
                "enabled": true,
                "channel": "push",
                "destination": serde_json::json!({
                    "application_token": application_token,
                    "user_key": user_key,
                    "priority": 0
                }).to_string(),
                "template": { "title": "Alert", "body": "Open KeepPeek" },
                "attachment": "never",
                "allow_second_delivery": false
            }],
            "failure": {
                "maximum_attempts": 3,
                "maximum_retry_interval_ms": 60000,
                "expiry_ms": 3600000
            }
        }))
        .unwrap()
    }
}
