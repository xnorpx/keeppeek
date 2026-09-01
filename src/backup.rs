//! Validates versioned KeepPeek configuration backup bundles before restore.
//!
//! Inspection is read-only. A successful result proves that the archive inventory, declared
//! sizes, paths, and checksums satisfy this format version. Section-specific schema validation
//! remains the responsibility of a later restore plan.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Seek},
};
use zip::ZipArchive;

/// The ZIP member that describes every section in a backup bundle.
pub const MANIFEST_PATH: &str = "manifest.json";

const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy)]
struct InspectionLimits {
    maximum_archive_members: usize,
    maximum_manifest_bytes: u64,
    maximum_section_bytes: u64,
    maximum_total_bytes: u64,
}

const DEFAULT_INSPECTION_LIMITS: InspectionLimits = InspectionLimits {
    maximum_archive_members: 64,
    maximum_manifest_bytes: 1024 * 1024,
    maximum_section_bytes: 512 * 1024 * 1024,
    maximum_total_bytes: 1024 * 1024 * 1024,
};
const MAX_METADATA_VALUE_BYTES: usize = 128;
const MAX_FEATURE_CAPABILITIES: usize = 64;

/// A section that can be carried by a KeepPeek configuration backup.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BackupSection {
    RuntimeConfig,
    CameraDatabase,
    RecordingCatalog,
    EventMetadata,
    EventThumbnails,
    Groups,
    Layouts,
    Notifications,
    Integrations,
    Access,
    StateStore,
}

impl BackupSection {
    /// Returns the stable manifest name for this section.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeConfig => "runtime_config",
            Self::CameraDatabase => "camera_database",
            Self::RecordingCatalog => "recording_catalog",
            Self::EventMetadata => "event_metadata",
            Self::EventThumbnails => "event_thumbnails",
            Self::Groups => "groups",
            Self::Layouts => "layouts",
            Self::Notifications => "notifications",
            Self::Integrations => "integrations",
            Self::Access => "access",
            Self::StateStore => "state_store",
        }
    }
}

/// The policy used for sensitive values in a backup bundle.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupSecretPolicy {
    /// Resolved secrets are omitted while their external references remain intact.
    ReferencesOnly,
}

/// The operating-system and architecture provenance of a backup bundle.
#[derive(Debug, PartialEq, Eq)]
pub struct BackupSource {
    os: String,
    arch: String,
}

impl BackupSource {
    /// Returns the source operating-system identifier.
    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    /// Returns the source architecture identifier.
    #[must_use]
    pub fn arch(&self) -> &str {
        &self.arch
    }
}

/// Metadata for one independently restorable backup section.
#[derive(Debug, PartialEq, Eq)]
pub struct BackupManifestSection {
    kind: BackupSection,
    path: String,
    schema_version: u32,
    bytes: u64,
    sha256: String,
}

impl BackupManifestSection {
    /// Returns the domain represented by this section.
    #[must_use]
    pub const fn kind(&self) -> BackupSection {
        self.kind
    }

    /// Returns the section's deterministic relative ZIP path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the persisted schema version for this section.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the section's uncompressed byte length.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the lowercase or uppercase SHA-256 digest recorded by the creator.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// A validated backup manifest whose declared sections match the bundle contents.
#[derive(Debug, PartialEq, Eq)]
pub struct BackupManifest {
    format_version: u32,
    created_at_ms: u64,
    keeppeek_version: String,
    source: BackupSource,
    feature_capabilities: Vec<String>,
    secret_policy: BackupSecretPolicy,
    sections: Vec<BackupManifestSection>,
}

#[derive(Deserialize)]
struct UnvalidatedBackupSource {
    os: String,
    arch: String,
}

#[derive(Deserialize)]
struct UnvalidatedBackupManifestSection {
    kind: BackupSection,
    path: String,
    schema_version: u32,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct UnvalidatedBackupManifest {
    format_version: u32,
    created_at_ms: u64,
    keeppeek_version: String,
    source: UnvalidatedBackupSource,
    feature_capabilities: Vec<String>,
    secret_policy: BackupSecretPolicy,
    sections: Vec<UnvalidatedBackupManifestSection>,
}

impl From<UnvalidatedBackupManifest> for BackupManifest {
    fn from(manifest: UnvalidatedBackupManifest) -> Self {
        Self {
            format_version: manifest.format_version,
            created_at_ms: manifest.created_at_ms,
            keeppeek_version: manifest.keeppeek_version,
            source: BackupSource {
                os: manifest.source.os,
                arch: manifest.source.arch,
            },
            feature_capabilities: manifest.feature_capabilities,
            secret_policy: manifest.secret_policy,
            sections: manifest
                .sections
                .into_iter()
                .map(|section| BackupManifestSection {
                    kind: section.kind,
                    path: section.path,
                    schema_version: section.schema_version,
                    bytes: section.bytes,
                    sha256: section.sha256,
                })
                .collect(),
        }
    }
}

impl BackupManifest {
    /// Returns the backup bundle format version.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Returns the bundle creation time as Unix milliseconds.
    #[must_use]
    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    /// Returns the KeepPeek version that created the bundle.
    #[must_use]
    pub fn keeppeek_version(&self) -> &str {
        &self.keeppeek_version
    }

    /// Returns the platform that created the bundle.
    #[must_use]
    pub const fn source(&self) -> &BackupSource {
        &self.source
    }

    /// Returns the feature capabilities recorded when the snapshot was created.
    #[must_use]
    pub fn feature_capabilities(&self) -> &[String] {
        &self.feature_capabilities
    }

    /// Returns the bundle's declared handling of sensitive values.
    #[must_use]
    pub const fn secret_policy(&self) -> BackupSecretPolicy {
        self.secret_policy
    }

    /// Returns the validated sections in manifest order.
    #[must_use]
    pub fn sections(&self) -> &[BackupManifestSection] {
        &self.sections
    }
}

/// Parses a backup ZIP and verifies the manifest, member sizes, paths, and checksums.
///
/// # Errors
///
/// Returns an error when the input is not a supported KeepPeek backup or any archive member fails
/// the format's integrity, inventory, path, type, or size constraints.
pub fn inspect_bundle<R: Read + Seek>(reader: R) -> anyhow::Result<BackupManifest> {
    inspect_bundle_with_limits(reader, DEFAULT_INSPECTION_LIMITS)
}

fn inspect_bundle_with_limits<R: Read + Seek>(
    reader: R,
    limits: InspectionLimits,
) -> anyhow::Result<BackupManifest> {
    let mut archive = ZipArchive::new(reader)
        .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup ZIP: {error}"))?;
    let archive_paths = validate_archive_entries(&mut archive, limits)?;

    let manifest_bytes = read_member(&mut archive, MANIFEST_PATH, limits.maximum_manifest_bytes)?;
    let manifest: UnvalidatedBackupManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup manifest: {error}"))?;
    if manifest.format_version != FORMAT_VERSION {
        anyhow::bail!(
            "unsupported KeepPeek backup format {}",
            manifest.format_version
        );
    }
    let manifest = BackupManifest::from(manifest);
    validate_manifest_metadata(&manifest)?;

    let mut section_paths = HashSet::with_capacity(manifest.sections.len());
    let mut section_kinds = HashSet::with_capacity(manifest.sections.len());
    for section in &manifest.sections {
        if section.schema_version == 0 {
            anyhow::bail!("backup section schemas must be nonzero");
        }
        if !section_kinds.insert(section.kind) {
            anyhow::bail!(
                "backup manifest contains duplicate {} section",
                section.kind.as_str()
            );
        }
        if section.path == MANIFEST_PATH || !section_paths.insert(section.path.as_str()) {
            anyhow::bail!(
                "backup manifest contains duplicate section path {:?}",
                section.path
            );
        }
        if section.bytes > limits.maximum_section_bytes {
            anyhow::bail!("backup section {:?} exceeds the size limit", section.path);
        }
        verify_section(&mut archive, section)?;
    }
    if let Some(path) = archive_paths
        .iter()
        .find(|path| path.as_str() != MANIFEST_PATH && !section_paths.contains(path.as_str()))
    {
        anyhow::bail!("KeepPeek backup member {path:?} is not listed in the manifest");
    }

    Ok(manifest)
}

fn verify_section<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    section: &BackupManifestSection,
) -> anyhow::Result<()> {
    let entry = archive.by_name(&section.path).map_err(|error| {
        anyhow::anyhow!("KeepPeek backup is missing {:?}: {error}", section.path)
    })?;
    if !entry.is_file() || entry.size() != section.bytes {
        anyhow::bail!(
            "backup section {:?} does not match its declared size",
            section.path
        );
    }

    let mut reader = entry.take(section.bytes.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut actual_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            anyhow::anyhow!(
                "failed to read KeepPeek backup section {:?}: {error}",
                section.path
            )
        })?;
        if read == 0 {
            break;
        }
        actual_bytes = actual_bytes
            .checked_add(u64::try_from(read)?)
            .ok_or_else(|| anyhow::anyhow!("backup section size overflow"))?;
        hasher.update(&buffer[..read]);
    }
    if actual_bytes != section.bytes {
        anyhow::bail!(
            "backup section {:?} does not match its declared size",
            section.path
        );
    }

    let actual = encode_lower_hex(hasher.finalize());
    if section.sha256.len() != 64
        || !section.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !section.sha256.eq_ignore_ascii_case(&actual)
    {
        anyhow::bail!("backup section {:?} checksum does not match", section.path);
    }
    Ok(())
}

fn validate_manifest_metadata(manifest: &BackupManifest) -> anyhow::Result<()> {
    if manifest.created_at_ms == 0 {
        anyhow::bail!("backup manifest created_at_ms must be nonzero");
    }
    if manifest.secret_policy != BackupSecretPolicy::ReferencesOnly {
        anyhow::bail!("backup manifest secret_policy must be references_only");
    }
    if manifest.sections.is_empty() {
        anyhow::bail!("backup manifest must contain at least one section");
    }
    validate_metadata_value("keeppeek_version", &manifest.keeppeek_version)?;
    validate_metadata_value("source.os", &manifest.source.os)?;
    validate_metadata_value("source.arch", &manifest.source.arch)?;
    if manifest.feature_capabilities.len() > MAX_FEATURE_CAPABILITIES {
        anyhow::bail!("backup manifest has too many feature capabilities");
    }
    let mut capabilities = HashSet::with_capacity(manifest.feature_capabilities.len());
    for capability in &manifest.feature_capabilities {
        validate_metadata_value("feature capability", capability)?;
        if !capabilities.insert(capability) {
            anyhow::bail!("backup manifest contains duplicate feature capability {capability:?}");
        }
    }
    Ok(())
}

fn validate_metadata_value(name: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > MAX_METADATA_VALUE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        anyhow::bail!(
            "backup manifest {name} must be 1 to {MAX_METADATA_VALUE_BYTES} printable bytes"
        );
    }
    Ok(())
}

fn validate_archive_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: InspectionLimits,
) -> anyhow::Result<HashSet<String>> {
    if archive.len() > limits.maximum_archive_members {
        anyhow::bail!(
            "KeepPeek backup has {} members, exceeding the limit of {}",
            archive.len(),
            limits.maximum_archive_members
        );
    }
    let mut names = HashSet::with_capacity(archive.len());
    let mut case_folded_names = HashSet::with_capacity(archive.len());
    let mut total_bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup ZIP entry: {error}"))?;
        let name = entry.name().to_owned();
        if entry.is_symlink() {
            anyhow::bail!("KeepPeek backup contains symlink entry {name:?}");
        }
        if !entry.is_file() || entry.encrypted() {
            anyhow::bail!("KeepPeek backup contains unsupported entry {name:?}");
        }
        if entry.enclosed_name().is_none()
            || name.starts_with('/')
            || has_windows_drive_prefix(&name)
            || name.contains('\\')
            || name
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            anyhow::bail!("KeepPeek backup contains unsafe entry {name:?}");
        }
        if !names.insert(name.clone()) {
            anyhow::bail!("KeepPeek backup contains duplicate entry {name:?}");
        }
        if !case_folded_names.insert(name.to_lowercase()) {
            anyhow::bail!("KeepPeek backup contains case-colliding entry {name:?}");
        }
        let maximum_bytes = if name == MANIFEST_PATH {
            limits.maximum_manifest_bytes
        } else {
            limits.maximum_section_bytes
        };
        if entry.size() > maximum_bytes {
            anyhow::bail!("KeepPeek backup member {name:?} exceeds the size limit");
        }
        total_bytes = total_bytes
            .checked_add(entry.size())
            .ok_or_else(|| anyhow::anyhow!("KeepPeek backup uncompressed size overflow"))?;
        if total_bytes > limits.maximum_total_bytes {
            anyhow::bail!("KeepPeek backup exceeds the total uncompressed size limit");
        }
    }
    Ok(names)
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn read_member<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    maximum_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    let entry = archive
        .by_name(name)
        .map_err(|error| anyhow::anyhow!("KeepPeek backup is missing {name:?}: {error}"))?;
    if entry.is_dir() || entry.size() > maximum_bytes {
        anyhow::bail!("KeepPeek backup member {name:?} has an invalid size");
    }
    let declared_size = entry.size();
    let capacity = usize::try_from(declared_size)?;
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            anyhow::anyhow!("failed to read KeepPeek backup member {name:?}: {error}")
        })?;
    if bytes.len() as u64 != declared_size {
        anyhow::bail!("KeepPeek backup member {name:?} does not match its declared size");
    }
    Ok(bytes)
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    const RUNTIME_CONFIG_PATH: &str = "config/runtime.toml";

    #[test]
    fn inspects_a_versioned_bundle_and_verifies_its_section() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None);

        let manifest = inspect_bundle(Cursor::new(bundle)).unwrap();

        assert_eq!(manifest.format_version, 1);
        assert_eq!(manifest.created_at_ms(), 1_788_000_000_000);
        assert_eq!(manifest.keeppeek_version(), "0.1.0");
        assert_eq!(manifest.source().os(), "linux");
        assert_eq!(manifest.source().arch(), "x86_64");
        assert_eq!(manifest.feature_capabilities(), ["runtime_config"]);
        assert_eq!(manifest.secret_policy(), BackupSecretPolicy::ReferencesOnly);
        assert_eq!(manifest.sections.len(), 1);
        assert_eq!(manifest.sections[0].kind, BackupSection::RuntimeConfig);
        assert_eq!(manifest.sections[0].path, RUNTIME_CONFIG_PATH);
    }

    #[test]
    fn rejects_a_bundle_with_path_traversal() {
        let bundle = bundle_with("../runtime.toml", b"[storage]\n", None);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("unsafe"));
    }

    #[test]
    fn rejects_a_bundle_with_tampered_section_contents() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", Some("00".repeat(32)));

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("checksum"));
    }

    #[test]
    fn accepts_uppercase_sha256_digests() {
        let contents = b"[storage]\n";
        let bundle = bundle_with(
            RUNTIME_CONFIG_PATH,
            contents,
            Some(sha256(contents).to_uppercase()),
        );

        inspect_bundle(Cursor::new(bundle)).unwrap();
    }

    #[test]
    fn rejects_case_colliding_archive_members() {
        let bundle = bundle_with_extra(
            RUNTIME_CONFIG_PATH,
            b"[storage]\n",
            None,
            &[("Config/runtime.toml", b"collision")],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("case-colliding"));
    }

    #[test]
    fn rejects_members_that_are_not_listed_in_the_manifest() {
        let bundle = bundle_with_extra(
            RUNTIME_CONFIG_PATH,
            b"[storage]\n",
            None,
            &[("secrets/camera-passwords.txt", b"do-not-export")],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("not listed"));
    }

    #[test]
    fn rejects_symlink_archive_members() {
        let target = b"../../outside.toml";
        let manifest = manifest_with(RUNTIME_CONFIG_PATH, target, None);
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file(MANIFEST_PATH, options).unwrap();
        writer.write_all(&manifest).unwrap();
        writer
            .add_symlink(RUNTIME_CONFIG_PATH, "../../outside.toml", options)
            .unwrap();
        let bundle = writer.finish().unwrap().into_inner();

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("symlink"));
    }

    #[test]
    fn rejects_directory_archive_members() {
        let contents = b"[storage]\n";
        let manifest = manifest_with(RUNTIME_CONFIG_PATH, contents, None);
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file(MANIFEST_PATH, options).unwrap();
        writer.write_all(&manifest).unwrap();
        writer.start_file(RUNTIME_CONFIG_PATH, options).unwrap();
        writer.write_all(contents).unwrap();
        writer.add_directory("unexpected/", options).unwrap();
        let bundle = writer.finish().unwrap().into_inner();

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("unsupported entry"));
    }

    #[test]
    fn rejects_encrypted_archive_members() {
        let mut bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None);
        mark_member_encrypted(&mut bundle, RUNTIME_CONFIG_PATH);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid KeepPeek backup ZIP entry"),
            "{error:#}"
        );
    }

    #[test]
    fn rejects_sections_above_the_uncompressed_size_limit() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, &[0; 32], None);
        let limits = InspectionLimits {
            maximum_section_bytes: 16,
            ..DEFAULT_INSPECTION_LIMITS
        };

        let error = inspect_bundle_with_limits(Cursor::new(bundle), limits).unwrap_err();

        assert!(error.to_string().contains("size limit"));
    }

    #[test]
    fn rejects_bundles_above_the_member_count_limit() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None);
        let limits = InspectionLimits {
            maximum_archive_members: 1,
            ..DEFAULT_INSPECTION_LIMITS
        };

        let error = inspect_bundle_with_limits(Cursor::new(bundle), limits).unwrap_err();

        assert!(error.to_string().contains("members, exceeding the limit"));
    }

    #[test]
    fn rejects_bundles_above_the_total_uncompressed_size_limit() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None);
        let limits = InspectionLimits {
            maximum_total_bytes: 1,
            ..DEFAULT_INSPECTION_LIMITS
        };

        let error = inspect_bundle_with_limits(Cursor::new(bundle), limits).unwrap_err();

        assert!(error.to_string().contains("total uncompressed size limit"));
    }

    #[test]
    fn rejects_windows_drive_prefixed_paths_on_every_platform() {
        let bundle = bundle_with("C:/keeppeek/runtime.toml", b"[storage]\n", None);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("unsafe"));
    }

    #[test]
    fn rejects_duplicate_logical_sections() {
        let first = b"[storage]\n";
        let second = b"[storage]\nminimum_free_gb = 20\n";
        let manifest = json!({
            "format_version": 1,
            "created_at_ms": 1_788_000_000_000_u64,
            "keeppeek_version": "0.1.0",
            "source": { "os": "linux", "arch": "x86_64" },
            "feature_capabilities": ["runtime_config"],
            "secret_policy": "references_only",
            "sections": [
                section_manifest("config/runtime.toml", first),
                section_manifest("config/runtime-copy.toml", second)
            ]
        });
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(
            &manifest,
            &[
                ("config/runtime.toml", first.as_slice()),
                ("config/runtime-copy.toml", second.as_slice()),
            ],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("duplicate runtime_config section")
        );
    }

    #[test]
    fn rejects_zero_section_schema_versions() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, contents, None)).unwrap();
        manifest["sections"][0]["schema_version"] = json!(0);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("schemas must be nonzero"));
    }

    #[test]
    fn rejects_zero_creation_timestamps() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, contents, None)).unwrap();
        manifest["created_at_ms"] = json!(0);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("created_at_ms must be nonzero"));
    }

    #[test]
    fn rejects_manifests_without_sections() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None))
                .unwrap();
        manifest["sections"] = json!([]);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("at least one section"));
    }

    #[test]
    fn rejects_the_manifest_as_a_section_path() {
        let manifest = manifest_with(MANIFEST_PATH, b"not-the-manifest", None);
        let bundle = archive_with_manifest(&manifest, &[]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("duplicate section path"));
    }

    #[test]
    fn rejects_future_bundle_format_versions() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None))
                .unwrap();
        manifest["format_version"] = json!(2);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(
            &manifest,
            &[(RUNTIME_CONFIG_PATH, b"[storage]\n".as_slice())],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported KeepPeek backup format 2")
        );
    }

    #[test]
    fn rejects_a_manifest_without_an_explicit_secret_policy() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, contents, None)).unwrap();
        manifest.as_object_mut().unwrap().remove("secret_policy");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("secret_policy"));
    }

    #[test]
    fn rejects_non_reference_only_secret_policies() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, contents, None)).unwrap();
        manifest["secret_policy"] = json!("encrypted");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("references_only"));
    }

    #[test]
    fn rejects_blank_source_version_metadata() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with(RUNTIME_CONFIG_PATH, contents, None)).unwrap();
        manifest["keeppeek_version"] = json!("  ");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("keeppeek_version"));
    }

    fn bundle_with(path: &str, contents: &[u8], hash_override: Option<String>) -> Vec<u8> {
        bundle_with_extra(path, contents, hash_override, &[])
    }

    fn bundle_with_extra(
        path: &str,
        contents: &[u8],
        hash_override: Option<String>,
        extra_members: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let manifest = manifest_with(path, contents, hash_override);
        let mut members = vec![(path, contents)];
        members.extend_from_slice(extra_members);
        archive_with_manifest(&manifest, &members)
    }

    fn archive_with_manifest(manifest: &[u8], members: &[(&str, &[u8])]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file(MANIFEST_PATH, options).unwrap();
        writer.write_all(manifest).unwrap();
        for &(name, extra_contents) in members {
            writer.start_file(name, options).unwrap();
            writer.write_all(extra_contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn manifest_with(path: &str, contents: &[u8], hash_override: Option<String>) -> Vec<u8> {
        let manifest = json!({
            "format_version": 1,
            "created_at_ms": 1_788_000_000_000_u64,
            "keeppeek_version": "0.1.0",
            "source": {
                "os": "linux",
                "arch": "x86_64"
            },
            "feature_capabilities": ["runtime_config"],
            "secret_policy": "references_only",
            "sections": [{
                "kind": "runtime_config",
                "path": path,
                "schema_version": 1,
                "bytes": contents.len(),
                "sha256": hash_override.unwrap_or_else(|| sha256(contents))
            }]
        });
        serde_json::to_vec(&manifest).unwrap()
    }

    fn section_manifest(path: &str, contents: &[u8]) -> serde_json::Value {
        json!({
            "kind": "runtime_config",
            "path": path,
            "schema_version": 1,
            "bytes": contents.len(),
            "sha256": sha256(contents)
        })
    }

    fn mark_member_encrypted(archive: &mut [u8], member_name: &str) {
        const LOCAL_HEADER: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
        const CENTRAL_HEADER: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];

        let mut changed_headers = 0;
        for offset in 0..archive.len().saturating_sub(4) {
            let (header_bytes, name_length_offset, flags_offset) =
                if archive[offset..].starts_with(&LOCAL_HEADER) {
                    (30, 26, 6)
                } else if archive[offset..].starts_with(&CENTRAL_HEADER) {
                    (46, 28, 8)
                } else {
                    continue;
                };
            let Some(header) = archive.get(offset..offset + header_bytes) else {
                continue;
            };
            let name_length = usize::from(u16::from_le_bytes([
                header[name_length_offset],
                header[name_length_offset + 1],
            ]));
            let name_start = offset + header_bytes;
            let name_end = name_start + name_length;
            if archive.get(name_start..name_end) != Some(member_name.as_bytes()) {
                continue;
            }
            archive[offset + flags_offset] |= 1;
            changed_headers += 1;
        }
        assert_eq!(changed_headers, 2);
    }

    fn sha256(contents: &[u8]) -> String {
        encode_lower_hex(Sha256::digest(contents))
    }
}
