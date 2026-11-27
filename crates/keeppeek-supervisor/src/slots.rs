//! Content-addressed immutable A/B executable slots, GC, and atomic crash-recovery journal.
//!
//! Layout under a supervisor root directory:
//! - `supervisor-state.json`: atomic slot, per-channel floor, ETag, and probation state (schema v2).
//! - `supervisor-state.lock`: cross-process OS lock serializing state updates.
//! - `versions/<slot-id>/keeppeek[.exe]` and `manifest.json` (or `local-slot.json`).
//! - `vision/<slot-id>/keeppeek-vision[.exe]` and `manifest.json` (or `local-slot.json`).

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::acl::create_private_dir_all;
use crate::manifest::{
    ComponentKind, DEFAULT_STORAGE_SCHEMA, LOCAL_SLOT_META_SCHEMA_V1, MAX_BINARY_SIZE_BYTES,
    ManifestError, SchemaCompatibility, SignedReleaseManifest, TrustedSigningKey,
    UnsignedSlotMetadata, current_platform, encode_hex, hash_binary_file, pinned_keys_for_channel,
    read_manifest_file, read_unsigned_slot_metadata, verify_manifest, verify_manifest_and_file,
    verify_manifest_signature,
};
use crate::version::{
    MainBuildFloor, ReleaseVersion, SlotId, UpdateChannel, current_supervisor_version,
};

/// Supported on-disk schema version for `supervisor-state.json`.
pub const STATE_SCHEMA_VERSION_V2: u32 = 2;

/// Maximum serialized byte size allowed for `supervisor-state.json`.
pub const MAX_STATE_FILE_BYTES: u64 = 64 * 1024;

/// Maximum number of quarantined slot identifiers retained per component.
pub const MAX_QUARANTINED_SLOTS: usize = 32;

/// Default maximum boot attempts allowed for a pending slot before rollback.
pub const DEFAULT_MAX_BOOT_ATTEMPTS: u32 = 3;

/// Maximum unreferenced slots retained per component in addition to active/previous/pending.
pub const MAX_RETAINED_UNREFERENCED_SLOTS: usize = 2;

const STATE_FILE_NAME: &str = "supervisor-state.json";
const STATE_LOCK_FILE_NAME: &str = "supervisor-state.lock";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const LOCAL_SLOT_META_FILE_NAME: &str = "local-slot.json";
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const COPY_SHARE_RETRY_ATTEMPTS: usize = 8;
const COPY_SHARE_RETRY_BACKOFF_MS: u64 = 100;

static STATE_IO_LOCK: Mutex<()> = Mutex::new(());
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persisted crash-recovery and A/B slot state across core and Vision components (schema v2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorState {
    /// State schema version (must be [`STATE_SCHEMA_VERSION_V2`]).
    pub schema_version: u32,
    /// Slot state for the main `keeppeek` executable.
    pub core: ComponentSlotState,
    /// Slot state for the optional `keeppeek-vision` executable.
    pub vision: ComponentSlotState,
}

impl Default for SupervisorState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION_V2,
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

/// Persisted HTTP caching and last-observed channel metadata for remote update checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelFetchCache {
    /// Cached GitHub API `ETag` header for `release` checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_etag: Option<String>,
    /// Cached GitHub API `ETag` header for `main` (`master`) checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_etag: Option<String>,
    /// Unix timestamp in seconds of the most recent remote check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_unix_secs: Option<u64>,
    /// Last remote channel observed so explicit channel switches are logged cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_channel: Option<UpdateChannel>,
}

impl ChannelFetchCache {
    /// Returns the stored ETag for `channel`, if any.
    #[must_use]
    pub fn etag_for(&self, channel: UpdateChannel) -> Option<&str> {
        match channel {
            UpdateChannel::Release => self.release_etag.as_deref(),
            UpdateChannel::Main => self.main_etag.as_deref(),
            UpdateChannel::Local => None,
        }
    }

    /// Stores `etag` and check timestamp for `channel`.
    pub fn record_check(
        &mut self,
        channel: UpdateChannel,
        etag: Option<String>,
        now_unix_secs: u64,
    ) {
        match channel {
            UpdateChannel::Release => self.release_etag = etag,
            UpdateChannel::Main => self.main_etag = etag,
            UpdateChannel::Local => {}
        }
        self.last_checked_unix_secs = Some(now_unix_secs);
        self.last_channel = Some(channel);
    }
}

/// Persisted A/B slot pointers, per-channel floors, and probation counters for one component.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSlotState {
    /// Currently promoted healthy slot identifier, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<SlotId>,
    /// Last known-good slot identifier prior to the current active slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<SlotId>,
    /// Newly staged slot awaiting readiness and probation promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingUpgrade>,
    /// Monotonic `release` channel semantic version floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_floor: Option<ReleaseVersion>,
    /// Monotonic `main` channel `(version, build_seq)` floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_floor: Option<MainBuildFloor>,
    /// Highest storage schema revision committed by a signed slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_storage_schema: Option<u32>,
    /// Bounded list of content-addressed slot IDs quarantined after failed probation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantined: Vec<SlotId>,
    /// Remote channel fetch cache metadata.
    #[serde(default)]
    pub fetch_cache: ChannelFetchCache,
}

/// Tracks a staged upgrade currently undergoing launch probation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUpgrade {
    /// Content-addressed target slot identifier.
    pub slot: SlotId,
    /// Provenance channel of this pending slot.
    pub channel: UpdateChannel,
    /// Package semantic version of the staged executable.
    pub version: ReleaseVersion,
    /// CI build sequence (`0` for `release` or `local`, positive for `main`).
    pub build_seq: u64,
    /// Storage schema revision declared in a signed manifest (defaults to 1 for local).
    pub storage_schema: u32,
    /// Unix timestamp in seconds when the slot was staged.
    pub staged_at_unix_secs: u64,
    /// Number of boot attempts recorded so far for this pending slot.
    pub boot_attempts: u32,
    /// Maximum boot attempts allowed before automatic quarantine and rollback.
    pub max_boot_attempts: u32,
    /// Schema compatibility declared for this pending slot.
    pub schema_compatibility: SchemaCompatibility,
}

/// Options controlling staging of a signed `release` or `main` binary.
#[derive(Debug, Clone)]
pub struct StageOptions {
    /// Expected manifest channel (`release` or `main`).
    pub expected_channel: UpdateChannel,
    /// Host `<os>-<arch>` platform expected in the manifest.
    pub expected_platform: String,
    /// Installed supervisor version used to enforce same-major in-place compatibility.
    pub supervisor_version: ReleaseVersion,
    /// Whole-second Unix timestamp used for expiry validation.
    pub now_unix_secs: u64,
    /// Allows staging manifests that declare breaking or increasing storage schemas.
    pub allow_breaking: bool,
    /// Maximum boot attempts granted to the staged slot during probation.
    pub max_boot_attempts: u32,
}

impl StageOptions {
    /// Constructs default staging options for `channel` on the current host.
    #[must_use]
    pub fn for_current_host(channel: UpdateChannel, now_unix_secs: u64) -> Self {
        Self {
            expected_channel: channel,
            expected_platform: current_platform(),
            supervisor_version: current_supervisor_version(),
            now_unix_secs,
            allow_breaking: false,
            max_boot_attempts: DEFAULT_MAX_BOOT_ATTEMPTS,
        }
    }
}

/// Policy controlling which slots a supervisor host is allowed to verify and launch.
#[derive(Debug, Clone)]
pub struct LaunchPolicy {
    /// Permits executing unsigned `local-*` developer slots (CLI `--allow-local` only).
    pub allow_local: bool,
    /// Permits executing explicitly staged unsigned `main-*` SHA-256 slots.
    pub allow_unsigned_main: bool,
    /// Expected host `<os>-<arch>` platform for signed manifest verification.
    pub expected_platform: String,
    /// Trusted Ed25519 keys for `release` slots (defaults to compiled-in keys).
    pub release_keys: Vec<TrustedSigningKey>,
    /// Trusted Ed25519 keys for `main` slots (defaults to compiled-in keys).
    pub main_keys: Vec<TrustedSigningKey>,
}

impl Default for LaunchPolicy {
    fn default() -> Self {
        Self {
            allow_local: false,
            allow_unsigned_main: false,
            expected_platform: current_platform(),
            release_keys: pinned_keys_for_channel(UpdateChannel::Release),
            main_keys: pinned_keys_for_channel(UpdateChannel::Main),
        }
    }
}

/// Options for explicitly staging an unsigned `main` binary verified against a SHA-256 checksum.
#[derive(Debug, Clone)]
pub struct UnsignedMainStageOptions<'a> {
    /// Supervised component being staged.
    pub component: ComponentKind,
    /// Package semantic version of the artifact.
    pub version: ReleaseVersion,
    /// Monotonic CI build sequence number.
    pub build_seq: u64,
    /// Expected lowercase 64-character SHA-256 hex digest from the `.sha256` asset.
    pub expected_sha256_hex: &'a str,
    /// Installed supervisor version used for same-major compatibility checks.
    pub supervisor_version: ReleaseVersion,
    /// Whole-second Unix timestamp when staging occurs.
    pub now_unix_secs: u64,
    /// Maximum boot attempts allowed during probation.
    pub max_boot_attempts: u32,
}

struct UnsignedSlotSpec<'a> {
    component: ComponentKind,
    channel: UpdateChannel,
    version: ReleaseVersion,
    build_seq: Option<u64>,
    expected_sha256_hex: Option<&'a str>,
    now_unix_secs: u64,
    max_boot_attempts: u32,
}

/// Result returned after staging a signed, unsigned-main, or local slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageOutcome {
    /// Content-addressed slot identifier selected or reused.
    pub slot_id: SlotId,
    /// Updated persisted supervisor state.
    pub state: SupervisorState,
    /// False when the identical binary digest was already active or pending (idempotent no-op).
    pub changed: bool,
}

/// Resolved executable launch plan prepared by [`SlotStore::prepare_launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Component being launched.
    pub component: ComponentKind,
    /// Resolved executable path on disk.
    pub binary_path: PathBuf,
    /// Managed slot identifier, or `None` when using an unmanaged fallback binary.
    pub slot_id: Option<SlotId>,
    /// Semantic version of the managed slot, or `None` for fallback binaries.
    pub version: Option<ReleaseVersion>,
    /// True when this launch is probing a `pending` upgrade slot.
    pub is_pending_probation: bool,
}

/// Outcome returned when rolling back a failed pending upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    /// Pending slot identifier that was quarantined.
    pub quarantined_slot: SlotId,
    /// Fallback active slot remaining after rollback, if any.
    pub fallback_active: Option<SlotId>,
}

/// Errors produced while managing content-addressed A/B slots and the supervisor journal.
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
    /// Candidate requires a major version increment and fresh supervisor install.
    MajorUpgradeRequiresFreshInstall {
        current_major: u32,
        candidate: ReleaseVersion,
    },
    /// Target slot digest was previously quarantined after failing probation.
    SlotQuarantined(SlotId),
    /// Target release version is strictly lower than `release_floor`.
    ReleaseDowngradeRejected {
        candidate: ReleaseVersion,
        floor: ReleaseVersion,
    },
    /// Target `main` build is strictly lower than `main_floor`.
    MainDowngradeRejected {
        candidate: MainBuildFloor,
        floor: MainBuildFloor,
    },
    /// Manifest declares a breaking or increasing storage schema without explicit operator opt-in.
    BreakingSchemaRequiresOptIn(ReleaseVersion),
    /// Host policy forbids launching an unsigned `local` or unsigned `main` slot.
    ChannelDisallowedByHostPolicy {
        slot_id: SlotId,
        channel: UpdateChannel,
    },
    /// No managed slot or fallback executable exists for launch.
    NoExecutableAvailable(ComponentKind),
    /// No previous slot is available for manual rollback.
    NoPreviousSlot(ComponentKind),
    /// Expected executable file is missing inside a slot directory or fallback path.
    MissingSlotBinary { path: PathBuf },
    /// Pre-launch integrity or signature verification failed for a published slot directory.
    SlotIntegrityFailed { slot_id: SlotId, detail: String },
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
            Self::MajorUpgradeRequiresFreshInstall {
                current_major,
                candidate,
            } => write!(
                formatter,
                "candidate version {candidate} crosses major line {current_major}.x; upgrade the supervisor via a fresh install"
            ),
            Self::SlotQuarantined(slot_id) => {
                write!(
                    formatter,
                    "slot {slot_id} is quarantined after a failed boot"
                )
            }
            Self::ReleaseDowngradeRejected { candidate, floor } => write!(
                formatter,
                "staging release {candidate} rejected because release version floor is {floor}"
            ),
            Self::MainDowngradeRejected { candidate, floor } => write!(
                formatter,
                "staging main build {candidate} rejected because main channel floor is {floor}"
            ),
            Self::BreakingSchemaRequiresOptIn(version) => write!(
                formatter,
                "version {version} declares a breaking or higher storage schema and requires explicit --allow-breaking opt-in"
            ),
            Self::ChannelDisallowedByHostPolicy { slot_id, channel } => write!(
                formatter,
                "host policy forbids launching `{channel}` slot {slot_id}; start the host with explicit opt-in"
            ),
            Self::NoExecutableAvailable(component) => write!(
                formatter,
                "no managed slot or fallback binary is configured for `{component}`"
            ),
            Self::NoPreviousSlot(component) => write!(
                formatter,
                "no previous release slot is recorded for `{component}`"
            ),
            Self::MissingSlotBinary { path } => write!(
                formatter,
                "expected slot binary is missing at {}",
                path.display()
            ),
            Self::SlotIntegrityFailed { slot_id, detail } => {
                write!(formatter, "slot {slot_id} integrity check failed: {detail}")
            }
            Self::Io { path, source } => write!(
                formatter,
                "supervisor slot I/O failed at {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SlotError {}

impl From<ManifestError> for SlotError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

/// Manages content-addressed A/B slot directories and `supervisor-state.json` under one root path.
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

    /// Returns the immutable directory path for `component` at `slot_id`.
    #[must_use]
    pub fn slot_dir(&self, component: ComponentKind, slot_id: &SlotId) -> PathBuf {
        self.root
            .join(component.slot_dir_name())
            .join(slot_id.as_str())
    }

    /// Returns the executable path for `component` at `slot_id` for the current host OS.
    #[must_use]
    pub fn slot_binary_path(&self, component: ComponentKind, slot_id: &SlotId) -> PathBuf {
        self.slot_dir(component, slot_id)
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

    /// Persists `state` atomically to `supervisor-state.json`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if serialization, syncing, or atomic rename fails.
    pub fn save_state(&self, state: &SupervisorState) -> Result<(), SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        self.save_state_unlocked(state)
    }

    /// Updates remote channel ETag and check timestamp atomically in `supervisor-state.json`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if loading or persisting state fails.
    pub fn record_fetch_check(
        &self,
        component: ComponentKind,
        channel: UpdateChannel,
        etag: Option<String>,
        now_unix_secs: u64,
    ) -> Result<(), SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        state
            .component_mut(component)
            .fetch_cache
            .record_check(channel, etag, now_unix_secs);
        self.save_state_unlocked(&state)
    }

    /// Removes stale `.staging-*` directories and `.download-*.tmp` files older than 10 minutes.
    pub fn cleanup_stale_staging_dirs(&self, component: ComponentKind) {
        const STALE_AGE: Duration = Duration::from_secs(10 * 60);
        let base = self.root.join(component.slot_dir_name());
        let Ok(entries) = fs::read_dir(&base) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let is_staging_dir = name.starts_with(".staging-");
            let is_download_tmp = name.starts_with(".download-") && name.ends_with(".tmp");
            if !is_staging_dir && !is_download_tmp {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= STALE_AGE);
            if !stale {
                continue;
            }
            if is_staging_dir {
                let _ = fs::remove_dir_all(entry.path());
            } else {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Verifies a signed Manifest v2 and binary, copies it into a content-addressed slot, and marks it pending.
    ///
    /// Same-digest staging is an idempotent no-op. Automatically runs bounded slot GC after staging.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] on signature, channel, major-line, digest, expiry,
    /// floor, quarantine, breaking-schema policy, or filesystem errors without mutating state.
    pub fn stage_signed_binary(
        &self,
        manifest: &SignedReleaseManifest,
        source_binary: &Path,
        trusted_keys: &[TrustedSigningKey],
        options: &StageOptions,
    ) -> Result<StageOutcome, SlotError> {
        verify_manifest(
            manifest,
            trusted_keys,
            options.expected_channel,
            &options.expected_platform,
            options.now_unix_secs,
        )?;
        let payload = &manifest.payload;
        let build_seq = (payload.channel == UpdateChannel::Main).then_some(payload.build_seq);
        let slot_id = SlotId::new(
            payload.channel,
            payload.version,
            build_seq,
            &payload.sha256_hex,
        )
        .map_err(|err| {
            SlotError::Manifest(ManifestError::InvalidField {
                field: "sha256_hex",
                detail: err.to_string(),
            })
        })?;

        {
            let _lock = StateFileLock::acquire(&self.root)?;
            let current = self.load_state_unlocked()?;
            let comp = current.component(payload.component);
            if is_already_active_or_pending(comp, &slot_id) {
                return Ok(StageOutcome {
                    slot_id,
                    state: current,
                    changed: false,
                });
            }
            validate_signed_stage_policy(comp, payload, &slot_id, options)?;
        }

        let staging_dir = self.create_staging_dir(payload.component, &slot_id)?;
        let staged_binary = staging_dir.join(&payload.binary_name);
        if let Err(error) = copy_and_verify_signed_staged(
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
        let comp = state.component(payload.component);
        if is_already_active_or_pending(comp, &slot_id) {
            let _ = fs::remove_dir_all(&staging_dir);
            return Ok(StageOutcome {
                slot_id,
                state,
                changed: false,
            });
        }
        if let Err(error) = validate_signed_stage_policy(comp, payload, &slot_id, options) {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }
        publish_content_addressed_slot(
            &staging_dir,
            &self.slot_dir(payload.component, &slot_id),
            comp.previous.as_ref() == Some(&slot_id),
        )?;

        let slot = state.component_mut(payload.component);
        slot.pending = Some(PendingUpgrade {
            slot: slot_id.clone(),
            channel: payload.channel,
            version: payload.version,
            build_seq: payload.build_seq,
            storage_schema: payload.storage_schema,
            staged_at_unix_secs: options.now_unix_secs,
            boot_attempts: 0,
            max_boot_attempts: options.max_boot_attempts.max(1),
            schema_compatibility: payload.schema_compatibility,
        });
        self.save_state_unlocked(&state)?;
        self.gc_unreferenced_slots(payload.component, state.component(payload.component));
        Ok(StageOutcome {
            slot_id,
            state,
            changed: true,
        })
    }

    /// Stages an unsigned local developer build into a content-addressed `local-*` slot.
    ///
    /// Copies the executable with bounded Windows sharing-violation retries, computes
    /// SHA-256 on the staged copy, skips work idempotently when the identical digest
    /// is already active or pending, never advances signed floors, and runs bounded GC.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if reading/copying fails or if that exact digest is quarantined.
    pub fn stage_local_binary(
        &self,
        component: ComponentKind,
        source_binary: &Path,
        version: ReleaseVersion,
        now_unix_secs: u64,
        max_boot_attempts: u32,
    ) -> Result<StageOutcome, SlotError> {
        let spec = UnsignedSlotSpec {
            component,
            channel: UpdateChannel::Local,
            version,
            build_seq: None,
            expected_sha256_hex: None,
            now_unix_secs,
            max_boot_attempts,
        };
        self.stage_unsigned_slot(source_binary, &spec)
    }

    /// Stages an explicitly opted-in unsigned `main` binary verified against an expected SHA-256 hex digest.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] on digest mismatch, quarantine, major-line mismatch, or I/O failure.
    pub fn stage_unsigned_main_binary(
        &self,
        source_binary: &Path,
        options: &UnsignedMainStageOptions<'_>,
    ) -> Result<StageOutcome, SlotError> {
        if options.component == ComponentKind::Core
            && !options
                .supervisor_version
                .supports_in_place_upgrade_to(options.version)
        {
            return Err(SlotError::MajorUpgradeRequiresFreshInstall {
                current_major: options.supervisor_version.major,
                candidate: options.version,
            });
        }
        let spec = UnsignedSlotSpec {
            component: options.component,
            channel: UpdateChannel::Main,
            version: options.version,
            build_seq: Some(options.build_seq),
            expected_sha256_hex: Some(options.expected_sha256_hex),
            now_unix_secs: options.now_unix_secs,
            max_boot_attempts: options.max_boot_attempts,
        };
        self.stage_unsigned_slot(source_binary, &spec)
    }

    /// Selects the next binary to spawn, verifies its slot authenticity/digest under `policy`, and records pending attempts.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] if no managed slot or fallback executable exists, or
    /// if the active slot fails policy, signature, or digest verification.
    pub fn prepare_launch(
        &self,
        component: ComponentKind,
        fallback_binary: Option<&Path>,
        policy: &LaunchPolicy,
    ) -> Result<LaunchPlan, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        if let Some(plan) = self.try_prepare_pending(&mut state, component, policy)? {
            return Ok(plan);
        }
        if let Some(active) = state.component(component).active.clone() {
            let binary_path = self.verify_published_slot_binary(component, &active, policy)?;
            return Ok(LaunchPlan {
                component,
                binary_path,
                version: Some(active.version()),
                slot_id: Some(active),
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
                slot_id: None,
                version: None,
                is_pending_probation: false,
            });
        }
        Err(SlotError::NoExecutableAvailable(component))
    }

    /// Promotes a healthy pending slot to `active` and advances the channel-specific floor.
    ///
    /// `local` slots never advance `release_floor` or `main_floor`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError`] when loading or saving `supervisor-state.json` fails.
    pub fn commit_healthy(
        &self,
        component: ComponentKind,
        slot_id: &SlotId,
    ) -> Result<SupervisorState, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let slot = state.component_mut(component);
        let pending_match = slot
            .pending
            .as_ref()
            .filter(|pending| &pending.slot == slot_id)
            .cloned();
        if let Some(pending) = pending_match {
            if let Some(previous_active) = slot
                .active
                .as_ref()
                .filter(|current| *current != slot_id)
                .cloned()
            {
                slot.previous = Some(previous_active);
            }
            slot.active = Some(pending.slot.clone());
            slot.pending = None;
            // Only cryptographically signed slots may advance anti-rollback floors; unsigned
            // `main` or `local` slots must never pin `main_floor` above CI's signed build_seq.
            let is_signed = self
                .slot_dir(component, &pending.slot)
                .join(MANIFEST_FILE_NAME)
                .is_file();
            if is_signed {
                advance_channel_floors(slot, &pending);
            }
            self.save_state_unlocked(&state)?;
            self.gc_unreferenced_slots(component, state.component(component));
        }
        Ok(state)
    }

    /// Quarantines the current pending slot immediately and retains the active fallback.
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
        quarantine_slot(slot, pending.slot.clone());
        let fallback_active = slot.active.clone();
        self.save_state_unlocked(&state)?;
        Ok(Some(RollbackOutcome {
            quarantined_slot: pending.slot,
            fallback_active,
        }))
    }

    /// Explicitly rolls `active` back to `previous` for `component` after verifying integrity under `policy`.
    ///
    /// # Errors
    ///
    /// Returns [`SlotError::NoPreviousSlot`] when no previous slot exists
    /// or [`SlotError`] when the previous slot fails policy or integrity checks.
    pub fn manual_rollback(
        &self,
        component: ComponentKind,
        policy: &LaunchPolicy,
    ) -> Result<SupervisorState, SlotError> {
        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let previous = state
            .component(component)
            .previous
            .clone()
            .ok_or(SlotError::NoPreviousSlot(component))?;
        let _ = self.verify_published_slot_binary(component, &previous, policy)?;
        let slot = state.component_mut(component);
        let displaced = slot.active.take();
        slot.active = Some(previous.clone());
        slot.previous = displaced.filter(|item| item != &previous);
        slot.pending = None;
        self.save_state_unlocked(&state)?;
        Ok(state)
    }

    fn stage_unsigned_slot(
        &self,
        source_binary: &Path,
        spec: &UnsignedSlotSpec<'_>,
    ) -> Result<StageOutcome, SlotError> {
        assert!(
            spec.channel != UpdateChannel::Release,
            "release channel must never be staged via unsigned path"
        );
        let staging_dir = self.create_temp_staging_dir(spec.component, spec.channel)?;
        let binary_name = spec.component.current_binary_file_name();
        let staged_binary = staging_dir.join(binary_name);
        let copy_result = copy_binary_with_retry(source_binary, &staged_binary);
        let (size_bytes, sha256_hex) = match copy_result {
            Ok(stats) => stats,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_dir);
                return Err(error);
            }
        };
        if let Some(expected) = spec.expected_sha256_hex
            && sha256_hex != expected
        {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(SlotError::Manifest(ManifestError::DigestMismatch {
                expected: expected.to_owned(),
                actual: sha256_hex,
            }));
        }
        let slot_id = SlotId::new(spec.channel, spec.version, spec.build_seq, &sha256_hex)
            .map_err(|err| {
                let _ = fs::remove_dir_all(&staging_dir);
                SlotError::Manifest(ManifestError::InvalidField {
                    field: "sha256_hex",
                    detail: err.to_string(),
                })
            })?;
        let meta = UnsignedSlotMetadata {
            schema_version: LOCAL_SLOT_META_SCHEMA_V1,
            channel: spec.channel,
            component: spec.component,
            version: spec.version,
            build_seq: spec.build_seq,
            binary_name: binary_name.to_owned(),
            sha256_hex,
            size_bytes,
            staged_at_unix_secs: spec.now_unix_secs,
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(SlotError::ParseState)?;
        if let Err(error) =
            write_atomically(&staging_dir.join(LOCAL_SLOT_META_FILE_NAME), &meta_bytes)
        {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        let _lock = StateFileLock::acquire(&self.root)?;
        let mut state = self.load_state_unlocked()?;
        let comp = state.component(spec.component);
        if is_already_active_or_pending(comp, &slot_id) {
            let _ = fs::remove_dir_all(&staging_dir);
            return Ok(StageOutcome {
                slot_id,
                state,
                changed: false,
            });
        }
        if comp.quarantined.contains(&slot_id) {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(SlotError::SlotQuarantined(slot_id));
        }
        publish_content_addressed_slot(
            &staging_dir,
            &self.slot_dir(spec.component, &slot_id),
            comp.previous.as_ref() == Some(&slot_id),
        )?;

        let slot = state.component_mut(spec.component);
        slot.pending = Some(PendingUpgrade {
            slot: slot_id.clone(),
            channel: spec.channel,
            version: spec.version,
            build_seq: spec.build_seq.unwrap_or(0),
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            staged_at_unix_secs: spec.now_unix_secs,
            boot_attempts: 0,
            max_boot_attempts: spec.max_boot_attempts.max(1),
            schema_compatibility: SchemaCompatibility::ExpandContract,
        });
        self.save_state_unlocked(&state)?;
        self.gc_unreferenced_slots(spec.component, state.component(spec.component));
        Ok(StageOutcome {
            slot_id,
            state,
            changed: true,
        })
    }

    fn try_prepare_pending(
        &self,
        state: &mut SupervisorState,
        component: ComponentKind,
        policy: &LaunchPolicy,
    ) -> Result<Option<LaunchPlan>, SlotError> {
        let Some(pending) = state.component(component).pending.clone() else {
            return Ok(None);
        };
        if pending.boot_attempts < pending.max_boot_attempts {
            match self.verify_published_slot_binary(component, &pending.slot, policy) {
                Ok(binary_path) => {
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
                        slot_id: Some(pending.slot),
                        version: Some(pending.version),
                        is_pending_probation: true,
                    }));
                }
                Err(
                    error
                    @ (SlotError::ChannelDisallowedByHostPolicy { .. } | SlotError::Io { .. }),
                ) => {
                    // Fail closed without quarantining when host policy disallows the channel or a transient I/O lock occurs.
                    return Err(error);
                }
                Err(_) => {}
            }
        }
        let slot = state.component_mut(component);
        quarantine_slot(slot, pending.slot);
        slot.pending = None;
        self.save_state_unlocked(state)?;
        Ok(None)
    }

    fn verify_published_slot_binary(
        &self,
        component: ComponentKind,
        slot_id: &SlotId,
        policy: &LaunchPolicy,
    ) -> Result<PathBuf, SlotError> {
        let slot_dir = self.slot_dir(component, slot_id);
        let binary_path = self.slot_binary_path(component, slot_id);
        if !binary_path.is_file() {
            return Err(SlotError::MissingSlotBinary { path: binary_path });
        }
        let manifest_path = slot_dir.join(MANIFEST_FILE_NAME);
        let local_meta_path = slot_dir.join(LOCAL_SLOT_META_FILE_NAME);

        match slot_id.channel() {
            UpdateChannel::Local => {
                if !policy.allow_local {
                    return Err(SlotError::ChannelDisallowedByHostPolicy {
                        slot_id: slot_id.clone(),
                        channel: UpdateChannel::Local,
                    });
                }
                verify_unsigned_slot_files(component, slot_id, &local_meta_path, &binary_path)
            }
            UpdateChannel::Release => verify_signed_slot_files(
                component,
                slot_id,
                &manifest_path,
                &binary_path,
                &policy.release_keys,
                &policy.expected_platform,
            ),
            UpdateChannel::Main => {
                if manifest_path.is_file() {
                    verify_signed_slot_files(
                        component,
                        slot_id,
                        &manifest_path,
                        &binary_path,
                        &policy.main_keys,
                        &policy.expected_platform,
                    )
                } else if local_meta_path.is_file() {
                    if !policy.allow_unsigned_main {
                        return Err(SlotError::ChannelDisallowedByHostPolicy {
                            slot_id: slot_id.clone(),
                            channel: UpdateChannel::Main,
                        });
                    }
                    verify_unsigned_slot_files(component, slot_id, &local_meta_path, &binary_path)
                } else {
                    Err(SlotError::SlotIntegrityFailed {
                        slot_id: slot_id.clone(),
                        detail: "slot manifest is missing".to_owned(),
                    })
                }
            }
        }
    }

    fn gc_unreferenced_slots(&self, component: ComponentKind, slot: &ComponentSlotState) {
        let base = self.root.join(component.slot_dir_name());
        let Ok(entries) = fs::read_dir(&base) else {
            return;
        };
        let mut unreferenced: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Ok(candidate) = SlotId::from_str(name) else {
                continue;
            };
            let referenced = slot.active.as_ref() == Some(&candidate)
                || slot.previous.as_ref() == Some(&candidate)
                || slot
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.slot == candidate);
            if !referenced {
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                unreferenced.push((modified, entry.path()));
            }
        }
        if unreferenced.len() <= MAX_RETAINED_UNREFERENCED_SLOTS {
            return;
        }
        // Sort newest first so we preserve the most recent N unreferenced slots and remove older ones.
        unreferenced.sort_by_key(|item| std::cmp::Reverse(item.0));
        for (_, path) in unreferenced
            .into_iter()
            .skip(MAX_RETAINED_UNREFERENCED_SLOTS)
        {
            let _ = fs::remove_dir_all(path);
        }
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
        if state.schema_version != STATE_SCHEMA_VERSION_V2 {
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
        slot_id: &SlotId,
    ) -> Result<PathBuf, SlotError> {
        self.create_named_staging_dir(component, slot_id.as_str())
    }

    fn create_temp_staging_dir(
        &self,
        component: ComponentKind,
        channel: UpdateChannel,
    ) -> Result<PathBuf, SlotError> {
        self.create_named_staging_dir(component, channel.as_str())
    }

    fn create_named_staging_dir(
        &self,
        component: ComponentKind,
        label: &str,
    ) -> Result<PathBuf, SlotError> {
        create_private_dir_all(&self.root).map_err(|source| SlotError::Io {
            path: self.root.clone(),
            source,
        })?;
        let base = self.root.join(component.slot_dir_name());
        create_private_dir_all(&base).map_err(|source| SlotError::Io {
            path: base.clone(),
            source,
        })?;
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let staging = base.join(format!(
            ".staging-{label}-{}-{sequence}",
            std::process::id()
        ));
        create_private_dir_all(&staging).map_err(|source| SlotError::Io {
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
        create_private_dir_all(root).map_err(|source| SlotError::Io {
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

fn is_already_active_or_pending(slot: &ComponentSlotState, candidate: &SlotId) -> bool {
    slot.active.as_ref() == Some(candidate)
        || slot
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.slot == candidate)
}

fn validate_signed_stage_policy(
    slot: &ComponentSlotState,
    payload: &crate::manifest::ReleaseManifestPayload,
    slot_id: &SlotId,
    options: &StageOptions,
) -> Result<(), SlotError> {
    match payload.component {
        ComponentKind::Core => {
            if !options
                .supervisor_version
                .supports_in_place_upgrade_to(payload.version)
            {
                return Err(SlotError::MajorUpgradeRequiresFreshInstall {
                    current_major: options.supervisor_version.major,
                    candidate: payload.version,
                });
            }
        }
        ComponentKind::Vision => {
            if let Some(active) = slot.active.as_ref()
                && !active
                    .version()
                    .supports_in_place_upgrade_to(payload.version)
            {
                return Err(SlotError::MajorUpgradeRequiresFreshInstall {
                    current_major: active.version().major,
                    candidate: payload.version,
                });
            }
        }
    }
    if slot.quarantined.contains(slot_id) {
        return Err(SlotError::SlotQuarantined(slot_id.clone()));
    }
    match payload.channel {
        UpdateChannel::Release => {
            if let Some(floor) = slot.release_floor
                && payload.version < floor
            {
                return Err(SlotError::ReleaseDowngradeRejected {
                    candidate: payload.version,
                    floor,
                });
            }
        }
        UpdateChannel::Main => {
            let candidate = MainBuildFloor::new(payload.version, payload.build_seq);
            if let Some(floor) = slot.main_floor
                && candidate < floor
            {
                return Err(SlotError::MainDowngradeRejected { candidate, floor });
            }
        }
        UpdateChannel::Local => {}
    }
    let schema_bumped = slot
        .active_storage_schema
        .is_some_and(|current| payload.storage_schema > current);
    if (payload.schema_compatibility == SchemaCompatibility::Breaking || schema_bumped)
        && !options.allow_breaking
    {
        return Err(SlotError::BreakingSchemaRequiresOptIn(payload.version));
    }
    Ok(())
}

fn advance_channel_floors(slot: &mut ComponentSlotState, pending: &PendingUpgrade) {
    match pending.channel {
        UpdateChannel::Release => {
            let next = slot
                .release_floor
                .map_or(pending.version, |floor| floor.max(pending.version));
            slot.release_floor = Some(next);
            slot.active_storage_schema = Some(
                slot.active_storage_schema
                    .map_or(pending.storage_schema, |cur| {
                        cur.max(pending.storage_schema)
                    }),
            );
            assert!(
                slot.release_floor >= Some(pending.version),
                "release floor must never trail promoted release version"
            );
        }
        UpdateChannel::Main => {
            let candidate = MainBuildFloor::new(pending.version, pending.build_seq);
            let next = slot
                .main_floor
                .map_or(candidate, |floor| floor.max(candidate));
            slot.main_floor = Some(next);
            slot.active_storage_schema = Some(
                slot.active_storage_schema
                    .map_or(pending.storage_schema, |cur| {
                        cur.max(pending.storage_schema)
                    }),
            );
            assert!(
                slot.main_floor >= Some(candidate),
                "main floor must never trail promoted main build"
            );
        }
        UpdateChannel::Local => {}
    }
}

fn verify_signed_slot_files(
    component: ComponentKind,
    slot_id: &SlotId,
    manifest_path: &Path,
    binary_path: &Path,
    trusted_keys: &[TrustedSigningKey],
    expected_platform: &str,
) -> Result<PathBuf, SlotError> {
    let manifest =
        read_manifest_file(manifest_path).map_err(|error| SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: format!("cannot read slot manifest: {error}"),
        })?;
    verify_manifest_signature(
        &manifest,
        trusted_keys,
        slot_id.channel(),
        expected_platform,
    )
    .map_err(|error| SlotError::SlotIntegrityFailed {
        slot_id: slot_id.clone(),
        detail: format!("manifest signature verification failed: {error}"),
    })?;
    if manifest.payload.component != component || manifest.payload.version != slot_id.version() {
        return Err(SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: "slot manifest component or version mismatch".to_owned(),
        });
    }
    let expected_build_seq = match slot_id.channel() {
        UpdateChannel::Main => slot_id.build_seq().unwrap_or(0),
        UpdateChannel::Release | UpdateChannel::Local => 0,
    };
    if manifest.payload.build_seq != expected_build_seq {
        return Err(SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: format!(
                "slot manifest build_seq {} does not match slot identifier build_seq {expected_build_seq}",
                manifest.payload.build_seq
            ),
        });
    }
    if !manifest
        .payload
        .sha256_hex
        .starts_with(slot_id.digest_prefix())
    {
        return Err(SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: "slot manifest digest does not match slot identifier".to_owned(),
        });
    }
    verify_binary_digest(
        slot_id,
        binary_path,
        manifest.payload.size_bytes,
        &manifest.payload.sha256_hex,
    )?;
    Ok(binary_path.to_path_buf())
}

fn verify_unsigned_slot_files(
    component: ComponentKind,
    slot_id: &SlotId,
    meta_path: &Path,
    binary_path: &Path,
) -> Result<PathBuf, SlotError> {
    let meta =
        read_unsigned_slot_metadata(meta_path).map_err(|error| SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: format!("cannot read unsigned slot metadata: {error}"),
        })?;
    if meta.component != component
        || meta.channel != slot_id.channel()
        || meta.version != slot_id.version()
        || !meta.sha256_hex.starts_with(slot_id.digest_prefix())
    {
        return Err(SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: "unsigned slot metadata does not match slot identifier".to_owned(),
        });
    }
    verify_binary_digest(slot_id, binary_path, meta.size_bytes, &meta.sha256_hex)?;
    Ok(binary_path.to_path_buf())
}

fn verify_binary_digest(
    slot_id: &SlotId,
    binary_path: &Path,
    expected_bytes: u64,
    expected_hex: &str,
) -> Result<(), SlotError> {
    let (actual_bytes, actual_hex) =
        hash_binary_file(binary_path).map_err(|error| match error {
            ManifestError::Io(source) => SlotError::Io {
                path: binary_path.to_path_buf(),
                source,
            },
            other => SlotError::SlotIntegrityFailed {
                slot_id: slot_id.clone(),
                detail: other.to_string(),
            },
        })?;
    if actual_bytes != expected_bytes || actual_hex != expected_hex {
        return Err(SlotError::SlotIntegrityFailed {
            slot_id: slot_id.clone(),
            detail: "slot executable digest does not match recorded metadata".to_owned(),
        });
    }
    Ok(())
}

fn copy_and_verify_signed_staged(
    source_binary: &Path,
    staged_binary: &Path,
    manifest: &SignedReleaseManifest,
    trusted_keys: &[TrustedSigningKey],
    options: &StageOptions,
) -> Result<(), SlotError> {
    let _ = copy_binary_with_retry(source_binary, staged_binary)?;
    verify_manifest_and_file(
        manifest,
        staged_binary,
        trusted_keys,
        options.expected_channel,
        &options.expected_platform,
        options.now_unix_secs,
    )?;
    Ok(())
}

/// Copies a source binary with read-only sharing on Windows (retrying transient linker locks)
/// and returns `(size_bytes, sha256_hex)` computed over the copied bytes.
fn copy_binary_with_retry(source: &Path, dest: &Path) -> Result<(u64, String), SlotError> {
    let mut last_error: Option<std::io::Error> = None;
    for attempt in 0..COPY_SHARE_RETRY_ATTEMPTS {
        match open_source_read_only(source) {
            Ok(mut reader) => {
                let stats = stream_copy_and_hash(&mut reader, dest)?;
                set_executable_permissions(dest)?;
                return Ok(stats);
            }
            Err(error)
                if is_transient_share_error(&error) && attempt + 1 < COPY_SHARE_RETRY_ATTEMPTS =>
            {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(COPY_SHARE_RETRY_BACKOFF_MS));
            }
            Err(source_err) => {
                return Err(SlotError::Io {
                    path: source.to_path_buf(),
                    source: source_err,
                });
            }
        }
    }
    Err(SlotError::Io {
        path: source.to_path_buf(),
        source: last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "source binary remained locked by another writer",
            )
        }),
    })
}

fn open_source_read_only(source: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Omit FILE_SHARE_WRITE so we wait cleanly if a linker is still writing target/*.exe.
        const FILE_SHARE_READ_ONLY: u32 = 0x0000_0001;
        options.share_mode(FILE_SHARE_READ_ONLY);
    }
    options.open(source)
}

#[cfg(windows)]
fn is_transient_share_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(not(windows))]
const fn is_transient_share_error(_error: &std::io::Error) -> bool {
    false
}

fn stream_copy_and_hash(reader: &mut File, dest: &Path) -> Result<(u64, String), SlotError> {
    let mut writer = File::create(dest).map_err(|source| SlotError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut total_bytes: u64 = 0;

    loop {
        let read_count = reader.read(&mut buffer).map_err(|source| SlotError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        if read_count == 0 {
            break;
        }
        total_bytes = total_bytes
            .checked_add(u64::try_from(read_count).unwrap_or(u64::MAX))
            .ok_or(SlotError::Manifest(ManifestError::InvalidBinarySize {
                size_bytes: u64::MAX,
            }))?;
        if total_bytes > MAX_BINARY_SIZE_BYTES {
            let _ = fs::remove_file(dest);
            return Err(SlotError::Manifest(ManifestError::InvalidBinarySize {
                size_bytes: total_bytes,
            }));
        }
        hasher.update(&buffer[..read_count]);
        writer
            .write_all(&buffer[..read_count])
            .map_err(|source| SlotError::Io {
                path: dest.to_path_buf(),
                source,
            })?;
    }
    if total_bytes == 0 {
        let _ = fs::remove_file(dest);
        return Err(SlotError::Manifest(ManifestError::InvalidBinarySize {
            size_bytes: 0,
        }));
    }
    writer.sync_all().map_err(|source| SlotError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok((total_bytes, encode_hex(&hasher.finalize())))
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), SlotError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| SlotError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
const fn set_executable_permissions(_path: &Path) -> Result<(), SlotError> {
    Ok(())
}

fn publish_content_addressed_slot(
    staging_dir: &Path,
    slot_dir: &Path,
    is_referenced_previous: bool,
) -> Result<(), SlotError> {
    if slot_dir.exists() {
        if is_referenced_previous {
            let _ = fs::remove_dir_all(staging_dir);
            return Ok(());
        }
        // Replace any stale or previously corrupted unreferenced directory with the fresh verified staging copy.
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

fn quarantine_slot(slot: &mut ComponentSlotState, slot_id: SlotId) {
    if slot.quarantined.contains(&slot_id) {
        return;
    }
    if slot.quarantined.len() >= MAX_QUARANTINED_SLOTS {
        slot.quarantined.remove(0);
    }
    slot.quarantined.push(slot_id);
    assert!(
        slot.quarantined.len() <= MAX_QUARANTINED_SLOTS,
        "quarantined slot list exceeded maximum capacity"
    );
}

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), SlotError> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dir_all(parent).map_err(|source| SlotError::Io {
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
