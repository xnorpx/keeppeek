//! Immutable A/B versioned executable slots and atomic crash-recovery journal.
//!
//! Layout under a supervisor root directory:
//! - `supervisor-state.json`: atomic slot and probation state journal.
//! - `supervisor-state.lock`: cross-process OS lock serializing state updates.
//! - `versions/<version>/keeppeek[.exe]` and `manifest.json` for core releases.
//! - `vision/<version>/keeppeek-vision[.exe]` and `manifest.json` for Vision releases.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::manifest::{
    ComponentKind, ManifestError, SchemaCompatibility, SignedReleaseManifest, TrustedSigningKey,
    hash_binary_file, read_manifest_file, verify_manifest, verify_manifest_and_file,
};
use crate::version::ReleaseVersion;

/// Supported on-disk schema version for `supervisor-state.json`.
pub const STATE_SCHEMA_VERSION_V1: u32 = 1;

/// Maximum serialized byte size allowed for `supervisor-state.json`.
pub const MAX_STATE_FILE_BYTES: u64 = 64 * 1024;

/// Maximum number of quarantined release versions retained per component.
pub const MAX_QUARANTINED_VERSIONS: usize = 32;

/// Default maximum boot attempts allowed for a pending version before rollback.
pub const DEFAULT_MAX_BOOT_ATTEMPTS: u32 = 3;

const STATE_FILE_NAME: &str = "supervisor-state.json";
const STATE_LOCK_FILE_NAME: &str = "supervisor-state.lock";
const MANIFEST_FILE_NAME: &str = "manifest.json";
static STATE_IO_LOCK: Mutex<()> = Mutex::new(());
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persisted crash-recovery and A/B slot state across core and Vision components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorState {
    /// State schema version (must be [`STATE_SCHEMA_VERSION_V1`]).
    pub schema_version: u32,
    /// Slot state for the main `keeppeek` executable.
    pub core: ComponentSlotState,
    /// Slot state for the optional `keeppeek-vision` executable.
    pub vision: ComponentSlotState,
}

impl Default for SupervisorState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION_V1,
            core: ComponentSlotState::default(),
            vision: ComponentSlotState::default(),
        }
    }
}

impl SupervisorState {
    /// Returns a read-only reference to the requested component slot state.
    #[must_use]
    pub const fn component(&self, kind: ComponentKind) -> &ComponentSlotState {
        match kind {
            ComponentKind::Core => &self.core,
            ComponentKind::Vision => &self.vision,
        }
    }

    /// Returns a mutable reference to the requested component slot state.
    pub const fn component_mut(&mut self, kind: ComponentKind) -> &mut ComponentSlotState {
        match kind {
            ComponentKind::Core => &mut self.core,
            ComponentKind::Vision => &mut self.vision,
        }
    }
}

/// Persisted A/B slot pointers and probation counters for one component.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSlotState {
    /// Currently promoted healthy release version, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ReleaseVersion>,
    /// Last known-good release version prior to the current active version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<ReleaseVersion>,
    /// Newly staged version awaiting readiness and probation promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingUpgrade>,
    /// Monotonic version floor blocking downgrade staging of older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version_floor: Option<ReleaseVersion>,
    /// Bounded list of versions quarantined after failed probation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantined: Vec<ReleaseVersion>,
}

/// Tracks a staged upgrade currently undergoing launch probation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUpgrade {
    /// Target version staged in the component slot directory.
    pub version: ReleaseVersion,
    /// Unix timestamp in seconds when the version was staged.
    pub staged_at_unix_secs: u64,
    /// Number of boot attempts recorded so far for this pending version.
    pub boot_attempts: u32,
    /// Maximum boot attempts allowed before automatic quarantine and rollback.
    pub max_boot_attempts: u32,
    /// Schema compatibility declared in the signed manifest.
    pub schema_compatibility: SchemaCompatibility,
}

/// Options controlling offline staging of a signed release binary.
#[derive(Debug, Clone)]
pub struct StageOptions {
    /// Host `<os>-<arch>` platform expected in the manifest.
    pub expected_platform: String,
    /// Whole-second Unix timestamp used for expiry validation.
    pub now_unix_secs: u64,
    /// Allows staging manifests that declare [`SchemaCompatibility::Breaking`].
    pub allow_breaking: bool,
    /// Maximum boot attempts granted to the staged version during probation.
    pub max_boot_attempts: u32,
}

/// Resolved executable launch plan prepared by [`SlotStore::prepare_launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Component being launched.
    pub component: ComponentKind,
    /// Resolved executable path on disk.
    pub binary_path: PathBuf,
    /// Managed version being launched, or `None` when using an unmanaged fallback binary.
    pub version: Option<ReleaseVersion>,
    /// True when this launch is probing a `pending` upgrade slot.
    pub is_pending_probation: bool,
}

/// Outcome returned when rolling back a failed pending upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    /// Pending version that was quarantined.
    pub quarantined_version: ReleaseVersion,
    /// Fallback active version remaining after rollback, if any.
    pub fallback_active: Option<ReleaseVersion>,
}

/// Errors produced while managing A/B slots and the supervisor journal.
#[derive(Debug)]
pub enum SlotError {
    /// Manifest or binary cryptographic verification failed.
    Manifest(ManifestError),
    /// State file exceeded [`MAX_STATE_FILE_BYTES`].
    StateTooLarge { size_bytes: u64 },
    /// State file used an unsupported schema version.
    UnsupportedStateSchema(u32),
    /// State JSON parsing failed.
    ParseState(serde_json::Error),
    /// Target version is already the active running release.
    VersionAlreadyActive(ReleaseVersion),
    /// Target version is already staged in an immutable slot directory or pending probation.
    VersionAlreadyStaged(ReleaseVersion),
    /// Target version was previously quarantined after failing probation.
    VersionQuarantined(ReleaseVersion),
    /// Target version is strictly lower than `min_version_floor`.
    DowngradeRejected {
        candidate: ReleaseVersion,
        floor: ReleaseVersion,
    },
    /// Manifest declares a breaking schema migration without explicit operator opt-in.
    BreakingSchemaRequiresOptIn(ReleaseVersion),
    /// No managed slot or fallback executable exists for launch.
    NoExecutableAvailable(ComponentKind),
    /// No previous version is available for manual rollback.
    NoPreviousVersion(ComponentKind),
    /// Expected executable file is missing inside an active or previous slot directory.
    MissingSlotBinary { path: PathBuf },
    /// Pre-launch integrity verification failed for a published slot directory.
    SlotIntegrityFailed {
        version: ReleaseVersion,
        detail: String,
    },
    /// Filesystem I/O failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for SlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => error.fmt(formatter),
            Self::StateTooLarge { size_bytes } => write!(
                formatter,
                "supervisor state file is {size_bytes} bytes, exceeding {MAX_STATE_FILE_BYTES} bytes"
            ),
            Self::UnsupportedStateSchema(version) => {
                write!(formatter, "unsupported supervisor state schema {version}")
            }
            Self::ParseState(error) => {
                write!(formatter, "invalid supervisor state JSON: {error}")
            }
            Self::VersionAlreadyActive(version) => {
                write!(formatter, "version {version} is already the active release")
            }
            Self::VersionAlreadyStaged(version) => {
                write!(
                    formatter,
                    "version {version} is already staged in an immutable slot"
                )
            }
            Self::VersionQuarantined(version) => {
                write!(
                    formatter,
                    "version {version} is quarantined after a failed boot"
                )
            }
            Self::DowngradeRejected { candidate, floor } => write!(
                formatter,
                "staging version {candidate} rejected because minimum version floor is {floor}"
            ),
            Self::BreakingSchemaRequiresOptIn(version) => write!(
                formatter,
                "version {version} declares a breaking schema migration and requires explicit opt-in"
            ),
            Self::NoExecutableAvailable(component) => write!(
                formatter,
                "no managed slot or fallback binary is configured for `{component}`"
            ),
            Self::NoPreviousVersion(component) => write!(
                formatter,
                "no previous release version is recorded for `{component}`"
            ),
            Self::MissingSlotBinary { path } => {
                write!(
                    formatter,
                    "expected slot binary is missing at {}",
                    path.display()
                )
            }
            Self::SlotIntegrityFailed { version, detail } => {
                write!(formatter, "slot {version} integrity check failed: {detail}")
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "supervisor slot I/O failed at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SlotError {}

impl From<ManifestError> for SlotError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

/// Manages versioned A/B slot directories and `supervisor-state.json` under one root path.
#[derive(Debug, Clone)]
pub struct SlotStore {
    root: PathBuf,
}

impl SlotStore {
    /// Creates a slot store rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the root directory managed by this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the path to `supervisor-state.json`.
    #[must_use]
    pub fn state_path(&self) -> PathBuf {
        self.root.join(STATE_FILE_NAME)
    }

    /// Returns the immutable directory path for `component` at `version`.
    #[must_use]
    pub fn slot_dir(&self, component: ComponentKind, version: ReleaseVersion) -> PathBuf {
        self.root
            .join(component.slot_dir_name())
            .join(version.to_string())
    }

    /// Returns the executable path for `component` at `version` for the current host OS.
    #[must_use]
    pub fn slot_binary_path(&self, component: ComponentKind, version: ReleaseVersion) -> PathBuf {
        self.slot_dir(component, version)
            .join(component.current_binary_file_name())
    }

    /// Loads `supervisor-state.json`, returning default empty state if absent.
    ///
    /// Does not create `root` on disk when the directory does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if the file is unreadable, oversized, malformed,
    /// or uses an unsupported schema version.
    pub fn load_state(&self) -> Result<SupervisorState, SlotError> {
        if !self.root.exists() {
            return Ok(SupervisorState::default());
        }
        let _lock = StateFileLock::acquire(&self.root)?;
        self.load_state_unlocked()
    }

    /// Removes leftover `.staging-*` directories left by interrupted staging operations.
    pub fn cleanup_stale_staging_dirs(&self, component: ComponentKind) {
        let base = self.root.join(component.slot_dir_name());
        let Ok(entries) = fs::read_dir(&base) else {
            return;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if file_name
                .to_str()
                .is_some_and(|name| name.starts_with(".staging-"))
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Persists `state` atomically to `supervisor-state.json`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if serialization, syncing, or atomic rename fails.
    pub fn save_state(&self, state: &SupervisorState) -> Result<(), SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        self.save_state_unlocked(state)
    }

    /// Verifies a signed manifest and binary, copies it into an immutable slot, and marks it pending.
    ///
    /// Verification runs against the staged copy inside the slot directory before
    /// publishing `supervisor-state.json`, eliminating verify-then-copy TOCTOU gaps.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] on signature, digest, expiry, floor, quarantine,
    /// immutability, breaking-schema policy, or filesystem errors without mutating state.
    pub fn stage_signed_binary(
        &self,
        manifest: &SignedReleaseManifest,
        source_binary: &Path,
        trusted_keys: &[TrustedSigningKey],
        options: &StageOptions,
    ) -> Result<SupervisorState, SlotError> {
        verify_manifest(
            manifest,
            trusted_keys,
            &options.expected_platform,
            options.now_unix_secs,
        )?;
        let payload = &manifest.payload;
        let slot_dir = self.slot_dir(payload.component, payload.version);
        {
            let _lock = StateFileLock::acquire(&self.root)?;
            let current = self.load_state_unlocked()?;
            validate_stage_policy(current.component(payload.component), payload, options)?;
        }

        let staging_dir = self.create_staging_dir(payload.component, payload.version)?;
        let staged_binary = staging_dir.join(&payload.binary_name);
        if let Err(error) = copy_and_verify_staged(
            source_binary,
            &staged_binary,
            manifest,
            trusted_keys,
            options,
        ) {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }
        let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(SlotError::ParseState)?;
        if let Err(error) = write_atomically(&staging_dir.join(MANIFEST_FILE_NAME), &manifest_bytes)
        {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        if let Err(error) =
            validate_stage_policy(state.component(payload.component), payload, options)
        {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }
        publish_unreferenced_slot(&staging_dir, &slot_dir)?;

        let slot = state.component_mut(payload.component);
        let max_attempts = options.max_boot_attempts.max(1);
        slot.pending = Some(PendingUpgrade {
            version: payload.version,
            staged_at_unix_secs: options.now_unix_secs,
            boot_attempts: 0,
            max_boot_attempts: max_attempts,
            schema_compatibility: payload.schema_compatibility,
        });
        self.save_state_unlocked(&state)?;
        Ok(state)
    }

    /// Selects the next binary to spawn, verifies its stored digest against its slot manifest for corruption detection, and records pending attempts.
    ///
    /// Offline Ed25519 signature verification runs during [`Self::stage_signed_binary`].
    /// If a `pending` version exhausted `max_boot_attempts` or fails pre-launch
    /// corruption checks, this method quarantines it and falls back to `active`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if no managed slot or fallback executable exists,
    /// or if the active slot fails pre-launch corruption verification.
    pub fn prepare_launch(
        &self,
        component: ComponentKind,
        fallback_binary: Option<&Path>,
    ) -> Result<LaunchPlan, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        if let Some(plan) = self.try_prepare_pending(&mut state, component)? {
            return Ok(plan);
        }
        if let Some(active) = state.component(component).active {
            let binary_path = self.verify_published_slot_binary(component, active)?;
            return Ok(LaunchPlan {
                component,
                binary_path,
                version: Some(active),
                is_pending_probation: false,
            });
        }
        if let Some(fallback) = fallback_binary {
            if !fallback.is_file() {
                return Err(SlotError::MissingSlotBinary {
                    path: fallback.to_path_buf(),
                });
            }
            return Ok(LaunchPlan {
                component,
                binary_path: fallback.to_path_buf(),
                version: None,
                is_pending_probation: false,
            });
        }
        Err(SlotError::NoExecutableAvailable(component))
    }

    /// Promotes a healthy pending upgrade to `active` and advances `min_version_floor`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] when loading or saving `supervisor-state.json` fails.
    pub fn commit_healthy(
        &self,
        component: ComponentKind,
        version: ReleaseVersion,
    ) -> Result<SupervisorState, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let slot = state.component_mut(component);
        let promoted = slot
            .pending
            .as_ref()
            .is_some_and(|pending| pending.version == version);
        if promoted {
            if let Some(previous_active) = slot.active.filter(|&current| current != version) {
                slot.previous = Some(previous_active);
            }
            slot.active = Some(version);
            slot.pending = None;
            let next_floor = slot
                .min_version_floor
                .map_or(version, |floor| floor.max(version));
            slot.min_version_floor = Some(next_floor);
            assert!(
                slot.min_version_floor >= slot.active,
                "minimum version floor must never trail active version"
            );
            self.save_state_unlocked(&state)?;
        }
        Ok(state)
    }

    /// Quarantines the current pending upgrade immediately and retains the active fallback.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] when loading or persisting state fails.
    pub fn rollback_failed_pending(
        &self,
        component: ComponentKind,
    ) -> Result<Option<RollbackOutcome>, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let slot = state.component_mut(component);
        let Some(pending) = slot.pending.take() else {
            return Ok(None);
        };
        quarantine_version(slot, pending.version);
        let fallback_active = slot.active;
        self.save_state_unlocked(&state)?;
        Ok(Some(RollbackOutcome {
            quarantined_version: pending.version,
            fallback_active,
        }))
    }

    /// Explicitly rolls `active` back to `previous` for `component`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError::NoPreviousVersion`] when no previous version exists
    /// or [`SlotError`] when the previous slot binary fails integrity verification.
    pub fn manual_rollback(&self, component: ComponentKind) -> Result<SupervisorState, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let previous = state
            .component(component)
            .previous
            .ok_or(SlotError::NoPreviousVersion(component))?;
        let _ = self.verify_published_slot_binary(component, previous)?;
        let slot = state.component_mut(component);
        let displaced = slot.active;
        slot.active = Some(previous);
        slot.previous = displaced.filter(|&version| version != previous);
        slot.pending = None;
        self.save_state_unlocked(&state)?;
        Ok(state)
    }

    fn try_prepare_pending(
        &self,
        state: &mut SupervisorState,
        component: ComponentKind,
    ) -> Result<Option<LaunchPlan>, SlotError> {
        let Some(pending) = state.component(component).pending.clone() else {
            return Ok(None);
        };
        if pending.boot_attempts < pending.max_boot_attempts
            && let Ok(binary_path) = self.verify_published_slot_binary(component, pending.version)
        {
            let slot = state.component_mut(component);
            if let Some(pending_mut) = slot.pending.as_mut() {
                pending_mut.boot_attempts = pending_mut.boot_attempts.saturating_add(1);
                assert!(
                    pending_mut.boot_attempts <= pending_mut.max_boot_attempts,
                    "pending boot attempts exceeded configured limit"
                );
            }
            self.save_state_unlocked(state)?;
            return Ok(Some(LaunchPlan {
                component,
                binary_path,
                version: Some(pending.version),
                is_pending_probation: true,
            }));
        }
        let slot = state.component_mut(component);
        quarantine_version(slot, pending.version);
        slot.pending = None;
        self.save_state_unlocked(state)?;
        Ok(None)
    }

    fn verify_published_slot_binary(
        &self,
        component: ComponentKind,
        version: ReleaseVersion,
    ) -> Result<PathBuf, SlotError> {
        let slot_dir = self.slot_dir(component, version);
        let binary_path = self.slot_binary_path(component, version);
        if !binary_path.is_file() {
            return Err(SlotError::MissingSlotBinary { path: binary_path });
        }
        let manifest_path = slot_dir.join(MANIFEST_FILE_NAME);
        let manifest =
            read_manifest_file(&manifest_path).map_err(|error| SlotError::SlotIntegrityFailed {
                version,
                detail: format!("cannot read slot manifest: {error}"),
            })?;
        if manifest.payload.component != component || manifest.payload.version != version {
            return Err(SlotError::SlotIntegrityFailed {
                version,
                detail: "slot manifest component or version mismatch".to_owned(),
            });
        }
        let (actual_bytes, actual_hex) =
            hash_binary_file(&binary_path).map_err(|error| SlotError::SlotIntegrityFailed {
                version,
                detail: error.to_string(),
            })?;
        if actual_bytes != manifest.payload.size_bytes || actual_hex != manifest.payload.sha256_hex
        {
            return Err(SlotError::SlotIntegrityFailed {
                version,
                detail: "slot executable digest does not match slot manifest".to_owned(),
            });
        }
        Ok(binary_path)
    }

    fn load_state_unlocked(&self) -> Result<SupervisorState, SlotError> {
        let path = self.state_path();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SupervisorState::default());
            }
            Err(source) => return Err(SlotError::Io { path, source }),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| SlotError::Io {
                path: path.clone(),
                source,
            })?;
        let read_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if read_len > MAX_STATE_FILE_BYTES {
            return Err(SlotError::StateTooLarge {
                size_bytes: read_len,
            });
        }
        let state: SupervisorState =
            serde_json::from_slice(&bytes).map_err(SlotError::ParseState)?;
        if state.schema_version != STATE_SCHEMA_VERSION_V1 {
            return Err(SlotError::UnsupportedStateSchema(state.schema_version));
        }
        Ok(state)
    }

    fn save_state_unlocked(&self, state: &SupervisorState) -> Result<(), SlotError> {
        let bytes = serde_json::to_vec_pretty(state).map_err(SlotError::ParseState)?;
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_len > MAX_STATE_FILE_BYTES {
            return Err(SlotError::StateTooLarge {
                size_bytes: byte_len,
            });
        }
        write_atomically(&self.state_path(), &bytes)
    }

    fn create_staging_dir(
        &self,
        component: ComponentKind,
        version: ReleaseVersion,
    ) -> Result<PathBuf, SlotError> {
        let base = self.root.join(component.slot_dir_name());
        fs::create_dir_all(&base).map_err(|source| SlotError::Io {
            path: base.clone(),
            source,
        })?;
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let staging = base.join(format!(
            ".staging-{version}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&staging).map_err(|source| SlotError::Io {
            path: staging.clone(),
            source,
        })?;
        Ok(staging)
    }
}

struct StateFileLock {
    _process_guard: MutexGuard<'static, ()>,
    _lock_file: File,
}

impl StateFileLock {
    fn acquire(root: &Path) -> Result<Self, SlotError> {
        let process_guard = STATE_IO_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fs::create_dir_all(root).map_err(|source| SlotError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let lock_path = root.join(STATE_LOCK_FILE_NAME);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| SlotError::Io {
                path: lock_path.clone(),
                source,
            })?;
        lock_file.lock().map_err(|source| SlotError::Io {
            path: lock_path,
            source,
        })?;
        Ok(Self {
            _process_guard: process_guard,
            _lock_file: lock_file,
        })
    }
}

fn validate_stage_policy(
    slot: &ComponentSlotState,
    payload: &crate::manifest::ReleaseManifestPayload,
    options: &StageOptions,
) -> Result<(), SlotError> {
    if slot.active == Some(payload.version) {
        return Err(SlotError::VersionAlreadyActive(payload.version));
    }
    if slot.previous == Some(payload.version)
        || slot
            .pending
            .as_ref()
            .is_some_and(|pending| pending.version == payload.version)
    {
        return Err(SlotError::VersionAlreadyStaged(payload.version));
    }
    if slot.quarantined.contains(&payload.version) {
        return Err(SlotError::VersionQuarantined(payload.version));
    }
    if let Some(floor) = slot.min_version_floor
        && payload.version < floor
    {
        return Err(SlotError::DowngradeRejected {
            candidate: payload.version,
            floor,
        });
    }
    if payload.schema_compatibility == SchemaCompatibility::Breaking && !options.allow_breaking {
        return Err(SlotError::BreakingSchemaRequiresOptIn(payload.version));
    }
    Ok(())
}

fn copy_and_verify_staged(
    source_binary: &Path,
    staged_binary: &Path,
    manifest: &SignedReleaseManifest,
    trusted_keys: &[TrustedSigningKey],
    options: &StageOptions,
) -> Result<(), SlotError> {
    fs::copy(source_binary, staged_binary).map_err(|source| SlotError::Io {
        path: staged_binary.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staged_binary, fs::Permissions::from_mode(0o755)).map_err(
            |source| SlotError::Io {
                path: staged_binary.to_path_buf(),
                source,
            },
        )?;
    }
    verify_manifest_and_file(
        manifest,
        staged_binary,
        trusted_keys,
        &options.expected_platform,
        options.now_unix_secs,
    )?;
    Ok(())
}

fn publish_unreferenced_slot(staging_dir: &Path, slot_dir: &Path) -> Result<(), SlotError> {
    // Only unreferenced orphan directories (from a crash between rename and state save) reach here.
    if slot_dir.exists() {
        fs::remove_dir_all(slot_dir).map_err(|source| SlotError::Io {
            path: slot_dir.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(staging_dir, slot_dir) {
        let _ = fs::remove_dir_all(staging_dir);
        return Err(SlotError::Io {
            path: slot_dir.to_path_buf(),
            source,
        });
    }
    sync_parent_directory(slot_dir);
    Ok(())
}

fn quarantine_version(slot: &mut ComponentSlotState, version: ReleaseVersion) {
    if slot.quarantined.contains(&version) {
        return;
    }
    if slot.quarantined.len() >= MAX_QUARANTINED_VERSIONS {
        // Evict an entry already blocked by `min_version_floor` before evicting newer versions.
        let evict_index = slot
            .min_version_floor
            .and_then(|floor| slot.quarantined.iter().position(|&item| item < floor))
            .unwrap_or(0);
        slot.quarantined.remove(evict_index);
    }
    slot.quarantined.push(version);
    assert!(
        slot.quarantined.len() <= MAX_QUARANTINED_VERSIONS,
        "quarantined version list exceeded maximum capacity"
    );
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), SlotError> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| SlotError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(STATE_FILE_NAME);
    let temp_path = parent.join(format!(
        ".{file_name}.{}-{sequence}.tmp",
        std::process::id()
    ));
    let mut file = File::create(&temp_path).map_err(|source| SlotError::Io {
        path: temp_path.clone(),
        source,
    })?;
    if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(SlotError::Io {
            path: temp_path,
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(SlotError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    sync_parent_directory(path);
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty())
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
const fn sync_parent_directory(_path: &Path) {}
