use keeppeek::{
    config::{
        self, Config,
        usage_reporting::{UpdateError, UsageReportingConfig, table_revision, update_with_writer},
    },
    usage_reporting::{
        model::{
            Architecture, BasicSystemData, CURRENT_DISCLOSURE_REVISION, CpuModelFamily, CpuVendor,
            DeploymentType, MemoryBucket, OsFamily, ReleaseChannel, SCHEMA_VERSION_V1,
            UsageReportV1, VideoAcceleration,
        },
        sender::{
            Clock, JitterSource, REPORT_INTERVAL_MS, STARTUP_DELAY_MS, SendAttemptOutcome,
            TelemetryTransport, TransportResponse, UsageReportingController,
        },
        state::{FailureKind, LifecycleState, MAX_PENDING_DELETIONS, StateError},
    },
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct FakeClock(AtomicI64);

impl FakeClock {
    const fn new(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::Release);
    }

    fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::AcqRel);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

struct FixedJitter(i64);

impl JitterSource for FixedJitter {
    fn non_negative_jitter_ms(&self, max_ms: i64) -> i64 {
        self.0.clamp(0, max_ms)
    }
}

#[derive(Default)]
struct MockTransport {
    put_calls: AtomicUsize,
    delete_calls: AtomicUsize,
    put_results: Mutex<VecDeque<Result<TransportResponse, FailureKind>>>,
    delete_results: Mutex<VecDeque<Result<TransportResponse, FailureKind>>>,
    seen_puts: Mutex<Vec<(String, Vec<u8>)>>,
    seen_deletes: Mutex<Vec<String>>,
}

impl MockTransport {
    fn push_put(&self, result: Result<TransportResponse, FailureKind>) {
        self.put_results.lock().unwrap().push_back(result);
    }

    fn push_delete(&self, result: Result<TransportResponse, FailureKind>) {
        self.delete_results.lock().unwrap().push_back(result);
    }
}

impl TelemetryTransport for MockTransport {
    fn put_report(
        &self,
        _origin: &str,
        installation_id: &str,
        _installation_token: &str,
        body: &[u8],
    ) -> Result<TransportResponse, FailureKind> {
        self.put_calls.fetch_add(1, Ordering::AcqRel);
        self.seen_puts
            .lock()
            .unwrap()
            .push((installation_id.to_owned(), body.to_vec()));
        self.put_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(TransportResponse { status: 200 }))
    }

    fn delete_installation(
        &self,
        _origin: &str,
        installation_id: &str,
        _installation_token: &str,
    ) -> Result<TransportResponse, FailureKind> {
        self.delete_calls.fetch_add(1, Ordering::AcqRel);
        self.seen_deletes
            .lock()
            .unwrap()
            .push(installation_id.to_owned());
        self.delete_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(TransportResponse { status: 204 }))
    }
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("keeppeek-usage-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_report(id: &str, sequence: u64) -> UsageReportV1 {
    UsageReportV1 {
        schema_version: SCHEMA_VERSION_V1,
        disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        report_sequence: sequence,
        installation_id: id.to_owned(),
        basic: BasicSystemData {
            keeppeek_version: "0.1.0-alpha.1".to_owned(),
            release_channel: ReleaseChannel::Alpha,
            deployment_type: DeploymentType::Native,
            os_family: OsFamily::Linux,
            os_version: "24.04".to_owned(),
            architecture: Architecture::X86_64,
            cpu_vendor: CpuVendor::Intel,
            cpu_model_family: CpuModelFamily::IntelNSeries,
            physical_cores: Some(4),
            logical_cores: 4,
            memory_bucket: MemoryBucket::From8To16Gb,
            video_acceleration: vec![VideoAcceleration::Vaapi],
        },
        camera_compatibility: None,
        feature_adoption: None,
    }
}

#[test]
fn disabled_by_default_creates_no_uuid_no_state_file_and_no_requests() {
    let dir = temp_dir("disabled");
    let state_path = dir.join("state.json");
    let clock = Arc::new(FakeClock::new(1_790_000_000_000));
    let transport = Arc::new(MockTransport::default());
    let controller = UsageReportingController::open(
        state_path.clone(),
        clock.clone(),
        Arc::new(FixedJitter(0)),
        transport.clone(),
    )
    .unwrap();

    let default_cfg = UsageReportingConfig::default();
    assert!(!default_cfg.any_enabled());
    assert!(controller.snapshot_state().identity.is_none());
    assert!(!state_path.exists());

    // Advance far past 15 minutes and 24 hours: still zero network requests and zero UUID.
    clock.advance(REPORT_INTERVAL_MS * 3);
    let outcome =
        controller.maybe_send_due(&default_cfg, |id, seq| Ok(sample_report(id, seq)), || false);
    assert_eq!(outcome, SendAttemptOutcome::Disabled);
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 0);
    assert_eq!(transport.delete_calls.load(Ordering::Acquire), 0);
    assert!(!state_path.exists());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn schedule_enforces_15m_startup_24h_interval_jitter_rollback_clamp_and_no_retry_storm() {
    let dir = temp_dir("schedule");
    let state_path = dir.join("state.json");
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));
    let jitter_ms = 120_000;
    let transport = Arc::new(MockTransport::default());
    let controller = UsageReportingController::open(
        state_path.clone(),
        clock.clone(),
        Arc::new(FixedJitter(jitter_ms)),
        transport.clone(),
    )
    .unwrap();

    let cfg = UsageReportingConfig {
        basic: true,
        camera_compatibility: false,
        feature_adoption: false,
        consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        receiver_origin: Some("https://usage.example".to_owned()),
    };
    let enabled_state = controller.apply_consent_transition(&cfg).unwrap();
    let first_id = enabled_state
        .identity
        .as_ref()
        .unwrap()
        .installation_id
        .clone();
    assert!(state_path.exists());

    // Just before 15 min + jitter -> NotDue.
    clock.advance(STARTUP_DELAY_MS + jitter_ms - 1);
    assert_eq!(
        controller.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::NotDue
    );
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 0);

    // Exact due time -> sends once and succeeds.
    clock.advance(1);
    assert_eq!(
        controller.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::Succeeded
    );
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 1);

    // Simulate process restart right after send: identity survives and 24h interval is respected.
    let restarted = UsageReportingController::open(
        state_path,
        clock.clone(),
        Arc::new(FixedJitter(jitter_ms)),
        transport.clone(),
    )
    .unwrap();
    assert_eq!(
        restarted
            .snapshot_state()
            .identity
            .as_ref()
            .unwrap()
            .installation_id,
        first_id
    );
    clock.advance(STARTUP_DELAY_MS * 2);
    assert_eq!(
        restarted.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::NotDue
    );

    // Simulate clock rollback before last_attempt_at_ms -> clamped, never accelerates!
    clock.set(900_000_000_000);
    assert_eq!(
        restarted.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::NotDue
    );

    // Advance past clamped 24h + jitter and inject a timeout -> fails once, NO retry storm!
    clock.advance(REPORT_INTERVAL_MS + jitter_ms);
    transport.push_put(Err(FailureKind::Timeout));
    assert_eq!(
        restarted.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::Failed(FailureKind::Timeout)
    );
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 2);
    // Immediate follow-up poll must be NotDue (waits full next 24h interval).
    assert_eq!(
        restarted.maybe_send_due(&cfg, |id, seq| Ok(sample_report(id, seq)), || true),
        SendAttemptOutcome::NotDue
    );
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 2);

    // Consent revoked while building report -> aborts cleanly with zero extra network call.
    clock.advance(REPORT_INTERVAL_MS + jitter_ms);
    let consented = AtomicBool::new(false);
    assert_eq!(
        restarted.maybe_send_due(
            &cfg,
            |id, seq| Ok(sample_report(id, seq)),
            || consented.load(Ordering::Acquire),
        ),
        SendAttemptOutcome::AbortedConsentChanged
    );
    assert_eq!(transport.put_calls.load(Ordering::Acquire), 2);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn disable_reset_offline_pending_cap_and_explicit_retry_behave_truthfully() {
    let dir = temp_dir("deletion");
    let state_path = dir.join("state.json");
    let clock = Arc::new(FakeClock::new(1_790_000_000_000));
    let transport = Arc::new(MockTransport::default());
    let controller = UsageReportingController::open(
        state_path,
        clock,
        Arc::new(FixedJitter(0)),
        transport.clone(),
    )
    .unwrap();

    let cfg = UsageReportingConfig {
        basic: true,
        camera_compatibility: true,
        feature_adoption: false,
        consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        receiver_origin: Some("https://usage.example".to_owned()),
    };
    let initial = controller.apply_consent_transition(&cfg).unwrap();
    let initial_id = initial.identity.as_ref().unwrap().installation_id.clone();

    // Fill all 16 pending deletion slots while receiver is offline (timeout).
    for _ in 0..MAX_PENDING_DELETIONS {
        for _ in 0..=MAX_PENDING_DELETIONS {
            transport.push_delete(Err(FailureKind::NetworkUnavailable));
        }
        let state = controller.reset_identity(&cfg).unwrap();
        assert_eq!(
            state.lifecycle_state(true, false),
            LifecycleState::DeletionPending
        );
        transport.delete_results.lock().unwrap().clear();
    }
    let full = controller.snapshot_state();
    assert_eq!(full.pending_deletions.len(), MAX_PENDING_DELETIONS);
    assert_eq!(full.pending_deletions[0].installation_id, initial_id);
    assert_ne!(full.identity.as_ref().unwrap().installation_id, initial_id);

    // 17th reset must fail cleanly rather than silently dropping undeleted rows!
    assert_eq!(
        controller.reset_identity(&cfg).unwrap_err(),
        StateError::PendingDeletionQueueFull
    );

    // Receiver comes back online -> explicit retry drains all 16 pending deletions.
    let drained = controller
        .process_pending_deletions(cfg.receiver_origin.as_deref())
        .unwrap();
    assert!(drained.pending_deletions.is_empty());
    assert_eq!(drained.last_deletion_failure, FailureKind::None);
    assert!(drained.last_deletion_success_at_ms.is_some());

    // Disabling stops telemetry and deletes the active installation ID immediately.
    let disabled_cfg = UsageReportingConfig {
        basic: false,
        camera_compatibility: false,
        feature_adoption: false,
        consented_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        receiver_origin: cfg.receiver_origin,
    };
    let after_disable = controller.apply_consent_transition(&disabled_cfg).unwrap();
    assert!(after_disable.identity.is_none());
    assert!(after_disable.pending_deletions.is_empty());
    assert_eq!(
        after_disable.lifecycle_state(false, false),
        LifecycleState::Disabled
    );

    // Debug output must never leak installation IDs or proof tokens.
    let debug = format!("{full:?}");
    assert!(!debug.contains(&initial_id));
    assert!(!debug.contains(&full.pending_deletions[0].installation_token));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn config_validation_roundtrip_revision_conflict_and_renewal_gating() {
    let dir = temp_dir("config");
    let config_path = dir.join("config.toml");
    let initial_toml = r#"
unrelated = "keep-me"
host = "127.0.0.1"
port = 8081
"#;
    std::fs::write(&config_path, initial_toml).unwrap();
    let root: toml::Table = toml::from_str(initial_toml).unwrap();
    let rev = table_revision(&root);

    // Upgrade from existing config without [usage_reporting] defaults all categories to false.
    let loaded = config::load_config(&config_path).unwrap();
    assert!(!loaded.usage_reporting.any_enabled());
    assert_eq!(loaded.usage_reporting.consented_disclosure_revision, 0);

    // Enabling camera_compatibility without basic is rejected.
    let invalid_dep = UsageReportingConfig {
        basic: false,
        camera_compatibility: true,
        feature_adoption: false,
        consented_disclosure_revision: 1,
        receiver_origin: None,
    };
    assert!(invalid_dep.validate().is_err());

    // Simulated schema renewal: consented rev 1 against future rev 2 requires renewed consent.
    let existing = UsageReportingConfig {
        basic: true,
        camera_compatibility: true,
        feature_adoption: false,
        consented_disclosure_revision: 1,
        receiver_origin: Some("https://usage.example".to_owned()),
    };
    assert!(!existing.renewal_required(1));
    assert!(existing.renewal_required(2));
    assert!(existing.validate_against_disclosure(2).is_ok());

    // Wrong revision fails cleanly without modifying disk.
    assert_eq!(
        update_with_writer(&config_path, "stale-revision", existing.clone(), |_, _| Ok(
            ()
        ),)
        .err(),
        Some(UpdateError::RevisionConflict)
    );

    // Failed writer leaves existing config.toml intact.
    assert_eq!(
        update_with_writer(&config_path, &rev, existing.clone(), |_, _| {
            Err(std::io::Error::other("simulated disk full"))
        })
        .err(),
        Some(UpdateError::Persistence)
    );
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), initial_toml);

    // Valid atomic update persists [usage_reporting] and preserves unrelated sections.
    let (updated_cfg, next_rev) =
        config::usage_reporting::update(&config_path, &rev, existing.clone()).unwrap();
    assert_ne!(next_rev, rev);
    assert_eq!(updated_cfg.usage_reporting, existing);
    let persisted: toml::Table =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(persisted["unrelated"].as_str(), Some("keep-me"));

    // Invalid receiver origin rejects private values without echoing them.
    let bad_origin = UsageReportingConfig {
        basic: true,
        camera_compatibility: false,
        feature_adoption: false,
        consented_disclosure_revision: 1,
        receiver_origin: Some("https://user:SECRET_CANARY@usage.example/path".to_owned()),
    };
    let err = bad_origin.validate().unwrap_err();
    assert!(!err.to_string().contains("SECRET_CANARY"));
    assert!(!Config::default().usage_reporting.any_enabled());

    let _ = std::fs::remove_dir_all(dir);
}
