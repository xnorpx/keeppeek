use super::{BackupSection, MANIFEST_PATH};
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
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ConfigurationSectionDocument {
    pub schema_version: u32,
    pub revision: String,
    pub values: serde_json::Value,
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
        options.config_path,
        &sanitized,
        &selected,
        &config_revision,
        &mut required_secret_references,
        options.recording_catalog,
        options.notifications,
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
    if references.is_empty() && !value.is_empty() && !public_string(path, value) {
        let reference = generated_secret_reference(path);
        required.insert(reference.clone());
        *value = reference;
    } else {
        required.extend(references);
    }
    Ok(())
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
    config_path: &Path,
    root: &toml::Table,
    selected: &[BackupSection],
    config_revision: &str,
    required_secret_references: &mut BTreeSet<String>,
    recording_catalog: Option<&crate::storage::RecordingCatalogHandle>,
    notifications: Option<&crate::notifications::Handle>,
) -> anyhow::Result<Vec<PreparedSection>> {
    let (runtime, cameras, integrations) = split_configuration(root);
    let mut prepared = Vec::with_capacity(selected.len());
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
            BackupSection::Access => toml_sidecar(*kind, config_path, "access.toml")?,
            BackupSection::Layouts => json_sidecar(
                *kind,
                config_path,
                "peek-layouts.json",
                required_secret_references,
            )?,
            BackupSection::ConfigurationTemplates => json_sidecar(
                *kind,
                config_path,
                "configuration-templates.json",
                required_secret_references,
            )?,
            BackupSection::RecordingCatalog => {
                database_section(*kind, config_path, |destination| {
                    recording_catalog
                        .ok_or_else(|| anyhow::anyhow!("recording catalog is unavailable"))?
                        .snapshot_to(
                            destination,
                            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
                        )
                })?
            }
            BackupSection::Notifications => database_section(*kind, config_path, |destination| {
                notifications
                    .ok_or_else(|| anyhow::anyhow!("notification store is unavailable"))?
                    .snapshot_to(
                        destination,
                        super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
                    )
            })?,
            _ => unreachable!("selected_sections permits only implemented file sections"),
        };
        prepared.push(section);
    }
    Ok(prepared)
}

fn database_section(
    kind: BackupSection,
    config_path: &Path,
    snapshot: impl FnOnce(&Path) -> anyhow::Result<u64>,
) -> anyhow::Result<PreparedSection> {
    let destination = config_path.with_file_name(format!(
        ".keeppeek-backup-{}-{}.db",
        kind.as_str(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        snapshot(&destination)?;
        let bytes = read_bounded_file(&destination)?;
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

fn toml_sidecar(
    kind: BackupSection,
    config_path: &Path,
    file_name: &str,
) -> anyhow::Result<PreparedSection> {
    let path = config_path.with_file_name(file_name);
    let bytes = read_bounded_file(&path)?;
    toml::from_str::<toml::Table>(std::str::from_utf8(&bytes)?)?;
    let revision = super::encode_lower_hex(Sha256::digest(&bytes));
    Ok(PreparedSection {
        kind,
        bytes,
        revision,
    })
}

fn json_sidecar(
    kind: BackupSection,
    config_path: &Path,
    file_name: &str,
    required_secret_references: &mut BTreeSet<String>,
) -> anyhow::Result<PreparedSection> {
    let path = config_path.with_file_name(file_name);
    let bytes = read_bounded_file(&path)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    collect_json_secret_references(&value, required_secret_references)?;
    let revision = super::encode_lower_hex(Sha256::digest(&bytes));
    json_section(kind, value, &revision)
}

fn read_bounded_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let size = std::fs::metadata(path)?.len();
    if size > MAXIMUM_FILE_SECTION_BYTES {
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
        std::fs::write(
            directory.join("access.toml"),
            "version = 1\ncredentials = []\naudit = []\n",
        )
        .unwrap();
        std::fs::write(
            directory.join("peek-layouts.json"),
            r#"{"schema_version":1,"revision":1,"shared_layouts":[],"users":{}}"#,
        )
        .unwrap();
        std::fs::write(
            directory.join("configuration-templates.json"),
            r#"{"document_version":1,"revision":1,"templates":[{"password_secret_reference":"{secret:TEMPLATE_PASSWORD}"}]}"#,
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
        };

        let (bundle, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();

        assert_eq!(manifest.sections.len(), 5);
        assert!(
            manifest
                .required_secret_references
                .contains(&"{secret:TEMPLATE_PASSWORD}".to_owned())
        );
        super::super::inspect_bundle(Cursor::new(bundle.into_inner())).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creates_consistent_live_database_sections_with_source_paths() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let catalog_path = directory.join("recordings.db");
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
                path: "front/main/recording-1.mp4".to_owned(),
                init_offset: 0,
                init_len: 512,
                finalized: true,
            })
            .unwrap();
        let notification_path = directory.join("notifications.db");
        let notifications = crate::notifications::Runtime::open(&notification_path).unwrap();
        let notification_handle = notifications.handle();
        let options = CreateBundleOptions {
            config_path: &config_path,
            sections: &[
                BackupSection::RuntimeConfig,
                BackupSection::CameraDatabase,
                BackupSection::RecordingCatalog,
                BackupSection::Notifications,
            ],
            created_at_unix_ms: 1_788_000_000_000,
            recording_catalog: Some(&catalog_handle),
            notifications: Some(&notification_handle),
        };

        let (bundle, manifest) = create_bundle(Cursor::new(Vec::new()), options).unwrap();
        let canonical_catalog = std::fs::canonicalize(&catalog_path).unwrap();
        let canonical_notifications = std::fs::canonicalize(&notification_path).unwrap();

        assert!(manifest.source_paths.iter().any(|path| {
            path.kind == backup_proto::BackupPathKind::RecordingCatalog as i32
                && path.path == canonical_catalog.to_string_lossy()
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
        super::super::inspect_bundle(Cursor::new(bundle.into_inner())).unwrap();
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
}
