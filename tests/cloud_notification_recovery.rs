use std::path::PathBuf;

use keeppeek::notifications::{
    cloud_limits::{
        CloudFailureCode, CloudState, INTENT_TTL_MS, NVR_TEST_HOURLY_LIMIT, PaceDecision,
    },
    cloud_protocol::parse_and_verify_signal,
    cloud_protocol::public_key_from_seed,
    cloud_state::{
        CLAIM_LEASE_MS, CloudStateStore, CommitFaultHook, SignalDeliveryOutcome, SigningMaterial,
    },
};

const TEST_SEED: [u8; 32] = [0x33; 32];
const TEST_NVR_ID: [u8; 16] = [0x11; 16];

fn temp_db(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("keeppeek-cloud-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("cloud-notifications.db");
    (dir, db)
}

const fn signing() -> SigningMaterial<'static> {
    SigningMaterial {
        nvr_id: TEST_NVR_ID,
        signing_key_version: 2,
        seed: &TEST_SEED,
    }
}

#[test]
fn commit_fault_rolls_back_enqueue_and_claim_without_corrupting_state() {
    let (dir, db) = temp_db("commit-fault");
    let fault = CommitFaultHook::default();
    let store = CloudStateStore::open_with_fault(&db, 10_000, fault.clone()).unwrap();

    fault.arm_next_failure();
    assert!(
        store
            .enqueue_logical_event("logical-1", "rule-1", "evt-1", "cam-1", 1, 10_000)
            .is_err()
    );
    assert_eq!(store.queue_depth(), (0, 0));

    let queued = store
        .enqueue_logical_event("logical-1", "rule-1", "evt-1", "cam-1", 1, 10_000)
        .unwrap();
    assert!(queued.queued);
    assert_eq!(store.queue_depth(), (1, 0));

    fault.arm_next_failure();
    assert!(
        store
            .claim_next_attempt(true, Some(signing()), 10_000)
            .is_err()
    );
    assert_eq!(store.queue_depth(), (1, 0));

    let claimed = store
        .claim_next_attempt(true, Some(signing()), 10_000)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempt_ordinal, 1);
    assert_eq!(claimed.cloud_event_id, queued.cloud_event_id);
    assert_eq!(store.queue_depth(), (0, 1));

    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn crash_after_claim_reuses_exact_109_bytes_and_preserves_attempt_budget_and_alias() {
    let (dir, db) = temp_db("crash-recovery");
    let store = CloudStateStore::open(&db, 100_000).unwrap();

    let first = store
        .enqueue_logical_event("logical-a", "rule-1", "evt-a", "cam-front", 1, 100_000)
        .unwrap();
    let second = store
        .enqueue_logical_event("logical-b", "rule-1", "evt-b", "cam-front", 1, 101_000)
        .unwrap();
    assert!(second.replaced_previous);
    assert_ne!(first.cloud_event_id, second.cloud_event_id);

    // Same logical event keeps its exact 128-bit alias and updates revision.
    let updated = store
        .enqueue_logical_event("logical-b", "rule-1", "evt-b", "cam-front", 2, 102_000)
        .unwrap();
    assert_eq!(updated.cloud_event_id, second.cloud_event_id);
    assert!(!updated.replaced_previous);

    let claimed_1 = store
        .claim_next_attempt(true, Some(signing()), 102_000)
        .unwrap()
        .unwrap();
    assert_eq!(claimed_1.attempt_ordinal, 1);
    assert_eq!(claimed_1.cloud_event_id, second.cloud_event_id);

    // Simulate abrupt process death before finish_attempt is recorded.
    drop(store);

    let recovered = CloudStateStore::open(&db, 105_000).unwrap();
    let alias = recovered
        .resolve_alias(&second.cloud_event_id, 105_000)
        .unwrap()
        .unwrap();
    assert_eq!(alias.event_id, "evt-b");
    assert_eq!(alias.event_revision, 2);

    // Recovered attempt 2 must reuse exact canonical bytes and original TTL.
    let claimed_2 = recovered
        .claim_next_attempt(true, Some(signing()), 105_000)
        .unwrap()
        .unwrap();
    assert_eq!(claimed_2.attempt_ordinal, 2);
    assert_eq!(claimed_2.canonical_body, claimed_1.canonical_body);
    assert_eq!(claimed_2.expires_at_ms, claimed_1.expires_at_ms);

    let pub_key = public_key_from_seed(&TEST_SEED).unwrap();
    let verified = parse_and_verify_signal(&claimed_2.canonical_body, &pub_key).unwrap();
    assert_eq!(verified.cloud_event_id, second.cloud_event_id);

    // Lease takeover after lease expiry consumes attempt 3 with exact same bytes.
    assert!(
        recovered
            .claim_next_attempt(true, Some(signing()), 105_000 + CLAIM_LEASE_MS - 1)
            .unwrap()
            .is_none()
    );
    let claimed_3 = recovered
        .claim_next_attempt(true, Some(signing()), 105_000 + CLAIM_LEASE_MS)
        .unwrap()
        .unwrap();
    assert_eq!(claimed_3.attempt_ordinal, 3);
    assert_eq!(claimed_3.canonical_body, claimed_1.canonical_body);

    // After attempt 3 fails transiently, no fourth attempt is permitted.
    let finish = recovered
        .finish_attempt(
            claimed_3.cloud_event_id,
            3,
            SignalDeliveryOutcome::DependencyFailure,
            true,
            125_000,
            0.5,
        )
        .unwrap();
    assert!(finish.terminal);
    assert!(!finish.accepted);
    assert_eq!(recovered.queue_depth(), (0, 0));

    drop(recovered);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restart_preserves_accepted_dedup_spent_capacity_and_clock_floor_across_disable() {
    let (dir, db) = temp_db("restart-capacity");
    let store = CloudStateStore::open(&db, 500_000).unwrap();

    let _queued = store
        .enqueue_logical_event("logical-1", "rule-1", "evt-1", "cam-1", 1, 500_000)
        .unwrap();
    let claimed = store
        .claim_next_attempt(true, Some(signing()), 500_000)
        .unwrap()
        .unwrap();
    let done = store
        .finish_attempt(
            claimed.cloud_event_id,
            1,
            SignalDeliveryOutcome::Accepted,
            true,
            501_000,
            0.0,
        )
        .unwrap();
    assert!(done.accepted);

    for step in 0..NVR_TEST_HOURLY_LIMIT {
        assert_eq!(
            store
                .reserve_test_signal(502_000 + i64::try_from(step).unwrap() * 1_000)
                .unwrap()
                .0,
            PaceDecision::Allowed
        );
    }

    // Disabling cancels pending work without erasing spent capacity or accepted IDs.
    store.cancel_pending_on_disable(510_000).unwrap();
    drop(store);

    // Reopen with a rolled-back clock.
    let reopened = CloudStateStore::open(&db, 100_000).unwrap();
    let duplicate = reopened
        .enqueue_logical_event("logical-1", "rule-1", "evt-1", "cam-1", 2, 100_000)
        .unwrap();
    assert!(duplicate.already_accepted);
    assert!(!duplicate.queued);

    let (test_decision, clamped_now) = reopened.reserve_test_signal(100_000).unwrap();
    assert!(matches!(test_decision, PaceDecision::Delayed { .. }));
    assert!(clamped_now >= 510_000);

    let status = reopened.status_snapshot(true, true, 100_000);
    assert_eq!(status.accepted_last_hour, 1);
    assert_eq!(status.accepted_last_day, 1);
    assert_eq!(status.state, CloudState::Ready);

    // Verify TTL expiry on restart when a pending intent outlives 5 minutes.
    let expiring = reopened
        .enqueue_logical_event("logical-2", "rule-1", "evt-2", "cam-1", 1, 600_000)
        .unwrap();
    assert!(expiring.queued);
    drop(reopened);

    let after_ttl = CloudStateStore::open(&db, 600_000 + INTENT_TTL_MS).unwrap();
    assert_eq!(after_ttl.queue_depth(), (0, 0));
    let expired_status = after_ttl.status_snapshot(true, true, 600_000 + INTENT_TTL_MS);
    assert_eq!(expired_status.last_failure, CloudFailureCode::Expired);

    drop(after_ttl);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn credential_rejected_recovers_after_pairing_repair_on_restart() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use keeppeek::{
        config::cloud_notifications::Settings,
        notifications::{
            cloud::{CloudSignalRuntime, SignalTransport},
            cloud_protocol::{CANONICAL_BODY_BYTES, parse_unverified_signal},
        },
    };

    struct ToggleAuthTransport {
        accept: AtomicBool,
        last_issued_at: std::sync::atomic::AtomicU64,
    }

    impl SignalTransport for ToggleAuthTransport {
        fn send_signal(
            &self,
            _service_origin: &str,
            body: &[u8; CANONICAL_BODY_BYTES],
        ) -> SignalDeliveryOutcome {
            let parsed = parse_unverified_signal(body).unwrap();
            self.last_issued_at
                .store(parsed.issued_at_secs, Ordering::Relaxed);
            if self.accept.load(Ordering::Relaxed) {
                SignalDeliveryOutcome::Accepted
            } else {
                SignalDeliveryOutcome::CredentialRejected
            }
        }
    }

    let (dir, db) = temp_db("cred-repair");
    let config_path = dir.join("config.toml");
    std::fs::write(
        dir.join("secrets.toml"),
        format!(
            "KEY_V1 = \"{}\"\nKEY_V2 = \"{}\"\n",
            STANDARD.encode(TEST_SEED),
            STANDARD.encode([0x55; 32])
        ),
    )
    .unwrap();
    let transport = Arc::new(ToggleAuthTransport {
        accept: AtomicBool::new(false),
        last_issued_at: std::sync::atomic::AtomicU64::new(0),
    });
    let settings_v1 = Settings {
        revision: 1,
        enabled: true,
        image_previews_enabled: false,
        service_origin: Some("https://service.keeppeek.example".to_owned()),
        nvr_id: Some(uuid::Uuid::from_bytes(TEST_NVR_ID).hyphenated().to_string()),
        signing_key_version: Some(1),
        signing_key: Some("{secret:KEY_V1}".to_owned()),
    };

    // Trigger HTTP 401/403 credential rejection -> latches Unavailable in cloud-notifications.db.
    let runtime = CloudSignalRuntime::open_with_transport(
        config_path.clone(),
        &db,
        settings_v1.clone(),
        transport.clone(),
        500_000,
    )
    .unwrap();
    let failed_test = runtime.send_test_signal(500_000).unwrap();
    assert!(!failed_test.accepted);
    assert_eq!(failed_test.failure, CloudFailureCode::CredentialRejected);
    assert_eq!(runtime.status(501_000).state, CloudState::Unavailable);
    drop(runtime);

    // Restart with the SAME unchanged broken pairing must stay Unavailable.
    let still_broken = CloudSignalRuntime::open_with_transport(
        config_path.clone(),
        &db,
        settings_v1,
        transport.clone(),
        510_000,
    )
    .unwrap();
    assert_eq!(still_broken.status(510_000).state, CloudState::Unavailable);
    drop(still_broken);

    // Restart after repairing pairing on disk (new key version & secret ref) clears the latch
    // automatically without deleting cloud-notifications.db, and respects monotonic clock floor.
    transport.accept.store(true, Ordering::Relaxed);
    let settings_v2 = Settings {
        revision: 2,
        enabled: true,
        image_previews_enabled: false,
        service_origin: Some("https://service.keeppeek.example".to_owned()),
        nvr_id: Some(uuid::Uuid::from_bytes(TEST_NVR_ID).hyphenated().to_string()),
        signing_key_version: Some(2),
        signing_key: Some("{secret:KEY_V2}".to_owned()),
    };
    let repaired = CloudSignalRuntime::open_with_transport(
        config_path,
        &db,
        settings_v2,
        transport.clone(),
        520_000,
    )
    .unwrap();
    let status = repaired.status(520_000);
    assert_eq!(status.state, CloudState::Ready);
    assert_eq!(status.last_failure, CloudFailureCode::None);

    // Rolled-back wall clock (100_000 < floor 520_000) still signs with monotonic issued_at >= 520.
    let ok_test = repaired.send_test_signal(100_000).unwrap();
    assert!(ok_test.accepted);
    assert!(transport.last_issued_at.load(Ordering::Relaxed) >= 520);

    drop(repaired);
    std::fs::remove_dir_all(dir).unwrap();
}
