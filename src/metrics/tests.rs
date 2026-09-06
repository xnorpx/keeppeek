mod fixtures;

const MODES: [&str; 4] = ["onvif-pullpoint", "rtsp-metadata", "vendor", "unknown"];
const STATES: [&str; 12] = [
    "starting",
    "discovered",
    "discovering",
    "observed",
    "subscribed",
    "reconnecting",
    "stopped",
    "commit_unknown",
    "delivery-stalled",
    "authentication-failed",
    "unsupported",
    "unknown",
];

#[test]
fn renders_camera_event_counters_and_gauges() {
    let health = fixtures::health();
    let reports = [fixtures::report(Some(fixtures::evidence()))];
    let output = super::encode_health_with_events(
        &health,
        super::HealthMetricSnapshots::default(),
        &reports,
    )
    .expect("event health metrics encode");

    for (name, value) in [
        ("pulls", 101),
        ("empty_pulls", 2),
        ("notifications", 103),
        ("parse_errors", 4),
        ("reconnects", 5),
        ("renewals", 106),
        ("renew_failures", 7),
        ("deduplicated", 8),
        ("metadata_bytes", 4096),
        ("metadata_documents", 110),
        ("metadata_loss", 11),
        ("metadata_errors", 12),
        ("queue_drops", 13),
        ("delivery_stalls", 14),
        ("dropped", 15),
        ("snapshots", 116),
        ("snapshot_failures", 17),
    ] {
        assert_metric(&output, name, "counter", &format!("{name}_total"), value);
    }
    for (name, value) in [
        ("pull_advertised", 1),
        ("pull_capable", 0),
        ("unsubscribed", 0),
        ("lease_milliseconds", 12345),
        ("active", 3),
        ("metadata_available", 1),
    ] {
        assert_metric(&output, name, "gauge", name, value);
    }
    assert!(output.contains("keeppeek_server_uptime_seconds 42\n"));
    assert!(!output.contains("untrusted-report-name"));
}

#[test]
fn leaves_absent_event_evidence_untouched() {
    let health = fixtures::health();
    let baseline =
        super::encode_health_metrics(&health, None, None, None, None, None, None).unwrap();
    let mut baseline_lines = baseline.lines().collect::<Vec<_>>();
    baseline_lines.sort_unstable();
    let mut unmatched = fixtures::report(Some(fixtures::evidence()));
    unmatched.ip = "192.0.2.11".parse().unwrap();

    for reports in [Vec::new(), vec![fixtures::report(None)], vec![unmatched]] {
        let output = render(&health, &reports);
        assert!(!output.contains("keeppeek_camera_events_"));
        let mut lines = output.lines().collect::<Vec<_>>();
        lines.sort_unstable();
        assert_eq!(lines, baseline_lines);
    }
}

#[test]
fn keeps_advertised_and_working_pull_evidence_separate() {
    let health = fixtures::health();
    for pull_advertised in [None, Some(false), Some(true)] {
        for pull_capable in [false, true] {
            let reports = [fixtures::report(Some(crate::camera_events::Evidence {
                mode: "onvif-pullpoint",
                state: "discovered",
                pull_advertised,
                pull_capable,
                ..Default::default()
            }))];
            let output = render(&health, &reports);
            assert_metric(
                &output,
                "pull_capable",
                "gauge",
                "pull_capable",
                u64::from(pull_capable),
            );
            if let Some(advertised) = pull_advertised {
                assert_metric(
                    &output,
                    "pull_advertised",
                    "gauge",
                    "pull_advertised",
                    u64::from(advertised),
                );
            } else {
                assert!(!output.contains("keeppeek_camera_events_pull_advertised{"));
            }
        }
    }
}

#[test]
fn renders_fixed_mode_and_state_enumerations() {
    let health = fixtures::health();
    for mode in MODES {
        for state in STATES {
            let reports = [fixtures::report(Some(crate::camera_events::Evidence {
                mode,
                state,
                ..Default::default()
            }))];
            let output = render(&health, &reports);
            assert_enumeration(&output, "mode", &MODES, mode);
            assert_enumeration(&output, "state", &STATES, state);
        }
    }
}

#[test]
fn bounds_event_series_without_exporting_peer_data() {
    let mut health = fixtures::health();
    health.cameras[0].last_error = Some("Authorization: Bearer private-auth".to_owned());
    let mut evidence = fixtures::evidence();
    let baseline = render(&health, &[fixtures::report(Some(evidence.clone()))]);
    assert_eq!(event_samples(&baseline).len(), 39);

    evidence.mode = "http://private-user:private-password@camera.invalid/?token=private-token";
    evidence.state = "<ReferenceParameters>private-reference</ReferenceParameters>";
    evidence.kinds = (0..128)
        .map(|index| format!("private-kind-{index}"))
        .collect();
    evidence.kinds.extend([
        "{secret:private-camera-password}".to_owned(),
        "ANPR private-ABC123".to_owned(),
    ]);
    let output = render(&health, &[fixtures::report(Some(evidence))]);

    assert_eq!(event_samples(&output).len(), 39);
    assert_enumeration(&output, "mode", &MODES, "unknown");
    assert_enumeration(&output, "state", &STATES, "unknown");
    for forbidden in [
        "private-",
        "camera.invalid",
        "ReferenceParameters",
        "Authorization",
        "{secret:",
        "ANPR",
        "kind=",
    ] {
        assert!(!output.contains(forbidden), "exported {forbidden}");
    }
}

#[test]
fn rebuilds_event_metrics_from_each_snapshot() {
    let health = fixtures::health();
    let mut reports = [fixtures::report(Some(fixtures::evidence()))];
    let first = render(&health, &reports);
    let repeated = render(&health, &reports);
    assert_eq!(event_samples(&first), event_samples(&repeated));

    reports[0].events = Some(crate::camera_events::Evidence {
        mode: "vendor",
        state: "observed",
        unsubscribed: true,
        ..Default::default()
    });
    let reset = render(&health, &reports);
    assert_metric(&reset, "pulls", "counter", "pulls_total", 0);
    assert_metric(&reset, "unsubscribed", "gauge", "unsubscribed", 1);
    for name in ["lease_milliseconds", "active", "metadata_available"] {
        assert_metric(&reset, name, "gauge", name, 0);
    }
    assert_enumeration(&reset, "mode", &MODES, "vendor");
    assert_enumeration(&reset, "state", &STATES, "observed");
    assert!(!reset.contains("keeppeek_camera_events_pull_advertised{"));

    reports[0].events = None;
    let absent = render(&health, &reports);
    assert!(!absent.contains("keeppeek_camera_events_"));
}

#[test]
fn matches_event_reports_to_configured_camera_identities() {
    let mut health = fixtures::health();
    health.cameras.extend(fixtures::health().cameras);
    health.cameras[1].id = "back-door".to_owned();
    health.cameras[1].name = "Back Door".to_owned();
    health.cameras[1].ip = "192.0.2.11".to_owned();
    let first = fixtures::report(Some(fixtures::evidence()));
    let mut second = fixtures::report(Some(crate::camera_events::Evidence {
        pulls: 202,
        ..fixtures::evidence()
    }));
    second.ip = "192.0.2.11".parse().unwrap();
    let mut reports = [second, first];

    let output = render(&health, &reports);
    assert_metric(&output, "pulls", "counter", "pulls_total", 101);
    assert!(output.lines().any(|line| {
        line == "keeppeek_camera_events_pulls_total{camera_id=\"back-door\",camera_name=\"Back Door\"} 202"
    }));
    assert_eq!(event_samples(&output).len(), 78);
    assert_eq!(
        output
            .lines()
            .filter(|line| *line == "# TYPE keeppeek_camera_events_pulls counter")
            .count(),
        1
    );
    assert!(!output.contains("untrusted-report-name"));

    reports[0].events = None;
    let partial = render(&health, &reports);
    assert_eq!(event_samples(&partial).len(), 39);
    assert!(
        event_samples(&partial)
            .iter()
            .all(|line| !line.contains("back-door"))
    );
}

#[test]
fn preserves_large_counters_and_saturates_gauges() {
    let health = fixtures::health();
    let reports = [fixtures::report(Some(crate::camera_events::Evidence {
        pulls: u64::MAX,
        lease_ms: u64::MAX,
        ..Default::default()
    }))];
    let output = render(&health, &reports);
    assert_metric(&output, "pulls", "counter", "pulls_total", u64::MAX);
    assert_metric(
        &output,
        "lease_milliseconds",
        "gauge",
        "lease_milliseconds",
        i64::MAX.unsigned_abs(),
    );
}

fn render(
    health: &crate::health::ServerHealthResponse,
    reports: &[crate::stats::CameraHealthReport],
) -> String {
    super::encode_health_with_events(health, super::HealthMetricSnapshots::default(), reports)
        .expect("event health metrics encode")
}

fn event_samples(output: &str) -> Vec<&str> {
    let mut samples = output
        .lines()
        .filter(|line| line.starts_with("keeppeek_camera_events_"))
        .collect::<Vec<_>>();
    samples.sort_unstable();
    samples
}

fn assert_enumeration(output: &str, name: &str, candidates: &[&str], selected: &str) {
    let metadata = format!("# TYPE keeppeek_camera_events_{name} gauge");
    assert!(output.lines().any(|line| line == metadata), "{metadata}");
    let prefix = format!("keeppeek_camera_events_{name}{{");
    assert_eq!(
        output
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .count(),
        candidates.len()
    );
    for candidate in candidates {
        let value = u64::from(*candidate == selected);
        let sample = format!(
            "{prefix}camera_id=\"front-door\",camera_name=\"Front Door\",{name}=\"{candidate}\"}} {value}"
        );
        assert!(output.lines().any(|line| line == sample), "{sample}");
    }
}

fn assert_metric(output: &str, name: &str, kind: &str, sample: &str, value: u64) {
    let metadata = format!("# TYPE keeppeek_camera_events_{name} {kind}");
    assert!(output.lines().any(|line| line == metadata), "{metadata}");
    let sample = format!(
        "keeppeek_camera_events_{sample}{{camera_id=\"front-door\",camera_name=\"Front Door\"}} {value}"
    );
    assert!(output.lines().any(|line| line == sample), "{sample}");
}
