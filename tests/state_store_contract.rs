//! Recovery contract for the configuration bundle and the runtime state store.
//!
//! These tests pin the public recovery surface used by slice 7: the format-3
//! configuration ZIP holds exactly `config.toml` and `secrets.toml`, carries
//! its manifest in the ZIP comment, and excludes every database and media
//! member. Validation fails closed on interrupted writes and smuggled members,
//! oversize input is rejected at the byte ceiling, and an older snapshot still
//! plans cleanly against a newer target.
//!
//! Each test states what it proves. Store-level crash atomicity, quota
//! accounting, expiry, and compaction are covered by the `DurableStore` unit
//! tests in `src/server/state_store_durable.rs`; the live watch protocol is
//! covered by the watch unit tests. This file does not duplicate them.

mod harness;

use harness::TestHarness;
use keeppeek::api::backup_proto;
use keeppeek::backup::{
    BackupSecretPolicy, BackupSection, CreateBundleOptions, RestorePlanOptions, create_bundle,
    inspect_bundle, plan_restore, target_revision,
};
use keeppeek::test_support::TestCameraCatalog;
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(0);

const CREATED_AT_MS: u64 = 1_788_000_000_000;
const CONFIG_V1: &str =
    "access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n[storage]\nlong_term_max_gb = 10\n";
const CONFIG_V2: &str =
    "access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n[storage]\nlong_term_max_gb = 20\n";
const SECRETS_V1: &str = "KEEPPEEK_ACCESS_KEY = \"00000000-0000-4000-8000-000000000001\"\n";

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "keeppeek-state-store-contract-{name}-{}-{id}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("scratch dir must be cleaned");
        }
        std::fs::create_dir_all(&path).expect("scratch dir must be created");
        Self { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_config_set(dir: &Path, config: &str, secrets: &str) -> PathBuf {
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, config).expect("config fixture must be written");
    std::fs::write(dir.join("secrets.toml"), secrets).expect("secrets fixture must be written");
    config_path
}

fn bundle_bytes(config_path: &Path) -> Vec<u8> {
    let options = CreateBundleOptions {
        config_path,
        sections: &[],
        created_at_unix_ms: CREATED_AT_MS,
    };
    let (bundle, _) =
        create_bundle(Cursor::new(Vec::new()), options).expect("bundle creation must succeed");
    bundle.into_inner()
}

fn read_member(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>, name: &str) -> String {
    let mut text = String::new();
    archive
        .by_name(name)
        .expect("bundle member must exist")
        .read_to_string(&mut text)
        .expect("bundle member must be UTF-8");
    text
}

fn plan_request(
    source_path: &str,
    target_dir: &Path,
    target_config: &Path,
) -> backup_proto::CreateRestorePlanRequest {
    backup_proto::CreateRestorePlanRequest {
        client_request_id: "contract-request".to_owned(),
        backup_id: "contract-backup".to_owned(),
        sections: Vec::new(),
        path_mappings: vec![backup_proto::RestorePathMapping {
            kind: backup_proto::BackupPathKind::ConfigDirectory as i32,
            source_path: source_path.to_owned(),
            target_path: target_dir.to_string_lossy().into_owned(),
        }],
        expected_target_revision: target_revision(target_config)
            .expect("target revision must be readable"),
    }
}

/// Proves the format-3 contract: exactly two TOML members, manifest in the ZIP
/// comment, plaintext secrets policy, and an explicit media/catalog exclusion
/// list. It does not prove restore activation, which needs a restart.
#[test]
fn format3_bundle_contains_exactly_the_two_toml_members() {
    let dir = ScratchDir::new("format3");
    let config_path = write_config_set(&dir.path, CONFIG_V1, SECRETS_V1);
    let bytes = bundle_bytes(&config_path);

    let manifest = inspect_bundle(Cursor::new(bytes.clone())).expect("fresh bundle must validate");
    assert_eq!(manifest.format_version(), 3);
    assert_eq!(manifest.secret_policy(), BackupSecretPolicy::Included);
    assert_eq!(manifest.sections().len(), 1);
    assert_eq!(manifest.sections()[0].kind(), BackupSection::RuntimeConfig);
    assert_eq!(manifest.sections()[0].path(), "config.toml");
    assert_eq!(manifest.snapshot_revision().len(), 64);
    for omitted in [
        "recording_media",
        "recording_catalog",
        "event_thumbnails",
        "sessions",
        "derived_caches",
    ] {
        assert!(
            manifest.omitted_data().contains(&omitted.to_owned()),
            "bundle must declare {omitted} as excluded"
        );
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("bundle must be a ZIP");
    assert_eq!(archive.len(), 2);
    assert!(
        !archive.comment().is_empty(),
        "manifest must travel in the ZIP comment, not as a third member"
    );
    assert_eq!(read_member(&mut archive, "config.toml"), CONFIG_V1);
    assert_eq!(read_member(&mut archive, "secrets.toml"), SECRETS_V1);
    assert!(archive.by_name("manifest.json").is_err());
    assert!(archive.by_name("recordings.db").is_err());
}

/// Proves interrupted writes fail closed: truncated and bit-flipped bundles
/// never validate, and restore planning rejects the truncated file. It does
/// not prove disk-level write atomicity, which the transactional store covers.
#[test]
fn interrupted_bundle_write_fails_closed() {
    let dir = ScratchDir::new("interrupted");
    let config_path = write_config_set(&dir.path, CONFIG_V1, SECRETS_V1);
    let bytes = bundle_bytes(&config_path);

    for len in [0, 1, 128, bytes.len() / 2] {
        let truncated = bytes[..len.min(bytes.len())].to_vec();
        assert!(
            inspect_bundle(Cursor::new(truncated)).is_err(),
            "truncated bundle of {len} bytes must not validate"
        );
    }
    let mut flipped = bytes.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0xFF;
    assert!(
        inspect_bundle(Cursor::new(flipped)).is_err(),
        "bit-flipped bundle must not validate"
    );

    let bundle_path = dir.path.join("interrupted.zip");
    std::fs::write(&bundle_path, &bytes[..bytes.len() / 2])
        .expect("truncated bundle must be written");
    let target_config = write_config_set(&dir.path, CONFIG_V2, SECRETS_V1);
    let request = plan_request("unknown-source", &dir.path, &target_config);
    assert!(
        plan_restore(RestorePlanOptions {
            bundle_path: &bundle_path,
            target_config_path: &target_config,
            request: &request,
            now_unix_ms: CREATED_AT_MS + 1_000,
        })
        .is_err(),
        "restore planning must reject the interrupted bundle"
    );
}

/// Proves the no-database boundary: a third member smuggled into an otherwise
/// valid archive is rejected because it is not listed in the manifest.
#[test]
fn smuggled_database_member_is_rejected() {
    let dir = ScratchDir::new("smuggled");
    let config_path = write_config_set(&dir.path, CONFIG_V1, SECRETS_V1);
    let bytes = bundle_bytes(&config_path);

    let mut source = zip::ZipArchive::new(Cursor::new(bytes)).expect("bundle must be a ZIP");
    let comment = source.comment().to_vec();
    let config = read_member(&mut source, "config.toml");
    let secrets = read_member(&mut source, "secrets.toml");
    drop(source);

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.set_comment(String::from_utf8(comment).expect("manifest comment must be UTF-8"));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, contents) in [
        ("config.toml", config.as_bytes()),
        ("secrets.toml", secrets.as_bytes()),
        ("recordings.db", b"SQLite format 3\0smuggled".as_slice()),
    ] {
        writer.start_file(name, options).expect("member must start");
        writer.write_all(contents).expect("member must be written");
    }
    let smuggled = writer
        .finish()
        .expect("smuggled archive must finish")
        .into_inner();

    let error = inspect_bundle(Cursor::new(smuggled)).expect_err("third member must be rejected");
    assert!(
        error.to_string().contains("not listed"),
        "unexpected error: {error}"
    );
}

/// Proves oversize input is rejected at the enforced section byte ceiling.
/// This simulates quota and full-disk exhaustion where real failure injection
/// is impractical; it does not prove `ENOSPC` behavior on a live disk.
#[test]
fn oversize_configuration_is_rejected_at_the_byte_ceiling() {
    let dir = ScratchDir::new("quota");
    let config_path = dir.path.join("config.toml");
    std::fs::write(&config_path, vec![b'#'; 16 * 1024 * 1024 + 1])
        .expect("oversize fixture must be written");
    std::fs::write(dir.path.join("secrets.toml"), SECRETS_V1)
        .expect("secrets fixture must be written");

    let options = CreateBundleOptions {
        config_path: &config_path,
        sections: &[],
        created_at_unix_ms: CREATED_AT_MS,
    };
    let error =
        create_bundle(Cursor::new(Vec::new()), options).expect_err("oversize input must fail");
    assert!(
        error.to_string().contains("size limit"),
        "unexpected error: {error}"
    );
}

/// Proves an older snapshot stays restorable: its revision differs from the
/// newer bundle, it produces an activatable plan against a newer target, and
/// a fresh read afterward validates with the same revision. The fresh read is
/// the bundle-level analogue of a fresh watch; live watch re-establishment
/// after a state restore is specified in `docs/state-store.md`.
#[test]
fn older_snapshot_plans_cleanly_and_fresh_read_validates() {
    let source = ScratchDir::new("older-source");
    let target = ScratchDir::new("older-target");

    let source_config = write_config_set(&source.path, CONFIG_V1, SECRETS_V1);
    let older_bytes = bundle_bytes(&source_config);
    let older_manifest =
        inspect_bundle(Cursor::new(older_bytes.clone())).expect("older bundle must validate");

    write_config_set(&source.path, CONFIG_V2, SECRETS_V1);
    let newer_bytes = bundle_bytes(&source_config);
    let newer_manifest =
        inspect_bundle(Cursor::new(newer_bytes)).expect("newer bundle must validate");
    assert_ne!(
        older_manifest.snapshot_revision(),
        newer_manifest.snapshot_revision(),
        "a config change must move the snapshot revision"
    );

    let target_config = write_config_set(&target.path, CONFIG_V2, SECRETS_V1);
    let bundle_path = source.path.join("older.zip");
    std::fs::write(&bundle_path, &older_bytes).expect("older bundle must be written");
    let request = plan_request(
        older_manifest.source_paths()[0].path(),
        &target.path,
        &target_config,
    );
    let plan = plan_restore(RestorePlanOptions {
        bundle_path: &bundle_path,
        target_config_path: &target_config,
        request: &request,
        now_unix_ms: CREATED_AT_MS + 1_000,
    })
    .expect("older snapshot must plan");
    assert!(
        plan.can_activate,
        "older snapshot plan must be activatable: {:?}",
        plan.issues
    );
    assert_eq!(
        plan.target_revision,
        target_revision(&target_config).expect("target revision must be readable")
    );

    let fresh = inspect_bundle(Cursor::new(older_bytes)).expect("fresh read must validate");
    assert_eq!(
        fresh.snapshot_revision(),
        older_manifest.snapshot_revision()
    );
}

/// Proves repeated bundle creation keeps output proportional to input: the
/// last bundle matches the first within a small delta and every bundle holds
/// two members. This guards the create path against cross-call accumulation;
/// store-level churn, expiry, and compaction are covered by the
/// `DurableStore` unit tests.
#[test]
fn churn_keeps_bundles_within_a_steady_ceiling() {
    let dir = ScratchDir::new("churn");
    let mut first_len = 0;
    let mut last_len = 0;
    for round in 0..25u32 {
        let config = format!(
            "access_key = \"{{secret:KEEPPEEK_ACCESS_KEY}}\"\n[storage]\nlong_term_max_gb = {}\n",
            10 + round % 7
        );
        let config_path = write_config_set(&dir.path, &config, SECRETS_V1);
        let bytes = bundle_bytes(&config_path);
        assert!(
            bytes.len() < 16 * 1024,
            "round {round}: bundle must stay far below the section ceiling"
        );
        let archive =
            zip::ZipArchive::new(Cursor::new(bytes.clone())).expect("bundle must be a ZIP");
        assert_eq!(
            archive.len(),
            2,
            "round {round}: bundle must keep two members"
        );
        if round == 0 {
            first_len = bytes.len();
        }
        last_len = bytes.len();
    }
    let drift = first_len.abs_diff(last_len);
    assert!(
        drift < 1_024,
        "churn must not accumulate output: first {first_len} bytes, last {last_len} bytes"
    );
}

/// Proves the test server shuts down cleanly and restarts on a new listener.
/// The harness has no process-kill support, so crash durability is covered by
/// the `durable_reopen_*` unit tests instead of this test.
#[test]
fn harness_restart_serves_health_on_a_new_listener() {
    let harness = TestHarness::start();
    assert_eq!(
        harness
            .client
            .health()
            .expect("first health must succeed")
            .status,
        "ok"
    );
    drop(harness);

    let restarted = TestHarness::start_with_test_camera_catalog(TestCameraCatalog::standard());
    assert_eq!(
        restarted
            .client
            .health()
            .expect("restarted health must succeed")
            .status,
        "ok"
    );
}
