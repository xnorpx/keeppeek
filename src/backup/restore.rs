use super::{BackupManifest, BackupPathKind, BackupSection};
use crate::{
    api::backup_proto,
    config,
    storage::{catalog::rewrite_recording_paths, safety::filesystem_capacity},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};
use zip::ZipArchive;

const RESTORE_PLAN_TTL_MS: u64 = 10 * 60 * 1_000;
const ROLLBACK_WINDOW_MS: u64 = 30 * 60 * 1_000;
const RESTORE_JOURNAL_VERSION: u32 = 1;
const RESTORE_JOURNAL_FILE: &str = "restore-journal.json";

/// Inputs for a non-mutating restore dry run.
pub struct RestorePlanOptions<'a> {
    pub bundle_path: &'a Path,
    pub target_config_path: &'a Path,
    pub request: &'a backup_proto::CreateRestorePlanRequest,
    pub now_unix_ms: u64,
}

/// Inputs that activate an immutable plan by preparing a restart journal.
pub struct StageRestoreOptions<'a> {
    pub bundle_path: &'a Path,
    pub target_config_path: &'a Path,
    pub plan: &'a backup_proto::RestorePlan,
    pub now_unix_ms: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RestoreJournalState {
    Preparing,
    Ready,
    Applying,
    AwaitingHealth,
    Complete,
    RollbackRequested,
    RolledBack,
}

#[derive(Deserialize, Serialize)]
struct RestoreJournalTarget {
    target: PathBuf,
    staged: PathBuf,
    rollback: PathBuf,
    original_sha256: Option<String>,
    expected_sha256: String,
    database: bool,
    target_existed: bool,
    #[serde(default)]
    before_image_ready: bool,
    prepared: bool,
    applied: bool,
}

#[derive(Deserialize, Serialize)]
struct RestoreJournal {
    version: u32,
    restore_id: String,
    plan_id: String,
    backup_id: String,
    archive_sha256: String,
    target_revision: String,
    created_at_unix_ms: u64,
    rollback_expires_at_unix_ms: u64,
    state: RestoreJournalState,
    #[serde(default)]
    health_checks: Vec<backup_proto::RestoreHealthCheck>,
    targets: Vec<RestoreJournalTarget>,
}

enum PreparedContent {
    Bytes(Vec<u8>),
    ArchiveSection(BackupSection),
}

struct TargetPreparation {
    content: PreparedContent,
}

/// Returns the revision of file-backed state rooted beside `config.toml`.
///
/// # Errors
///
/// Returns an error when the configuration or a present sidecar cannot be read.
pub fn target_revision(config_path: &Path) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    hash_revision_file(&mut hasher, config_path)?;
    let access = crate::access::backup_catalog_document(config_path)?;
    hash_revision_value(&mut hasher, "access.toml", access.as_deref());
    hash_revision_file(
        &mut hasher,
        &config_path.with_file_name("peek-layouts.json"),
    )?;
    hash_revision_file(
        &mut hasher,
        &config_path.with_file_name("configuration-templates.json"),
    )?;
    Ok(super::encode_lower_hex(hasher.finalize()))
}

fn hash_revision_file(hasher: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    hasher.update(path.file_name().unwrap_or_default().as_encoded_bytes());
    hasher.update([0]);
    match std::fs::File::open(path) {
        Ok(mut file) => hash_reader(hasher, &mut file)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update(b"missing"),
        Err(error) => return Err(error.into()),
    }
    hasher.update([0]);
    Ok(())
}

fn hash_revision_value(hasher: &mut Sha256, name: &str, value: Option<&[u8]>) {
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(value.unwrap_or(b"missing"));
    hasher.update([0]);
}

/// Validates a bundle and produces an immutable restore plan without changing live state.
///
/// # Errors
///
/// Returns an error for malformed requests or unreadable bundles. Compatibility and environment
/// failures are returned as structured plan issues so an Administrator can inspect them.
pub fn plan_restore(options: RestorePlanOptions<'_>) -> anyhow::Result<backup_proto::RestorePlan> {
    validate_request(options.request, options.now_unix_ms)?;
    let archive_sha256 = hash_file(
        options.bundle_path,
        super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes,
    )?;
    let manifest = super::inspect_bundle(std::fs::File::open(options.bundle_path)?)?;
    let selected = selected_sections(&manifest, &options.request.sections)?;
    let current_revision = target_revision(options.target_config_path)?;
    let mut issues = Vec::new();
    if current_revision != options.request.expected_target_revision {
        issues.push(issue(
            "target_revision_conflict",
            "The target configuration changed after the restore editor was opened.",
            None,
            "expectedTargetRevision",
        ));
    }
    validate_selected_dependencies(&manifest, &selected, &mut issues);
    let mappings = validate_mappings(&manifest, options.request, &mut issues)?;
    let path_mappings = options
        .request
        .path_mappings
        .iter()
        .map(|mapping| {
            let kind = BackupPathKind::from_proto(mapping.kind)?;
            Ok(backup_proto::RestorePathMapping {
                kind: mapping.kind,
                source_path: mapping.source_path.clone(),
                target_path: mappings.get(&kind).map_or_else(
                    || mapping.target_path.clone(),
                    |target| target.to_string_lossy().into_owned(),
                ),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    validate_required_secrets(
        options.target_config_path,
        manifest.required_secret_references(),
        &mut issues,
    );
    validate_merged_configuration(
        options.bundle_path,
        options.target_config_path,
        &manifest,
        &selected,
        &path_mappings,
        &mut issues,
    )?;
    validate_camera_references(
        options.bundle_path,
        options.target_config_path,
        &manifest,
        &selected,
        &mut issues,
    )?;
    let capacity_checks = capacity_checks(&manifest, &selected, &mappings, &mut issues);
    validate_event_thumbnails(
        options.bundle_path,
        &manifest,
        &selected,
        &mappings,
        &mut issues,
    )?;
    let migrations = migrations(&manifest, &selected);
    let restart_impact = restart_impact(&selected);
    let can_activate = !issues
        .iter()
        .any(|entry| entry.severity == backup_proto::RestoreIssueSeverity::Blocking as i32);
    Ok(backup_proto::RestorePlan {
        plan_id: uuid::Uuid::new_v4().to_string(),
        backup_id: options.request.backup_id.clone(),
        archive_sha256,
        created_at_unix_ms: options.now_unix_ms,
        expires_at_unix_ms: options.now_unix_ms.saturating_add(RESTORE_PLAN_TTL_MS),
        target_revision: current_revision,
        selected_sections: selected
            .iter()
            .map(|section| section.to_proto() as i32)
            .collect(),
        path_mappings,
        migrations,
        issues,
        capacity_checks,
        required_secret_references: manifest.required_secret_references().to_vec(),
        restart_impact: Some(restart_impact),
        can_activate,
    })
}

/// Stages every selected target and persists a restart-ready restore journal.
///
/// # Errors
///
/// Returns an error without changing live state when the plan is blocked, stale, expired, changed,
/// already superseded by another restore, or cannot be fully staged.
pub fn stage_restore(
    options: StageRestoreOptions<'_>,
) -> anyhow::Result<backup_proto::RestoreRecord> {
    validate_stage_request(&options)?;
    let journal_path = restore_journal_path(options.target_config_path);
    if journal_path.exists() {
        anyhow::bail!("another restore journal is already active");
    }
    let manifest = super::inspect_bundle(std::fs::File::open(options.bundle_path)?)?;
    let selected = options
        .plan
        .selected_sections
        .iter()
        .map(|section| BackupSection::from_proto(*section))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let restore_id = uuid::Uuid::new_v4().to_string();
    let (targets, preparations) = prepare_targets(
        options.bundle_path,
        options.target_config_path,
        options.plan,
        &manifest,
        &selected,
        &restore_id,
    )?;
    let mut journal = RestoreJournal {
        version: RESTORE_JOURNAL_VERSION,
        restore_id,
        plan_id: options.plan.plan_id.clone(),
        backup_id: options.plan.backup_id.clone(),
        archive_sha256: options.plan.archive_sha256.clone(),
        target_revision: options.plan.target_revision.clone(),
        created_at_unix_ms: options.now_unix_ms,
        rollback_expires_at_unix_ms: options.now_unix_ms.saturating_add(ROLLBACK_WINDOW_MS),
        state: RestoreJournalState::Preparing,
        health_checks: Vec::new(),
        targets,
    };
    persist_journal(&journal_path, &journal)?;
    if let Err(error) = write_preparations(
        options.bundle_path,
        options.target_config_path,
        options.plan,
        &manifest,
        &journal_path,
        &mut journal,
        preparations,
    ) {
        cleanup_prepared_targets(&journal);
        let _ = std::fs::remove_file(journal_path);
        return Err(error);
    }
    journal.state = RestoreJournalState::Ready;
    persist_journal(&journal_path, &journal)?;
    Ok(restore_record(
        &journal,
        backup_proto::RestoreState::AwaitingRestart,
    ))
}

/// Applies or recovers the pending restore journal before application state opens.
///
/// # Errors
///
/// Returns an error after restoring all available before-images when activation fails.
pub fn recover_pending_restore(
    config_path: &Path,
    now_unix_ms: u64,
) -> anyhow::Result<Option<backup_proto::RestoreRecord>> {
    let path = restore_journal_path(config_path);
    if !path.exists() {
        return Ok(None);
    }
    let mut journal = load_journal(&path)?;
    match journal.state {
        RestoreJournalState::Preparing => {
            cleanup_prepared_targets(&journal);
            std::fs::remove_file(path)?;
            Ok(None)
        }
        RestoreJournalState::Ready => {
            apply_journal(&path, &mut journal)?;
            Ok(Some(restore_record_at(
                &journal,
                backup_proto::RestoreState::Verifying,
                now_unix_ms,
            )))
        }
        RestoreJournalState::Applying | RestoreJournalState::AwaitingHealth => {
            reconcile_applied_targets(&mut journal);
            rollback_journal(&path, &mut journal)?;
            Ok(Some(restore_record_at(
                &journal,
                backup_proto::RestoreState::RolledBack,
                now_unix_ms,
            )))
        }
        RestoreJournalState::RollbackRequested => {
            reconcile_applied_targets(&mut journal);
            rollback_journal(&path, &mut journal)?;
            Ok(Some(restore_record_at(
                &journal,
                backup_proto::RestoreState::RolledBack,
                now_unix_ms,
            )))
        }
        RestoreJournalState::Complete if now_unix_ms > journal.rollback_expires_at_unix_ms => {
            cleanup_rollback_targets(&journal);
            std::fs::remove_file(path)?;
            Ok(None)
        }
        RestoreJournalState::RolledBack => {
            cleanup_prepared_targets(&journal);
            cleanup_rollback_targets(&journal);
            std::fs::remove_file(path)?;
            Ok(None)
        }
        RestoreJournalState::Complete => Ok(None),
    }
}

/// Marks an applied restore healthy after the application and camera workers start.
///
/// # Errors
///
/// Returns an error when an activated target changed, its checksum is invalid, or restored
/// configuration cannot load with the target's external secrets.
pub fn mark_restore_healthy(
    config_path: &Path,
    now_unix_ms: u64,
) -> anyhow::Result<Option<backup_proto::RestoreRecord>> {
    let path = restore_journal_path(config_path);
    if !path.exists() {
        return Ok(None);
    }
    let mut journal = load_journal(&path)?;
    if journal.state != RestoreJournalState::AwaitingHealth {
        return Ok(None);
    }
    for target in &journal.targets {
        let metadata = std::fs::symlink_metadata(&target.target)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!("restored target is not a regular file");
        }
    }
    config::load_config(config_path)?;
    journal.target_revision = target_revision(config_path)?;
    journal.state = RestoreJournalState::Complete;
    journal.health_checks = vec![
        backup_proto::RestoreHealthCheck {
            name: "target_files".to_owned(),
            passed: true,
            detail: "Every activated target is a regular file.".to_owned(),
        },
        backup_proto::RestoreHealthCheck {
            name: "configuration_load".to_owned(),
            passed: true,
            detail: "The restored configuration loaded with target secrets.".to_owned(),
        },
    ];
    persist_journal(&path, &journal)?;
    Ok(Some(restore_record_at(
        &journal,
        backup_proto::RestoreState::Complete,
        now_unix_ms,
    )))
}

/// Requests rollback of a completed restore during its retained rollback window.
///
/// # Errors
///
/// Returns an error for a different restore, an incomplete restore, or an expired rollback point.
pub fn request_restore_rollback(
    config_path: &Path,
    restore_id: &str,
    now_unix_ms: u64,
) -> anyhow::Result<backup_proto::RestoreRecord> {
    let path = restore_journal_path(config_path);
    let mut journal = load_journal(&path)?;
    if journal.restore_id != restore_id {
        anyhow::bail!("restore ID does not match the active rollback point");
    }
    if journal.state != RestoreJournalState::Complete {
        anyhow::bail!("restore is not complete");
    }
    if now_unix_ms > journal.rollback_expires_at_unix_ms {
        anyhow::bail!("restore rollback window expired");
    }
    journal.state = RestoreJournalState::RollbackRequested;
    persist_journal(&path, &journal)?;
    Ok(restore_record_at(
        &journal,
        backup_proto::RestoreState::AwaitingRestart,
        now_unix_ms,
    ))
}

/// Returns the current restore or retained rollback point.
pub fn current_restore(
    config_path: &Path,
    restore_id: &str,
    now_unix_ms: u64,
) -> anyhow::Result<backup_proto::RestoreRecord> {
    let journal = load_journal(&restore_journal_path(config_path))?;
    if journal.restore_id != restore_id {
        anyhow::bail!("restore was not found");
    }
    Ok(restore_record_at(
        &journal,
        restore_state(journal.state),
        now_unix_ms,
    ))
}

/// Returns the server-owned active or retained restore record.
pub fn active_restore(
    config_path: &Path,
    now_unix_ms: u64,
) -> anyhow::Result<Option<backup_proto::RestoreRecord>> {
    let path = restore_journal_path(config_path);
    if !path.exists() {
        return Ok(None);
    }
    let journal = load_journal(&path)?;
    Ok(Some(restore_record_at(
        &journal,
        restore_state(journal.state),
        now_unix_ms,
    )))
}

const fn restore_state(state: RestoreJournalState) -> backup_proto::RestoreState {
    match state {
        RestoreJournalState::Preparing => backup_proto::RestoreState::Staging,
        RestoreJournalState::Ready | RestoreJournalState::RollbackRequested => {
            backup_proto::RestoreState::AwaitingRestart
        }
        RestoreJournalState::Applying | RestoreJournalState::AwaitingHealth => {
            backup_proto::RestoreState::Verifying
        }
        RestoreJournalState::Complete => backup_proto::RestoreState::Complete,
        RestoreJournalState::RolledBack => backup_proto::RestoreState::RolledBack,
    }
}

fn validate_stage_request(options: &StageRestoreOptions<'_>) -> anyhow::Result<()> {
    let plan = options.plan;
    if !plan.can_activate {
        anyhow::bail!("restore plan has blocking issues");
    }
    if options.now_unix_ms == 0 || options.now_unix_ms > plan.expires_at_unix_ms {
        anyhow::bail!("restore plan expired");
    }
    if plan.plan_id.is_empty() || plan.backup_id.is_empty() || plan.archive_sha256.len() != 64 {
        anyhow::bail!("restore plan identity is invalid");
    }
    if hash_file(
        options.bundle_path,
        super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes,
    )? != plan.archive_sha256
    {
        anyhow::bail!("backup changed after the restore dry run");
    }
    if target_revision(options.target_config_path)? != plan.target_revision {
        anyhow::bail!("target revision changed after the restore dry run");
    }
    Ok(())
}

fn prepare_targets(
    bundle_path: &Path,
    target_config_path: &Path,
    plan: &backup_proto::RestorePlan,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    restore_id: &str,
) -> anyhow::Result<(Vec<RestoreJournalTarget>, Vec<TargetPreparation>)> {
    let mut archive = ZipArchive::new(std::fs::File::open(bundle_path)?)?;
    let mut targets = Vec::new();
    let mut preparations = Vec::new();
    if selected
        .iter()
        .any(|section| is_configuration_section(*section))
    {
        let config_directory = mapping_target(plan, BackupPathKind::ConfigDirectory)?;
        let expected_config = config_directory.join(
            target_config_path
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("config.toml")),
        );
        let canonical_config = canonical_target_path(target_config_path)?;
        if expected_config != canonical_config {
            anyhow::bail!("config directory mapping does not match the target configuration");
        }
        let bytes =
            restored_configuration(&mut archive, manifest, selected, target_config_path, plan)?;
        push_bytes_target(
            &mut targets,
            &mut preparations,
            &canonical_config,
            bytes,
            restore_id,
        )?;
    }
    for section in selected {
        let (target, content) = match section {
            BackupSection::Access => (
                mapping_target(plan, BackupPathKind::ConfigDirectory)?.join("access.toml"),
                PreparedContent::Bytes(section_bytes(&mut archive, manifest, *section)?),
            ),
            BackupSection::Layouts => (
                mapping_target(plan, BackupPathKind::ConfigDirectory)?.join("peek-layouts.json"),
                PreparedContent::Bytes(unwrapped_json_section(&mut archive, manifest, *section)?),
            ),
            BackupSection::ConfigurationTemplates => (
                mapping_target(plan, BackupPathKind::ConfigDirectory)?
                    .join("configuration-templates.json"),
                PreparedContent::Bytes(unwrapped_json_section(&mut archive, manifest, *section)?),
            ),
            BackupSection::RecordingCatalog => (
                mapping_target(plan, BackupPathKind::RecordingCatalog)?,
                PreparedContent::ArchiveSection(*section),
            ),
            BackupSection::Notifications => (
                mapping_target(plan, BackupPathKind::NotificationDatabase)?,
                PreparedContent::ArchiveSection(*section),
            ),
            BackupSection::RuntimeConfig
            | BackupSection::CameraDatabase
            | BackupSection::EventMetadata
            | BackupSection::EventThumbnails
            | BackupSection::Integrations => continue,
            _ => anyhow::bail!(
                "restore staging does not support {} section",
                section.as_str()
            ),
        };
        push_target(
            &mut targets,
            &mut preparations,
            target,
            content,
            manifest,
            *section,
            restore_id,
        )?;
    }
    let unique_targets = targets
        .iter()
        .map(|target| target.target.as_path())
        .collect::<HashSet<_>>();
    if unique_targets.len() != targets.len() {
        anyhow::bail!("restore plan maps multiple sections to one target");
    }
    Ok((targets, preparations))
}

const fn is_configuration_section(section: BackupSection) -> bool {
    matches!(
        section,
        BackupSection::RuntimeConfig | BackupSection::CameraDatabase | BackupSection::Integrations
    )
}

fn mapping_target(
    plan: &backup_proto::RestorePlan,
    kind: BackupPathKind,
) -> anyhow::Result<PathBuf> {
    let declared = plan
        .path_mappings
        .iter()
        .find(|mapping| mapping.kind == kind.to_proto() as i32)
        .map(|mapping| PathBuf::from(&mapping.target_path))
        .ok_or_else(|| anyhow::anyhow!("restore plan is missing a required path mapping"))?;
    let resolved = canonical_target_path(&declared)?;
    if resolved != declared {
        anyhow::bail!("restore target path changed after the dry run");
    }
    Ok(resolved)
}

fn restored_configuration<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    target_config_path: &Path,
    plan: &backup_proto::RestorePlan,
) -> anyhow::Result<Vec<u8>> {
    let mut target = config::load_configuration_table(target_config_path)?;
    if selected.contains(&BackupSection::RuntimeConfig) {
        let source = section_bytes(archive, manifest, BackupSection::RuntimeConfig)?;
        let mut source: toml::Table = toml::from_str(std::str::from_utf8(&source)?)?;
        let target_storage_paths = take_storage_paths(&mut target);
        take_storage_paths(&mut source);
        target.retain(|key, value| !runtime_owned(key, value));
        target.extend(source);
        apply_target_storage_paths(&mut target, target_storage_paths, manifest, plan)?;
    }
    if selected.contains(&BackupSection::CameraDatabase) {
        let source = section_table(archive, manifest, BackupSection::CameraDatabase)?;
        target.retain(|key, value| {
            key != "camera_defaults" && !config::is_camera_namespace(key, value)
        });
        target.extend(source);
    }
    if selected.contains(&BackupSection::Integrations) {
        let source = section_table(archive, manifest, BackupSection::Integrations)?;
        target.remove("event_forwarder");
        target.extend(source);
    }
    target.remove("storage_migration");
    config::validate_configuration_table(target_config_path, &target)?;
    Ok(toml::to_string_pretty(&target)?.into_bytes())
}

const STORAGE_PATH_FIELDS: [&str; 4] = [
    "medium_term_path",
    "long_term_path",
    "recording_catalog_path",
    "event_thumbnail_path",
];

fn take_storage_paths(root: &mut toml::Table) -> HashMap<String, toml::Value> {
    let Some(storage) = root.get_mut("storage").and_then(toml::Value::as_table_mut) else {
        return HashMap::new();
    };
    STORAGE_PATH_FIELDS
        .iter()
        .filter_map(|field| {
            storage
                .remove(*field)
                .map(|value| ((*field).to_owned(), value))
        })
        .collect()
}

fn apply_target_storage_paths(
    root: &mut toml::Table,
    mut target_paths: HashMap<String, toml::Value>,
    manifest: &BackupManifest,
    plan: &backup_proto::RestorePlan,
) -> anyhow::Result<()> {
    for (kind, field) in [
        (BackupPathKind::LongTermMedia, "long_term_path"),
        (BackupPathKind::RecordingCatalog, "recording_catalog_path"),
        (BackupPathKind::EventThumbnails, "event_thumbnail_path"),
    ] {
        if manifest
            .source_paths()
            .iter()
            .any(|path| path.kind() == kind)
        {
            target_paths.insert(
                field.to_owned(),
                toml::Value::String(mapping_target(plan, kind)?.to_string_lossy().into_owned()),
            );
        }
    }
    if target_paths.is_empty() {
        return Ok(());
    }
    let storage = root
        .entry("storage")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("restored storage configuration is not a table"))?;
    storage.extend(target_paths);
    Ok(())
}

fn runtime_owned(key: &str, value: &toml::Value) -> bool {
    key != "camera_defaults"
        && key != "event_forwarder"
        && key != "storage_migration"
        && !config::is_camera_namespace(key, value)
}

fn section_table<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    section: BackupSection,
) -> anyhow::Result<toml::Table> {
    let bytes = section_bytes(archive, manifest, section)?;
    let document: super::create::ConfigurationSectionDocument = serde_json::from_slice(&bytes)?;
    serde_json::from_value(document.values).map_err(Into::into)
}

fn unwrapped_json_section<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    section: BackupSection,
) -> anyhow::Result<Vec<u8>> {
    let bytes = section_bytes(archive, manifest, section)?;
    let document: super::create::ConfigurationSectionDocument = serde_json::from_slice(&bytes)?;
    serde_json::to_vec_pretty(&document.values).map_err(Into::into)
}

fn section_bytes<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    section: BackupSection,
) -> anyhow::Result<Vec<u8>> {
    let descriptor = manifest
        .sections()
        .iter()
        .find(|candidate| candidate.kind() == section)
        .ok_or_else(|| anyhow::anyhow!("backup does not contain {} section", section.as_str()))?;
    super::read_member(archive, descriptor.path(), descriptor.bytes())
}

fn push_bytes_target(
    targets: &mut Vec<RestoreJournalTarget>,
    preparations: &mut Vec<TargetPreparation>,
    target: &Path,
    bytes: Vec<u8>,
    restore_id: &str,
) -> anyhow::Result<()> {
    let expected_sha256 = super::encode_lower_hex(Sha256::digest(&bytes));
    targets.push(journal_target(target, expected_sha256, restore_id, false)?);
    preparations.push(TargetPreparation {
        content: PreparedContent::Bytes(bytes),
    });
    Ok(())
}

fn push_target(
    targets: &mut Vec<RestoreJournalTarget>,
    preparations: &mut Vec<TargetPreparation>,
    target: PathBuf,
    content: PreparedContent,
    manifest: &BackupManifest,
    section: BackupSection,
    restore_id: &str,
) -> anyhow::Result<()> {
    let expected_sha256 = match &content {
        PreparedContent::Bytes(bytes) => super::encode_lower_hex(Sha256::digest(bytes)),
        PreparedContent::ArchiveSection(_) => manifest
            .sections()
            .iter()
            .find(|candidate| candidate.kind() == section)
            .map(|descriptor| descriptor.sha256().to_owned())
            .ok_or_else(|| anyhow::anyhow!("backup section descriptor is missing"))?,
    };
    let database = matches!(
        section,
        BackupSection::RecordingCatalog | BackupSection::Notifications
    );
    targets.push(journal_target(
        &target,
        expected_sha256,
        restore_id,
        database,
    )?);
    preparations.push(TargetPreparation { content });
    Ok(())
}

fn journal_target(
    target: &Path,
    expected_sha256: String,
    restore_id: &str,
    database: bool,
) -> anyhow::Result<RestoreJournalTarget> {
    if !target.is_absolute() {
        anyhow::bail!("restore targets must be absolute");
    }
    let target = canonical_target_path(target)?;
    let target_existed = match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => true,
        Ok(_) => anyhow::bail!("restore targets must be regular files"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !target_existed && database && database_sidecars_exist(&target) {
        anyhow::bail!("restore database sidecars exist without their primary target");
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore target has no parent"))?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("restore target file name is invalid"))?;
    let staged = parent.join(format!(".{file_name}.keeppeek-{restore_id}.staged"));
    let rollback = parent.join(format!(".{file_name}.keeppeek-{restore_id}.rollback"));
    let original_sha256 = (target_existed && !database)
        .then(|| target_family_sha256(&target, database))
        .transpose()?;
    Ok(RestoreJournalTarget {
        target,
        staged,
        rollback,
        original_sha256,
        expected_sha256,
        database,
        target_existed,
        before_image_ready: false,
        prepared: false,
        applied: false,
    })
}

fn canonical_target_path(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("restore targets must be absolute");
    }
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(metadata) => {
                if !missing.is_empty() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    anyhow::bail!("restore target ancestor is not a directory");
                }
                let mut resolved = existing.canonicalize()?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("restore target has no existing ancestor"))?;
                missing.push(component.to_owned());
                existing = existing
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("restore target has no existing ancestor"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_preparations(
    bundle_path: &Path,
    target_config_path: &Path,
    plan: &backup_proto::RestorePlan,
    manifest: &BackupManifest,
    journal_path: &Path,
    journal: &mut RestoreJournal,
    preparations: Vec<TargetPreparation>,
) -> anyhow::Result<()> {
    if preparations.len() != journal.targets.len() {
        anyhow::bail!("restore preparation count does not match its journal");
    }
    let mut archive = ZipArchive::new(std::fs::File::open(bundle_path)?)?;
    for (index, preparation) in preparations.into_iter().enumerate() {
        let target = &journal.targets[index];
        if target.staged.exists() || target.rollback.exists() {
            anyhow::bail!("restore staging path already exists");
        }
        if let Some(parent) = target.staged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match preparation.content {
            PreparedContent::Bytes(bytes) => write_staged_bytes(target, &bytes)?,
            PreparedContent::ArchiveSection(section) => {
                write_staged_section(&mut archive, manifest, section, target)?;
                if section == BackupSection::RecordingCatalog {
                    rewrite_recording_paths(
                        &target.staged,
                        &recording_path_routes(manifest, plan)?,
                    )?;
                }
                if section == BackupSection::Notifications {
                    crate::notifications::resolve_backup_snapshot_references(
                        &target.staged,
                        target_config_path,
                    )?;
                }
                if target.database {
                    super::database::compact_turso_database(
                        &target.staged,
                        &target.rollback,
                        super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
                    )?;
                }
                journal.targets[index].expected_sha256 =
                    target_family_sha256(&target.staged, target.database)?;
            }
        }
        journal.targets[index].prepared = true;
        persist_journal(journal_path, journal)?;
    }
    Ok(())
}

fn recording_path_routes(
    manifest: &BackupManifest,
    plan: &backup_proto::RestorePlan,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let Some(source) = manifest
        .source_paths()
        .iter()
        .find(|path| path.kind() == BackupPathKind::LongTermMedia)
    else {
        return Ok(Vec::new());
    };
    let target = mapping_target(plan, BackupPathKind::LongTermMedia)?;
    Ok(vec![(PathBuf::from(source.path()), target)])
}

fn write_staged_bytes(target: &RestoreJournalTarget, bytes: &[u8]) -> anyhow::Result<()> {
    config::write_private_file(&target.staged, bytes)?;
    std::fs::File::options()
        .write(true)
        .open(&target.staged)?
        .sync_all()?;
    let actual = hash_file(
        &target.staged,
        super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
    )?;
    if actual != target.expected_sha256 {
        anyhow::bail!("staged restore target checksum does not match");
    }
    Ok(())
}

fn write_staged_section<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    section: BackupSection,
    target: &RestoreJournalTarget,
) -> anyhow::Result<()> {
    let descriptor = manifest
        .sections()
        .iter()
        .find(|candidate| candidate.kind() == section)
        .ok_or_else(|| anyhow::anyhow!("backup section descriptor is missing"))?;
    let mut source = archive.by_name(descriptor.path())?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut destination = options.open(&target.staged)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read)?)
            .ok_or_else(|| anyhow::anyhow!("staged restore size overflow"))?;
        if total > descriptor.bytes() {
            anyhow::bail!("staged restore section exceeds its declared size");
        }
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    destination.sync_all()?;
    if total != descriptor.bytes()
        || !descriptor
            .sha256()
            .eq_ignore_ascii_case(&super::encode_lower_hex(hasher.finalize()))
    {
        anyhow::bail!("staged restore section checksum does not match");
    }
    Ok(())
}

fn restore_journal_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".backups")
        .join(RESTORE_JOURNAL_FILE)
}

fn persist_journal(path: &Path, journal: &RestoreJournal) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    config::write_private_file_atomically(path, &serde_json::to_vec_pretty(journal)?)?;
    Ok(())
}

fn load_journal(path: &Path) -> anyhow::Result<RestoreJournal> {
    const MAXIMUM_JOURNAL_BYTES: u64 = 1024 * 1024;
    if std::fs::metadata(path)?.len() > MAXIMUM_JOURNAL_BYTES {
        anyhow::bail!("restore journal exceeds its size limit");
    }
    let journal: RestoreJournal = serde_json::from_slice(&std::fs::read(path)?)?;
    if journal.version != RESTORE_JOURNAL_VERSION || journal.targets.is_empty() {
        anyhow::bail!("restore journal is invalid");
    }
    Ok(journal)
}

fn apply_journal(path: &Path, journal: &mut RestoreJournal) -> anyhow::Result<()> {
    journal.state = RestoreJournalState::Applying;
    persist_journal(path, journal)?;
    for index in 0..journal.targets.len() {
        if !journal.targets[index].prepared || journal.targets[index].applied {
            rollback_journal(path, journal)?;
            anyhow::bail!("restore journal target state is invalid");
        }
        let target = &journal.targets[index];
        let staged_hash = if target.database {
            target_family_sha256(&target.staged, true)
        } else {
            hash_file(
                &target.staged,
                super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
            )
        };
        let staged_hash_matches = staged_hash
            .as_ref()
            .is_ok_and(|actual| actual == &target.expected_sha256);
        if !staged_hash_matches {
            let error = staged_hash
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("staged restore target checksum changed"));
            reconcile_applied_targets(journal);
            rollback_journal(path, journal)?;
            return Err(error);
        }
        if let Err(error) = verify_original_target(target) {
            reconcile_applied_targets(journal);
            rollback_journal(path, journal)?;
            return Err(error);
        }
        if journal.targets[index].database
            && journal.targets[index].target_existed
            && !journal.targets[index].before_image_ready
        {
            let snapshot = super::database::snapshot_turso_database_path(
                &journal.targets[index].target,
                &journal.targets[index].rollback,
                super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
            );
            if let Err(error) = snapshot {
                super::database::remove_database_family(&journal.targets[index].rollback);
                reconcile_applied_targets(journal);
                rollback_journal(path, journal)?;
                return Err(error);
            }
            journal.targets[index].before_image_ready = true;
            persist_journal(path, journal)?;
        }
        let target = &journal.targets[index];
        if let Err(error) = apply_target(target) {
            reconcile_applied_targets(journal);
            rollback_journal(path, journal)?;
            return Err(error);
        }
        journal.targets[index].applied = true;
        persist_journal(path, journal)?;
    }
    journal.state = RestoreJournalState::AwaitingHealth;
    persist_journal(path, journal)
}

fn apply_target(target: &RestoreJournalTarget) -> anyhow::Result<()> {
    if target.database {
        if target.target_existed {
            if !target.before_image_ready || !target.rollback.is_file() {
                anyhow::bail!("database restore before-image is unavailable");
            }
            remove_target_checked(&target.target, true)?;
        } else if target.target.exists() || database_sidecars_exist(&target.target) {
            anyhow::bail!("restore target appeared after staging");
        }
        std::fs::rename(&target.staged, &target.target)?;
        return Ok(());
    }
    if target.target_existed {
        move_target(&target.target, &target.rollback, target.database)?;
    } else if target.target.exists() {
        anyhow::bail!("restore target appeared after staging");
    }
    match move_target(&target.staged, &target.target, target.database) {
        Ok(()) => Ok(()),
        Err(error) => {
            if target.target_existed {
                let _ = move_target(&target.rollback, &target.target, target.database);
            }
            Err(error)
        }
    }
}

fn verify_original_target(target: &RestoreJournalTarget) -> anyhow::Result<()> {
    if target.database {
        if target.target_existed {
            let metadata = std::fs::symlink_metadata(&target.target)
                .map_err(|_| anyhow::anyhow!("restore target changed after staging"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                anyhow::bail!("restore target changed after staging");
            }
        } else if target.target.exists() || database_sidecars_exist(&target.target) {
            anyhow::bail!("restore target changed after staging");
        }
        return Ok(());
    }
    match &target.original_sha256 {
        Some(expected) => {
            let actual = target_family_sha256(&target.target, target.database)
                .map_err(|_| anyhow::anyhow!("restore target changed after staging"))?;
            if &actual != expected {
                anyhow::bail!("restore target changed after staging");
            }
        }
        None if target.target.exists()
            || (target.database && database_sidecars_exist(&target.target)) =>
        {
            anyhow::bail!("restore target changed after staging");
        }
        None => {}
    }
    Ok(())
}

fn target_family_sha256(path: &Path, database: bool) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let suffixes = if database {
        ["", "-wal", "-shm"].as_slice()
    } else {
        [""].as_slice()
    };
    for suffix in suffixes {
        hasher.update(suffix.as_bytes());
        hasher.update([0]);
        let member = if suffix.is_empty() {
            path.to_owned()
        } else {
            suffixed_path(path, suffix)
        };
        match std::fs::symlink_metadata(&member) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                total_bytes = total_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| anyhow::anyhow!("restore target size overflow"))?;
                if total_bytes > super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes {
                    anyhow::bail!("restore target exceeds the supported size limit");
                }
                hash_reader(&mut hasher, &mut std::fs::File::open(member)?)?;
            }
            Ok(_) => anyhow::bail!("restore target family contains a non-file member"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update(b"missing"),
            Err(error) => return Err(error.into()),
        }
        hasher.update([0]);
    }
    Ok(super::encode_lower_hex(hasher.finalize()))
}

fn database_sidecars_exist(path: &Path) -> bool {
    ["-wal", "-shm"]
        .iter()
        .any(|suffix| suffixed_path(path, suffix).exists())
}

fn reconcile_applied_targets(journal: &mut RestoreJournal) {
    for target in &mut journal.targets {
        if target.applied {
            continue;
        }
        if target.database {
            if target.target_existed {
                if target.before_image_ready {
                    target.applied = true;
                } else {
                    super::database::remove_database_family(&target.rollback);
                }
            } else {
                target.applied = target.target.is_file();
                if !target.applied {
                    super::database::remove_database_sidecars(&target.target);
                }
            }
            continue;
        }
        let rollback_exists = target.rollback.exists();
        let replacement_exists = target.target.exists() && !target.staged.exists();
        if (target.target_existed && rollback_exists)
            || (!target.target_existed && replacement_exists)
        {
            target.applied = true;
        }
    }
}

fn rollback_journal(path: &Path, journal: &mut RestoreJournal) -> anyhow::Result<()> {
    for index in (0..journal.targets.len()).rev() {
        if !journal.targets[index].applied {
            continue;
        }
        let target = &journal.targets[index];
        remove_target_checked(&target.target, target.database)?;
        if target.target_existed {
            move_target(&target.rollback, &target.target, target.database)?;
        }
        journal.targets[index].applied = false;
        persist_journal(path, journal)?;
    }
    cleanup_prepared_targets(journal);
    journal.state = RestoreJournalState::RolledBack;
    persist_journal(path, journal)
}

fn move_target(source: &Path, destination: &Path, database: bool) -> anyhow::Result<()> {
    std::fs::rename(source, destination)?;
    if database {
        for suffix in ["-wal", "-shm"] {
            let source = suffixed_path(source, suffix);
            if source.exists() {
                std::fs::rename(source, suffixed_path(destination, suffix))?;
            }
        }
    }
    Ok(())
}

fn remove_target(path: &Path, database: bool) {
    let _ = std::fs::remove_file(path);
    if database {
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(suffixed_path(path, suffix));
        }
    }
}

fn remove_target_checked(path: &Path, database: bool) -> anyhow::Result<()> {
    remove_file_checked(path)?;
    if database {
        for suffix in ["-wal", "-shm"] {
            remove_file_checked(&suffixed_path(path, suffix))?;
        }
    }
    Ok(())
}

fn remove_file_checked(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn cleanup_prepared_targets(journal: &RestoreJournal) {
    for target in &journal.targets {
        super::database::remove_database_family(&target.staged);
    }
}

fn cleanup_rollback_targets(journal: &RestoreJournal) {
    for target in &journal.targets {
        remove_target(&target.rollback, target.database);
    }
}

fn restore_record(
    journal: &RestoreJournal,
    state: backup_proto::RestoreState,
) -> backup_proto::RestoreRecord {
    restore_record_at(journal, state, journal.created_at_unix_ms)
}

fn restore_record_at(
    journal: &RestoreJournal,
    state: backup_proto::RestoreState,
    updated_at_unix_ms: u64,
) -> backup_proto::RestoreRecord {
    backup_proto::RestoreRecord {
        restore_id: journal.restore_id.clone(),
        plan_id: journal.plan_id.clone(),
        backup_id: journal.backup_id.clone(),
        state: state as i32,
        created_at_unix_ms: journal.created_at_unix_ms,
        updated_at_unix_ms,
        rollback_expires_at_unix_ms: Some(journal.rollback_expires_at_unix_ms),
        target_revision: journal.target_revision.clone(),
        restart_required: true,
        health_checks: journal.health_checks.clone(),
        progress: Some(backup_proto::BackupProgress {
            completed_per_mille: 1_000,
            completed_bytes: 0,
            total_bytes: None,
            active_section: None,
        }),
        error: None,
    }
}

fn validate_request(
    request: &backup_proto::CreateRestorePlanRequest,
    now_unix_ms: u64,
) -> anyhow::Result<()> {
    for (name, value) in [
        ("client_request_id", request.client_request_id.as_str()),
        ("backup_id", request.backup_id.as_str()),
        (
            "expected_target_revision",
            request.expected_target_revision.as_str(),
        ),
    ] {
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            anyhow::bail!("{name} must contain 1 to 128 printable bytes");
        }
    }
    if now_unix_ms == 0 {
        anyhow::bail!("restore planning time must be nonzero");
    }
    Ok(())
}

fn selected_sections(
    manifest: &BackupManifest,
    requested: &[i32],
) -> anyhow::Result<Vec<BackupSection>> {
    let available = manifest
        .sections()
        .iter()
        .map(|section| section.kind())
        .collect::<HashSet<_>>();
    let mut selected = if requested.is_empty() {
        available.iter().copied().collect::<Vec<_>>()
    } else {
        requested
            .iter()
            .map(|section| BackupSection::from_proto(*section))
            .collect::<anyhow::Result<Vec<_>>>()?
    };
    selected.sort_unstable();
    let original_len = selected.len();
    selected.dedup();
    if selected.len() != original_len {
        anyhow::bail!("restore sections must not contain duplicates");
    }
    if let Some(section) = selected.iter().find(|section| !available.contains(section)) {
        anyhow::bail!("backup does not contain {} section", section.as_str());
    }
    Ok(selected)
}

fn validate_selected_dependencies(
    manifest: &BackupManifest,
    selected: &[BackupSection],
    issues: &mut Vec<backup_proto::RestoreIssue>,
) {
    for section in manifest
        .sections()
        .iter()
        .filter(|section| selected.contains(&section.kind()))
    {
        for dependency in section.dependencies() {
            if !selected.contains(dependency) {
                issues.push(issue(
                    "missing_section_dependency",
                    &format!(
                        "The {} section requires the {} section.",
                        section.kind().as_str(),
                        dependency.as_str()
                    ),
                    Some(section.kind()),
                    "sections",
                ));
            }
        }
    }
}

fn validate_mappings(
    manifest: &BackupManifest,
    request: &backup_proto::CreateRestorePlanRequest,
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> anyhow::Result<HashMap<BackupPathKind, PathBuf>> {
    let mut mappings = HashMap::with_capacity(request.path_mappings.len());
    for mapping in &request.path_mappings {
        let kind = BackupPathKind::from_proto(mapping.kind)?;
        if mappings.contains_key(&kind) {
            anyhow::bail!("restore path mapping kinds must be unique");
        }
        let source = manifest
            .source_paths()
            .iter()
            .find(|path| path.kind() == kind);
        if source.is_none_or(|path| path.path() != mapping.source_path) {
            issues.push(issue(
                "source_path_mismatch",
                "The path mapping does not match the backup source path.",
                None,
                "pathMappings.sourcePath",
            ));
        }
        let target_path = Path::new(&mapping.target_path);
        if !target_path.is_absolute() {
            issues.push(issue(
                "target_path_not_absolute",
                "The target path must be absolute on this platform.",
                None,
                "pathMappings.targetPath",
            ));
            mappings.insert(kind, target_path.to_owned());
        } else {
            match canonical_target_path(target_path) {
                Ok(resolved) => {
                    mappings.insert(kind, resolved);
                }
                Err(_) => {
                    mappings.insert(kind, target_path.to_owned());
                    issues.push(issue(
                        "target_path_unsafe",
                        "The target path cannot be resolved safely.",
                        None,
                        "pathMappings.targetPath",
                    ));
                }
            }
        }
    }
    for source in manifest.source_paths() {
        if !mappings.contains_key(&source.kind()) {
            issues.push(issue(
                "missing_path_mapping",
                &format!(
                    "The source path {} requires an explicit target.",
                    source.path()
                ),
                None,
                "pathMappings",
            ));
        }
    }
    Ok(mappings)
}

fn validate_required_secrets(
    config_path: &Path,
    references: &[String],
    issues: &mut Vec<backup_proto::RestoreIssue>,
) {
    for reference in references {
        if config::resolve_secret_references(config_path, reference).is_err() {
            issues.push(issue(
                "missing_secret",
                &format!("The target does not provide required reference {reference}."),
                None,
                "requiredSecretReferences",
            ));
        }
    }
}

fn validate_merged_configuration(
    bundle_path: &Path,
    target_config_path: &Path,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    path_mappings: &[backup_proto::RestorePathMapping],
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> anyhow::Result<()> {
    if !selected
        .iter()
        .any(|section| is_configuration_section(*section))
    {
        return Ok(());
    }
    let mut archive = ZipArchive::new(std::fs::File::open(bundle_path)?)?;
    let plan = backup_proto::RestorePlan {
        path_mappings: path_mappings.to_vec(),
        ..Default::default()
    };
    if restored_configuration(&mut archive, manifest, selected, target_config_path, &plan).is_err()
    {
        issues.push(issue(
            "configuration_invalid",
            "The selected configuration cannot load on this target.",
            None,
            "sections",
        ));
    }
    Ok(())
}

fn validate_camera_references(
    bundle_path: &Path,
    target_config_path: &Path,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> anyhow::Result<()> {
    if !selected.contains(&BackupSection::CameraDatabase)
        && !selected.contains(&BackupSection::Layouts)
    {
        return Ok(());
    }
    let mut archive = ZipArchive::new(std::fs::File::open(bundle_path)?)?;
    let source = section_table(&mut archive, manifest, BackupSection::CameraDatabase)?;
    let source_cameras = camera_tables(&source)?;
    if selected.contains(&BackupSection::CameraDatabase) {
        let target = load_optional_configuration_table(target_config_path)?;
        let target_cameras = camera_tables(&target)?;
        let collisions = source_cameras
            .iter()
            .filter(|(camera_id, source)| {
                target_cameras
                    .get(*camera_id)
                    .is_some_and(|candidate| camera_identity(source) != camera_identity(candidate))
            })
            .count();
        if collisions > 0 {
            issues.push(issue(
                "camera_id_conflict",
                &format!("{collisions} stable camera IDs identify different target cameras."),
                Some(BackupSection::CameraDatabase),
                "sections",
            ));
        }
    }
    if selected.contains(&BackupSection::Layouts) {
        let layout = unwrapped_json_section(&mut archive, manifest, BackupSection::Layouts)?;
        let missing = crate::server::backup_layout_camera_ids(&layout)?
            .into_iter()
            .filter(|camera_id| !source_cameras.contains_key(camera_id.as_str()))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let visible = missing
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            issues.push(restore_issue(
                backup_proto::RestoreIssueSeverity::Warning,
                "layout_cameras_missing",
                &format!(
                    "The layout registry retains {} missing camera IDs: {visible}. They remain visible placeholders.",
                    missing.len()
                ),
                Some(BackupSection::Layouts),
                "sections",
            ));
        }
    }
    Ok(())
}

fn camera_tables(root: &toml::Table) -> anyhow::Result<HashMap<&str, &toml::Value>> {
    let mut cameras = HashMap::new();
    for (namespace, value) in root {
        if !config::is_camera_namespace(namespace, value) {
            continue;
        }
        let table = value
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("camera namespace is not a table"))?;
        for (camera_id, camera) in table {
            if cameras.insert(camera_id.as_str(), camera).is_some() {
                anyhow::bail!("camera configuration contains duplicate stable camera IDs");
            }
        }
    }
    Ok(cameras)
}

fn load_optional_configuration_table(path: &Path) -> anyhow::Result<toml::Table> {
    match config::load_configuration_table(path) {
        Ok(table) => Ok(table),
        Err(_error) if !path.exists() => Ok(toml::Table::new()),
        Err(error) => Err(error),
    }
}

fn camera_identity(value: &toml::Value) -> Option<&str> {
    value
        .as_table()
        .and_then(|camera| camera.get("ip"))
        .and_then(toml::Value::as_str)
}

fn capacity_checks(
    manifest: &BackupManifest,
    selected: &[BackupSection],
    mappings: &HashMap<BackupPathKind, PathBuf>,
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> Vec<backup_proto::RestoreCapacityCheck> {
    let mut checks = Vec::with_capacity(mappings.len());
    for (kind, target) in mappings {
        let required_bytes = required_bytes(manifest, selected, *kind);
        let capacity = filesystem_capacity(target, 0);
        let available_bytes = capacity.as_ref().map_or(0, |value| value.available_bytes);
        let writable = writable_probe_target(*kind, target).is_ok();
        let sufficient = capacity.is_ok() && available_bytes >= required_bytes;
        if !writable || !sufficient {
            issues.push(issue(
                if writable {
                    "insufficient_capacity"
                } else {
                    "target_not_writable"
                },
                "The restore target failed its permission or capacity check.",
                None,
                "pathMappings.targetPath",
            ));
        }
        checks.push(backup_proto::RestoreCapacityCheck {
            kind: kind.to_proto() as i32,
            target_path: target.to_string_lossy().into_owned(),
            required_bytes,
            available_bytes,
            writable,
            sufficient,
        });
    }
    checks.sort_unstable_by_key(|check| check.kind);
    checks
}

fn validate_event_thumbnails(
    bundle_path: &Path,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    mappings: &HashMap<BackupPathKind, PathBuf>,
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> anyhow::Result<()> {
    if !selected.contains(&BackupSection::EventThumbnails) {
        return Ok(());
    }
    let Some(target) = mappings.get(&BackupPathKind::EventThumbnails) else {
        return Ok(());
    };
    let mut archive = ZipArchive::new(std::fs::File::open(bundle_path)?)?;
    let bytes = section_bytes(&mut archive, manifest, BackupSection::EventThumbnails)?;
    let document: super::create::ConfigurationSectionDocument = serde_json::from_slice(&bytes)?;
    let inventory: super::create::EventThumbnailInventory =
        serde_json::from_value(document.values)?;
    validate_event_thumbnail_target(&inventory, target, issues)
}

fn validate_event_thumbnail_target(
    inventory: &super::create::EventThumbnailInventory,
    target: &Path,
    issues: &mut Vec<backup_proto::RestoreIssue>,
) -> anyhow::Result<()> {
    const MAXIMUM_VERIFY_BYTES: u64 = 1024 * 1024 * 1024;
    let mut verified = 0_usize;
    let mut missing = 0_usize;
    let mut changed = 0_usize;
    let mut checked_bytes = 0_u64;
    for entry in &inventory.entries {
        let Some(next_bytes) = checked_bytes.checked_add(entry.bytes) else {
            break;
        };
        if next_bytes > MAXIMUM_VERIFY_BYTES {
            break;
        }
        checked_bytes = next_bytes;
        let path = target.join(&entry.file_name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() == entry.bytes
                    && hash_file(&path, entry.bytes)
                        .is_ok_and(|digest| digest.eq_ignore_ascii_case(&entry.sha256)) =>
            {
                verified += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing += 1,
            Ok(_) | Err(_) => changed += 1,
        }
    }
    let unverified = inventory
        .entries
        .len()
        .saturating_sub(verified + missing + changed);
    if missing > 0 || changed > 0 || unverified > 0 {
        issues.push(restore_issue(
            backup_proto::RestoreIssueSeverity::Warning,
            "event_thumbnails_unavailable",
            &format!(
                "The mapped thumbnail archive has {missing} missing, {changed} changed, and {unverified} unverified files; {verified} files match. Event metadata remains restorable."
            ),
            Some(BackupSection::EventThumbnails),
            "pathMappings.targetPath",
        ));
    } else {
        issues.push(restore_issue(
            backup_proto::RestoreIssueSeverity::Information,
            "event_thumbnails_verified",
            &format!("All {verified} mapped event thumbnails match the backup inventory."),
            Some(BackupSection::EventThumbnails),
            "pathMappings.targetPath",
        ));
    }
    Ok(())
}

fn required_bytes(
    manifest: &BackupManifest,
    selected: &[BackupSection],
    kind: BackupPathKind,
) -> u64 {
    manifest
        .sections()
        .iter()
        .filter(|section| selected.contains(&section.kind()))
        .filter(|section| path_kind(section.kind()) == kind)
        .fold(0_u64, |total, section| {
            total.saturating_add(section.bytes())
        })
}

const fn path_kind(section: BackupSection) -> BackupPathKind {
    match section {
        BackupSection::RecordingCatalog | BackupSection::EventMetadata => {
            BackupPathKind::RecordingCatalog
        }
        BackupSection::Notifications => BackupPathKind::NotificationDatabase,
        BackupSection::EventThumbnails => BackupPathKind::EventThumbnails,
        _ => BackupPathKind::ConfigDirectory,
    }
}

fn writable_probe_target(kind: BackupPathKind, target: &Path) -> anyhow::Result<()> {
    let directory = match kind {
        BackupPathKind::RecordingCatalog | BackupPathKind::NotificationDatabase => {
            target.parent().unwrap_or_else(|| Path::new("."))
        }
        _ => target,
    };
    let existing = nearest_existing(directory)?;
    let probe = existing.join(format!(".keeppeek-restore-probe-{}", uuid::Uuid::new_v4()));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|file| file.sync_all());
    let _ = std::fs::remove_file(probe);
    result.map_err(Into::into)
}

fn nearest_existing(mut path: &Path) -> anyhow::Result<&Path> {
    loop {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => return Ok(path),
            Ok(_) => {
                path = path
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("no target directory"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                path = path.parent().ok_or(error)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn migrations(
    manifest: &BackupManifest,
    selected: &[BackupSection],
) -> Vec<backup_proto::BackupMigration> {
    if manifest.format_version() != super::LEGACY_FORMAT_VERSION {
        return Vec::new();
    }
    manifest
        .sections()
        .iter()
        .filter(|section| selected.contains(&section.kind()))
        .map(|section| backup_proto::BackupMigration {
            section: section.kind().to_proto() as i32,
            source_schema_version: section.schema_version(),
            target_schema_version: 1,
            description: "Migrate legacy bundle metadata to format 2.".to_owned(),
        })
        .collect()
}

fn restart_impact(selected: &[BackupSection]) -> backup_proto::RestoreRestartImpact {
    let mut components = selected
        .iter()
        .map(|section| section.as_str().to_owned())
        .collect::<Vec<_>>();
    components.sort_unstable();
    backup_proto::RestoreRestartImpact {
        server_restart_required: !components.is_empty(),
        components,
        consequence: "Selected state activates during a controlled recorder restart.".to_owned(),
    }
}

fn issue(
    code: &str,
    message: &str,
    section: Option<BackupSection>,
    field: &str,
) -> backup_proto::RestoreIssue {
    restore_issue(
        backup_proto::RestoreIssueSeverity::Blocking,
        code,
        message,
        section,
        field,
    )
}

fn restore_issue(
    severity: backup_proto::RestoreIssueSeverity,
    code: &str,
    message: &str,
    section: Option<BackupSection>,
    field: &str,
) -> backup_proto::RestoreIssue {
    backup_proto::RestoreIssue {
        severity: severity as i32,
        code: code.to_owned(),
        message: message.to_owned(),
        section: section.map(|section| section.to_proto() as i32),
        field: field.to_owned(),
    }
}

fn hash_file(path: &Path, maximum_bytes: u64) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > maximum_bytes {
        anyhow::bail!("backup exceeds the compressed size limit");
    }
    let mut hasher = Sha256::new();
    hash_reader(&mut hasher, &mut std::fs::File::open(path)?)?;
    Ok(super::encode_lower_hex(hasher.finalize()))
}

fn hash_reader(hasher: &mut Sha256, reader: &mut impl Read) -> anyhow::Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup;
    use std::io::Cursor;

    #[cfg(unix)]
    #[test]
    fn staging_binds_a_symlinked_ancestor_to_its_canonical_target() {
        use std::os::unix::fs::symlink;

        let directory = test_directory("symlinked-target");
        let actual = directory.join("actual");
        let mapped = directory.join("mapped");
        std::fs::create_dir_all(&actual).unwrap();
        symlink(&actual, &mapped).unwrap();

        let target = journal_target(
            &mapped.join("recordings.db"),
            "00".repeat(32),
            "restore-1",
            true,
        )
        .unwrap();

        assert_eq!(
            target.target,
            actual.canonicalize().unwrap().join("recordings.db")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn target_revision_ignores_access_audit_but_tracks_credentials() {
        let directory = test_directory("access-revision");
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let access_key =
            crate::access::AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let manager = crate::access::AccessManager::open(&config_path, access_key).unwrap();
        let initial = target_revision(&config_path).unwrap();

        manager.record_audit(crate::access::NewAccessAuditEvent {
            timestamp_ms: 1_788_000_000_000,
            principal_id: Some("local-administrator"),
            role: Some(crate::access::AccessRole::Administrator),
            action: "backup_list",
            target_id: None,
            result: "success",
            client_classification: crate::access::ClientClassificationReason::DirectLocal,
        });
        manager.flush_audit(true).unwrap();
        assert_eq!(target_revision(&config_path).unwrap(), initial);

        manager
            .create_credential(
                "Recovery operator",
                None,
                crate::access::AccessRole::Administrator,
                None,
                1_788_000_000_001,
            )
            .unwrap();
        assert_ne!(target_revision(&config_path).unwrap(), initial);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dry_run_binds_explicit_paths_revision_capacity_and_restart_impact() {
        let directory = test_directory("complete");
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_config = source.join("config.toml");
        std::fs::write(
            &source_config,
            "access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n[storage]\nlong_term_max_gb = 10\n",
        )
        .unwrap();
        let target_config = target.join("config.toml");
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        std::fs::write(
            target.join("secrets.toml"),
            "KEEPPEEK_ACCESS_KEY = \"00000000-0000-4000-8000-000000000001\"\n",
        )
        .unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[
                    backup::BackupSection::RuntimeConfig,
                    backup::BackupSection::CameraDatabase,
                ],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let source_directory = manifest.source_paths[0].path.clone();
        let expected_revision = target_revision(&target_config).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "request-1".to_owned(),
            backup_id: "backup-1".to_owned(),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: source_directory,
                target_path: target.to_string_lossy().into_owned(),
            }],
            expected_target_revision: expected_revision.clone(),
        };

        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(plan.can_activate);
        assert_eq!(plan.target_revision, expected_revision);
        assert_eq!(plan.archive_sha256.len(), 64);
        assert_eq!(plan.capacity_checks.len(), 1);
        assert!(plan.capacity_checks[0].writable);
        assert!(plan.capacity_checks[0].sufficient);
        assert!(plan.restart_impact.unwrap().server_restart_required);
        assert!(plan.issues.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dry_run_reports_missing_secrets_without_hiding_inspection() {
        let directory = test_directory("missing-secret");
        let source_config = directory.join("source.toml");
        let target_config = directory.join("target.toml");
        std::fs::write(
            &source_config,
            "access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n[storage]\nlong_term_max_gb = 10\n",
        )
        .unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[backup::BackupSection::RuntimeConfig],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "request-2".to_owned(),
            backup_id: "backup-2".to_owned(),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: manifest.source_paths[0].path.clone(),
                target_path: directory.to_string_lossy().into_owned(),
            }],
            expected_target_revision: target_revision(&target_config).unwrap(),
        };

        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(!plan.can_activate);
        assert!(plan.issues.iter().any(|issue| {
            issue.code == "missing_secret"
                && issue.severity == backup_proto::RestoreIssueSeverity::Blocking as i32
        }));
        assert_eq!(
            plan.required_secret_references,
            ["{secret:KEEPPEEK_ACCESS_KEY}"]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dry_run_blocks_an_invalid_merged_configuration() {
        let directory = test_directory("invalid-configuration");
        let source_config = directory.join("source.toml");
        let target_config = directory.join("target.toml");
        std::fs::write(
            &source_config,
            "[storage]\nwarning_free_gb = 1\ncritical_free_gb = 2\n",
        )
        .unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[backup::BackupSection::RuntimeConfig],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "invalid-configuration".to_owned(),
            backup_id: "backup-invalid-configuration".to_owned(),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: manifest.source_paths[0].path.clone(),
                target_path: directory.to_string_lossy().into_owned(),
            }],
            expected_target_revision: target_revision(&target_config).unwrap(),
        };

        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(!plan.can_activate);
        assert!(plan.issues.iter().any(|issue| {
            issue.code == "configuration_invalid"
                && issue.severity == backup_proto::RestoreIssueSeverity::Blocking as i32
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dry_run_blocks_a_stable_camera_id_collision() {
        let directory = test_directory("camera-id-conflict");
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        let source_config = source.join("config.toml");
        let target_config = target.join("config.toml");
        std::fs::write(&source_config, "[cameras.front]\nip = \"192.0.2.10\"\n").unwrap();
        std::fs::write(&target_config, "[cameras.front]\nip = \"192.0.2.99\"\n").unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[backup::BackupSection::CameraDatabase],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &restore_request(&manifest, &target, &target_config, "camera-conflict"),
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(!plan.can_activate);
        assert!(
            plan.issues
                .iter()
                .any(|issue| issue.code == "camera_id_conflict")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dry_run_reports_layout_camera_placeholders() {
        let directory = test_directory("layout-placeholders");
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        let source_config = source.join("config.toml");
        let target_config = target.join("config.toml");
        std::fs::write(&source_config, "[cameras.front]\nip = \"192.0.2.10\"\n").unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        std::fs::write(
            source.join("peek-layouts.json"),
            r#"{"schema_version":1,"revision":1,"shared_layouts":[{"id":"default","name":"All cameras","scope":"shared","owner_id":"server","audience":{"everyone":true,"credential_ids":[]},"activity_focus":true,"tiles":[{"camera_id":"retired-camera","column":1,"row":1,"column_span":6,"row_span":6,"pinned":false}]}],"users":{}}"#,
        )
        .unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[
                    backup::BackupSection::CameraDatabase,
                    backup::BackupSection::Layouts,
                ],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &restore_request(&manifest, &target, &target_config, "layout-placeholders"),
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(plan.can_activate, "{:#?}", plan.issues);
        assert!(plan.issues.iter().any(|issue| {
            issue.code == "layout_cameras_missing"
                && issue.severity == backup_proto::RestoreIssueSeverity::Warning as i32
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn restore_request(
        manifest: &backup_proto::BackupManifest,
        target: &Path,
        target_config: &Path,
        client_request_id: &str,
    ) -> backup_proto::CreateRestorePlanRequest {
        backup_proto::CreateRestorePlanRequest {
            client_request_id: client_request_id.to_owned(),
            backup_id: format!("backup-{client_request_id}"),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: manifest.source_paths[0].path.clone(),
                target_path: target.to_string_lossy().into_owned(),
            }],
            expected_target_revision: target_revision(target_config).unwrap(),
        }
    }

    #[test]
    fn thumbnail_inventory_reports_missing_files_without_blocking_restore() {
        let directory = test_directory("thumbnail-inventory");
        std::fs::create_dir_all(&directory).unwrap();
        let inventory = super::super::create::EventThumbnailInventory {
            policy: "inventory_only".to_owned(),
            catalog_revision: "catalog-1".to_owned(),
            entries: vec![crate::storage::catalog::CatalogEventThumbnailBackupEntry {
                event_id: "event-1".to_owned(),
                file_name: "event-1.jpg".to_owned(),
                bytes: 128,
                sha256: "00".repeat(32),
            }],
        };
        let mut issues = Vec::new();

        validate_event_thumbnail_target(&inventory, &directory, &mut issues).unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "event_thumbnails_unavailable");
        assert_eq!(
            issues[0].severity,
            backup_proto::RestoreIssueSeverity::Warning as i32
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn staging_rejects_a_stale_plan_without_creating_a_journal() {
        let (directory, bundle_path, target_config, plan) = activatable_fixture("stale");
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 30\n").unwrap();
        let changed = std::fs::read(&target_config).unwrap();

        let error = stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap_err();

        assert!(error.to_string().contains("target revision changed"));
        assert_eq!(std::fs::read(&target_config).unwrap(), changed);
        assert!(!directory.join(".backups/restore-journal.json").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn staging_rejects_an_archive_changed_after_dry_run() {
        let (directory, bundle_path, target_config, plan) = activatable_fixture("changed-archive");
        std::fs::write(&bundle_path, b"changed after planning").unwrap();
        let original = std::fs::read(&target_config).unwrap();

        let error = stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap_err();

        assert!(error.to_string().contains("backup changed"));
        assert_eq!(std::fs::read(&target_config).unwrap(), original);
        assert!(!directory.join(".backups/restore-journal.json").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_recovery_preserves_a_target_changed_after_staging() {
        let (directory, bundle_path, target_config, plan) =
            activatable_fixture("changed-after-stage");
        stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap();
        let changed = b"[storage]\nlong_term_max_gb = 99\n";
        std::fs::write(&target_config, changed).unwrap();

        let error =
            recover_pending_restore(&target_config, plan.created_at_unix_ms + 2).unwrap_err();

        assert!(error.to_string().contains("changed after staging"));
        assert_eq!(std::fs::read(&target_config).unwrap(), changed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn staged_restore_applies_after_restart_and_rolls_back_exactly() {
        let (directory, bundle_path, target_config, plan) = activatable_fixture("lifecycle");
        let original = std::fs::read(&target_config).unwrap();
        let staged = stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap();
        assert_eq!(std::fs::read(&target_config).unwrap(), original);
        assert_eq!(
            staged.state,
            backup_proto::RestoreState::AwaitingRestart as i32
        );

        let applied = recover_pending_restore(&target_config, plan.created_at_unix_ms + 2)
            .unwrap()
            .unwrap();
        assert_eq!(applied.state, backup_proto::RestoreState::Verifying as i32);
        assert!(
            std::fs::read_to_string(&target_config)
                .unwrap()
                .contains("long_term_max_gb = 10")
        );
        let healthy = mark_restore_healthy(&target_config, plan.created_at_unix_ms + 3)
            .unwrap()
            .unwrap();
        assert_eq!(healthy.state, backup_proto::RestoreState::Complete as i32);
        assert_eq!(healthy.health_checks.len(), 2);
        assert_eq!(
            active_restore(&target_config, plan.created_at_unix_ms + 3)
                .unwrap()
                .unwrap()
                .health_checks,
            healthy.health_checks
        );

        let rollback = request_restore_rollback(
            &target_config,
            &staged.restore_id,
            plan.created_at_unix_ms + 4,
        )
        .unwrap();
        assert_eq!(
            rollback.state,
            backup_proto::RestoreState::AwaitingRestart as i32
        );
        let rolled_back = recover_pending_restore(&target_config, plan.created_at_unix_ms + 5)
            .unwrap()
            .unwrap();
        assert_eq!(
            rolled_back.state,
            backup_proto::RestoreState::RolledBack as i32
        );
        assert_eq!(std::fs::read(&target_config).unwrap(), original);
        assert!(
            recover_pending_restore(&target_config, plan.created_at_unix_ms + 6)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rollback_is_available_through_its_deadline_and_expires_after_it() {
        let (directory, bundle_path, target_config, plan) =
            activatable_fixture("rollback-deadline");
        let staged = stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap();
        recover_pending_restore(&target_config, plan.created_at_unix_ms + 2).unwrap();
        let healthy = mark_restore_healthy(&target_config, plan.created_at_unix_ms + 3)
            .unwrap()
            .unwrap();
        let deadline = healthy.rollback_expires_at_unix_ms.unwrap();

        request_restore_rollback(&target_config, &staged.restore_id, deadline).unwrap();
        recover_pending_restore(&target_config, deadline).unwrap();

        let (expired_directory, expired_bundle, expired_config, expired_plan) =
            activatable_fixture("rollback-expired");
        let expired_staged = stage_restore(StageRestoreOptions {
            bundle_path: &expired_bundle,
            target_config_path: &expired_config,
            plan: &expired_plan,
            now_unix_ms: expired_plan.created_at_unix_ms + 1,
        })
        .unwrap();
        recover_pending_restore(&expired_config, expired_plan.created_at_unix_ms + 2).unwrap();
        let expired_healthy =
            mark_restore_healthy(&expired_config, expired_plan.created_at_unix_ms + 3)
                .unwrap()
                .unwrap();
        let expired_deadline = expired_healthy.rollback_expires_at_unix_ms.unwrap();

        let error = request_restore_rollback(
            &expired_config,
            &expired_staged.restore_id,
            expired_deadline + 1,
        )
        .unwrap_err();

        assert!(error.to_string().contains("rollback window expired"));
        std::fs::remove_dir_all(directory).unwrap();
        std::fs::remove_dir_all(expired_directory).unwrap();
    }

    #[test]
    fn startup_recovery_rolls_back_a_partially_applied_restore() {
        let (directory, bundle_path, target_config, plan) = activatable_fixture("partial");
        let target_access = target_config.with_file_name("access.toml");
        let original_config = std::fs::read(&target_config).unwrap();
        let original_access = std::fs::read(&target_access).unwrap();
        stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap();
        let journal_path = restore_journal_path(&target_config);
        let mut journal = load_journal(&journal_path).unwrap();
        journal.state = RestoreJournalState::Applying;
        let first = &journal.targets[0];
        apply_target(first).unwrap();
        persist_journal(&journal_path, &journal).unwrap();

        let recovered = recover_pending_restore(&target_config, plan.created_at_unix_ms + 2)
            .unwrap()
            .unwrap();

        assert_eq!(
            recovered.state,
            backup_proto::RestoreState::RolledBack as i32
        );
        assert_eq!(std::fs::read(&target_config).unwrap(), original_config);
        assert_eq!(std::fs::read(&target_access).unwrap(), original_access);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn database_rollback_preserves_the_latest_pre_activation_state() {
        let directory = test_directory("database-before-image");
        let target = directory.join("target.db");
        let staged = directory.join("staged.db");
        write_test_database(&target, "staged-at-one");
        write_test_database(&staged, "replacement");
        super::super::database::compact_turso_database(
            &staged,
            &directory.join("compact.db"),
            16 * 1024 * 1024,
        )
        .unwrap();
        let expected_sha256 = target_family_sha256(&staged, true).unwrap();
        let mut target_state = journal_target(&target, expected_sha256, "restore-1", true).unwrap();
        target_state.staged = staged;
        target_state.prepared = true;
        write_test_database(&target, "latest-before-restart");
        let journal_path = directory.join("restore-journal.json");
        let mut journal = RestoreJournal {
            version: RESTORE_JOURNAL_VERSION,
            restore_id: "restore-1".to_owned(),
            plan_id: "plan-1".to_owned(),
            backup_id: "backup-1".to_owned(),
            archive_sha256: "00".repeat(32),
            target_revision: "target-1".to_owned(),
            created_at_unix_ms: 1_788_000_000_000,
            rollback_expires_at_unix_ms: 1_788_001_800_000,
            state: RestoreJournalState::Ready,
            health_checks: Vec::new(),
            targets: vec![target_state],
        };
        persist_journal(&journal_path, &journal).unwrap();

        apply_journal(&journal_path, &mut journal).unwrap();
        assert_eq!(read_test_database(&target), "replacement");
        rollback_journal(&journal_path, &mut journal).unwrap();
        assert_eq!(read_test_database(&target), "latest-before-restart");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_health_verification_restores_the_previous_configuration() {
        let (directory, bundle_path, target_config, plan) = activatable_fixture("health-failure");
        let original = std::fs::read(&target_config).unwrap();
        stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: plan.created_at_unix_ms + 1,
        })
        .unwrap();
        recover_pending_restore(&target_config, plan.created_at_unix_ms + 2).unwrap();
        std::fs::write(&target_config, b"not valid TOML = [").unwrap();

        assert!(mark_restore_healthy(&target_config, plan.created_at_unix_ms + 3).is_err());
        let rolled_back = recover_pending_restore(&target_config, plan.created_at_unix_ms + 4)
            .unwrap()
            .unwrap();

        assert_eq!(
            rolled_back.state,
            backup_proto::RestoreState::RolledBack as i32
        );
        assert_eq!(std::fs::read(&target_config).unwrap(), original);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn interrupted_database_before_image_keeps_the_latest_target() {
        let directory = test_directory("database-before-image-interrupted");
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let target = directory.join("target.db");
        let staged = directory.join("staged.db");
        write_test_database(&target, "latest-before-restart");
        write_test_database(&staged, "replacement");
        super::super::database::compact_turso_database(
            &staged,
            &directory.join("compact.db"),
            16 * 1024 * 1024,
        )
        .unwrap();
        let mut target_state = journal_target(
            &target,
            target_family_sha256(&staged, true).unwrap(),
            "restore-1",
            true,
        )
        .unwrap();
        target_state.staged = staged;
        target_state.prepared = true;
        std::fs::write(&target_state.rollback, b"partial snapshot").unwrap();
        let journal_path = restore_journal_path(&config_path);
        persist_journal(
            &journal_path,
            &RestoreJournal {
                version: RESTORE_JOURNAL_VERSION,
                restore_id: "restore-1".to_owned(),
                plan_id: "plan-1".to_owned(),
                backup_id: "backup-1".to_owned(),
                archive_sha256: "00".repeat(32),
                target_revision: "target-1".to_owned(),
                created_at_unix_ms: 1_788_000_000_000,
                rollback_expires_at_unix_ms: 1_788_001_800_000,
                state: RestoreJournalState::Applying,
                health_checks: Vec::new(),
                targets: vec![target_state],
            },
        )
        .unwrap();

        let recovered = recover_pending_restore(&config_path, 1_788_000_000_001)
            .unwrap()
            .unwrap();

        assert_eq!(
            recovered.state,
            backup_proto::RestoreState::RolledBack as i32
        );
        assert_eq!(read_test_database(&target), "latest-before-restart");
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn write_test_database(path: &Path, value: &str) {
        super::super::database::remove_database_family(path);
        let database = pollster::block_on(
            turso::Builder::new_local(path.to_str().unwrap())
                .experimental_vacuum(true)
                .build(),
        )
        .unwrap();
        let connection = database.connect().unwrap();
        pollster::block_on(connection.execute_batch("CREATE TABLE state (value TEXT NOT NULL);"))
            .unwrap();
        pollster::block_on(connection.execute(
            "INSERT INTO state (value) VALUES (?1)",
            turso::params![value],
        ))
        .unwrap();
    }

    fn read_test_database(path: &Path) -> String {
        let database =
            pollster::block_on(turso::Builder::new_local(path.to_str().unwrap()).build()).unwrap();
        let connection = database.connect().unwrap();
        pollster::block_on(async {
            connection
                .query("SELECT value FROM state", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap()
        })
    }

    #[test]
    fn full_supported_round_trip_preserves_ids_references_and_mapped_paths() {
        let directory = test_directory("full-round-trip");
        let source = create_full_source(&directory);
        let target = create_full_target(&directory, &source.manifest);
        let bundle_path = directory.join("full-backup.zip");
        std::fs::write(&bundle_path, &source.bundle).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "full-plan".to_owned(),
            backup_id: "full-backup".to_owned(),
            sections: Vec::new(),
            path_mappings: restore_mappings(&source.manifest, &target),
            expected_target_revision: target_revision(&target.config).unwrap(),
        };
        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target.config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();
        assert!(plan.can_activate, "{:#?}", plan.issues);
        let staged = stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target.config,
            plan: &plan,
            now_unix_ms: 1_788_000_001_001,
        })
        .unwrap();
        recover_pending_restore(&target.config, 1_788_000_001_002).unwrap();
        mark_restore_healthy(&target.config, 1_788_000_001_003).unwrap();

        assert_full_target(&target, &source);

        request_restore_rollback(&target.config, &staged.restore_id, 1_788_000_001_004).unwrap();
        recover_pending_restore(&target.config, 1_788_000_001_005).unwrap();
        assert_eq!(
            std::fs::read(&target.config).unwrap(),
            target.original_config
        );
        assert!(!target.catalog.exists());
        assert!(!target.notifications.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "run explicitly to collect issue 128 performance evidence"]
    fn backup_restore_performance_benchmark() {
        const DEFAULT_SAMPLES: usize = 10;
        const CREATE_P95_BUDGET_MS: f64 = 2_000.0;
        const PLAN_P95_BUDGET_MS: f64 = 500.0;
        const STAGE_P95_BUDGET_MS: f64 = 2_000.0;
        let samples = std::env::var("KEEPPEEK_BACKUP_BENCH_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_SAMPLES);
        assert!((1..=10).contains(&samples));
        let directory = test_directory("performance");
        let source = create_full_source(&directory);
        let source_root = directory.join("source");
        let source_files = [
            "config.toml",
            "access.toml",
            "peek-layouts.json",
            "configuration-templates.json",
            "recordings.db",
            "notifications.db",
        ];
        let mut baseline_us = Vec::with_capacity(samples);
        for sample in 0..samples {
            let target = directory.join(format!("baseline-{sample}"));
            std::fs::create_dir(&target).unwrap();
            let started = std::time::Instant::now();
            for file_name in source_files {
                std::fs::copy(source_root.join(file_name), target.join(file_name)).unwrap();
            }
            baseline_us.push(started.elapsed().as_micros());
            std::fs::remove_dir_all(target).unwrap();
        }

        let catalog_path = source_root.join("recordings.db");
        let catalog = crate::storage::RecordingCatalog::open(&catalog_path).unwrap();
        let notifications_path = source_root.join("notifications.db");
        let notifications = crate::notifications::Runtime::open(&notifications_path).unwrap();
        let storage_paths = super::super::BackupStoragePaths::new(
            source_root.join("media"),
            source_root.join("thumbnails"),
        );
        let manager = super::super::BackupManager::open(
            source_root.join("config.toml"),
            Some(catalog.handle()),
            Some(notifications.handle()),
            Some(storage_paths),
        )
        .unwrap();
        let mut create_us = Vec::with_capacity(samples);
        for sample in 0..samples {
            let started = std::time::Instant::now();
            manager
                .create(
                    &backup_proto::CreateBackupRequest {
                        client_request_id: format!("benchmark-{sample}"),
                        sections: Vec::new(),
                        expected_archive_bytes: 0,
                    },
                    1_788_000_000_000 + u64::try_from(sample).unwrap(),
                )
                .unwrap();
            create_us.push(started.elapsed().as_micros());
        }
        drop(manager);
        notifications.shutdown();
        catalog.shutdown();

        let bundle_path = directory.join("benchmark.zip");
        std::fs::write(&bundle_path, &source.bundle).unwrap();
        let mut plan_us = Vec::with_capacity(samples);
        let mut stage_us = Vec::with_capacity(samples);
        for sample in 0..samples {
            let sample_root = directory.join(format!("restore-{sample}"));
            let target = create_full_target(&sample_root, &source.manifest);
            let request = backup_proto::CreateRestorePlanRequest {
                client_request_id: format!("plan-{sample}"),
                backup_id: "benchmark-backup".to_owned(),
                sections: Vec::new(),
                path_mappings: restore_mappings(&source.manifest, &target),
                expected_target_revision: target_revision(&target.config).unwrap(),
            };
            let started = std::time::Instant::now();
            let plan = plan_restore(RestorePlanOptions {
                bundle_path: &bundle_path,
                target_config_path: &target.config,
                request: &request,
                now_unix_ms: 1_788_000_001_000 + u64::try_from(sample).unwrap(),
            })
            .unwrap();
            plan_us.push(started.elapsed().as_micros());
            assert!(plan.can_activate, "{:#?}", plan.issues);
            let started = std::time::Instant::now();
            stage_restore(StageRestoreOptions {
                bundle_path: &bundle_path,
                target_config_path: &target.config,
                plan: &plan,
                now_unix_ms: plan.created_at_unix_ms + 1,
            })
            .unwrap();
            stage_us.push(started.elapsed().as_micros());
            std::fs::remove_dir_all(sample_root).unwrap();
        }
        baseline_us.sort_unstable();
        create_us.sort_unstable();
        plan_us.sort_unstable();
        stage_us.sort_unstable();
        let report = [
            ("baseline_copy", &baseline_us),
            ("validated_create", &create_us),
            ("dry_run", &plan_us),
            ("stage", &stage_us),
        ];
        println!("samples={samples}");
        println!("archive_bytes={}", source.bundle.len());
        for (name, values) in report {
            println!("{name}_p50_ms={:.3}", percentile_us(values, 50));
            println!("{name}_p95_ms={:.3}", percentile_us(values, 95));
        }
        assert!(percentile_us(&create_us, 95) <= CREATE_P95_BUDGET_MS);
        assert!(percentile_us(&plan_us, 95) <= PLAN_P95_BUDGET_MS);
        assert!(percentile_us(&stage_us, 95) <= STAGE_P95_BUDGET_MS);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn percentile_us(sorted: &[u128], percentile: usize) -> f64 {
        let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
        sorted[rank.saturating_sub(1)] as f64 / 1_000.0
    }

    #[test]
    fn legacy_format_one_runtime_config_migrates_atomically() {
        let directory = test_directory("legacy-format-one");
        let target = directory.join("target");
        std::fs::create_dir(&target).unwrap();
        let target_config = target.join("config.toml");
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let bundle_path = directory.join("legacy.zip");
        std::fs::write(&bundle_path, legacy_runtime_bundle()).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "legacy-plan".to_owned(),
            backup_id: "legacy-backup".to_owned(),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: "legacy://config-directory".to_owned(),
                target_path: target.to_string_lossy().into_owned(),
            }],
            expected_target_revision: target_revision(&target_config).unwrap(),
        };

        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();

        assert!(plan.can_activate, "{:#?}", plan.issues);
        assert_eq!(plan.migrations.len(), 1);
        stage_restore(StageRestoreOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            plan: &plan,
            now_unix_ms: 1_788_000_001_001,
        })
        .unwrap();
        recover_pending_restore(&target_config, 1_788_000_001_002).unwrap();
        assert!(
            std::fs::read_to_string(&target_config)
                .unwrap()
                .contains("long_term_max_gb = 10")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cross_platform_source_paths_converge_on_one_explicit_target() {
        let directory = test_directory("cross-platform-paths");
        let target = directory.join("target");
        std::fs::create_dir(&target).unwrap();
        let target_config = target.join("config.toml");
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        for (index, source_path) in [
            "/var/lib/keeppeek",
            "/Library/Application Support/KeepPeek",
            r"C:\ProgramData\KeepPeek",
            r"\\recording-host\KeepPeek",
        ]
        .into_iter()
        .enumerate()
        {
            let bundle_path = directory.join(format!("portable-{index}.zip"));
            std::fs::write(&bundle_path, portable_runtime_bundle(source_path)).unwrap();
            let request = backup_proto::CreateRestorePlanRequest {
                client_request_id: format!("portable-plan-{index}"),
                backup_id: format!("portable-backup-{index}"),
                sections: Vec::new(),
                path_mappings: vec![backup_proto::RestorePathMapping {
                    kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                    source_path: source_path.to_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                }],
                expected_target_revision: target_revision(&target_config).unwrap(),
            };
            let plan = plan_restore(RestorePlanOptions {
                bundle_path: &bundle_path,
                target_config_path: &target_config,
                request: &request,
                now_unix_ms: 1_788_000_001_000 + u64::try_from(index).unwrap(),
            })
            .unwrap();
            assert!(plan.can_activate, "{source_path}: {:#?}", plan.issues);
            assert_eq!(
                plan.path_mappings[0].target_path,
                target.canonicalize().unwrap().to_string_lossy()
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn legacy_runtime_bundle() -> Vec<u8> {
        use std::io::Write as _;
        let contents = b"[storage]\nlong_term_max_gb = 10\n";
        let manifest = serde_json::json!({
            "format_version": 1,
            "created_at_ms": 1_788_000_000_000_u64,
            "keeppeek_version": "0.0.1",
            "source": { "os": "linux", "arch": "x86_64" },
            "feature_capabilities": ["runtime_config"],
            "secret_policy": "references_only",
            "sections": [{
                "kind": "runtime_config",
                "path": "config/runtime.toml",
                "schema_version": 1,
                "bytes": contents.len(),
                "sha256": super::super::encode_lower_hex(Sha256::digest(contents))
            }]
        });
        let cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        archive
            .start_file(super::super::MANIFEST_PATH, options)
            .unwrap();
        archive
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        archive.start_file("config/runtime.toml", options).unwrap();
        archive.write_all(contents).unwrap();
        archive.finish().unwrap().into_inner()
    }

    fn portable_runtime_bundle(source_path: &str) -> Vec<u8> {
        use std::io::Write as _;
        let contents = b"[storage]\nlong_term_max_gb = 10\n";
        let digest = super::super::encode_lower_hex(Sha256::digest(contents));
        let manifest = backup_proto::BackupManifest {
            format_version: super::super::FORMAT_VERSION,
            created_at_unix_ms: 1_788_000_000_000,
            keeppeek_version: "0.1.0".to_owned(),
            source: Some(backup_proto::BackupSource {
                operating_system: "portable-fixture".to_owned(),
                architecture: "x86_64".to_owned(),
            }),
            feature_capabilities: vec!["keeppeek.backup.v1".to_owned()],
            secret_policy: backup_proto::BackupSecretPolicy::ReferencesOnly as i32,
            sections: vec![backup_proto::BackupSectionDescriptor {
                section: backup_proto::BackupSection::RuntimeConfig as i32,
                path: "config/runtime.toml".to_owned(),
                schema_version: 1,
                bytes: u64::try_from(contents.len()).unwrap(),
                sha256: digest.clone(),
                revision: digest,
                dependencies: Vec::new(),
            }],
            omitted_data: vec!["recording_media".to_owned()],
            required_secret_references: Vec::new(),
            source_paths: vec![backup_proto::BackupPath {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                path: source_path.to_owned(),
            }],
            snapshot_revision: "portable-snapshot".to_owned(),
        };
        let cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        archive
            .start_file(super::super::MANIFEST_PATH, options)
            .unwrap();
        archive
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        archive.start_file("config/runtime.toml", options).unwrap();
        archive.write_all(contents).unwrap();
        archive.finish().unwrap().into_inner()
    }

    struct FullSource {
        bundle: Vec<u8>,
        manifest: backup_proto::BackupManifest,
        credential_id: uuid::Uuid,
        notification_destination: String,
    }

    struct FullTarget {
        root: PathBuf,
        config: PathBuf,
        catalog: PathBuf,
        notifications: PathBuf,
        media: PathBuf,
        thumbnails: PathBuf,
        original_config: Vec<u8>,
    }

    fn create_full_source(directory: &Path) -> FullSource {
        let root = directory.join("source");
        let media = root.join("media");
        let thumbnails = root.join("thumbnails");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&thumbnails).unwrap();
        let config = root.join("config.toml");
        std::fs::write(&config, full_source_config(&root, &media, &thumbnails)).unwrap();
        std::fs::write(
            root.join("peek-layouts.json"),
            r#"{"schema_version":1,"revision":7,"shared_layouts":[{"id":"default","name":"All cameras","scope":"shared","owner_id":"server","audience":{"everyone":true,"credential_ids":[]},"activity_focus":true,"tiles":[]}],"users":{}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("configuration-templates.json"),
            r#"{"document_version":1,"templates":[]}"#,
        )
        .unwrap();
        let access = crate::access::AccessManager::open(
            &config,
            crate::access::AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        )
        .unwrap();
        let credential_id = access
            .create_credential(
                "Recovery viewer",
                None,
                crate::access::AccessRole::User,
                None,
                1_000,
            )
            .unwrap()
            .metadata
            .id;
        let catalog_path = root.join("recordings.db");
        let catalog = crate::storage::RecordingCatalog::open(&catalog_path).unwrap();
        seed_full_catalog(&catalog.handle(), &media, &thumbnails);
        let notification_path = root.join("notifications.db");
        let notifications = crate::notifications::Runtime::open(&notification_path).unwrap();
        let notification_destination = seed_notification_rule(&notifications.handle());
        let storage_paths = super::super::BackupStoragePaths::new(media, thumbnails);
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &config,
                sections: &[],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: Some(&catalog.handle()),
                notifications: Some(&notifications.handle()),
                storage_paths: Some(&storage_paths),
            },
        )
        .unwrap();
        notifications.shutdown();
        catalog.shutdown();
        FullSource {
            bundle: bundle.into_inner(),
            manifest,
            credential_id,
            notification_destination,
        }
    }

    fn create_full_target(directory: &Path, manifest: &backup_proto::BackupManifest) -> FullTarget {
        let root = directory.join("target");
        let media = root.join("media");
        let thumbnails = root.join("thumbnails");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&thumbnails).unwrap();
        std::fs::write(media.join("recording-1.mp4"), b"external recording bytes").unwrap();
        std::fs::write(thumbnails.join("event-1.jpg"), b"external thumbnail bytes").unwrap();
        let config = root.join("config.toml");
        let original_config = b"[storage]\nlong_term_max_gb = 20\n".to_vec();
        std::fs::write(&config, &original_config).unwrap();
        write_full_target_secrets(&root, manifest);
        FullTarget {
            catalog: root.join("recordings.db"),
            notifications: root.join("notifications.db"),
            root,
            config,
            media,
            thumbnails,
            original_config,
        }
    }

    fn full_source_config(root: &Path, media: &Path, thumbnails: &Path) -> String {
        format!(
            r#"[storage]
medium_term_path = {media:?}
long_term_path = {media:?}
recording_catalog_path = {catalog:?}
event_thumbnail_path = {thumbnails:?}
long_term_max_gb = 10

[cameras.front]
ip = "192.0.2.10"
username = "{{secret:CAMERA_USERNAME}}"
password = "{{secret:CAMERA_PASSWORD}}"

[event_forwarder.mqtt]
enabled = false
broker_url = "mqtt://broker.example:1883"
username = "{{secret:MQTT_USERNAME}}"
password = "{{secret:MQTT_PASSWORD}}"
"#,
            media = media.to_string_lossy(),
            catalog = root.join("recordings.db").to_string_lossy(),
            thumbnails = thumbnails.to_string_lossy(),
        )
    }

    fn seed_full_catalog(
        catalog: &crate::storage::RecordingCatalogHandle,
        media: &Path,
        thumbnails: &Path,
    ) {
        let recording = media.join("recording-1.mp4");
        std::fs::write(&recording, b"external recording bytes").unwrap();
        catalog
            .upsert_recording(crate::storage::CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front/main".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: recording.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 1,
                finalized: true,
            })
            .unwrap();
        catalog
            .insert_event(crate::storage::metadata::TimelineEvent {
                id: "event-1".to_owned(),
                revision: 1,
                camera_id: "front".to_owned(),
                stream: Some("main".to_owned()),
                source: crate::storage::metadata::EventSource::Camera,
                kind: "person".to_owned(),
                start_time_ms: 1_500,
                end_time_ms: Some(1_600),
                confidence: Some(0.95),
                bbox: None,
                bbox_attachment_id: None,
                zone: Some("porch".to_owned()),
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        std::fs::write(thumbnails.join("event-1.jpg"), b"external thumbnail bytes").unwrap();
        catalog
            .attach_event_thumbnail("event-1", "event-1.jpg", 24)
            .unwrap();
    }

    fn seed_notification_rule(handle: &crate::notifications::Handle) -> String {
        let destination = serde_json::json!({
            "application_token": "a23456789012345678901234567890",
            "user_key": "u23456789012345678901234567890",
            "priority": 0
        })
        .to_string();
        let mut rule =
            serde_json::from_value::<crate::notifications::model::Rule>(serde_json::json!({
                "id": "rule-1", "name": "Recovery rule", "enabled": true,
                "revision": 0, "owner_id": "owner-1", "triggers": ["test"], "filter": {},
                "schedule": { "timezone": "UTC", "active_windows": [], "quiet_hours": null },
                "critical_bypass": null,
                "enrichment": { "deadline_ms": 10000, "maximum_revisions": 2,
                    "maximum_attempts": 2, "maximum_attachment_bytes": 1048576,
                    "wake_after_deadline": false },
                "actions": [{ "enabled": true, "channel": "push", "destination": "",
                    "template": { "title": "Alert", "body": "Open KeepPeek" },
                    "attachment": "never", "allow_second_delivery": false }],
                "failure": { "maximum_attempts": 3, "maximum_retry_interval_ms": 60000,
                    "expiry_ms": 3600000 }
            }))
            .unwrap();
        rule.actions[0].destination.clone_from(&destination);
        let saved = handle.save_draft(rule, 0, 1_000).unwrap();
        handle
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        destination
    }

    fn write_full_target_secrets(root: &Path, manifest: &backup_proto::BackupManifest) {
        let mut secrets = toml::Table::new();
        for reference in &manifest.required_secret_references {
            let key = reference
                .strip_prefix("{secret:")
                .and_then(|value| value.strip_suffix('}'))
                .unwrap();
            let value = match key {
                "CAMERA_USERNAME" => "operator",
                "CAMERA_PASSWORD" => "camera-password",
                "MQTT_PASSWORD" => "mqtt-password",
                "MQTT_USERNAME" => "mqtt-user",
                _ if key.starts_with("BACKUP_NOTIFICATION_") => {
                    r#"{"application_token":"a23456789012345678901234567890","user_key":"u23456789012345678901234567890","priority":0}"#
                }
                _ => panic!("unexpected secret reference {reference}"),
            };
            secrets.insert(key.to_owned(), toml::Value::String(value.to_owned()));
        }
        std::fs::write(
            root.join("secrets.toml"),
            toml::to_string(&secrets).unwrap(),
        )
        .unwrap();
    }

    fn restore_mappings(
        manifest: &backup_proto::BackupManifest,
        target: &FullTarget,
    ) -> Vec<backup_proto::RestorePathMapping> {
        manifest
            .source_paths
            .iter()
            .map(|source| backup_proto::RestorePathMapping {
                kind: source.kind,
                source_path: source.path.clone(),
                target_path: match backup_proto::BackupPathKind::try_from(source.kind).unwrap() {
                    backup_proto::BackupPathKind::ConfigDirectory => &target.root,
                    backup_proto::BackupPathKind::RecordingCatalog => &target.catalog,
                    backup_proto::BackupPathKind::LongTermMedia => &target.media,
                    backup_proto::BackupPathKind::EventThumbnails => &target.thumbnails,
                    backup_proto::BackupPathKind::NotificationDatabase => &target.notifications,
                    backup_proto::BackupPathKind::Unspecified => unreachable!(),
                }
                .to_string_lossy()
                .into_owned(),
            })
            .collect()
    }

    fn assert_full_target(target: &FullTarget, source: &FullSource) {
        let config = std::fs::read_to_string(&target.config).unwrap();
        assert!(config.contains("[cameras.front]"));
        assert!(config.contains("{secret:CAMERA_PASSWORD}"));
        let config: toml::Table = toml::from_str(&config).unwrap();
        assert_eq!(
            config["storage"]["long_term_path"].as_str(),
            Some(target.media.to_string_lossy().as_ref())
        );
        let access = crate::access::AccessManager::open(
            &target.config,
            crate::access::AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        )
        .unwrap();
        assert!(
            access
                .list_credentials()
                .iter()
                .any(|credential| credential.id == source.credential_id)
        );
        let catalog = crate::storage::RecordingCatalog::open(&target.catalog).unwrap();
        let handle = catalog.handle();
        assert_eq!(handle.stats().unwrap().recording_files, 1);
        assert_eq!(handle.stats().unwrap().events, 1);
        assert_eq!(handle.event_by_id("event-1").unwrap().unwrap().revision, 2);
        drop(handle);
        catalog.shutdown();
        let notifications = crate::notifications::Runtime::open(&target.notifications).unwrap();
        let rules = notifications.handle().rules("owner-1").unwrap();
        assert_eq!(rules[0].id, "rule-1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &rules[0].active.as_ref().unwrap().actions[0].destination
            )
            .unwrap(),
            serde_json::from_str::<serde_json::Value>(&source.notification_destination).unwrap()
        );
        notifications.shutdown();
        assert!(
            std::fs::read_to_string(target.root.join("peek-layouts.json"))
                .unwrap()
                .contains("\"revision\": 7")
        );
    }

    fn activatable_fixture(
        name: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        backup_proto::RestorePlan,
    ) {
        let directory = test_directory(name);
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_config = source.join("config.toml");
        let target_config = target.join("config.toml");
        std::fs::write(&source_config, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        std::fs::write(
            source.join("access.toml"),
            "version = 1\ncredentials = []\naudit = []\n",
        )
        .unwrap();
        std::fs::write(
            target.join("access.toml"),
            "version = 1\ncredentials = []\naudit = [{ id = \"00000000-0000-4000-8000-000000000001\", timestamp_ms = 1, action = \"existing\", result = \"success\", client_classification = \"direct_local\" }]\n",
        )
        .unwrap();
        let bundle_path = directory.join("backup.zip");
        let (bundle, manifest) = backup::create_bundle(
            Cursor::new(Vec::new()),
            backup::CreateBundleOptions {
                config_path: &source_config,
                sections: &[
                    backup::BackupSection::RuntimeConfig,
                    backup::BackupSection::CameraDatabase,
                    backup::BackupSection::Access,
                ],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        std::fs::write(&bundle_path, bundle.into_inner()).unwrap();
        let request = backup_proto::CreateRestorePlanRequest {
            client_request_id: "request-fixture".to_owned(),
            backup_id: "backup-fixture".to_owned(),
            sections: Vec::new(),
            path_mappings: vec![backup_proto::RestorePathMapping {
                kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                source_path: manifest.source_paths[0].path.clone(),
                target_path: target.to_string_lossy().into_owned(),
            }],
            expected_target_revision: target_revision(&target_config).unwrap(),
        };
        let plan = plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: 1_788_000_001_000,
        })
        .unwrap();
        assert!(plan.can_activate);
        (directory, bundle_path, target_config, plan)
    }

    fn test_directory(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("keeppeek-restore-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }
}
