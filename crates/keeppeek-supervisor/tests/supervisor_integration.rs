use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keeppeek_supervisor::acl::verify_supervisor_root_acl;
use keeppeek_supervisor::control::{
    ControlAction, ControlError, is_supervisor_running, read_live_status,
    wait_for_control_completion, write_control_request,
};
use keeppeek_supervisor::manifest::{
    ComponentKind, DEFAULT_STORAGE_SCHEMA, MANIFEST_SCHEMA_VERSION_V2, ManifestError,
    ReleaseManifestPayload, SchemaCompatibility, TrustedSigningKey, current_platform,
    hash_binary_file, public_key_from_seed, sign_manifest, verify_manifest_and_file,
};
use keeppeek_supervisor::slots::{
    LaunchPolicy, MAX_RETAINED_UNREFERENCED_SLOTS, SlotError, SlotStore, StageOptions,
};
use keeppeek_supervisor::supervisor::{
    ComponentConfig, ComponentPhase, SupervisorConfig, SupervisorError, SupervisorHandle,
};
use keeppeek_supervisor::version::{ReleaseVersion, UpdateChannel};
use keeppeek_supervisor::{
    ENV_ACTIVE_SLOT, ENV_ACTIVE_VERSION, ENV_COMPONENT, ENV_SUPERVISED,
    EXIT_CODE_RESTART_REQUESTED, signal_ready_from_env, stop_file_from_env, stop_requested,
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    dir
}

fn test_signing_material(seed_byte: u8, key_id: &str) -> ([u8; 32], TrustedSigningKey) {
    let seed = [seed_byte; 32];
    let public_key_bytes = public_key_from_seed(&seed).expect("seed must derive public key");
    let trusted = TrustedSigningKey {
        key_id: key_id.to_owned(),
        public_key_bytes,
    };
    (seed, trusted)
}

struct TestManifestSpec<'a> {
    channel: UpdateChannel,
    component: ComponentKind,
    version: ReleaseVersion,
    build_seq: u64,
    storage_schema: u32,
    key_id: &'a str,
}

fn signed_manifest_for_file(
    spec: &TestManifestSpec<'_>,
    binary_path: &Path,
    seed: &[u8; 32],
) -> keeppeek_supervisor::manifest::SignedReleaseManifest {
    let (size_bytes, sha256_hex) = hash_binary_file(binary_path).expect("binary must hash");
    let payload = ReleaseManifestPayload {
        schema_version: MANIFEST_SCHEMA_VERSION_V2,
        channel: spec.channel,
        component: spec.component,
        version: spec.version,
        build_seq: spec.build_seq,
        commit_sha: "0123456789abcdef".to_owned(),
        storage_schema: spec.storage_schema,
        platform: current_platform(),
        binary_name: spec.component.current_binary_file_name().to_owned(),
        sha256_hex,
        size_bytes,
        schema_compatibility: SchemaCompatibility::ExpandContract,
        issued_at_unix_secs: 1_750_000_000,
        expires_at_unix_secs: 1_760_000_000,
        signing_key_id: spec.key_id.to_owned(),
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

fn copy_test_exe_with_unique_tag(dir: &Path, tag: &str) -> PathBuf {
    let current_exe = std::env::current_exe().expect("test executable path must exist");
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let dest = dir.join(format!("helper-{tag}{ext}"));
    let mut bytes = fs::read(&current_exe).expect("test executable must be readable");
    // Appending distinct trailing comment bytes produces a distinct SHA-256 while preserving OS execution.
    bytes.extend_from_slice(format!("\nKEEPPEEK_TEST_TAG={tag}\n").as_bytes());
    fs::write(&dest, bytes).expect("unique test executable copy must write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dest
}

#[test]
fn child_helper_entrypoint() {
    let Ok(mode) = std::env::var(CHILD_MODE_ENV) else {
        return;
    };
    let version = std::env::var(ENV_ACTIVE_VERSION).unwrap_or_else(|_| "fallback".to_owned());
    let slot = std::env::var(ENV_ACTIVE_SLOT).unwrap_or_else(|_| version.clone());
    let component = std::env::var(ENV_COMPONENT).unwrap_or_else(|_| "keeppeek".to_owned());
    let scenario_dir = std::env::var_os(CHILD_SCENARIO_DIR_ENV).map(PathBuf::from);
    let stop_file = stop_file_from_env();

    let effective_mode = scenario_dir
        .as_ref()
        .and_then(|dir| {
            fs::read_to_string(dir.join(format!("{component}-{slot}.mode")))
                .or_else(|_| fs::read_to_string(dir.join(format!("{component}-{version}.mode"))))
                .ok()
        })
        .map_or(mode, |content| content.trim().to_owned());

    match effective_mode.as_str() {
        "crash_immediately" => std::process::exit(9),
        "hang_without_ready" => {
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
fn manifest_v2_enforces_channel_isolation_major_line_launch_signature_and_gc() {
    let dir = temp_dir("manifest-v2-policy");
    let binary_v1 = dir.join("payload-v1.bin");
    let binary_v2 = dir.join("payload-v2.bin");
    fs::write(&binary_v1, b"keeppeek-signed-binary-content-v1").unwrap();
    fs::write(&binary_v2, b"keeppeek-signed-binary-content-v2").unwrap();
    let (rel_seed, rel_key) = test_signing_material(0x42, "release-key-1");
    let (main_seed, main_key) = test_signing_material(0x77, "main-key-1");

    let rel_manifest = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 2, 0),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &binary_v1,
        &rel_seed,
    );
    let main_manifest = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Main,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 1, 0),
            build_seq: 42,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "main-key-1",
        },
        &binary_v2,
        &main_seed,
    );

    verify_manifest_and_file(
        &rel_manifest,
        &binary_v1,
        std::slice::from_ref(&rel_key),
        UpdateChannel::Release,
        &current_platform(),
        1_750_000_000,
    )
    .expect("valid release manifest v2 must verify");

    // Cross-channel replay MUST fail even if the key is provided.
    assert!(matches!(
        verify_manifest_and_file(
            &main_manifest,
            &binary_v2,
            std::slice::from_ref(&main_key),
            UpdateChannel::Release,
            &current_platform(),
            1_750_000_000,
        ),
        Err(ManifestError::ChannelMismatch {
            expected: UpdateChannel::Release,
            actual: UpdateChannel::Main
        })
    ));

    // Major increment (0.x -> 1.0.0) MUST require a fresh install.
    let store = SlotStore::new(dir.join("supervisor"));
    let major_jump = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(1, 0, 0),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &binary_v2,
        &rel_seed,
    );
    let mut stage_opts = StageOptions::for_current_host(UpdateChannel::Release, 1_750_000_000);
    stage_opts.supervisor_version = ReleaseVersion::new(0, 1, 0);
    assert!(matches!(
        store.stage_signed_binary(
            &major_jump,
            &binary_v2,
            std::slice::from_ref(&rel_key),
            &stage_opts,
        ),
        Err(SlotError::MajorUpgradeRequiresFreshInstall {
            current_major: 0,
            candidate
        }) if candidate == ReleaseVersion::new(1, 0, 0)
    ));

    // Same major (0.1.0 -> 0.2.0) stages cleanly, and identical digest re-stage is an idempotent no-op.
    let first = store
        .stage_signed_binary(
            &rel_manifest,
            &binary_v1,
            std::slice::from_ref(&rel_key),
            &stage_opts,
        )
        .expect("0.2.0 same-major release must stage");
    assert!(first.changed);
    let second = store
        .stage_signed_binary(
            &rel_manifest,
            &binary_v1,
            std::slice::from_ref(&rel_key),
            &stage_opts,
        )
        .expect("identical digest re-stage must be idempotent");
    assert!(!second.changed);
    assert_eq!(second.slot_id, first.slot_id);

    store
        .commit_healthy(ComponentKind::Core, &first.slot_id)
        .expect("commit must promote 0.2.0");

    // Launch re-verifies Ed25519 signature WITHOUT failing on expired staging windows.
    let valid_policy = LaunchPolicy {
        release_keys: vec![rel_key.clone()],
        ..LaunchPolicy::default()
    };
    assert!(
        store
            .prepare_launch(ComponentKind::Core, None, &valid_policy)
            .is_ok()
    );
    // Launch fails closed when the signing key is missing from LaunchPolicy.
    let empty_policy = LaunchPolicy::default();
    assert!(matches!(
        store.prepare_launch(ComponentKind::Core, None, &empty_policy),
        Err(SlotError::SlotIntegrityFailed { .. })
    ));

    // Higher storage_schema requires --allow-breaking even if schema_compatibility claims ExpandContract.
    let schema_bump = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 3, 0),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA + 1,
            key_id: "release-key-1",
        },
        &binary_v2,
        &rel_seed,
    );
    assert!(matches!(
        store.stage_signed_binary(
            &schema_bump,
            &binary_v2,
            std::slice::from_ref(&rel_key),
            &stage_opts,
        ),
        Err(SlotError::BreakingSchemaRequiresOptIn(v)) if v == ReleaseVersion::new(0, 3, 0)
    ));

    // 8 consecutive local rebuilds at 0.1.0 create distinct slots without collision, leave release_floor untouched,
    // and GC unreferenced directories to MAX_RETAINED_UNREFERENCED_SLOTS.
    for index in 0_u32..8 {
        let local_bin = dir.join(format!("local-build-{index}.bin"));
        fs::write(&local_bin, format!("local-build-payload-{index}")).unwrap();
        let staged = store
            .stage_local_binary(
                ComponentKind::Core,
                &local_bin,
                ReleaseVersion::new(0, 1, 0),
                1_750_000_100 + u64::from(index),
                1,
            )
            .expect("local build must stage");
        assert!(staged.changed);
        store
            .commit_healthy(ComponentKind::Core, &staged.slot_id)
            .expect("local commit must succeed");
    }
    let state = store.load_state().unwrap();
    assert_eq!(state.core.release_floor, Some(ReleaseVersion::new(0, 2, 0)));
    let version_dir_count = fs::read_dir(store.root().join("versions"))
        .unwrap()
        .flatten()
        .count();
    assert!(
        version_dir_count <= 2 + MAX_RETAINED_UNREFERENCED_SLOTS,
        "expected GC to bound slot directories to <= {}, got {version_dir_count}",
        2 + MAX_RETAINED_UNREFERENCED_SLOTS
    );

    // Local slot refuses to launch unless host policy opts into allow_local = true.
    assert!(matches!(
        store.prepare_launch(ComponentKind::Core, None, &valid_policy),
        Err(SlotError::ChannelDisallowedByHostPolicy {
            channel: UpdateChannel::Local,
            ..
        })
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn live_local_dev_loop_applies_waits_rolls_back_and_recovers_with_acl_and_lock() {
    let dir = temp_dir("live-local-loop");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenarios");
    fs::create_dir_all(&scenario_dir).unwrap();
    let store = SlotStore::new(&root);

    let bin_v1 = copy_test_exe_with_unique_tag(&dir, "local-v1");
    let bin_bad = copy_test_exe_with_unique_tag(&dir, "local-bad");
    let bin_v3 = copy_test_exe_with_unique_tag(&dir, "local-v3");

    let staged_v1 = store
        .stage_local_binary(
            ComponentKind::Core,
            &bin_v1,
            ReleaseVersion::new(0, 1, 0),
            1_750_000_001,
            1,
        )
        .expect("v1 local must stage");
    verify_supervisor_root_acl(&root).expect("user-owned temp supervisor root must pass ACL check");

    let mut config = SupervisorConfig::new(&root);
    config.poll_interval_ms = 20;
    config.launch_policy.allow_local = true;
    config.enforce_root_acl = true;
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

    let mut handle = SupervisorHandle::start(config.clone()).expect("supervisor must start");
    assert!(is_supervisor_running(&root));
    // Second supervisor on the same root MUST fail closed on runtime/supervisor.lock.
    assert!(matches!(
        SupervisorHandle::start(config),
        Err(SupervisorError::Control(
            ControlError::AlreadyRunning { .. }
        ))
    ));

    let initial = handle
        .wait_until_running(ComponentKind::Core, 8_000)
        .expect("v1 must commit healthy");
    assert_eq!(initial.active_slot.as_ref(), Some(&staged_v1.slot_id));
    let live = read_live_status(&root)
        .unwrap()
        .expect("live status JSON must exist");
    assert_eq!(live.core.pid, initial.pid);
    assert!(live.allow_local);

    // Stage a crashing local build (same semver 0.1.0, different digest) and apply live with --wait.
    let staged_bad = store
        .stage_local_binary(
            ComponentKind::Core,
            &bin_bad,
            ReleaseVersion::new(0, 1, 0),
            1_750_000_002,
            1,
        )
        .expect("bad local build must stage");
    fs::write(
        scenario_dir.join(format!("keeppeek-{}.mode", staged_bad.slot_id)),
        "crash_immediately",
    )
    .unwrap();
    let bad_req = write_control_request(&root, ComponentKind::Core, ControlAction::ApplyPending)
        .expect("control request must write");
    let bad_wait =
        wait_for_control_completion(&root, ComponentKind::Core, &bad_req.request_id, 10_000);
    assert!(
        matches!(bad_wait, Err(ControlError::RolledBack { .. })),
        "expected RolledBack error on bad local build, got: {bad_wait:?}"
    );
    let after_rollback = store.load_state().unwrap();
    assert_eq!(
        after_rollback.core.active.as_ref(),
        Some(&staged_v1.slot_id)
    );
    assert!(
        after_rollback
            .core
            .quarantined
            .contains(&staged_bad.slot_id)
    );

    // Stage a fixed local build at the SAME 0.1.0 semver — digest is new so quarantine does NOT block it!
    let staged_v3 = store
        .stage_local_binary(
            ComponentKind::Core,
            &bin_v3,
            ReleaseVersion::new(0, 1, 0),
            1_750_000_003,
            1,
        )
        .expect("fixed local build at 0.1.0 must stage");
    let v3_req = write_control_request(&root, ComponentKind::Core, ControlAction::ApplyPending)
        .expect("v3 control request must write");
    let v3_snap =
        wait_for_control_completion(&root, ComponentKind::Core, &v3_req.request_id, 10_000)
            .expect("fixed local build must apply and pass probation");
    assert_eq!(v3_snap.running_slot.as_ref(), Some(&staged_v3.slot_id));
    assert_eq!(v3_snap.active_slot.as_ref(), Some(&staged_v3.slot_id));
    assert_ne!(v3_snap.pid, initial.pid);

    handle.stop();
    assert!(!is_supervisor_running(&root));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn signed_exit_75_upgrade_and_vision_isolation_work_with_manifest_v2() {
    let dir = temp_dir("signed-restart-vision");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenarios");
    fs::create_dir_all(&scenario_dir).unwrap();
    let store = SlotStore::new(&root);
    let (seed, trusted) = test_signing_material(0x99, "release-key-1");

    let core_v1_bin = copy_test_exe_with_unique_tag(&dir, "core-v1");
    let core_v2_bin = copy_test_exe_with_unique_tag(&dir, "core-v2");
    let vision_v1_bin = copy_test_exe_with_unique_tag(&dir, "vision-v1");
    let vision_v2_bin = copy_test_exe_with_unique_tag(&dir, "vision-v2");

    let mut stage_opts = StageOptions::for_current_host(UpdateChannel::Release, 1_750_000_000);
    stage_opts.supervisor_version = ReleaseVersion::new(0, 1, 0);
    stage_opts.max_boot_attempts = 1;

    let m_core_v1 = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 3, 0),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &core_v1_bin,
        &seed,
    );
    let m_vision_v1 = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Vision,
            version: ReleaseVersion::new(0, 1, 0),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &vision_v1_bin,
        &seed,
    );
    let core_v1_staged = store
        .stage_signed_binary(
            &m_core_v1,
            &core_v1_bin,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();
    let vision_v1_staged = store
        .stage_signed_binary(
            &m_vision_v1,
            &vision_v1_bin,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();

    let mut config = SupervisorConfig::new(&root);
    config.poll_interval_ms = 20;
    config.launch_policy.release_keys = vec![trusted.clone()];
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
    let core_pid_before = core_initial.pid.expect("core pid");
    assert_eq!(
        vision_initial.active_slot.as_ref(),
        Some(&vision_v1_staged.slot_id)
    );

    // Crashing Vision upgrade rolls back without interrupting Core PID.
    let m_vision_v2 = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Vision,
            version: ReleaseVersion::new(0, 1, 1),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &vision_v2_bin,
        &seed,
    );
    let vision_v2_staged = store
        .stage_signed_binary(
            &m_vision_v2,
            &vision_v2_bin,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();
    fs::write(
        scenario_dir.join(format!("keeppeek-vision-{}.mode", vision_v2_staged.slot_id)),
        "crash_immediately",
    )
    .unwrap();
    let vis_req =
        write_control_request(&root, ComponentKind::Vision, ControlAction::ApplyPending).unwrap();
    assert!(matches!(
        wait_for_control_completion(&root, ComponentKind::Vision, &vis_req.request_id, 8_000),
        Err(ControlError::RolledBack { .. })
    ));
    assert_eq!(handle.snapshot().core.pid, Some(core_pid_before));

    // Stage Core 0.3.1 and trigger exit code 75 self-restart.
    let m_core_v2 = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Release,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 3, 1),
            build_seq: 0,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "release-key-1",
        },
        &core_v2_bin,
        &seed,
    );
    let core_v2_staged = store
        .stage_signed_binary(
            &m_core_v2,
            &core_v2_bin,
            std::slice::from_ref(&trusted),
            &stage_opts,
        )
        .unwrap();
    fs::write(scenario_dir.join("keeppeek-0.3.0.trigger_restart"), b"1").unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let core_snap = handle.snapshot().core;
        if core_snap.phase == ComponentPhase::Running
            && core_snap.active_slot.as_ref() == Some(&core_v2_staged.slot_id)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for core v2 promotion via exit 75: {core_snap:?}"
        );
        thread::sleep(Duration::from_millis(30));
    }

    handle.stop();
    let rolled = store
        .manual_rollback(
            ComponentKind::Core,
            &LaunchPolicy {
                release_keys: vec![trusted],
                ..LaunchPolicy::default()
            },
        )
        .expect("manual rollback to core v1 must succeed");
    assert_eq!(rolled.core.active.as_ref(), Some(&core_v1_staged.slot_id));
    assert_eq!(rolled.core.previous.as_ref(), Some(&core_v2_staged.slot_id));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unsigned_main_does_not_poison_signed_main_floor_and_failed_component_recovers_via_control() {
    let dir = temp_dir("unsigned-main-and-failed-recovery");
    let root = dir.join("supervisor");
    let scenario_dir = dir.join("scenario");
    fs::create_dir_all(&scenario_dir).unwrap();
    let store = SlotStore::new(&root);
    let (main_seed, main_key) = test_signing_material(0x55, "main-key-1");

    // 1. Staging & committing an unsigned `main` binary with unix-timestamp build_seq must NOT raise `main_floor`.
    let unsigned_bin = copy_test_exe_with_unique_tag(&dir, "unsigned-main-1");
    let unsigned_sha = hash_binary_file(&unsigned_bin).unwrap().1;
    let unsigned_staged = store
        .stage_unsigned_main_binary(
            &unsigned_bin,
            &keeppeek_supervisor::slots::UnsignedMainStageOptions {
                component: ComponentKind::Core,
                version: ReleaseVersion::new(0, 1, 0),
                build_seq: 1_780_000_000,
                expected_sha256_hex: &unsigned_sha,
                supervisor_version: ReleaseVersion::new(0, 1, 0),
                now_unix_secs: 1_780_000_000,
                max_boot_attempts: 2,
            },
        )
        .unwrap();
    let committed = store
        .commit_healthy(ComponentKind::Core, &unsigned_staged.slot_id)
        .unwrap();
    assert!(
        committed.core.main_floor.is_none(),
        "unsigned main commit must never advance main_floor"
    );

    // 2. Subsequent signed `main` manifest with CI run_number build_seq = 1 must stage cleanly.
    let signed_bin = copy_test_exe_with_unique_tag(&dir, "signed-main-1");
    let signed_manifest = signed_manifest_for_file(
        &TestManifestSpec {
            channel: UpdateChannel::Main,
            component: ComponentKind::Core,
            version: ReleaseVersion::new(0, 1, 0),
            build_seq: 1,
            storage_schema: DEFAULT_STORAGE_SCHEMA,
            key_id: "main-key-1",
        },
        &signed_bin,
        &main_seed,
    );
    let signed_staged = store
        .stage_signed_binary(
            &signed_manifest,
            &signed_bin,
            std::slice::from_ref(&main_key),
            &StageOptions {
                expected_channel: UpdateChannel::Main,
                expected_platform: current_platform(),
                supervisor_version: ReleaseVersion::new(0, 1, 0),
                now_unix_secs: 1_750_000_000,
                allow_breaking: false,
                max_boot_attempts: 2,
            },
        )
        .expect("signed main build_seq=1 must not be blocked by earlier unsigned main build");
    let committed_signed = store
        .commit_healthy(ComponentKind::Core, &signed_staged.slot_id)
        .unwrap();
    assert_eq!(
        committed_signed.core.main_floor,
        Some(keeppeek_supervisor::version::MainBuildFloor::new(
            ReleaseVersion::new(0, 1, 0),
            1
        ))
    );

    // 3. A supervisor whose component enters `Failed` (no binary available) recovers live via `ApplyPending`.
    let failed_root = dir.join("failed-recovery-root");
    let mut cfg = SupervisorConfig::new(&failed_root);
    cfg.launch_policy.allow_local = true;
    cfg.enforce_root_acl = true;
    cfg.poll_interval_ms = 25;
    cfg.core.args = child_helper_args();
    cfg.core
        .extra_env
        .push((CHILD_MODE_ENV.to_owned(), "healthy".to_owned()));
    cfg.core.extra_env.push((
        CHILD_SCENARIO_DIR_ENV.to_owned(),
        scenario_dir.to_string_lossy().into_owned(),
    ));
    cfg.core.probation_ms = 250;
    cfg.core.readiness_timeout_ms = 5_000;
    let mut handle = SupervisorHandle::start(cfg).unwrap();
    assert!(matches!(
        handle.wait_until_running(ComponentKind::Core, 5_000),
        Err(keeppeek_supervisor::supervisor::SupervisorError::ComponentFailed { .. })
    ));

    // Stage a valid local binary and send ApplyPending while the component is in Failed state.
    let recovery_store = SlotStore::new(&failed_root);
    let good_bin = copy_test_exe_with_unique_tag(&dir, "recovered-core-v1");
    let staged = recovery_store
        .stage_local_binary(
            ComponentKind::Core,
            &good_bin,
            ReleaseVersion::new(0, 1, 0),
            1_750_000_000,
            2,
        )
        .unwrap();
    let req = write_control_request(
        &failed_root,
        ComponentKind::Core,
        ControlAction::ApplyPending,
    )
    .unwrap();
    let recovered =
        wait_for_control_completion(&failed_root, ComponentKind::Core, &req.request_id, 8_000)
            .expect("Failed component must recover live when ApplyPending is issued");
    assert_eq!(recovered.phase, ComponentPhase::Running);
    assert_eq!(recovered.active_slot.as_ref(), Some(&staged.slot_id));

    handle.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unix_acl_check_rejects_world_writable_root() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("acl-world-writable");
    let root = dir.join("supervisor");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(verify_supervisor_root_acl(&root).is_err());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(verify_supervisor_root_acl(&root).is_ok());
    let _ = fs::remove_dir_all(&dir);
}
