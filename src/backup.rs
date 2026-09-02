//! Validates versioned KeepPeek configuration backup bundles before restore.
//!
//! Inspection is read-only. A successful result proves that the archive inventory, declared
//! sizes, paths, and checksums satisfy this format version. Section-specific schema validation
//! remains the responsibility of a later restore plan.

use crate::api::backup_proto;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom},
};
use zip::ZipArchive;

mod create;
pub(crate) mod database;

pub use create::{CreateBundleOptions, create_bundle};

/// The ZIP member that describes every section in a backup bundle.
pub const MANIFEST_PATH: &str = "manifest.json";

const LEGACY_FORMAT_VERSION: u32 = 1;
const FORMAT_VERSION: u32 = 2;

#[derive(Clone, Copy)]
struct InspectionLimits {
    maximum_archive_bytes: u64,
    maximum_archive_members: usize,
    maximum_manifest_bytes: u64,
    maximum_section_bytes: u64,
    maximum_total_bytes: u64,
}

const DEFAULT_INSPECTION_LIMITS: InspectionLimits = InspectionLimits {
    maximum_archive_bytes: 1024 * 1024 * 1024,
    maximum_archive_members: 64,
    maximum_manifest_bytes: 1024 * 1024,
    maximum_section_bytes: 512 * 1024 * 1024,
    maximum_total_bytes: 1024 * 1024 * 1024,
};
const MAX_METADATA_VALUE_BYTES: usize = 128;
const MAX_FEATURE_CAPABILITIES: usize = 64;
const MAX_OMITTED_DATA: usize = 64;
const MAX_REQUIRED_SECRET_REFERENCES: usize = 512;
const MAX_SOURCE_PATHS: usize = 16;
const MAX_SOURCE_PATH_BYTES: usize = 4 * 1024;

/// A section that can be carried by a KeepPeek configuration backup.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    ConfigurationTemplates,
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
            Self::ConfigurationTemplates => "configuration_templates",
        }
    }

    const fn to_proto(self) -> backup_proto::BackupSection {
        match self {
            Self::RuntimeConfig => backup_proto::BackupSection::RuntimeConfig,
            Self::CameraDatabase => backup_proto::BackupSection::CameraDatabase,
            Self::RecordingCatalog => backup_proto::BackupSection::RecordingCatalog,
            Self::EventMetadata => backup_proto::BackupSection::EventMetadata,
            Self::EventThumbnails => backup_proto::BackupSection::EventThumbnails,
            Self::Groups => backup_proto::BackupSection::Groups,
            Self::Layouts => backup_proto::BackupSection::Layouts,
            Self::Notifications => backup_proto::BackupSection::Notifications,
            Self::Integrations => backup_proto::BackupSection::Integrations,
            Self::Access => backup_proto::BackupSection::Access,
            Self::StateStore => backup_proto::BackupSection::StateStore,
            Self::ConfigurationTemplates => backup_proto::BackupSection::ConfigurationTemplates,
        }
    }

    fn from_proto(value: i32) -> anyhow::Result<Self> {
        match backup_proto::BackupSection::try_from(value) {
            Ok(backup_proto::BackupSection::RuntimeConfig) => Ok(Self::RuntimeConfig),
            Ok(backup_proto::BackupSection::CameraDatabase) => Ok(Self::CameraDatabase),
            Ok(backup_proto::BackupSection::RecordingCatalog) => Ok(Self::RecordingCatalog),
            Ok(backup_proto::BackupSection::EventMetadata) => Ok(Self::EventMetadata),
            Ok(backup_proto::BackupSection::EventThumbnails) => Ok(Self::EventThumbnails),
            Ok(backup_proto::BackupSection::Groups) => Ok(Self::Groups),
            Ok(backup_proto::BackupSection::Layouts) => Ok(Self::Layouts),
            Ok(backup_proto::BackupSection::Notifications) => Ok(Self::Notifications),
            Ok(backup_proto::BackupSection::Integrations) => Ok(Self::Integrations),
            Ok(backup_proto::BackupSection::Access) => Ok(Self::Access),
            Ok(backup_proto::BackupSection::StateStore) => Ok(Self::StateStore),
            Ok(backup_proto::BackupSection::ConfigurationTemplates) => {
                Ok(Self::ConfigurationTemplates)
            }
            Ok(backup_proto::BackupSection::Unspecified) | Err(_) => {
                anyhow::bail!("backup manifest contains an unspecified section")
            }
        }
    }

    const fn canonical_path(self) -> &'static str {
        match self {
            Self::RuntimeConfig => "config/runtime.toml",
            Self::CameraDatabase => "config/cameras.json",
            Self::RecordingCatalog => "catalog/recordings.db",
            Self::EventMetadata => "events/metadata.json",
            Self::EventThumbnails => "events/thumbnails.json",
            Self::Groups => "config/groups.json",
            Self::Layouts => "state/peek-layouts.json",
            Self::Notifications => "notifications/notifications.db",
            Self::Integrations => "config/integrations.json",
            Self::Access => "access/access.toml",
            Self::StateStore => "state/state-store.json",
            Self::ConfigurationTemplates => "config/configuration-templates.json",
        }
    }

    const fn required_dependencies(self) -> &'static [Self] {
        match self {
            Self::EventMetadata => &[Self::RecordingCatalog],
            Self::EventThumbnails => &[Self::EventMetadata],
            Self::Groups | Self::Layouts => &[Self::CameraDatabase],
            Self::Integrations => &[Self::RuntimeConfig],
            _ => &[],
        }
    }

    const fn encoding(self) -> SectionEncoding {
        match self {
            Self::RuntimeConfig | Self::Access => SectionEncoding::Toml,
            Self::RecordingCatalog | Self::Notifications => SectionEncoding::Sqlite,
            _ => SectionEncoding::Json,
        }
    }

    const fn maximum_document_bytes(self) -> u64 {
        match self.encoding() {
            SectionEncoding::Toml | SectionEncoding::Json => 16 * 1024 * 1024,
            SectionEncoding::Sqlite => DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
        }
    }
}

#[derive(Clone, Copy)]
enum SectionEncoding {
    Toml,
    Json,
    Sqlite,
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

/// A logical host path recorded for explicit restore mapping.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackupPathKind {
    ConfigDirectory,
    RecordingCatalog,
    LongTermMedia,
    EventThumbnails,
    NotificationDatabase,
}

impl BackupPathKind {
    fn from_proto(value: i32) -> anyhow::Result<Self> {
        match backup_proto::BackupPathKind::try_from(value) {
            Ok(backup_proto::BackupPathKind::ConfigDirectory) => Ok(Self::ConfigDirectory),
            Ok(backup_proto::BackupPathKind::RecordingCatalog) => Ok(Self::RecordingCatalog),
            Ok(backup_proto::BackupPathKind::LongTermMedia) => Ok(Self::LongTermMedia),
            Ok(backup_proto::BackupPathKind::EventThumbnails) => Ok(Self::EventThumbnails),
            Ok(backup_proto::BackupPathKind::NotificationDatabase) => {
                Ok(Self::NotificationDatabase)
            }
            Ok(backup_proto::BackupPathKind::Unspecified) | Err(_) => {
                anyhow::bail!("backup manifest contains an unspecified source path")
            }
        }
    }

    const fn to_proto(self) -> backup_proto::BackupPathKind {
        match self {
            Self::ConfigDirectory => backup_proto::BackupPathKind::ConfigDirectory,
            Self::RecordingCatalog => backup_proto::BackupPathKind::RecordingCatalog,
            Self::LongTermMedia => backup_proto::BackupPathKind::LongTermMedia,
            Self::EventThumbnails => backup_proto::BackupPathKind::EventThumbnails,
            Self::NotificationDatabase => backup_proto::BackupPathKind::NotificationDatabase,
        }
    }
}

/// One source path that a restore plan must map to the target installation.
#[derive(Debug, PartialEq, Eq)]
pub struct BackupPath {
    kind: BackupPathKind,
    path: String,
}

impl BackupPath {
    /// Returns the logical purpose of this source path.
    #[must_use]
    pub const fn kind(&self) -> BackupPathKind {
        self.kind
    }

    /// Returns the source installation path for display and explicit mapping.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
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
    revision: String,
    dependencies: Vec<BackupSection>,
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

    /// Returns the source revision captured for this section.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the sections that must accompany this section in the bundle.
    #[must_use]
    pub fn dependencies(&self) -> &[BackupSection] {
        &self.dependencies
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
    omitted_data: Vec<String>,
    required_secret_references: Vec<String>,
    source_paths: Vec<BackupPath>,
    snapshot_revision: String,
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

#[derive(Deserialize)]
struct BackupManifestVersion {
    #[serde(alias = "formatVersion")]
    format_version: u32,
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
                    revision: String::new(),
                    dependencies: Vec::new(),
                })
                .collect(),
            omitted_data: Vec::new(),
            required_secret_references: Vec::new(),
            source_paths: Vec::new(),
            snapshot_revision: String::new(),
        }
    }
}

impl TryFrom<backup_proto::BackupManifest> for BackupManifest {
    type Error = anyhow::Error;

    fn try_from(manifest: backup_proto::BackupManifest) -> Result<Self, Self::Error> {
        let source = manifest
            .source
            .ok_or_else(|| anyhow::anyhow!("backup manifest source is required"))?;
        if backup_proto::BackupSecretPolicy::try_from(manifest.secret_policy)
            != Ok(backup_proto::BackupSecretPolicy::ReferencesOnly)
        {
            anyhow::bail!("backup manifest secret policy must be references only");
        }
        let sections = manifest
            .sections
            .into_iter()
            .map(|section| {
                let dependencies = section
                    .dependencies
                    .into_iter()
                    .map(BackupSection::from_proto)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(BackupManifestSection {
                    kind: BackupSection::from_proto(section.section)?,
                    path: section.path,
                    schema_version: section.schema_version,
                    bytes: section.bytes,
                    sha256: section.sha256,
                    revision: section.revision,
                    dependencies,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let source_paths = manifest
            .source_paths
            .into_iter()
            .map(|path| {
                Ok(BackupPath {
                    kind: BackupPathKind::from_proto(path.kind)?,
                    path: path.path,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            format_version: manifest.format_version,
            created_at_ms: manifest.created_at_unix_ms,
            keeppeek_version: manifest.keeppeek_version,
            source: BackupSource {
                os: source.operating_system,
                arch: source.architecture,
            },
            feature_capabilities: manifest.feature_capabilities,
            secret_policy: BackupSecretPolicy::ReferencesOnly,
            sections,
            omitted_data: manifest.omitted_data,
            required_secret_references: manifest.required_secret_references,
            source_paths,
            snapshot_revision: manifest.snapshot_revision,
        })
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

    /// Returns the intentionally excluded data categories.
    #[must_use]
    pub fn omitted_data(&self) -> &[String] {
        &self.omitted_data
    }

    /// Returns the external secret references required after restore.
    #[must_use]
    pub fn required_secret_references(&self) -> &[String] {
        &self.required_secret_references
    }

    /// Returns source paths that require explicit target mappings.
    #[must_use]
    pub fn source_paths(&self) -> &[BackupPath] {
        &self.source_paths
    }

    /// Returns the revision shared by all current-format section snapshots.
    #[must_use]
    pub fn snapshot_revision(&self) -> &str {
        &self.snapshot_revision
    }

    /// Returns the generated message used by the HTTP ProtoJSON API.
    #[must_use]
    pub fn to_proto(&self) -> backup_proto::BackupManifest {
        backup_proto::BackupManifest {
            format_version: self.format_version,
            created_at_unix_ms: self.created_at_ms,
            keeppeek_version: self.keeppeek_version.clone(),
            source: Some(backup_proto::BackupSource {
                operating_system: self.source.os.clone(),
                architecture: self.source.arch.clone(),
            }),
            feature_capabilities: self.feature_capabilities.clone(),
            secret_policy: backup_proto::BackupSecretPolicy::ReferencesOnly as i32,
            sections: self
                .sections
                .iter()
                .map(|section| backup_proto::BackupSectionDescriptor {
                    section: section.kind.to_proto() as i32,
                    path: section.path.clone(),
                    schema_version: section.schema_version,
                    bytes: section.bytes,
                    sha256: section.sha256.clone(),
                    revision: section.revision.clone(),
                    dependencies: section
                        .dependencies
                        .iter()
                        .map(|dependency| dependency.to_proto() as i32)
                        .collect(),
                })
                .collect(),
            omitted_data: self.omitted_data.clone(),
            required_secret_references: self.required_secret_references.clone(),
            source_paths: self
                .source_paths
                .iter()
                .map(|path| backup_proto::BackupPath {
                    kind: path.kind.to_proto() as i32,
                    path: path.path.clone(),
                })
                .collect(),
            snapshot_revision: self.snapshot_revision.clone(),
        }
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
    mut reader: R,
    limits: InspectionLimits,
) -> anyhow::Result<BackupManifest> {
    let archive_bytes = reader
        .seek(SeekFrom::End(0))
        .map_err(|error| anyhow::anyhow!("failed to measure KeepPeek backup ZIP: {error}"))?;
    if archive_bytes > limits.maximum_archive_bytes {
        anyhow::bail!("KeepPeek backup exceeds the compressed size limit");
    }
    reader
        .rewind()
        .map_err(|error| anyhow::anyhow!("failed to rewind KeepPeek backup ZIP: {error}"))?;
    let mut archive = ZipArchive::new(reader)
        .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup ZIP: {error}"))?;
    let archive_paths = validate_archive_entries(&mut archive, limits)?;

    let manifest_bytes = read_member(&mut archive, MANIFEST_PATH, limits.maximum_manifest_bytes)?;
    let manifest = decode_manifest(&manifest_bytes)?;
    validate_manifest_metadata(&manifest)?;

    let section_paths = validate_sections(&manifest, limits)?;
    for section in &manifest.sections {
        verify_section(&mut archive, section)?;
        if manifest.format_version == FORMAT_VERSION {
            validate_section_content(&mut archive, section)?;
        }
    }
    if let Some(path) = archive_paths
        .iter()
        .find(|path| path.as_str() != MANIFEST_PATH && !section_paths.contains(path.as_str()))
    {
        anyhow::bail!("KeepPeek backup member {path:?} is not listed in the manifest");
    }

    Ok(manifest)
}

fn validate_sections(
    manifest: &BackupManifest,
    limits: InspectionLimits,
) -> anyhow::Result<HashSet<&str>> {
    let mut section_paths = HashSet::with_capacity(manifest.sections.len());
    let mut section_kinds = HashSet::with_capacity(manifest.sections.len());
    for section in &manifest.sections {
        if section.schema_version == 0 {
            anyhow::bail!("backup section schemas must be nonzero");
        }
        if manifest.format_version == FORMAT_VERSION && section.schema_version != 1 {
            anyhow::bail!(
                "backup {} section uses unsupported schema {}",
                section.kind.as_str(),
                section.schema_version
            );
        }
        if manifest.format_version == FORMAT_VERSION {
            validate_metadata_value("section revision", &section.revision)?;
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
        if manifest.format_version == FORMAT_VERSION
            && section.path != section.kind.canonical_path()
        {
            anyhow::bail!(
                "backup {} section must use canonical path {:?}",
                section.kind.as_str(),
                section.kind.canonical_path()
            );
        }
        if section.bytes > limits.maximum_section_bytes
            || section.bytes > section.kind.maximum_document_bytes()
        {
            anyhow::bail!("backup section {:?} exceeds the size limit", section.path);
        }
    }
    if manifest.format_version == FORMAT_VERSION {
        for section in &manifest.sections {
            let mut declared_dependencies = HashSet::with_capacity(section.dependencies.len());
            for dependency in &section.dependencies {
                if *dependency == section.kind || !declared_dependencies.insert(*dependency) {
                    anyhow::bail!(
                        "backup {} section has an invalid dependency declaration",
                        section.kind.as_str()
                    );
                }
                if !section_kinds.contains(dependency) {
                    anyhow::bail!(
                        "backup {} section requires {} section",
                        section.kind.as_str(),
                        dependency.as_str()
                    );
                }
            }
            for dependency in section.kind.required_dependencies() {
                if !section_kinds.contains(dependency) {
                    anyhow::bail!(
                        "backup {} section requires {} section",
                        section.kind.as_str(),
                        dependency.as_str()
                    );
                }
                if !declared_dependencies.contains(dependency) {
                    anyhow::bail!(
                        "backup {} section must declare {} dependency",
                        section.kind.as_str(),
                        dependency.as_str()
                    );
                }
            }
        }
    }
    Ok(section_paths)
}

fn decode_manifest(bytes: &[u8]) -> anyhow::Result<BackupManifest> {
    let version: BackupManifestVersion = serde_json::from_slice(bytes)
        .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup manifest: {error}"))?;
    match version.format_version {
        LEGACY_FORMAT_VERSION => serde_json::from_slice::<UnvalidatedBackupManifest>(bytes)
            .map(BackupManifest::from)
            .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup manifest: {error}")),
        FORMAT_VERSION => serde_json::from_slice::<backup_proto::BackupManifest>(bytes)
            .map_err(|error| anyhow::anyhow!("invalid KeepPeek backup manifest: {error}"))?
            .try_into(),
        unsupported => anyhow::bail!("unsupported KeepPeek backup format {unsupported}"),
    }
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

fn validate_section_content<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    section: &BackupManifestSection,
) -> anyhow::Result<()> {
    match section.kind.encoding() {
        SectionEncoding::Toml => {
            let bytes = read_member(
                archive,
                &section.path,
                section.kind.maximum_document_bytes(),
            )?;
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                anyhow::anyhow!("invalid {} section: {error}", section.kind.as_str())
            })?;
            toml::from_str::<toml::Table>(text).map_err(|error| {
                anyhow::anyhow!("invalid {} section: {error}", section.kind.as_str())
            })?;
        }
        SectionEncoding::Json => {
            let bytes = read_member(
                archive,
                &section.path,
                section.kind.maximum_document_bytes(),
            )?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                anyhow::anyhow!("invalid {} section: {error}", section.kind.as_str())
            })?;
            if !value.is_object() {
                anyhow::bail!(
                    "invalid {} section: expected a JSON object",
                    section.kind.as_str()
                );
            }
            if value
                .get("schemaVersion")
                .and_then(serde_json::Value::as_u64)
                != Some(u64::from(section.schema_version))
            {
                anyhow::bail!(
                    "invalid {} section: schemaVersion does not match the manifest",
                    section.kind.as_str()
                );
            }
        }
        SectionEncoding::Sqlite => {
            let mut entry = archive.by_name(&section.path).map_err(|error| {
                anyhow::anyhow!("KeepPeek backup is missing {:?}: {error}", section.path)
            })?;
            let mut header = [0_u8; 16];
            entry.read_exact(&mut header).map_err(|error| {
                anyhow::anyhow!("invalid {} section: {error}", section.kind.as_str())
            })?;
            if header != *b"SQLite format 3\0" {
                anyhow::bail!(
                    "invalid {} section: invalid SQLite header",
                    section.kind.as_str()
                );
            }
        }
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
    if manifest.format_version == FORMAT_VERSION {
        validate_current_manifest_metadata(manifest)?;
    }
    Ok(())
}

fn validate_current_manifest_metadata(manifest: &BackupManifest) -> anyhow::Result<()> {
    validate_metadata_value("snapshot revision", &manifest.snapshot_revision)?;
    if manifest.omitted_data.len() > MAX_OMITTED_DATA {
        anyhow::bail!("backup manifest has too many omitted data categories");
    }
    let mut omitted_data = HashSet::with_capacity(manifest.omitted_data.len());
    for omitted in &manifest.omitted_data {
        validate_metadata_value("omitted data category", omitted)?;
        if !omitted_data.insert(omitted) {
            anyhow::bail!("backup manifest contains duplicate omitted data category {omitted:?}");
        }
    }
    if manifest.required_secret_references.len() > MAX_REQUIRED_SECRET_REFERENCES {
        anyhow::bail!("backup manifest has too many required secret references");
    }
    let mut secret_references = HashSet::with_capacity(manifest.required_secret_references.len());
    for reference in &manifest.required_secret_references {
        validate_metadata_value("secret reference", reference)?;
        if !crate::config::is_secret_reference(reference) {
            anyhow::bail!("backup manifest contains an invalid secret reference");
        }
        if !secret_references.insert(reference) {
            anyhow::bail!("backup manifest contains a duplicate secret reference");
        }
    }
    validate_source_paths(&manifest.source_paths)
}

fn validate_source_paths(source_paths: &[BackupPath]) -> anyhow::Result<()> {
    if source_paths.len() > MAX_SOURCE_PATHS {
        anyhow::bail!("backup manifest has too many source paths");
    }
    let mut kinds = HashSet::with_capacity(source_paths.len());
    for source_path in source_paths {
        let path = &source_path.path;
        if path.is_empty()
            || path.len() > MAX_SOURCE_PATH_BYTES
            || path.trim() != path
            || path.chars().any(char::is_control)
            || !(path.starts_with('/')
                || path.starts_with("\\\\")
                || has_windows_drive_prefix(path))
        {
            anyhow::bail!("backup manifest contains an invalid absolute source path");
        }
        if !kinds.insert(source_path.kind) {
            anyhow::bail!("backup manifest contains duplicate source path kinds");
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

const fn has_windows_drive_prefix(path: &str) -> bool {
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
    fn inspects_the_current_protojson_manifest() {
        let contents = b"[storage]\nlong_term_max_gb = 1\n";
        let manifest = current_manifest_with(RUNTIME_CONFIG_PATH, contents);
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let manifest = inspect_bundle(Cursor::new(bundle)).unwrap();

        assert_eq!(manifest.format_version(), 2);
        assert_eq!(manifest.created_at_ms(), 1_788_000_000_000);
        assert_eq!(manifest.sections()[0].kind(), BackupSection::RuntimeConfig);
        assert_eq!(manifest.sections()[0].revision(), "revision-1");
        assert!(manifest.sections()[0].dependencies().is_empty());
        assert_eq!(manifest.omitted_data(), ["recording_media"]);
        assert_eq!(
            manifest.required_secret_references(),
            ["{secret:CAMERA_PASSWORD}"]
        );
        assert_eq!(manifest.source_paths()[0].path(), "/var/lib/keeppeek");
        assert_eq!(manifest.snapshot_revision(), "snapshot-1");
        let http_manifest = manifest.to_proto();
        assert_eq!(http_manifest.format_version, 2);
        assert_eq!(http_manifest.sections[0].revision, "revision-1");
        assert_eq!(http_manifest.omitted_data, ["recording_media"]);
        assert_eq!(
            http_manifest.required_secret_references,
            ["{secret:CAMERA_PASSWORD}"]
        );
    }

    #[test]
    fn rejects_a_noncanonical_current_section_path() {
        let contents = b"[storage]\n";
        let manifest = current_manifest_with("config/renamed.toml", contents);
        let bundle = archive_with_manifest(&manifest, &[("config/renamed.toml", contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("canonical path"));
    }

    #[test]
    fn rejects_an_unsupported_current_section_schema() {
        let contents = b"[storage]\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&current_manifest_with(RUNTIME_CONFIG_PATH, contents)).unwrap();
        manifest["sections"][0]["schemaVersion"] = json!(2);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("unsupported schema"));
    }

    #[test]
    fn rejects_a_current_section_without_its_dependency() {
        let contents = br#"{"schemaVersion":1,"thumbnails":[]}"#;
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&current_manifest_with(RUNTIME_CONFIG_PATH, contents)).unwrap();
        manifest["sections"][0]["section"] = json!("BACKUP_SECTION_EVENT_THUMBNAILS");
        manifest["sections"][0]["path"] = json!("events/thumbnails.json");
        manifest["sections"][0]["sha256"] = json!(sha256(contents));
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[("events/thumbnails.json", contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("requires event_metadata"));
    }

    #[test]
    fn rejects_a_current_section_that_does_not_declare_its_dependency() {
        let cameras = br#"{"schemaVersion":1,"cameras":[]}"#;
        let groups = br#"{"schemaVersion":1,"groups":[]}"#;
        let manifest = current_manifest(vec![
            current_section(
                crate::api::backup_proto::BackupSection::CameraDatabase,
                "config/cameras.json",
                cameras,
                Vec::new(),
            ),
            current_section(
                crate::api::backup_proto::BackupSection::Groups,
                "config/groups.json",
                groups,
                Vec::new(),
            ),
        ]);
        let bundle = archive_with_manifest(
            &manifest,
            &[
                ("config/cameras.json", cameras.as_slice()),
                ("config/groups.json", groups.as_slice()),
            ],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("must declare camera_database"));
    }

    #[test]
    fn rejects_a_json_section_without_its_schema_marker() {
        let contents = br#"{"cameras":[]}"#;
        let manifest = current_manifest(vec![current_section(
            crate::api::backup_proto::BackupSection::CameraDatabase,
            "config/cameras.json",
            contents,
            Vec::new(),
        )]);
        let bundle = archive_with_manifest(&manifest, &[("config/cameras.json", contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("schemaVersion"));
    }

    #[test]
    fn rejects_malformed_current_section_content() {
        let contents = b"[storage\n";
        let manifest = current_manifest_with(RUNTIME_CONFIG_PATH, contents);
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("invalid runtime_config section"));
    }

    #[test]
    fn rejects_a_current_manifest_without_a_snapshot_revision() {
        let contents = b"[storage]\nlong_term_max_gb = 1\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&current_manifest_with(RUNTIME_CONFIG_PATH, contents)).unwrap();
        manifest["snapshotRevision"] = json!("");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("snapshot revision"));
    }

    #[test]
    fn rejects_a_current_manifest_with_a_resolved_secret_requirement() {
        let contents = b"[storage]\nlong_term_max_gb = 1\n";
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&current_manifest_with(RUNTIME_CONFIG_PATH, contents)).unwrap();
        manifest["requiredSecretReferences"] = json!(["resolved-secret"]);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(&manifest, &[(RUNTIME_CONFIG_PATH, contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("secret reference"));
    }

    #[test]
    fn rejects_a_current_database_section_with_an_invalid_sqlite_header() {
        let contents = b"not a SQLite database";
        let manifest = current_manifest(vec![current_section(
            crate::api::backup_proto::BackupSection::RecordingCatalog,
            "catalog/recordings.db",
            contents,
            Vec::new(),
        )]);
        let bundle = archive_with_manifest(&manifest, &[("catalog/recordings.db", contents)]);

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(error.to_string().contains("invalid SQLite header"));
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
    fn rejects_archives_above_the_compressed_size_limit() {
        let bundle = bundle_with(RUNTIME_CONFIG_PATH, b"[storage]\n", None);
        let maximum_archive_bytes = u64::try_from(bundle.len() - 1).unwrap();
        let limits = InspectionLimits {
            maximum_archive_bytes,
            ..DEFAULT_INSPECTION_LIMITS
        };

        let error = inspect_bundle_with_limits(Cursor::new(bundle), limits).unwrap_err();

        assert!(error.to_string().contains("compressed size limit"));
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
        manifest["format_version"] = json!(3);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let bundle = archive_with_manifest(
            &manifest,
            &[(RUNTIME_CONFIG_PATH, b"[storage]\n".as_slice())],
        );

        let error = inspect_bundle(Cursor::new(bundle)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported KeepPeek backup format 3")
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

    fn current_manifest_with(path: &str, contents: &[u8]) -> Vec<u8> {
        current_manifest(vec![current_section(
            crate::api::backup_proto::BackupSection::RuntimeConfig,
            path,
            contents,
            Vec::new(),
        )])
    }

    fn current_manifest(
        sections: Vec<crate::api::backup_proto::BackupSectionDescriptor>,
    ) -> Vec<u8> {
        serde_json::to_vec(&crate::api::backup_proto::BackupManifest {
            format_version: 2,
            created_at_unix_ms: 1_788_000_000_000,
            keeppeek_version: "0.1.0".to_owned(),
            source: Some(crate::api::backup_proto::BackupSource {
                operating_system: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
            }),
            feature_capabilities: vec!["keeppeek.backup.v1".to_owned()],
            secret_policy: crate::api::backup_proto::BackupSecretPolicy::ReferencesOnly as i32,
            sections,
            omitted_data: vec!["recording_media".to_owned()],
            required_secret_references: vec!["{secret:CAMERA_PASSWORD}".to_owned()],
            source_paths: vec![crate::api::backup_proto::BackupPath {
                kind: crate::api::backup_proto::BackupPathKind::ConfigDirectory as i32,
                path: "/var/lib/keeppeek".to_owned(),
            }],
            snapshot_revision: "snapshot-1".to_owned(),
        })
        .unwrap()
    }

    fn current_section(
        section: crate::api::backup_proto::BackupSection,
        path: &str,
        contents: &[u8],
        dependencies: Vec<crate::api::backup_proto::BackupSection>,
    ) -> crate::api::backup_proto::BackupSectionDescriptor {
        crate::api::backup_proto::BackupSectionDescriptor {
            section: section as i32,
            path: path.to_owned(),
            schema_version: 1,
            bytes: u64::try_from(contents.len()).unwrap(),
            sha256: sha256(contents),
            revision: "revision-1".to_owned(),
            dependencies: dependencies
                .into_iter()
                .map(|dependency| dependency as i32)
                .collect(),
        }
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
