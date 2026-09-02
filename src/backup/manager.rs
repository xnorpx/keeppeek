use super::{BackupSection, BackupStoragePaths, CreateBundleOptions};
use crate::{api::backup_proto, notifications, storage::RecordingCatalogHandle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
};

const MAXIMUM_RETAINED_BACKUPS: usize = 16;
const MAXIMUM_RETAINED_RESTORE_PLANS: usize = 128;
const MAXIMUM_METADATA_BYTES: u64 = 64 * 1024;
const UPLOAD_TTL_MS: u64 = 10 * 60 * 1_000;

#[derive(Deserialize, Serialize)]
struct StoredBackup {
    backup_id: String,
    client_request_id: String,
    file_name: String,
    origin: i32,
    created_at_unix_ms: u64,
    archive_bytes: u64,
    archive_sha256: String,
}

#[derive(Clone)]
struct PendingUpload {
    transfer_id: String,
    backup_id: String,
    client_request_id: String,
    file_name: String,
    content_length: u64,
    archive_sha256: String,
    expires_at_unix_ms: u64,
    temporary_path: PathBuf,
}

#[derive(Clone)]
struct RetainedRestorePlan {
    client_request_id: String,
    plan: backup_proto::RestorePlan,
}

/// Owns bounded backup artifacts and their persistent metadata.
pub struct BackupManager {
    config_path: PathBuf,
    root: PathBuf,
    recording_catalog: Option<RecordingCatalogHandle>,
    notifications: Option<notifications::Handle>,
    storage_paths: Option<BackupStoragePaths>,
    operation: Mutex<()>,
    uploads: Mutex<HashMap<String, PendingUpload>>,
    restore_plans: Mutex<HashMap<String, RetainedRestorePlan>>,
    operation_successes: AtomicU64,
    operation_failures: AtomicU64,
}

impl BackupManager {
    /// Returns fixed limits and sections supported by this server build.
    pub fn capabilities(
        &self,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupCapabilities> {
        let config_directory = self
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?;
        let mut target_paths = vec![backup_proto::BackupPath {
            kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
            path: config_directory.to_string_lossy().into_owned(),
        }];
        if let Some(catalog) = &self.recording_catalog {
            target_paths.push(backup_proto::BackupPath {
                kind: backup_proto::BackupPathKind::RecordingCatalog as i32,
                path: catalog.database_path().to_string_lossy().into_owned(),
            });
        }
        if let Some(notifications) = &self.notifications {
            target_paths.push(backup_proto::BackupPath {
                kind: backup_proto::BackupPathKind::NotificationDatabase as i32,
                path: notifications.database_path().to_string_lossy().into_owned(),
            });
        }
        if let Some(storage_paths) = &self.storage_paths {
            for kind in [
                super::BackupPathKind::LongTermMedia,
                super::BackupPathKind::EventThumbnails,
            ] {
                target_paths.push(backup_proto::BackupPath {
                    kind: kind.to_proto() as i32,
                    path: storage_paths
                        .path(kind)?
                        .canonicalize()?
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
        Ok(backup_proto::BackupCapabilities {
            contract_id: "keeppeek.backup.v1".to_owned(),
            current_bundle_format: super::FORMAT_VERSION,
            minimum_bundle_format: super::LEGACY_FORMAT_VERSION,
            maximum_upload_bytes: super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes,
            maximum_expanded_bytes: super::DEFAULT_INSPECTION_LIMITS.maximum_total_bytes,
            maximum_section_bytes: super::DEFAULT_INSPECTION_LIMITS.maximum_section_bytes,
            maximum_archive_members: u32::try_from(
                super::DEFAULT_INSPECTION_LIMITS.maximum_archive_members,
            )
            .unwrap_or(u32::MAX),
            maximum_retained_backups: u32::try_from(MAXIMUM_RETAINED_BACKUPS).unwrap_or(u32::MAX),
            maximum_retained_restore_plans: u32::try_from(MAXIMUM_RETAINED_RESTORE_PLANS)
                .unwrap_or(u32::MAX),
            restore_plan_ttl_seconds: 10 * 60,
            rollback_window_seconds: 30 * 60,
            supported_sections: self
                .supported_sections()
                .into_iter()
                .map(|section| section.to_proto() as i32)
                .collect(),
            target_revision: super::target_revision(&self.config_path)?,
            target_paths,
            active_restore: super::active_restore(&self.config_path, now_unix_ms)?,
        })
    }

    fn supported_sections(&self) -> Vec<BackupSection> {
        let mut sections = vec![
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
            if self.config_path.with_file_name(file_name).is_file() {
                sections.push(section);
            }
        }
        if self.recording_catalog.is_some() {
            sections.extend([
                BackupSection::RecordingCatalog,
                BackupSection::EventMetadata,
            ]);
            if self.storage_paths.is_some() {
                sections.push(BackupSection::EventThumbnails);
            }
        }
        if self.notifications.is_some() {
            sections.push(BackupSection::Notifications);
        }
        sections.sort_unstable();
        sections
    }

    /// Opens the managed backup directory beside `config.toml`.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created or secured.
    pub fn open(
        config_path: PathBuf,
        recording_catalog: Option<RecordingCatalogHandle>,
        notifications: Option<notifications::Handle>,
        storage_paths: Option<BackupStoragePaths>,
    ) -> anyhow::Result<Self> {
        let root = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".backups");
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            config_path,
            root,
            recording_catalog,
            notifications,
            storage_paths,
            operation: Mutex::new(()),
            uploads: Mutex::new(HashMap::new()),
            restore_plans: Mutex::new(HashMap::new()),
            operation_successes: AtomicU64::new(0),
            operation_failures: AtomicU64::new(0),
        })
    }

    pub(crate) fn record_http_result(&self, success: bool) {
        let counter = if success {
            &self.operation_successes
        } else {
            &self.operation_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn metric_snapshot(
        &self,
        now_unix_ms: u64,
    ) -> anyhow::Result<crate::metrics::BackupMetricsSnapshot> {
        let metadata = self.metadata()?;
        Ok(crate::metrics::BackupMetricsSnapshot {
            operation_successes: self.operation_successes.load(Ordering::Relaxed),
            operation_failures: self.operation_failures.load(Ordering::Relaxed),
            retained_backups: u64::try_from(metadata.len())?,
            retained_archive_bytes: metadata.iter().fold(0_u64, |total, backup| {
                total.saturating_add(backup.archive_bytes)
            }),
            active_restore: u64::from(
                super::active_restore(&self.config_path, now_unix_ms)?.is_some(),
            ),
        })
    }

    /// Creates or replays one idempotent managed backup request.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid request, unavailable section, retention limit, concurrent
    /// operation, snapshot failure, or artifact that exceeds its declared bound.
    pub fn create(
        &self,
        request: &backup_proto::CreateBackupRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupRecord> {
        validate_identifier("client_request_id", &request.client_request_id)?;
        if now_unix_ms == 0 {
            anyhow::bail!("backup creation time must be nonzero");
        }
        if request.expected_archive_bytes > super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes {
            return Err(super::ServiceError::capacity(
                "expected backup size exceeds the archive limit",
            )
            .into());
        }
        let _operation = self.try_operation()?;
        if let Some(existing) = self.find_by_request(&request.client_request_id)? {
            return self.record(&existing);
        }
        if self.metadata()?.len() >= MAXIMUM_RETAINED_BACKUPS {
            return Err(
                super::ServiceError::capacity("managed backup retention limit reached").into(),
            );
        }
        let sections = request
            .sections
            .iter()
            .map(|section| BackupSection::from_proto(*section))
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|_| super::ServiceError::invalid("sections", "backup sections are invalid"))?;
        self.create_new(request, sections, now_unix_ms)
    }

    /// Lists every retained backup in newest-first order.
    ///
    /// # Errors
    ///
    /// Returns an error when retained metadata or an artifact is invalid.
    pub fn list(&self) -> anyhow::Result<backup_proto::ListBackupsResponse> {
        let mut backups = self
            .metadata()?
            .iter()
            .map(|metadata| self.record(metadata))
            .collect::<anyhow::Result<Vec<_>>>()?;
        backups.sort_unstable_by(|left, right| {
            right
                .created_at_unix_ms
                .cmp(&left.created_at_unix_ms)
                .then_with(|| right.backup_id.cmp(&left.backup_id))
        });
        Ok(backup_proto::ListBackupsResponse {
            backups,
            next_page_token: String::new(),
        })
    }

    /// Revalidates and returns one retained backup.
    pub fn inspect(&self, backup_id: &str) -> anyhow::Result<backup_proto::BackupRecord> {
        let metadata = self.read_metadata(backup_id)?;
        self.record(&metadata)
    }

    /// Returns the validated path used for an HTTP ZIP download.
    pub fn artifact_path(&self, backup_id: &str) -> anyhow::Result<PathBuf> {
        let backup_id = canonical_backup_id(backup_id)?;
        let path = self.root.join(format!("{backup_id}.zip"));
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(super::ServiceError::not_found("backup was not found").into());
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!("backup artifact is not a regular file");
        }
        Ok(path)
    }

    /// Returns a short-lived download description for one validated artifact.
    pub fn begin_download(
        &self,
        request: &backup_proto::BeginBackupDownloadRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupTransfer> {
        let record = self.inspect(&request.backup_id)?;
        Ok(backup_proto::BackupTransfer {
            transfer_id: uuid::Uuid::new_v4().to_string(),
            backup_id: record.backup_id.clone(),
            uri: format!("/api/backups/download?backup_id={}", record.backup_id),
            content_type: "application/zip".to_owned(),
            maximum_bytes: record.archive_bytes,
            expires_at_unix_ms: now_unix_ms.saturating_add(UPLOAD_TTL_MS),
            expected_sha256: record.archive_sha256,
        })
    }

    /// Deletes one retained backup and its metadata.
    pub fn delete(&self, backup_id: &str) -> anyhow::Result<backup_proto::DeleteBackupResponse> {
        let _operation = self.try_operation()?;
        let backup_id = canonical_backup_id(backup_id)?;
        let artifact = self.root.join(format!("{backup_id}.zip"));
        let metadata = self.root.join(format!("{backup_id}.json"));
        let deleted = remove_if_exists(&artifact)? | remove_if_exists(&metadata)?;
        Ok(backup_proto::DeleteBackupResponse { backup_id, deleted })
    }

    /// Reserves one bounded HTTP ZIP upload.
    pub fn begin_upload(
        &self,
        request: &backup_proto::BeginBackupUploadRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupTransfer> {
        validate_identifier("client_request_id", &request.client_request_id)?;
        validate_file_name(&request.file_name)?;
        if let Some(archive_sha256) = &request.archive_sha256 {
            validate_sha256(archive_sha256)?;
        }
        if now_unix_ms == 0
            || request.content_length == 0
            || request.content_length > super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes
        {
            return Err(super::ServiceError::invalid(
                "contentLength",
                "upload content length is outside the supported range",
            )
            .into());
        }
        let _operation = self.try_operation()?;
        if self.metadata()?.len() >= MAXIMUM_RETAINED_BACKUPS {
            return Err(
                super::ServiceError::capacity("managed backup retention limit reached").into(),
            );
        }
        let mut uploads = self
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        uploads.retain(|_, upload| {
            let retain = upload.expires_at_unix_ms >= now_unix_ms;
            if !retain {
                let _ = std::fs::remove_file(&upload.temporary_path);
            }
            retain
        });
        if let Some(existing) = uploads
            .values()
            .find(|upload| upload.client_request_id == request.client_request_id)
        {
            return Ok(upload_transfer(existing));
        }
        if !uploads.is_empty() {
            return Err(super::ServiceError::busy("another backup upload is active").into());
        }
        let transfer_id = uuid::Uuid::new_v4().to_string();
        let backup_id = uuid::Uuid::new_v4().to_string();
        let upload = PendingUpload {
            transfer_id: transfer_id.clone(),
            backup_id: backup_id.clone(),
            client_request_id: request.client_request_id.clone(),
            file_name: request.file_name.clone(),
            content_length: request.content_length,
            archive_sha256: request
                .archive_sha256
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            expires_at_unix_ms: now_unix_ms.saturating_add(UPLOAD_TTL_MS),
            temporary_path: self.root.join(format!(".{backup_id}.upload.tmp")),
        };
        let transfer = upload_transfer(&upload);
        uploads.insert(transfer_id, upload);
        Ok(transfer)
    }

    /// Streams and promotes a previously reserved HTTP ZIP upload.
    pub fn accept_upload(
        &self,
        transfer_id: &str,
        reader: impl Read,
        content_length: u64,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupRecord> {
        let _operation = self.try_operation()?;
        let upload = self
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(transfer_id)
            .cloned()
            .ok_or_else(|| {
                super::ServiceError::not_found("backup upload transfer was not found")
            })?;
        let result = self.accept_upload_inner(&upload, reader, content_length, now_unix_ms);
        self.uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(transfer_id);
        if result.is_err() {
            let _ = std::fs::remove_file(&upload.temporary_path);
        }
        result
    }

    /// Creates or replays one immutable restore dry-run plan.
    pub fn create_restore_plan(
        &self,
        request: &backup_proto::CreateRestorePlanRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::RestorePlan> {
        validate_identifier("client_request_id", &request.client_request_id)?;
        canonical_backup_id(&request.backup_id)?;
        let _operation = self.try_operation()?;
        let mut plans = self
            .restore_plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plans.retain(|_, retained| retained.plan.expires_at_unix_ms >= now_unix_ms);
        if let Some(existing) = plans
            .values()
            .find(|retained| retained.client_request_id == request.client_request_id)
        {
            return Ok(existing.plan.clone());
        }
        if plans.len() >= MAXIMUM_RETAINED_RESTORE_PLANS {
            return Err(super::ServiceError::busy("restore plan retention limit reached").into());
        }
        let plan = super::plan_restore(super::RestorePlanOptions {
            bundle_path: &self.artifact_path(&request.backup_id)?,
            target_config_path: &self.config_path,
            request,
            now_unix_ms,
        })?;
        plans.insert(
            plan.plan_id.clone(),
            RetainedRestorePlan {
                client_request_id: request.client_request_id.clone(),
                plan: plan.clone(),
            },
        );
        Ok(plan)
    }

    /// Confirms and stages one retained restore plan.
    pub fn activate_restore(
        &self,
        request: &backup_proto::ActivateRestoreRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::RestoreRecord> {
        validate_identifier("client_request_id", &request.client_request_id)?;
        if !request.confirm {
            return Err(super::ServiceError::invalid(
                "confirm",
                "restore activation requires explicit confirmation",
            )
            .into());
        }
        validate_sha256(&request.archive_sha256)?;
        let _operation = self.try_operation()?;
        let plan = self
            .restore_plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&request.plan_id)
            .map(|retained| retained.plan.clone())
            .ok_or_else(|| super::ServiceError::not_found("restore plan was not found"))?;
        if request.archive_sha256 != plan.archive_sha256 {
            return Err(super::ServiceError::conflict(
                "restore archive digest does not match its plan",
            )
            .into());
        }
        let record = super::stage_restore(super::StageRestoreOptions {
            bundle_path: &self.artifact_path(&plan.backup_id)?,
            target_config_path: &self.config_path,
            plan: &plan,
            now_unix_ms,
        })?;
        self.restore_plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request.plan_id);
        Ok(record)
    }

    /// Returns the current state of one staged or retained restore.
    pub fn get_restore(
        &self,
        request: &backup_proto::GetRestoreRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::RestoreRecord> {
        super::current_restore(&self.config_path, &request.restore_id, now_unix_ms)
    }

    /// Requests a confirmed rollback during the retained rollback window.
    pub fn rollback_restore(
        &self,
        request: &backup_proto::RollbackRestoreRequest,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::RestoreRecord> {
        validate_identifier("client_request_id", &request.client_request_id)?;
        if !request.confirm {
            return Err(super::ServiceError::invalid(
                "confirm",
                "restore rollback requires explicit confirmation",
            )
            .into());
        }
        super::request_restore_rollback(&self.config_path, &request.restore_id, now_unix_ms)
    }

    fn create_new(
        &self,
        request: &backup_proto::CreateBackupRequest,
        sections: Vec<BackupSection>,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupRecord> {
        let backup_id = uuid::Uuid::new_v4().to_string();
        let temporary = self.root.join(format!(".{backup_id}.zip.tmp"));
        let artifact = self.root.join(format!("{backup_id}.zip"));
        let result = (|| {
            let file = create_private_file(&temporary)?;
            let (file, manifest) = super::create_bundle(
                file,
                CreateBundleOptions {
                    config_path: &self.config_path,
                    sections: &sections,
                    created_at_unix_ms: now_unix_ms,
                    recording_catalog: self.recording_catalog.as_ref(),
                    notifications: self.notifications.as_ref(),
                    storage_paths: self.storage_paths.as_ref(),
                },
            )?;
            file.sync_all()?;
            let archive_bytes = std::fs::metadata(&temporary)?.len();
            if request.expected_archive_bytes != 0 && archive_bytes > request.expected_archive_bytes
            {
                return Err(super::ServiceError::capacity(
                    "created backup exceeds expectedArchiveBytes",
                )
                .into());
            }
            let archive_sha256 = hash_file(&temporary)?;
            std::fs::rename(&temporary, &artifact)?;
            let metadata = StoredBackup {
                backup_id: backup_id.clone(),
                client_request_id: request.client_request_id.clone(),
                file_name: format!("keeppeek-backup-{now_unix_ms}-{backup_id}.zip"),
                origin: backup_proto::BackupOrigin::Created as i32,
                created_at_unix_ms: now_unix_ms,
                archive_bytes,
                archive_sha256,
            };
            self.write_metadata(&metadata)?;
            self.record_with_manifest(&metadata, manifest)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
            let _ = std::fs::remove_file(&artifact);
        }
        result
    }

    fn accept_upload_inner(
        &self,
        upload: &PendingUpload,
        reader: impl Read,
        content_length: u64,
        now_unix_ms: u64,
    ) -> anyhow::Result<backup_proto::BackupRecord> {
        if now_unix_ms == 0 || now_unix_ms > upload.expires_at_unix_ms {
            return Err(super::ServiceError::expired("backup upload transfer expired").into());
        }
        if content_length != upload.content_length {
            return Err(super::ServiceError::invalid(
                "Content-Length",
                "upload Content-Length does not match its reservation",
            )
            .into());
        }
        let mut file = create_private_file(&upload.temporary_path)?;
        let actual_sha256 = stream_upload(reader, &mut file, content_length)?;
        file.sync_all()?;
        if !upload.archive_sha256.is_empty() && actual_sha256 != upload.archive_sha256 {
            return Err(
                super::ServiceError::checksum("uploaded backup checksum does not match").into(),
            );
        }
        let manifest = super::inspect_bundle(std::fs::File::open(&upload.temporary_path)?)
            .map_err(|_| {
                super::ServiceError::invalid("archive", "backup artifact failed validation")
            })?
            .to_proto();
        let artifact = self.root.join(format!("{}.zip", upload.backup_id));
        std::fs::rename(&upload.temporary_path, &artifact)?;
        let metadata = StoredBackup {
            backup_id: upload.backup_id.clone(),
            client_request_id: upload.client_request_id.clone(),
            file_name: upload.file_name.clone(),
            origin: backup_proto::BackupOrigin::Uploaded as i32,
            created_at_unix_ms: now_unix_ms,
            archive_bytes: content_length,
            archive_sha256: actual_sha256,
        };
        if let Err(error) = self.write_metadata(&metadata) {
            let _ = std::fs::remove_file(artifact);
            return Err(error);
        }
        self.record_with_manifest(&metadata, manifest)
    }

    fn record(&self, metadata: &StoredBackup) -> anyhow::Result<backup_proto::BackupRecord> {
        let manifest = super::inspect_bundle(std::fs::File::open(
            self.artifact_path(&metadata.backup_id)?,
        )?)?
        .to_proto();
        self.record_with_manifest(metadata, manifest)
    }

    fn record_with_manifest(
        &self,
        metadata: &StoredBackup,
        manifest: backup_proto::BackupManifest,
    ) -> anyhow::Result<backup_proto::BackupRecord> {
        let artifact = self.artifact_path(&metadata.backup_id)?;
        let archive_bytes = std::fs::metadata(&artifact)?.len();
        if archive_bytes != metadata.archive_bytes
            || hash_file(&artifact)? != metadata.archive_sha256
        {
            anyhow::bail!("managed backup artifact does not match its metadata");
        }
        Ok(backup_proto::BackupRecord {
            backup_id: metadata.backup_id.clone(),
            origin: metadata.origin,
            state: backup_proto::BackupState::Ready as i32,
            file_name: metadata.file_name.clone(),
            created_at_unix_ms: metadata.created_at_unix_ms,
            updated_at_unix_ms: metadata.created_at_unix_ms,
            archive_bytes,
            archive_sha256: metadata.archive_sha256.clone(),
            manifest: Some(manifest),
            progress: Some(backup_proto::BackupProgress {
                completed_per_mille: 1_000,
                completed_bytes: archive_bytes,
                total_bytes: Some(archive_bytes),
                active_section: None,
            }),
            error: None,
        })
    }

    fn metadata(&self) -> anyhow::Result<Vec<StoredBackup>> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if uuid::Uuid::parse_str(stem).is_err() {
                continue;
            }
            records.push(read_metadata_file(&path)?);
            if records.len() > MAXIMUM_RETAINED_BACKUPS {
                anyhow::bail!("managed backup metadata exceeds the retention limit");
            }
        }
        Ok(records)
    }

    fn find_by_request(&self, client_request_id: &str) -> anyhow::Result<Option<StoredBackup>> {
        Ok(self
            .metadata()?
            .into_iter()
            .find(|metadata| metadata.client_request_id == client_request_id))
    }

    fn read_metadata(&self, backup_id: &str) -> anyhow::Result<StoredBackup> {
        let backup_id = canonical_backup_id(backup_id)?;
        let path = self.root.join(format!("{backup_id}.json"));
        if !path.is_file() {
            return Err(super::ServiceError::not_found("backup was not found").into());
        }
        read_metadata_file(&path)
    }

    fn write_metadata(&self, metadata: &StoredBackup) -> anyhow::Result<()> {
        let path = self.root.join(format!("{}.json", metadata.backup_id));
        crate::config::write_private_file_atomically(&path, &serde_json::to_vec_pretty(metadata)?)?;
        Ok(())
    }

    fn try_operation(&self) -> anyhow::Result<MutexGuard<'_, ()>> {
        match self.operation.try_lock() {
            Ok(operation) => Ok(operation),
            Err(TryLockError::WouldBlock) => {
                Err(super::ServiceError::busy("another backup operation is active").into())
            }
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        }
    }
}

fn validate_identifier(_name: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(super::ServiceError::invalid(
            "identifier",
            "backup identifier must contain 1 to 128 printable bytes",
        )
        .into());
    }
    Ok(())
}

fn validate_file_name(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value.chars().any(char::is_control)
        || value.contains(['/', '\\'])
        || !value.to_ascii_lowercase().ends_with(".zip")
    {
        return Err(super::ServiceError::invalid(
            "fileName",
            "backup fileName must be a plain ZIP filename",
        )
        .into());
    }
    Ok(())
}

fn validate_sha256(value: &str) -> anyhow::Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(super::ServiceError::invalid(
            "archiveSha256",
            "archiveSha256 must be a 64-character hexadecimal digest",
        )
        .into());
    }
    Ok(())
}

fn upload_transfer(upload: &PendingUpload) -> backup_proto::BackupTransfer {
    backup_proto::BackupTransfer {
        transfer_id: upload.transfer_id.clone(),
        backup_id: upload.backup_id.clone(),
        uri: "/api/backups/transfers".to_owned(),
        content_type: "application/zip".to_owned(),
        maximum_bytes: upload.content_length,
        expires_at_unix_ms: upload.expires_at_unix_ms,
        expected_sha256: upload.archive_sha256.clone(),
    }
}

fn stream_upload(
    reader: impl Read,
    destination: &mut std::fs::File,
    content_length: u64,
) -> anyhow::Result<String> {
    let mut reader = reader.take(content_length.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read)?)
            .ok_or_else(|| anyhow::anyhow!("uploaded backup size overflow"))?;
        if total > content_length {
            anyhow::bail!("uploaded backup exceeds Content-Length");
        }
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    if total != content_length {
        anyhow::bail!("uploaded backup is shorter than Content-Length");
    }
    Ok(super::encode_lower_hex(hasher.finalize()))
}

fn canonical_backup_id(backup_id: &str) -> anyhow::Result<String> {
    let parsed = uuid::Uuid::parse_str(backup_id).map_err(|_| {
        super::ServiceError::invalid("backupId", "backupId must be a canonical UUID")
    })?;
    let canonical = parsed.to_string();
    if canonical != backup_id {
        return Err(
            super::ServiceError::invalid("backupId", "backupId must be a canonical UUID").into(),
        );
    }
    Ok(canonical)
}

fn read_metadata_file(path: &Path) -> anyhow::Result<StoredBackup> {
    if std::fs::metadata(path)?.len() > MAXIMUM_METADATA_BYTES {
        anyhow::bail!("backup metadata exceeds its size limit");
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(Into::into)
}

fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read)?);
        if total > super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes {
            anyhow::bail!("managed backup exceeds the archive size limit");
        }
        hasher.update(&buffer[..read]);
    }
    Ok(super::encode_lower_hex(hasher.finalize()))
}

fn remove_if_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_backup_create_list_inspect_download_and_delete_round_trip() {
        let directory = test_directory();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let manager = BackupManager::open(config_path, None, None, None).unwrap();
        let request = backup_proto::CreateBackupRequest {
            client_request_id: "request-1".to_owned(),
            sections: vec![
                backup_proto::BackupSection::RuntimeConfig as i32,
                backup_proto::BackupSection::CameraDatabase as i32,
            ],
            expected_archive_bytes: 1024 * 1024,
        };

        let created = manager.create(&request, 1_788_000_000_000).unwrap();
        let repeated = manager.create(&request, 1_788_000_000_001).unwrap();

        assert_eq!(created.backup_id, repeated.backup_id);
        assert_eq!(created.state, backup_proto::BackupState::Ready as i32);
        assert_eq!(created.archive_sha256.len(), 64);
        assert!(manager.artifact_path(&created.backup_id).unwrap().is_file());
        assert_eq!(manager.list().unwrap().backups.len(), 1);
        assert_eq!(
            manager.inspect(&created.backup_id).unwrap().manifest,
            created.manifest
        );
        assert!(manager.delete(&created.backup_id).unwrap().deleted);
        assert!(manager.list().unwrap().backups.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn managed_upload_streams_validates_and_promotes_one_archive() {
        let directory = test_directory();
        let source_config = directory.join("source.toml");
        let target_config = directory.join("target.toml");
        std::fs::write(&source_config, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let (bundle, _) = super::super::create_bundle(
            std::io::Cursor::new(Vec::new()),
            super::super::CreateBundleOptions {
                config_path: &source_config,
                sections: &[super::super::BackupSection::RuntimeConfig],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        let bytes = bundle.into_inner();
        let manager = BackupManager::open(target_config, None, None, None).unwrap();
        let request = backup_proto::BeginBackupUploadRequest {
            client_request_id: "upload-1".to_owned(),
            file_name: "portable-backup.zip".to_owned(),
            content_length: u64::try_from(bytes.len()).unwrap(),
            archive_sha256: Some(super::super::encode_lower_hex(Sha256::digest(&bytes))),
        };

        let transfer = manager.begin_upload(&request, 1_788_000_001_000).unwrap();
        let uploaded = manager
            .accept_upload(
                &transfer.transfer_id,
                std::io::Cursor::new(bytes),
                request.content_length,
                1_788_000_001_001,
            )
            .unwrap();

        assert_eq!(uploaded.backup_id, transfer.backup_id);
        assert_eq!(uploaded.origin, backup_proto::BackupOrigin::Uploaded as i32);
        assert_eq!(uploaded.file_name, "portable-backup.zip");
        assert!(
            manager
                .artifact_path(&uploaded.backup_id)
                .unwrap()
                .is_file()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn managed_restore_plan_requires_confirmation_and_stages_the_exact_artifact() {
        let directory = test_directory();
        let source = directory.join("source");
        let target = directory.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_config = source.join("config.toml");
        let target_config = target.join("config.toml");
        std::fs::write(&source_config, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        std::fs::write(&target_config, "[storage]\nlong_term_max_gb = 20\n").unwrap();
        let manager = BackupManager::open(target_config.clone(), None, None, None).unwrap();
        let (bundle, source_manifest) = super::super::create_bundle(
            std::io::Cursor::new(Vec::new()),
            super::super::CreateBundleOptions {
                config_path: &source_config,
                sections: &[
                    super::super::BackupSection::RuntimeConfig,
                    super::super::BackupSection::CameraDatabase,
                ],
                created_at_unix_ms: 1_788_000_000_000,
                recording_catalog: None,
                notifications: None,
                storage_paths: None,
            },
        )
        .unwrap();
        let bytes = bundle.into_inner();
        let upload = manager
            .begin_upload(
                &backup_proto::BeginBackupUploadRequest {
                    client_request_id: "upload-plan".to_owned(),
                    file_name: "restore.zip".to_owned(),
                    content_length: u64::try_from(bytes.len()).unwrap(),
                    archive_sha256: Some(super::super::encode_lower_hex(Sha256::digest(&bytes))),
                },
                1_788_000_001_000,
            )
            .unwrap();
        assert_eq!(upload.uri, "/api/backups/transfers");
        let uploaded = manager
            .accept_upload(
                &upload.transfer_id,
                std::io::Cursor::new(bytes),
                upload.maximum_bytes,
                1_788_000_001_001,
            )
            .unwrap();
        let plan = manager
            .create_restore_plan(
                &backup_proto::CreateRestorePlanRequest {
                    client_request_id: "plan-1".to_owned(),
                    backup_id: uploaded.backup_id,
                    sections: Vec::new(),
                    path_mappings: vec![backup_proto::RestorePathMapping {
                        kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
                        source_path: source_manifest.source_paths[0].path.clone(),
                        target_path: target.to_string_lossy().into_owned(),
                    }],
                    expected_target_revision: super::super::target_revision(&target_config)
                        .unwrap(),
                },
                1_788_000_001_002,
            )
            .unwrap();

        let unconfirmed = manager.activate_restore(
            &backup_proto::ActivateRestoreRequest {
                client_request_id: "activate-1".to_owned(),
                plan_id: plan.plan_id.clone(),
                archive_sha256: plan.archive_sha256.clone(),
                confirm: false,
            },
            1_788_000_001_003,
        );
        assert!(
            unconfirmed
                .unwrap_err()
                .to_string()
                .contains("confirmation")
        );
        let staged = manager
            .activate_restore(
                &backup_proto::ActivateRestoreRequest {
                    client_request_id: "activate-1".to_owned(),
                    plan_id: plan.plan_id,
                    archive_sha256: plan.archive_sha256,
                    confirm: true,
                },
                1_788_000_001_004,
            )
            .unwrap();
        assert_eq!(
            staged.state,
            backup_proto::RestoreState::AwaitingRestart as i32
        );
        assert!(
            std::fs::read_to_string(&target_config)
                .unwrap()
                .contains("20")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("keeppeek-backup-manager-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }
}
