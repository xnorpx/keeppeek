use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use keeppeek::{
    config::cloud_notifications::Settings,
    notifications::{
        Cooldown, CooldownScope, Lifecycle, Runtime, Stage,
        cloud::{CloudSignalRuntime, SignalTransport},
        cloud_limits::{NVR_DAILY_LIMIT, NVR_HOURLY_LIMIT, ROLLING_DAY_MS, ROLLING_HOUR_MS},
        cloud_protocol::CANONICAL_BODY_BYTES,
        cloud_state::SignalDeliveryOutcome,
        model::{
            Action, AttachmentPolicy, Candidate, Channel, EnrichmentPolicy, FailurePolicy, Filter,
            Rule, Schedule, Severity, Template, Trigger,
        },
    },
};

const TEST_SEED: [u8; 32] = [0x55; 32];
const TEST_NVR_ID: &str = "01020304-0506-0708-090a-0b0c0d0e0f10";

struct CountingAcceptTransport {
    sent: AtomicUsize,
}

impl SignalTransport for CountingAcceptTransport {
    fn send_signal(
        &self,
        _service_origin: &str,
        _body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome {
        self.sent.fetch_add(1, Ordering::Relaxed);
        SignalDeliveryOutcome::Accepted
    }
}

fn unix_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn setup_runtime(
    name: &str,
    now_ms: i64,
) -> (PathBuf, CloudSignalRuntime, Arc<CountingAcceptTransport>) {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-cloud-limits-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    let secrets_path = dir.join("secrets.toml");
    std::fs::write(
        &secrets_path,
        format!("CLOUD_KEY = \"{}\"\n", STANDARD.encode(TEST_SEED)),
    )
    .unwrap();
    let settings = Settings {
        revision: 1,
        enabled: true,
        image_previews_enabled: false,
        service_origin: Some("https://service.keeppeek.example".to_owned()),
        nvr_id: Some(TEST_NVR_ID.to_owned()),
        signing_key_version: Some(1),
        signing_key: Some("{secret:CLOUD_KEY}".to_owned()),
    };
    let transport = Arc::new(CountingAcceptTransport {
        sent: AtomicUsize::new(0),
    });
    let cloud = CloudSignalRuntime::open_with_transport(
        config_path,
        &dir.join("cloud-notifications.db"),
        settings,
        transport.clone(),
        now_ms,
    )
    .unwrap();
    (dir, cloud, transport)
}

fn stage_concurrent_storm(cloud: &CloudSignalRuntime, rule_id: &'static str, base_ms: i64) {
    let mut producers = Vec::new();
    for producer in 0..2_u64 {
        let cloud_clone = cloud.clone();
        producers.push(std::thread::spawn(move || {
            for index in 0..5_000_u64 {
                let seq = producer * 5_000 + index;
                assert!(cloud_clone.stage_logical_event(
                    &format!("logical-{seq}"),
                    rule_id,
                    &format!("evt-{seq}"),
                    "cam-1",
                    1,
                    base_ms + i64::try_from(seq).unwrap(),
                ));
            }
        }));
    }
    for producer in producers {
        producer.join().unwrap();
    }
}

#[test]
fn ten_thousand_concurrent_candidates_bound_pending_state_and_preserve_browser_delivery() {
    // Align test timestamps with wall-clock time because Runtime::open_with_cloud runs
    // CloudSignalWorker against unix_time_ms().
    let base_now_ms = unix_time_ms().saturating_sub(20_000);
    let (dir, cloud, _transport) = setup_runtime("storm", base_now_ms);
    let config_path = dir.join("config.toml");
    let runtime = Runtime::open_with_cloud(&config_path, cloud.clone()).unwrap();
    let handle = runtime.handle();

    let rule = Rule {
        id: "cloud-and-browser".to_owned(),
        name: "Cloud and browser".to_owned(),
        enabled: true,
        revision: 0,
        owner_id: "owner-1".to_owned(),
        triggers: vec![Trigger::EventCreated],
        filter: Filter::default(),
        schedule: Schedule {
            timezone: "UTC".to_owned(),
            active_windows: Vec::new(),
            quiet_hours: None,
        },
        cooldowns: vec![Cooldown {
            scope: CooldownScope::Event,
            duration_ms: 1_000,
        }],
        rate_limits: Vec::new(),
        critical_bypass: None,
        enrichment: EnrichmentPolicy {
            deadline_ms: 10_000,
            maximum_revisions: 4,
            maximum_attempts: 2,
            maximum_attachment_bytes: 1_048_576,
            wake_after_deadline: false,
        },
        actions: vec![
            Action {
                enabled: true,
                channel: Channel::Browser,
                destination: String::new(),
                template: Template {
                    title: "Motion".to_owned(),
                    body: "Open {{notification.deep_link}}".to_owned(),
                },
                attachment: AttachmentPolicy::Never,
                allow_second_delivery: false,
            },
            Action {
                enabled: true,
                channel: Channel::KeeppeekService,
                destination: String::new(),
                template: Template {
                    title: "Generic".to_owned(),
                    body: String::new(),
                },
                attachment: AttachmentPolicy::Never,
                allow_second_delivery: false,
            },
        ],
        failure: FailurePolicy {
            maximum_attempts: 3,
            maximum_retry_interval_ms: 5_000,
            expiry_ms: 60_000,
        },
    };
    let saved = handle.save_draft(rule, 0, base_now_ms).unwrap();
    handle
        .activate(
            "cloud-and-browser",
            "owner-1",
            0,
            saved.draft_revision,
            base_now_ms + 1,
        )
        .unwrap();

    // Two concurrent producers drive 10,000 candidates directly into the cloud staging boundary.
    stage_concurrent_storm(&cloud, "cloud-and-browser", base_now_ms + 5_000);
    cloud.flush_staged().unwrap();
    // Run one wall-clock worker step to exercise the slow-scheduler race deterministically.
    let _ = cloud.step_once(unix_time_ms(), 0.5).unwrap();
    let (pending, claimed) = cloud.store().queue_depth();
    assert!(pending <= 1, "pending queue must be <= 1, got {pending}");
    assert!(claimed <= 1, "claimed queue must be <= 1, got {claimed}");

    // Publishing through #134 runtime delivers browser notifications independently of cloud state.
    let publish_now_ms = unix_time_ms();
    handle.publish(Candidate {
        trigger: Trigger::EventCreated,
        source_id: "cam-1".to_owned(),
        source_name: Some("Front Door".to_owned()),
        source_identity: "evt-browser-check".to_owned(),
        lifecycle: Lifecycle::Event,
        event_kind: Some("person".to_owned()),
        payload: None,
        group_ids: Vec::new(),
        zone: None,
        confidence: Some(0.95),
        attachment_path: None,
        canonical_attachment: None,
        icon_key: None,
        image_available: false,
        duration_ms: None,
        severity: Severity::Info,
        reviewed: None,
        bookmarked: None,
        privacy_active: false,
        revision: 1,
        stage: Stage::Preliminary,
        occurred_at_ms: publish_now_ms,
        deep_link: "/events/evt-browser-check".to_owned(),
    });
    let inbox = handle.inbox_unrestricted("owner-1", 10).unwrap();
    assert!(
        inbox
            .items
            .iter()
            .any(|item| item.source_identity == "evt-browser-check"),
        "browser notification delivery must remain independent of cloud pacing"
    );
    let status = cloud.status(unix_time_ms());
    assert!(
        status.suppressed_last_day >= 9_998,
        "expected >= 9_998 suppressions, got {} (accepted={})",
        status.suppressed_last_day,
        status.accepted_last_day
    );

    runtime.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_stage_flush_and_worker_steps_preserve_suppressions_and_newest_alias() {
    let base_ms = 10_000_000_i64;
    let step_now_ms = base_ms + 10_000;
    let final_now_ms = base_ms + 10_001;
    let (dir, cloud, transport) = setup_runtime("concurrent-flush", base_ms);
    let done = Arc::new(AtomicBool::new(false));

    let mut flushers = Vec::new();
    for _ in 0..2 {
        let flusher_cloud = cloud.clone();
        let flusher_done = done.clone();
        flushers.push(std::thread::spawn(move || {
            while !flusher_done.load(Ordering::Acquire) {
                let _ = flusher_cloud.flush_staged().unwrap();
                let _ = flusher_cloud.step_once(step_now_ms, 0.5).unwrap();
                let _ = flusher_cloud.status(step_now_ms);
            }
        }));
    }

    stage_concurrent_storm(&cloud, "rule-1", base_ms);
    done.store(true, Ordering::Release);
    for flusher in flushers {
        flusher.join().unwrap();
    }

    assert!(cloud.stage_logical_event(
        "logical-final",
        "rule-1",
        "evt-final",
        "cam-1",
        1,
        final_now_ms,
    ));
    let final_outcome = cloud.flush_staged().unwrap().unwrap();
    let (pending, claimed) = cloud.store().queue_depth();
    assert_eq!((pending, claimed), (1, 0));

    let status = cloud.status(final_now_ms);
    assert!(status.accepted_last_day <= 1);
    assert!(status.suppressed_last_day >= 9_999);
    assert_eq!(
        usize::try_from(status.suppressed_last_day).unwrap()
            + usize::try_from(status.accepted_last_day).unwrap()
            + pending
            + claimed,
        10_001
    );
    assert_eq!(
        transport.sent.load(Ordering::Relaxed),
        usize::try_from(status.accepted_last_day).unwrap()
    );
    let alias = cloud
        .resolve_alias(&final_outcome.cloud_event_id, final_now_ms)
        .unwrap()
        .unwrap();
    assert_eq!(alias.logical_id, "logical-final");

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn controlled_clock_enforces_exact_120_per_hour_and_500_per_day_output() {
    let (dir, cloud, transport) = setup_runtime("exact-windows", 1_000_000);
    let mut now = 10_000_000_i64;
    let start = now;

    for index in 0..600_u64 {
        cloud
            .enqueue_logical_event(
                &format!("logical-{index}"),
                "rule-1",
                &format!("evt-{index}"),
                "cam-1",
                1,
                now,
            )
            .unwrap();
        let _ = cloud.step_once(now, 0.5).unwrap();
        now += 30_000;
        if index + 1 == u64::try_from(NVR_HOURLY_LIMIT).unwrap() {
            assert_eq!(transport.sent.load(Ordering::Relaxed), NVR_HOURLY_LIMIT);
        }
    }
    // Over 600 * 30s = 5 hours (< 24h), total output must cap at exactly 500/day.
    assert_eq!(transport.sent.load(Ordering::Relaxed), NVR_DAILY_LIMIT);
    let status = cloud.status(now);
    assert_eq!(
        status.accepted_last_day,
        u32::try_from(NVR_DAILY_LIMIT).unwrap()
    );

    // Advancing to exact 24h from first event unlocks slot 501; 1ms earlier stays blocked.
    let before_unlock = start + ROLLING_DAY_MS - 1;
    cloud
        .enqueue_logical_event(
            "logical-501",
            "rule-1",
            "evt-501",
            "cam-1",
            1,
            before_unlock,
        )
        .unwrap();
    assert!(!cloud.step_once(before_unlock, 0.5).unwrap());

    let at_unlock = start + ROLLING_DAY_MS;
    assert!(cloud.step_once(at_unlock, 0.5).unwrap());
    assert_eq!(transport.sent.load(Ordering::Relaxed), NVR_DAILY_LIMIT + 1);

    // Hourly test signals remain strictly separate (3/hour).
    for step in 0..3 {
        let res = cloud
            .send_test_signal(at_unlock + 1_000 + step * 1_000)
            .unwrap();
        assert!(res.accepted);
    }
    let fourth = cloud.send_test_signal(at_unlock + 5_000).unwrap();
    assert!(!fourth.accepted);
    assert_eq!(
        fourth.next_eligible_at_ms,
        Some(at_unlock + 1_000 + ROLLING_HOUR_MS)
    );

    std::fs::remove_dir_all(dir).unwrap();
}
