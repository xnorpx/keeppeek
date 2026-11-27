use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keeppeek_supervisor::manifest::{
    ComponentKind, MANIFEST_SCHEMA_VERSION_V1, ManifestError, ReleaseManifestPayload,
    SchemaCompatibility, TrustedSigningKey, current_platform, hash_binary_file,
    public_key_from_seed, sign_manifest, verify_manifest_and_file,
};
use keeppeek_supervisor::slots::{SlotError, SlotStore, StageOptions};
use keeppeek_supervisor::supervisor::{
    ComponentConfig, ComponentPhase, SupervisorConfig, SupervisorHandle,
};
use keeppeek_supervisor::version::ReleaseVersion;
use keeppeek_supervisor::{
    ENV_ACTIVE_VERSION, ENV_COMPONENT, ENV_SUPERVISED, EXIT_CODE_RESTART_REQUESTED,
    signal_ready_from_env, stop_file_from_env, stop_requested,
};

const CHILD_MODE_ENV: &str = "KEEPPEEK_SUPERVISOR_TEST_CHILD_MODE";
const CHILD_SCENARIO_DIR_ENV: &str = "KEEPPEEK_SUPERVISOR_TEST_SCENARIO_DIR";

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock must be after UNIX_EPOCH")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-supervisor-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp directory must be creatable");
    dir
}

fn test_signing_material() -> ([u8; 32], TrustedSigningKey) {
    let seed = [0x42_u8; 32];
    let public_key_bytes = public_key_from_seed(&seed).expect("seed must derive public key");
    let trusted = TrustedSigningKey {
        key_id: "release-key-1".to_owned(),
        public_key_bytes,
    };
    (seed, trusted)
}

fn signed_manifest_for_file(
    component: ComponentKind,
    version: ReleaseVersion,
    binary_path: &Path,
    seed: &[u8; 32],
    schema_compatibility: SchemaCompatibility,
    expires_at_unix_secs: u64,
) -> keeppeek_supervisor::manifest::SignedReleaseManifest {
    let (size_bytes, sha256_hex) = hash_binary_file(binary_path).expect("binary must hash");
    let payload = ReleaseManifestPayload {
        schema_version: MANIFEST_SCHEMA_VERSION_V1,
        component,
        version,
        platform: current_platform(),
        binary_name: component.current_binary_file_name().to_owned(),
        sha256_hex,
        size_bytes,
        schema_compatibility,
        issued_at_unix_secs: 1_750_000_000,
        expires_at_unix_secs,
        signing_key_id: "release-key-1".to_owned(),
    };
    sign_manifest(payload, seed).expect("manifest must sign")
}

fn child_helper_args() -> Vec<OsString> {
    vec![
        OsString::from("--exact"),
        OsString::from("child_helper_entrypoint"),
        OsString::from("--nocapture"),
    ]
}

#[test]
fn child_helper_entrypoint() {
    let Ok(mode) = std::env::var(CHILD_MODE_ENV) else {
        return;
    };
    let version = std::env::var(ENV_ACTIVE_VERSION).unwrap_or_else(|_| "fallback".to_owned());
    let component = std::env::var(ENV_COMPONENT).unwrap_or_else(|_| "keeppeek".to_owned());
    let scenario_dir = std::env::var_os(CHILD_SCENARIO_DIR_ENV).map(PathBuf::from);
    let stop_file = stop_file_from_env();

    let effective_mode = scenario_dir
        .as_ref()
        .and_then(|dir| fs::read_to_string(dir.join(format!("{component}-{version}.mode"))).ok())
        .map_or(mode, |content| content.trim().to_owned());

    match effective_mode.as_str() {
        "crash_immediately" => std::process::exit(9),
        "hang_without_ready" => {
            while !stop_requested(stop_file.as_deref()) {
                thread::sleep(Duration::from_millis(20));
            }
            std::process::exit(0);
        }
        "ready_then_restart_once" => {
            assert_eq!(std::env::var(ENV_SUPERVISED).as_deref(), Ok("1"));
            signal_ready_from_env(&version).expect("ready marker must write");
            if let Some(dir) = scenario_dir.as_ref() {
                let marker = dir.join(format!("{component}-{version}.restarted"));
                if !marker.exists() {
                    fs::write(&marker, b"1").expect("marker must write");
                    thread::sleep(Duration::from_millis(120));
                    std::process::exit(i32::from(EXIT_CODE_RESTART_REQUESTED));
                }
            }
            while !stop_requested(stop_file.as_deref()) {
                thread::sleep(Duration::from_millis(20));
            }
            std::process::exit(0);
        }
        _ => {
            assert_eq!(std::env::var(ENV_SUPERVISED).as_deref(), Ok("1"));
            signal_ready_from_env(&version).expect("ready marker must write");
            let restart_trigger = scenario_dir
                .as_ref()
                .map(|dir| dir.join(format!("{component}-{version}.trigger_restart")));
            while !stop_requested(stop_file.as_deref()) {
                if let Some(trigger) = restart_trigger.as_ref()
                    && trigger.exists()
                {
                    let _ = fs::remove_file(trigger);
                    std::process::exit(i32::from(EXIT_CODE_RESTART_REQUESTED));
                }
                thread::sleep(Duration::from_millis(20));
            }
            std::process::exit(0);
        }
    }
}

#[test]
fn manifest_verification_enforces_signature_expiry_digest_platform_and_policy_bounds() {
    let dir = temp_dir("manifest-verify");
    let binary = dir.join("payload.bin");
    fs::write(&binary, b"keeppeek-signed-binary-content").unwrap();
    let (seed, trusted) = test_signing_material();
    let manifest = signed_manifest_for_file(
        ComponentKind::Core,
        ReleaseVersion::new(0, 2, 0),
        &binary,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );

    verify_manifest_and_file(
        &manifest,
        &binary,
        std::slice::from_ref(&trusted),
        &current_platform(),
        1_750_000_000,
    )
    .expect("valid signed manifest must verify");

    // Reject future-dated manifest beyond clock skew and unbounded validity windows.
    assert!(matches!(
        verify_manifest_and_file(
            &manifest,
            &binary,
            std::slice::from_ref(&trusted),
            &current_platform(),
            1_749_999_000,
        ),
        Err(ManifestError::ExpiredOrInvalidWindow { .. })
    ));
    let unbounded = signed_manifest_for_file(
        ComponentKind::Core,
        ReleaseVersion::new(0, 2, 0),
        &binary,
        &seed,
        SchemaCompatibility::ExpandContract,
        u64::MAX,
    );
    assert!(matches!(
        verify_manifest_and_file(
            &unbounded,
            &binary,
            std::slice::from_ref(&trusted),
            &current_platform(),
            1_750_000_000,
        ),
        Err(ManifestError::ExpiredOrInvalidWindow { .. })
    ));

    let mut bad_sig = manifest.clone();
    bad_sig.signature_hex = "00".repeat(64);
    assert!(matches!(
        verify_manifest_and_file(
            &bad_sig,
            &binary,
            std::slice::from_ref(&trusted),
            &current_platform(),
            1_750_000_000,
        ),
        Err(ManifestError::InvalidSignature)
    ));

    assert!(matches!(
        verify_manifest_and_file(&manifest, &binary, &[], &current_platform(), 1_750_000_000,),
        Err(ManifestError::UnknownSigningKey(_))
    ));

    assert!(matches!(
        verify_manifest_and_file(
            &manifest,
            &binary,
            std::slice::from_ref(&trusted),
            &current_platform(),
            1_800_000_001,
        ),
        Err(ManifestError::ExpiredOrInvalidWindow { .. })
    ));

    assert!(matches!(
        verify_manifest_and_file(
            &manifest,
            &binary,
            std::slice::from_ref(&trusted),
            "linux-riscv64",
            1_750_000_000,
        ),
        Err(ManifestError::PlatformMismatch { .. })
    ));

    let tampered = dir.join("tampered.bin");
    fs::write(&tampered, b"keeppeek-tampered-binary-xxxxx").unwrap();
    assert!(matches!(
        verify_manifest_and_file(
            &manifest,
            &tampered,
            std::slice::from_ref(&trusted),
            &current_platform(),
            1_750_000_000,
        ),
        Err(ManifestError::DigestMismatch { .. })
    ));

    let store = SlotStore::new(dir.join("supervisor"));
    let breaking = signed_manifest_for_file(
        ComponentKind::Core,
        ReleaseVersion::new(1, 0, 0),
        &binary,
        &seed,
        SchemaCompatibility::Breaking,
        1_760_000_000,
    );
    let options = StageOptions {
        expected_platform: current_platform(),
        now_unix_secs: 1_750_000_000,
        allow_breaking: false,
        max_boot_attempts: 2,
    };
    assert!(matches!(
        store.stage_signed_binary(&breaking, &binary, std::slice::from_ref(&trusted), &options),
        Err(SlotError::BreakingSchemaRequiresOptIn(v)) if v == ReleaseVersion::new(1, 0, 0)
    ));

    let allow_options = StageOptions {
        allow_breaking: true,
        ..options.clone()
    };
    let state = store
        .stage_signed_binary(
            &breaking,
            &binary,
            std::slice::from_ref(&trusted),
            &allow_options,
        )
        .expect("breaking manifest succeeds when explicitly allowed");
    assert_eq!(
        state.core.pending.as_ref().map(|p| p.version),
        Some(ReleaseVersion::new(1, 0, 0))
    );
    // Re-staging an already-pending immutable slot must be rejected without mutating attempts.
    assert!(matches!(
        store.stage_signed_binary(
            &breaking,
            &binary,
            std::slice::from_ref(&trusted),
            &allow_options,
        ),
        Err(SlotError::VersionAlreadyStaged(v)) if v == ReleaseVersion::new(1, 0, 0)
    ));
    store
        .commit_healthy(ComponentKind::Core, ReleaseVersion::new(1, 0, 0))
        .expect("commit must advance version floor");

    // On-disk tampering of a published active slot must fail pre-launch integrity verification.
    let active_bin = store.slot_binary_path(ComponentKind::Core, ReleaseVersion::new(1, 0, 0));
    fs::write(&active_bin, b"tampered-on-disk").unwrap();
    assert!(matches!(
        store.prepare_launch(ComponentKind::Core, None),
        Err(SlotError::SlotIntegrityFailed { version, .. }) if version == ReleaseVersion::new(1, 0, 0)
    ));

    let downgrade = signed_manifest_for_file(
        ComponentKind::Core,
        ReleaseVersion::new(0, 9, 0),
        &binary,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    assert!(matches!(
        store.stage_signed_binary(&downgrade, &binary, &[trusted], &options),
        Err(SlotError::DowngradeRejected { .. })
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn supervisor_promotes_healthy_pending_and_rolls_back_crashing_upgrade() {
    let dir = temp_dir("promote-rollback");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenarios");
    fs::create_dir_all(&scenario_dir).unwrap();
    let current_exe = std::env::current_exe().expect("test executable path must exist");
    let (seed, trusted) = test_signing_material();
    let store = SlotStore::new(&root);

    let v1 = ReleaseVersion::new(0, 2, 0);
    let m1 = signed_manifest_for_file(
        ComponentKind::Core,
        v1,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    let stage_opts = StageOptions {
        expected_platform: current_platform(),
        now_unix_secs: 1_750_000_000,
        allow_breaking: false,
        max_boot_attempts: 1,
    };
    store
        .stage_signed_binary(
            &m1,
            &current_exe,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .expect("v1 must stage");

    let mut config = SupervisorConfig::new(&root);
    config.poll_interval_ms = 20;
    config.core.args = child_helper_args();
    config.core.extra_env = vec![
        (CHILD_MODE_ENV.to_owned(), "healthy".to_owned()),
        (
            CHILD_SCENARIO_DIR_ENV.to_owned(),
            scenario_dir.display().to_string(),
        ),
    ];
    config.core.readiness_timeout_ms = 5_000;
    config.core.probation_ms = 120;
    config.core.stop_grace_ms = 2_000;

    let mut handle = SupervisorHandle::start(config).expect("supervisor must start");
    let running_v1 = handle
        .wait_until_running(ComponentKind::Core, 8_000)
        .expect("v1 must commit healthy");
    assert_eq!(running_v1.active_version, Some(v1));
    assert_eq!(running_v1.pending_version, None);
    assert!(running_v1.pid.is_some());

    let v2 = ReleaseVersion::new(0, 2, 1);
    fs::write(
        scenario_dir.join(format!("keeppeek-{v2}.mode")),
        "crash_immediately",
    )
    .unwrap();
    let m2 = signed_manifest_for_file(
        ComponentKind::Core,
        v2,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(&m2, &current_exe, &[trusted], &stage_opts)
        .expect("v2 must stage");

    assert!(handle.request_restart(ComponentKind::Core));
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let snap = handle.snapshot().core;
        if snap.rollback_count >= 1
            && snap.phase == ComponentPhase::Running
            && snap.running_version == Some(v1)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rollback to v1; latest snapshot: {snap:?}"
        );
        thread::sleep(Duration::from_millis(30));
    }

    handle.stop();
    let final_state = store.load_state().expect("state must load");
    assert_eq!(final_state.core.active, Some(v1));
    assert_eq!(final_state.core.pending, None);
    assert!(final_state.core.quarantined.contains(&v2));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn supervised_restart_exit_code_activates_staged_upgrade_and_vision_is_isolated() {
    let dir = temp_dir("restart-and-vision");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenarios");
    fs::create_dir_all(&scenario_dir).unwrap();
    let current_exe = std::env::current_exe().expect("test executable path must exist");
    let (seed, trusted) = test_signing_material();
    let store = SlotStore::new(&root);
    let stage_opts = StageOptions {
        expected_platform: current_platform(),
        now_unix_secs: 1_750_000_000,
        allow_breaking: false,
        max_boot_attempts: 1,
    };

    let core_v1 = ReleaseVersion::new(0, 3, 0);
    let core_v2 = ReleaseVersion::new(0, 3, 1);
    let vision_v1 = ReleaseVersion::new(0, 1, 0);
    let vision_v2 = ReleaseVersion::new(0, 1, 1);

    let m_core_v1 = signed_manifest_for_file(
        ComponentKind::Core,
        core_v1,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    let m_vision_v1 = signed_manifest_for_file(
        ComponentKind::Vision,
        vision_v1,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(
            &m_core_v1,
            &current_exe,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();
    store
        .stage_signed_binary(
            &m_vision_v1,
            &current_exe,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();

    let mut config = SupervisorConfig::new(&root);
    config.poll_interval_ms = 20;
    config.core.args = child_helper_args();
    config.core.extra_env = vec![
        (CHILD_MODE_ENV.to_owned(), "healthy".to_owned()),
        (
            CHILD_SCENARIO_DIR_ENV.to_owned(),
            scenario_dir.display().to_string(),
        ),
    ];
    config.core.readiness_timeout_ms = 5_000;
    config.core.probation_ms = 100;
    config.core.stop_grace_ms = 2_000;

    let mut vision = ComponentConfig::new(ComponentKind::Vision);
    vision.args = child_helper_args();
    vision.extra_env = vec![
        (CHILD_MODE_ENV.to_owned(), "healthy".to_owned()),
        (
            CHILD_SCENARIO_DIR_ENV.to_owned(),
            scenario_dir.display().to_string(),
        ),
    ];
    vision.readiness_timeout_ms = 5_000;
    vision.probation_ms = 100;
    vision.stop_grace_ms = 2_000;
    config.vision = Some(vision);

    let mut handle = SupervisorHandle::start(config).expect("dual supervisor must start");
    let core_initial = handle
        .wait_until_running(ComponentKind::Core, 8_000)
        .expect("core must reach running");
    let vision_initial = handle
        .wait_until_running(ComponentKind::Vision, 8_000)
        .expect("vision must reach running");
    let core_pid_before_vision_upgrade = core_initial.pid.expect("core must have pid");
    assert_eq!(vision_initial.active_version, Some(vision_v1));

    // Upgrade Vision to a crashing binary and verify Core PID never changes.
    fs::write(
        scenario_dir.join(format!("keeppeek-vision-{vision_v2}.mode")),
        "crash_immediately",
    )
    .unwrap();
    let m_vision_v2 = signed_manifest_for_file(
        ComponentKind::Vision,
        vision_v2,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(
            &m_vision_v2,
            &current_exe,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();
    assert!(handle.request_restart(ComponentKind::Vision));

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let snap = handle.snapshot();
        let vision_snap = snap.vision.as_ref().expect("vision snapshot");
        assert_eq!(
            snap.core.pid,
            Some(core_pid_before_vision_upgrade),
            "Vision crash/rollback must never interrupt Core process"
        );
        if vision_snap.rollback_count >= 1
            && vision_snap.phase == ComponentPhase::Running
            && vision_snap.running_version == Some(vision_v1)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for Vision rollback: {vision_snap:?}"
        );
        thread::sleep(Duration::from_millis(30));
    }

    // Stage core v2 first, then trigger core v1 to self-exit with EXIT_CODE_RESTART_REQUESTED (75).
    let m_core_v2 = signed_manifest_for_file(
        ComponentKind::Core,
        core_v2,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(&m_core_v2, &current_exe, &[trusted], &stage_opts)
        .unwrap();
    let restart_trigger = scenario_dir.join(format!("keeppeek-{core_v1}.trigger_restart"));
    fs::write(&restart_trigger, b"1").unwrap();

    let core_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let core_snap = handle.snapshot().core;
        if core_snap.phase == ComponentPhase::Running && core_snap.active_version == Some(core_v2) {
            break;
        }
        assert!(
            std::time::Instant::now() < core_deadline,
            "timed out waiting for core v2 promotion via exit code 75: {core_snap:?}"
        );
        thread::sleep(Duration::from_millis(30));
    }

    handle.stop();
    let rolled = store
        .manual_rollback(ComponentKind::Core)
        .expect("manual rollback to core_v1 must succeed");
    assert_eq!(rolled.core.active, Some(core_v1));
    assert_eq!(rolled.core.previous, Some(core_v2));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn readiness_timeout_quarantines_hanging_pending_release_and_recovers_active() {
    let dir = temp_dir("readiness-timeout");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenarios");
    fs::create_dir_all(&scenario_dir).unwrap();
    let current_exe = std::env::current_exe().expect("test executable path must exist");
    let (seed, trusted) = test_signing_material();
    let store = SlotStore::new(&root);
    let stage_opts = StageOptions {
        expected_platform: current_platform(),
        now_unix_secs: 1_750_000_000,
        allow_breaking: false,
        max_boot_attempts: 1,
    };

    let v1 = ReleaseVersion::new(0, 4, 0);
    let v2 = ReleaseVersion::new(0, 4, 1);
    let m1 = signed_manifest_for_file(
        ComponentKind::Core,
        v1,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(
            &m1,
            &current_exe,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();

    let mut config = SupervisorConfig::new(&root);
    config.poll_interval_ms = 20;
    config.core.args = child_helper_args();
    config.core.extra_env = vec![
        (CHILD_MODE_ENV.to_owned(), "healthy".to_owned()),
        (
            CHILD_SCENARIO_DIR_ENV.to_owned(),
            scenario_dir.display().to_string(),
        ),
    ];
    config.core.readiness_timeout_ms = 350;
    config.core.probation_ms = 100;
    config.core.stop_grace_ms = 1_000;

    let mut handle = SupervisorHandle::start(config).unwrap();
    handle
        .wait_until_running(ComponentKind::Core, 8_000)
        .unwrap();

    fs::write(
        scenario_dir.join(format!("keeppeek-{v2}.mode")),
        "hang_without_ready",
    )
    .unwrap();
    let m2 = signed_manifest_for_file(
        ComponentKind::Core,
        v2,
        &current_exe,
        &seed,
        SchemaCompatibility::ExpandContract,
        1_760_000_000,
    );
    store
        .stage_signed_binary(&m2, &current_exe, &[trusted], &stage_opts)
        .unwrap();
    assert!(handle.request_restart(ComponentKind::Core));

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let snap = handle.snapshot().core;
        if snap.rollback_count >= 1
            && snap.phase == ComponentPhase::Running
            && snap.running_version == Some(v1)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for readiness-timeout rollback: {snap:?}"
        );
        thread::sleep(Duration::from_millis(30));
    }

    handle.stop();
    let final_state = store.load_state().unwrap();
    assert!(final_state.core.quarantined.contains(&v2));
    assert_eq!(final_state.core.active, Some(v1));

    let _ = fs::remove_dir_all(&dir);
}
