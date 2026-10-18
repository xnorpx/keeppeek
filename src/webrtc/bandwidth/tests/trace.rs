use super::*;

#[derive(Debug, Serialize)]
struct ControlTrace {
    seed: u64,
    initial_promotion_ms: Option<u64>,
    congestion_downgrade_ms: Option<u64>,
    recovery_promotion_ms: Option<u64>,
    transitions: u32,
    source_transition_notifications: u32,
    state_notifications: u32,
    max_queue_depth: usize,
    max_source_subscribers_per_track: usize,
    main_samples: u32,
}

#[test]
fn shared_feedback_seeded_control_traces_recover_without_oscillation() {
    let traces: Vec<_> = (1..=20).map(replay).collect();
    println!(
        "SHARED_AUTO_CONTROL_TRACE {}",
        serde_json::to_string(&serde_json::json!({
            "kind": "simulated_shared_control_admission",
            "tick_ms": 100,
            "keyframe_period_ms": 500,
            "duration_ms": 20_000,
            "runs": traces,
            "initial_promotion": summary(traces.iter().filter_map(|run| run.initial_promotion_ms)),
            "congestion_downgrade": summary(traces.iter().filter_map(|run| run.congestion_downgrade_ms)),
            "recovery_promotion": summary(traces.iter().filter_map(|run| run.recovery_promotion_ms)),
        }))
        .unwrap()
    );
    for trace in traces {
        assert!(
            trace
                .initial_promotion_ms
                .is_some_and(|delay| (3_000..=3_500).contains(&delay)),
            "{trace:?}"
        );
        assert!(
            trace
                .congestion_downgrade_ms
                .is_some_and(|delay| (1_000..=1_500).contains(&delay)),
            "{trace:?}"
        );
        assert!(
            trace
                .recovery_promotion_ms
                .is_some_and(|delay| (3_000..=3_500).contains(&delay)),
            "{trace:?}"
        );
        assert_eq!(trace.transitions, 3, "{trace:?}");
        assert_eq!(trace.source_transition_notifications, 3, "{trace:?}");
        assert!(trace.state_notifications <= 24, "{trace:?}");
        assert!(trace.max_queue_depth <= 2, "{trace:?}");
        assert!(trace.max_source_subscribers_per_track <= 2, "{trace:?}");
    }
}

fn replay(seed: u64) -> ControlTrace {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    fixture.prime();
    let mut last_snapshot = fixture.take_stream_states().pop();
    let mut last_announced_variant = fixture.active_stream(0).to_string();
    let mut trace = ControlTrace {
        seed,
        initial_promotion_ms: None,
        congestion_downgrade_ms: None,
        recovery_promotion_ms: None,
        transitions: 0,
        source_transition_notifications: 0,
        state_notifications: 0,
        max_queue_depth: 0,
        max_source_subscribers_per_track: 0,
        main_samples: 0,
    };
    let keyframe_phase_ms = seed.wrapping_mul(97) % 500;
    for time_ms in (100..=20_000).step_by(100) {
        let previous = fixture.active_stream(0);
        let budget_bps = match time_ms {
            0..6_000 | 10_000..16_000 => 6_000_000,
            6_000..10_000 => 2_000_000,
            _ => 4_600_000,
        };
        fixture.feedback(budget_bps, time_ms);
        let keyframe = (time_ms + keyframe_phase_ms) % 500 < 100;
        fixture.advance(time_ms, keyframe);
        let active = fixture.active_stream(0);
        trace.record_transition(previous, active, time_ms, keyframe);
        let states = fixture.take_stream_states();
        trace.state_notifications += u32::try_from(states.len()).unwrap();
        let changes =
            count_transition_notifications(states, &mut last_announced_variant, &mut last_snapshot);
        assert_eq!(changes, u32::from(active != previous));
        assert_eq!(last_announced_variant, active.to_string());
        trace.source_transition_notifications += changes;
        trace.main_samples += u32::from(active == StreamKind::Main);
        trace.max_queue_depth = trace.max_queue_depth.max(fixture.queue_high_water());
        trace.max_source_subscribers_per_track = trace
            .max_source_subscribers_per_track
            .max(fixture.subscriber_count(0));
        fixture.assert_subscriber_bounds();
    }
    fixture.close();
    trace
}

fn count_transition_notifications(
    states: Vec<crate::api::proto::SubscriptionStreamState>,
    last_variant: &mut String,
    last_snapshot: &mut Option<crate::api::proto::SubscriptionStreamState>,
) -> u32 {
    let mut changes = 0;
    for state in states {
        assert_eq!(state.subscription_id, "camera-0");
        assert_ne!(
            last_snapshot.as_ref(),
            Some(&state),
            "duplicate quality snapshot"
        );
        if state.active_variant_id != *last_variant {
            changes += 1;
            last_variant.clone_from(&state.active_variant_id);
        }
        *last_snapshot = Some(state);
    }
    changes
}

impl ControlTrace {
    fn record_transition(
        &mut self,
        previous: StreamKind,
        active: StreamKind,
        time_ms: u64,
        keyframe: bool,
    ) {
        if active == previous {
            return;
        }
        assert!(
            keyframe,
            "seed {} switched on a delta frame at {time_ms}ms",
            self.seed
        );
        self.transitions += 1;
        match (previous, active, time_ms) {
            (StreamKind::Sub, StreamKind::Main, 0..6_000) => {
                self.initial_promotion_ms = Some(time_ms - 100);
            }
            (StreamKind::Main, StreamKind::Sub, 6_000..10_000) => {
                self.congestion_downgrade_ms = Some(time_ms - 6_000);
            }
            (StreamKind::Sub, StreamKind::Main, 10_000..16_000) => {
                self.recovery_promotion_ms = Some(time_ms - 10_000);
            }
            _ => panic!("unexpected transition in seed {} at {time_ms}ms", self.seed),
        }
    }
}

fn summary(values: impl Iterator<Item = u64>) -> serde_json::Value {
    let mut samples: Vec<_> = values.collect();
    samples.sort_unstable();
    let completed = samples.len();
    serde_json::json!({
        "completed": completed,
        "censored": 20 - completed,
        "median_ms": samples.get(completed.saturating_sub(1) / 2),
        "p95_ms": samples.get((completed * 95).div_ceil(100).saturating_sub(1)),
    })
}
