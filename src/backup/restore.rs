use super::{BackupManifest, BackupPathKind, BackupSection};
use crate::{api::backup_proto, config, storage::safety::filesystem_capacity};
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
    expected_sha256: String,
    database: bool,
    target_existed: bool,
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
    for path in [
        config_path.to_path_buf(),
        config_path.with_file_name("access.toml"),
        config_path.with_file_name("peek-layouts.json"),
        config_path.with_file_name("configuration-templates.json"),
    ] {
        hasher.update(path.file_name().unwrap_or_default().as_encoded_bytes());
        hasher.update([0]);
        match std::fs::File::open(&path) {
            Ok(mut file) => hash_reader(&mut hasher, &mut file)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hasher.update(b"missing");
            }
            Err(error) => return Err(error.into()),
        }
        hasher.update([0]);
    }
    Ok(super::encode_lower_hex(hasher.finalize()))
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
    validate_required_secrets(
        options.target_config_path,
        manifest.required_secret_references(),
        &mut issues,
    );
    let capacity_checks = capacity_checks(&manifest, &selected, &mappings, &mut issues);
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
        path_mappings: options.request.path_mappings.clone(),
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
        targets,
    };
    persist_journal(&journal_path, &journal)?;
    if let Err(error) = write_preparations(
        options.bundle_path,
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
    persist_journal(&path, &journal)?;
    let mut record = restore_record_at(&journal, backup_proto::RestoreState::Complete, now_unix_ms);
    record.health_checks = vec![
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
    Ok(Some(record))
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
        if expected_config != target_config_path {
            anyhow::bail!("config directory mapping does not match the target configuration");
        }
        let bytes = restored_configuration(&mut archive, manifest, selected, target_config_path)?;
        push_bytes_target(
            &mut targets,
            &mut preparations,
            target_config_path,
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
    plan.path_mappings
        .iter()
        .find(|mapping| mapping.kind == kind.to_proto() as i32)
        .map(|mapping| PathBuf::from(&mapping.target_path))
        .ok_or_else(|| anyhow::anyhow!("restore plan is missing a required path mapping"))
}

fn restored_configuration<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &BackupManifest,
    selected: &[BackupSection],
    target_config_path: &Path,
) -> anyhow::Result<Vec<u8>> {
    let mut target = config::load_configuration_table(target_config_path)?;
    if selected.contains(&BackupSection::RuntimeConfig) {
        let source = section_bytes(archive, manifest, BackupSection::RuntimeConfig)?;
        let source: toml::Table = toml::from_str(std::str::from_utf8(&source)?)?;
        target.retain(|key, value| !runtime_owned(key, value));
        target.extend(source);
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
    if std::fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!("restore targets must not be symbolic links");
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore target has no parent"))?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("restore target file name is invalid"))?;
    Ok(RestoreJournalTarget {
        target: target.to_owned(),
        staged: parent.join(format!(".{file_name}.keeppeek-{restore_id}.staged")),
        rollback: parent.join(format!(".{file_name}.keeppeek-{restore_id}.rollback")),
        expected_sha256,
        database,
        target_existed: target.exists(),
        prepared: false,
        applied: false,
    })
}

fn write_preparations(
    bundle_path: &Path,
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
            }
        }
        journal.targets[index].prepared = true;
        persist_journal(journal_path, journal)?;
    }
    Ok(())
}

fn write_staged_bytes(target: &RestoreJournalTarget, bytes: &[u8]) -> anyhow::Result<()> {
    config::write_private_file(&target.staged, bytes)?;
    std::fs::File::open(&target.staged)?.sync_all()?;
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
        let staged_hash = hash_file(
            &target.staged,
            super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
        );
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
    if target.target_existed {
        move_target(&target.target, &target.rollback, target.database)?;
    } else if target.target.exists() {
        anyhow::bail!("restore target appeared after staging");
    }
    match std::fs::rename(&target.staged, &target.target) {
        Ok(()) => Ok(()),
        Err(error) => {
            if target.target_existed {
                let _ = move_target(&target.rollback, &target.target, target.database);
            }
            Err(error.into())
        }
    }
}

fn reconcile_applied_targets(journal: &mut RestoreJournal) {
    for target in &mut journal.targets {
        if target.applied {
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
        remove_target(&target.target, target.database);
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
        health_checks: Vec::new(),
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
        if mappings
            .insert(kind, PathBuf::from(&mapping.target_path))
            .is_some()
        {
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
        if !Path::new(&mapping.target_path).is_absolute() {
            issues.push(issue(
                "target_path_not_absolute",
                "The target path must be absolute on this platform.",
                None,
                "pathMappings.targetPath",
            ));
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
    backup_proto::RestoreIssue {
        severity: backup_proto::RestoreIssueSeverity::Blocking as i32,
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
