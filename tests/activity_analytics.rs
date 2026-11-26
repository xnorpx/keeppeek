//! End-to-end correctness, timezone/DST, failure-isolation, rebuild, and authorization tests for #137.

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use keeppeek::{
    config::{AnalyticsConfig, Config},
    storage::{
        RecordingCatalog,
        analytics::{
            ActivityDimension, ActivityMeasures, ActivitySummaryQuery, AuthorizedCamera,
            AuthorizedGroup, BucketGranularity, CameraHealthStatus, ComparisonMode,
            CompletenessState, ConfidenceBand, EventContribution, ExpectedSourceState, MAX_BUCKETS,
            MAX_CAMERAS, MAX_GROUP_BY_DIMENSIONS, QueryDataSource, RollupState, WeekStart, time,
        },
        catalog::workflow::{self, EventKey, ReviewChange, ReviewFilter},
        metadata::{EventSource, TimelineEvent},
        search::EventImageFilter,
    },
};
use std::collections::{BTreeMap, BTreeSet};

fn temp_catalog(name: &str) -> (std::path::PathBuf, RecordingCatalog) {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-analytics-{name}-{}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = RecordingCatalog::open(&dir.join("recordings.db")).unwrap();
    (dir, catalog)
}

fn utc_ms(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
    Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn make_event(
    id: &str,
    revision: u64,
    camera_id: &str,
    source: EventSource,
    kind: &str,
    start_time_ms: i64,
    end_time_ms: Option<i64>,
) -> TimelineEvent {
    TimelineEvent {
        id: id.to_owned(),
        revision,
        camera_id: camera_id.to_owned(),
        stream: Some("main".to_owned()),
        source,
        kind: kind.to_owned(),
        start_time_ms,
        end_time_ms,
        confidence: Some(0.9),
        bbox: None,
        bbox_attachment_id: None,
        zone: Some("Front Porch".to_owned()),
        text: None,
        payload: None,
        attachments: Vec::new(),
        canonical_attachment_id: None,
        icon_key: "person".to_owned(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

fn cameras_map(ids: &[(&str, &str)]) -> BTreeMap<String, AuthorizedCamera> {
    ids.iter()
        .map(|(id, name)| {
            (
                (*id).to_owned(),
                AuthorizedCamera {
                    camera_id: (*id).to_owned(),
                    display_name: (*name).to_owned(),
                    group_ids: Vec::new(),
                },
            )
        })
        .collect()
}

/// Independent brute-force reference oracle over raw canonical events.
fn oracle_totals(
    events: &[TimelineEvent],
    allowed_cameras: &BTreeSet<&str>,
    start_ms: i64,
    end_ms: i64,
    now_ms: i64,
) -> ActivityMeasures {
    let mut totals = ActivityMeasures::default();
    for event in events {
        if !allowed_cameras.contains(event.camera_id.as_str()) {
            continue;
        }
        if event.start_time_ms < start_ms || event.start_time_ms >= end_ms {
            continue;
        }
        let contribution = EventContribution::from_event(event).unwrap();
        let dur = contribution.clipped_duration_ms(start_ms, end_ms, now_ms);
        totals.add_contribution(&contribution, dur).unwrap();
    }
    totals.finalize();
    totals
}

#[test]
fn dst_spring_gap_fall_repeat_leap_day_week_start_and_non_hour_offsets() {
    let ny: Tz = time::parse_timezone("America/New_York").unwrap();
    // Spring forward: 2026-03-08 02:00 does not exist in America/New_York (23 hours total).
    let spring_start = utc_ms(2026, 3, 8, 5, 0, 0);
    let spring_end = utc_ms(2026, 3, 9, 4, 0, 0);
    let spring_hours = time::build_time_buckets(
        spring_start,
        spring_end,
        ny,
        BucketGranularity::Hour,
        WeekStart::Monday,
        spring_end + 1_000,
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(spring_hours.len(), 23);
    assert!(
        spring_hours
            .iter()
            .all(|bucket| !bucket.label.contains(" 02:00 "))
    );
    let spring_days = time::build_time_buckets(
        spring_start,
        spring_end,
        ny,
        BucketGranularity::Day,
        WeekStart::Monday,
        spring_end + 1_000,
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(spring_days.len(), 1);
    assert_eq!(
        spring_days[0].end_time_ms - spring_days[0].start_time_ms,
        23 * 3_600_000
    );

    // Fall back: 2026-11-01 01:00 repeats with dst_fold 0 (-04:00) and dst_fold 1 (-05:00) (25 hours).
    let fall_start = utc_ms(2026, 11, 1, 4, 0, 0);
    let fall_end = utc_ms(2026, 11, 2, 5, 0, 0);
    let fall_hours = time::build_time_buckets(
        fall_start,
        fall_end,
        ny,
        BucketGranularity::Hour,
        WeekStart::Monday,
        fall_end + 1_000,
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(fall_hours.len(), 25);
    let repeated: Vec<_> = fall_hours
        .iter()
        .filter(|bucket| bucket.label.contains("2026-11-01 01:00"))
        .collect();
    assert_eq!(repeated.len(), 2);
    assert_eq!(repeated[0].dst_fold, 0);
    assert_eq!(repeated[0].utc_offset_seconds, -4 * 3_600);
    assert_eq!(repeated[1].dst_fold, 1);
    assert_eq!(repeated[1].utc_offset_seconds, -5 * 3_600);
    assert!(repeated[1].label.contains("DST repeat"));

    // Leap day 2024-02-29 and calendar month boundaries.
    let utc: Tz = time::parse_timezone("UTC").unwrap();
    let leap_months = time::build_time_buckets(
        utc_ms(2024, 2, 1, 0, 0, 0),
        utc_ms(2024, 3, 1, 0, 0, 0),
        utc,
        BucketGranularity::Day,
        WeekStart::Monday,
        utc_ms(2024, 3, 2, 0, 0, 0),
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(leap_months.len(), 29);
    assert_eq!(leap_months[28].label, "2024-02-29");

    // Week start Monday vs Sunday.
    let mon_weeks = time::build_time_buckets(
        utc_ms(2026, 3, 8, 0, 0, 0),
        utc_ms(2026, 3, 10, 0, 0, 0),
        utc,
        BucketGranularity::Week,
        WeekStart::Monday,
        utc_ms(2026, 3, 11, 0, 0, 0),
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(mon_weeks.len(), 2);
    let sun_weeks = time::build_time_buckets(
        utc_ms(2026, 3, 8, 0, 0, 0),
        utc_ms(2026, 3, 10, 0, 0, 0),
        utc,
        BucketGranularity::Week,
        WeekStart::Sunday,
        utc_ms(2026, 3, 11, 0, 0, 0),
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(sun_weeks.len(), 1);

    // Partial current bucket detection.
    let now_mid = utc_ms(2026, 3, 8, 12, 30, 0);
    let partial = time::build_time_buckets(
        utc_ms(2026, 3, 8, 0, 0, 0),
        utc_ms(2026, 3, 9, 0, 0, 0),
        utc,
        BucketGranularity::Day,
        WeekStart::Monday,
        now_mid,
        MAX_BUCKETS,
    )
    .unwrap();
    assert!(partial[0].partial_current);

    // Non-hour offset zone (Asia/Kolkata +05:30): day boundaries are NOT UTC-hour aligned.
    let kolkata: Tz = time::parse_timezone("Asia/Kolkata").unwrap();
    let kolkata_days = time::build_time_buckets(
        utc_ms(2026, 4, 1, 18, 30, 0),
        utc_ms(2026, 4, 3, 18, 30, 0),
        kolkata,
        BucketGranularity::Day,
        WeekStart::Monday,
        utc_ms(2026, 4, 4, 0, 0, 0),
        MAX_BUCKETS,
    )
    .unwrap();
    assert_eq!(kolkata_days.len(), 2);
    assert!(!kolkata_days[0].hour_aligned_utc);
    assert_eq!(kolkata_days[0].label, "2026-04-02");

    // Previous comparison windows (duration vs calendar).
    let prev_dur = time::previous_comparison_window(
        spring_start,
        spring_end,
        ny,
        BucketGranularity::Day,
        WeekStart::Monday,
        ComparisonMode::PreviousDuration,
    )
    .unwrap()
    .unwrap();
    assert_eq!(prev_dur, (spring_start - 23 * 3_600_000, spring_start));
    let prev_cal = time::previous_comparison_window(
        spring_start,
        spring_end,
        ny,
        BucketGranularity::Day,
        WeekStart::Monday,
        ComparisonMode::PreviousCalendar,
    )
    .unwrap()
    .unwrap();
    assert_eq!(prev_cal, (utc_ms(2026, 3, 7, 5, 0, 0), spring_start));
}

#[test]
fn query_validation_enforces_bounds_and_rejects_invalid_inputs() {
    let mut query = ActivitySummaryQuery::new(
        utc_ms(2026, 1, 1, 0, 0, 0),
        utc_ms(2026, 1, 2, 0, 0, 0),
        "Not/A_Timezone",
        BucketGranularity::Day,
        utc_ms(2026, 1, 3, 0, 0, 0),
        cameras_map(&[("cam-1", "Front")]),
    );
    assert!(query.validate().is_err());

    query.timezone = "UTC".to_owned();
    query.max_buckets = MAX_BUCKETS + 1;
    assert!(query.validate().is_err());

    query.max_buckets = MAX_BUCKETS;
    query.end_time_ms = query.start_time_ms;
    assert!(query.validate().is_err());

    query.end_time_ms = utc_ms(2037, 1, 1, 0, 0, 0);
    assert!(query.validate().is_err());

    query.end_time_ms = utc_ms(2026, 1, 2, 0, 0, 0);
    query.group_by = vec![ActivityDimension::Camera; MAX_GROUP_BY_DIMENSIONS + 1];
    assert!(query.validate().is_err());

    query.group_by.clear();
    for idx in 0..=MAX_CAMERAS {
        let id = format!("cam-{idx}");
        query.authorized_cameras.insert(
            id.clone(),
            AuthorizedCamera {
                camera_id: id.clone(),
                display_name: id,
                group_ids: Vec::new(),
            },
        );
    }
    assert!(query.validate().is_err());
}

#[test]
fn incremental_rollups_match_oracle_across_revisions_mutations_and_rebuilds() {
    let (_dir, catalog) = temp_catalog("revisions-oracle");
    let handle = catalog.handle();

    let t0 = utc_ms(2026, 5, 10, 10, 0, 0);
    let t1 = utc_ms(2026, 5, 10, 11, 0, 0);
    let t2 = utc_ms(2026, 5, 10, 12, 0, 0);
    let now_ms = utc_ms(2026, 5, 10, 12, 30, 0);

    // 1. Point event (zero-length: excluded from average duration denominator).
    let mut e_point = make_event(
        "e-point",
        1,
        "cam-a",
        EventSource::Camera,
        "motion",
        t0 + 1_000,
        Some(t0 + 1_000),
    );
    e_point.confidence = None;
    e_point.zone = None;
    handle.insert_event(e_point.clone()).unwrap();

    // 2. Closed interval event, later reclassified and moved to a new hour and zone.
    let mut e_moved = make_event(
        "e-moved",
        1,
        "cam-a",
        EventSource::KeepPeek,
        "vehicle",
        t0 + 5_000,
        Some(t0 + 25_000),
    );
    e_moved.confidence = Some(0.45);
    e_moved.zone = Some("Driveway".to_owned());
    handle.insert_event(e_moved.clone()).unwrap();

    // Reject duplicate revision (canonical policy) without double counting.
    assert!(handle.insert_event(e_moved.clone()).is_err());

    // Newer revision moves e_moved to t1, changes kind/confidence/zone/duration.
    e_moved.revision = 2;
    e_moved.kind = "person".to_owned();
    e_moved.start_time_ms = t1 + 10_000;
    e_moved.end_time_ms = Some(t1 + 70_000);
    e_moved.confidence = Some(0.92);
    e_moved.zone = Some("Porch".to_owned());
    handle.insert_event(e_moved.clone()).unwrap();

    // 3. Open interval event, later closed via close_event + thumbnail attached then detached.
    let e_closed_init = make_event(
        "e-close",
        1,
        "cam-b",
        EventSource::KeepPeek,
        "person",
        t1 + 20_000,
        None,
    );
    handle.insert_event(e_closed_init).unwrap();
    handle.close_event("e-close", t1 + 50_000).unwrap();
    handle
        .attach_event_thumbnail("e-close", "e-close.jpg", 1024)
        .unwrap();
    handle.detach_event_thumbnail("e-close").unwrap();
    let e_closed_final = handle.event_by_id("e-close").unwrap().unwrap();

    // 4. Open interval event left open at snapshot now_ms.
    let mut e_open = make_event(
        "e-open",
        1,
        "cam-b",
        EventSource::Camera,
        "person",
        t2 + 10 * 60_000,
        None,
    );
    e_open.thumbnail_filename = Some("e-open.jpg".to_owned());
    handle.insert_event(e_open.clone()).unwrap();

    // 5. Late-arriving historical event + deleted event.
    let e_late = make_event(
        "e-late",
        1,
        "cam-a",
        EventSource::Camera,
        "package",
        t0 + 2_000,
        Some(t0 + 12_000),
    );
    handle.insert_event(e_late.clone()).unwrap();
    let e_del = make_event(
        "e-del",
        1,
        "cam-a",
        EventSource::Camera,
        "animal",
        t0 + 3_000,
        Some(t0 + 8_000),
    );
    handle.insert_event(e_del).unwrap();
    assert!(handle.delete_event("e-del", "manual_delete").unwrap());

    let canonical = vec![e_point, e_moved, e_closed_final, e_open, e_late];
    let allowed: BTreeSet<&str> = ["cam-a", "cam-b"].into_iter().collect();
    let expected = oracle_totals(&canonical, &allowed, t0, t2 + 3_600_000, now_ms);

    let mut query = ActivitySummaryQuery::new(
        t0,
        t2 + 3_600_000,
        "UTC",
        BucketGranularity::Hour,
        now_ms,
        cameras_map(&[("cam-a", "Front"), ("cam-b", "Back")]),
    );
    query.include_heatmap = true;
    query.group_by = vec![ActivityDimension::Camera, ActivityDimension::EventType];

    let incremental = handle.query_activity_summary(query.clone()).unwrap();
    assert_eq!(incremental.rollup_state, RollupState::Ready);
    assert_eq!(incremental.data_source, QueryDataSource::Rollups);
    assert_eq!(incremental.totals, expected);
    assert_eq!(incremental.totals.event_count, 5);
    assert_eq!(incremental.totals.distinct_event_count, 5);
    // Point event is excluded from interval denominator; 4 interval events (1 open, 3 closed).
    assert_eq!(incremental.totals.interval_event_count, 4);
    assert_eq!(incremental.totals.open_interval_event_count, 1);
    // Retention deletion evidence is preserved for e-del.
    assert!(incremental.retention.partial_retention);
    assert_eq!(incremental.retention.deleted_event_count_in_range, 1);
    assert!(incremental.buckets[0].retention_partial);
    assert_eq!(
        incremental.buckets[0].completeness,
        CompletenessState::PartialRetention
    );
    assert!(incremental.buckets[2].partial_current);

    // Shadow rebuild twice -> must match incremental totals, buckets, and breakdowns identically!
    let status1 = handle
        .rebuild_analytics_blocking("UTC", WeekStart::Monday)
        .unwrap();
    assert_eq!(status1.state, RollupState::Ready);
    let rebuilt1 = handle.query_activity_summary(query.clone()).unwrap();
    let status2 = handle
        .rebuild_analytics_blocking("UTC", WeekStart::Monday)
        .unwrap();
    assert_eq!(status2.state, RollupState::Ready);
    let rebuilt2 = handle.query_activity_summary(query).unwrap();

    assert_eq!(incremental.totals, rebuilt1.totals);
    assert_eq!(incremental.buckets, rebuilt1.buckets);
    assert_eq!(incremental.by_camera, rebuilt1.by_camera);
    assert_eq!(incremental.by_event_type, rebuilt1.by_event_type);
    assert_eq!(incremental.by_zone, rebuilt1.by_zone);
    assert_eq!(incremental.by_confidence_band, rebuilt1.by_confidence_band);
    assert_eq!(rebuilt1.totals, rebuilt2.totals);
    assert_eq!(rebuilt1.buckets, rebuilt2.buckets);
}

#[test]
fn hybrid_non_hour_timezone_long_range_matches_oracle_exactly() {
    let (_dir, catalog) = temp_catalog("kolkata-hybrid");
    let handle = catalog.handle();

    // 10-day range (> 7-day exact fallback) in Asia/Kolkata (+05:30).
    let start_ms = utc_ms(2026, 6, 1, 18, 30, 0);
    let end_ms = utc_ms(2026, 6, 11, 18, 30, 0);
    let now_ms = utc_ms(2026, 6, 12, 0, 0, 0);

    // Place events right before and right after 18:30 UTC (midnight IST) on day 1 and day 5.
    let events = vec![
        make_event(
            "k-before",
            1,
            "cam-1",
            EventSource::Camera,
            "person",
            utc_ms(2026, 6, 2, 18, 15, 0),
            Some(utc_ms(2026, 6, 2, 18, 16, 0)),
        ),
        make_event(
            "k-after",
            1,
            "cam-1",
            EventSource::Camera,
            "person",
            utc_ms(2026, 6, 2, 18, 45, 0),
            Some(utc_ms(2026, 6, 2, 18, 46, 0)),
        ),
        make_event(
            "k-mid",
            1,
            "cam-1",
            EventSource::KeepPeek,
            "vehicle",
            utc_ms(2026, 6, 5, 10, 0, 0),
            Some(utc_ms(2026, 6, 5, 10, 1, 0)),
        ),
    ];
    for event in &events {
        handle.insert_event(event.clone()).unwrap();
    }

    let query = ActivitySummaryQuery::new(
        start_ms,
        end_ms,
        "Asia/Kolkata",
        BucketGranularity::Day,
        now_ms,
        cameras_map(&[("cam-1", "Gate")]),
    );
    let result = handle.query_activity_summary(query).unwrap();
    assert_eq!(
        result.data_source,
        QueryDataSource::HybridRollupsAndExactEdges
    );
    assert_eq!(result.buckets.len(), 10);
    // 18:15 UTC on June 2 is 23:45 IST on June 2 (bucket 0 = 2026-06-02).
    // 18:45 UTC on June 2 is 00:15 IST on June 3 (bucket 1 = 2026-06-03).
    assert_eq!(result.buckets[0].label, "2026-06-02");
    assert_eq!(result.buckets[0].measures.event_count, 1);
    assert_eq!(result.buckets[1].label, "2026-06-03");
    assert_eq!(result.buckets[1].measures.event_count, 1);
    assert_eq!(result.buckets[3].label, "2026-06-05");
    assert_eq!(result.buckets[3].measures.event_count, 1);
    assert_eq!(result.totals.event_count, 3);
}

#[test]
fn sub_hour_timezone_hour_of_day_weekday_weekend_and_heatmap_match_oracle() {
    let (_dir, catalog) = temp_catalog("sub-hour-exact");
    let handle = catalog.handle();
    // 2026-06-05 is Friday; 2026-06-07 is Sunday. Asia/Kolkata is UTC+05:30.
    // 2026-06-05 10:40Z = Fri 16:10 IST (local hour 16, weekday).
    // 2026-06-05 18:40Z = Sat 00:10 IST (local hour 0, weekend!).
    // 2026-06-07 18:40Z = Mon 00:10 IST (local hour 0, weekday!).
    let start_ms = utc_ms(2026, 6, 5, 0, 0, 0);
    let end_ms = utc_ms(2026, 6, 8, 0, 0, 0);
    let now_ms = utc_ms(2026, 6, 9, 0, 0, 0);
    for (id, ts) in [
        ("ist-fri-16", utc_ms(2026, 6, 5, 10, 40, 0)),
        ("ist-sat-00", utc_ms(2026, 6, 5, 18, 40, 0)),
        ("ist-mon-00", utc_ms(2026, 6, 7, 18, 40, 0)),
    ] {
        handle
            .insert_event(make_event(
                id,
                1,
                "cam-1",
                EventSource::Camera,
                "person",
                ts,
                Some(ts + 5_000),
            ))
            .unwrap();
    }
    let cams = cameras_map(&[("cam-1", "Gate")]);

    let hod = handle
        .query_activity_summary(ActivitySummaryQuery::new(
            start_ms,
            end_ms,
            "Asia/Kolkata",
            BucketGranularity::HourOfDay,
            now_ms,
            cams.clone(),
        ))
        .unwrap();
    assert_eq!(hod.data_source, QueryDataSource::CanonicalExact);
    assert_eq!(hod.buckets[0].measures.event_count, 2);
    assert_eq!(hod.buckets[15].measures.event_count, 0);
    assert_eq!(hod.buckets[16].measures.event_count, 1);

    let ww = handle
        .query_activity_summary(ActivitySummaryQuery::new(
            start_ms,
            end_ms,
            "Asia/Kolkata",
            BucketGranularity::WeekdayWeekend,
            now_ms,
            cams.clone(),
        ))
        .unwrap();
    assert_eq!(ww.data_source, QueryDataSource::CanonicalExact);
    assert_eq!(ww.buckets[0].measures.event_count, 2);
    assert_eq!(ww.buckets[1].measures.event_count, 1);

    let mut day_hm = ActivitySummaryQuery::new(
        start_ms,
        end_ms,
        "Asia/Kolkata",
        BucketGranularity::Day,
        now_ms,
        cams.clone(),
    );
    day_hm.include_heatmap = true;
    let day_res = handle.query_activity_summary(day_hm).unwrap();
    assert_eq!(day_res.data_source, QueryDataSource::CanonicalExact);
    let cell = |wd: u8, hr: u8| {
        day_res
            .heatmap
            .iter()
            .find(|c| c.weekday == wd && c.local_hour == hr)
            .map_or(0, |c| c.event_count)
    };
    assert_eq!(cell(5, 16), 1);
    assert_eq!(cell(5, 15), 0);
    assert_eq!(cell(6, 0), 1);
    assert_eq!(cell(1, 0), 1);

    // > 7d in a sub-hour timezone must reject heatmap / HourOfDay rather than misbucket.
    let long_end = utc_ms(2026, 6, 20, 0, 0, 0);
    let mut long_hm = ActivitySummaryQuery::new(
        start_ms,
        long_end,
        "Asia/Kolkata",
        BucketGranularity::Day,
        utc_ms(2026, 6, 21, 0, 0, 0),
        cams.clone(),
    );
    long_hm.include_heatmap = true;
    assert!(handle.query_activity_summary(long_hm).is_err());

    let long_hod = ActivitySummaryQuery::new(
        start_ms,
        long_end,
        "Asia/Kolkata",
        BucketGranularity::HourOfDay,
        utc_ms(2026, 6, 21, 0, 0, 0),
        cams,
    );
    assert!(handle.query_activity_summary(long_hod).is_err());
}

#[test]
fn quota_degradation_and_concurrent_rebuild_preserve_canonical_events() {
    let (_dir, catalog) = temp_catalog("quota-rebuild");
    let handle = catalog.handle();
    handle.set_analytics_max_rollup_rows(2).unwrap();

    let t0 = utc_ms(2026, 7, 1, 0, 0, 0);
    // First event creates 2 rollup rows (1 hourly + 1 daily): still Ready.
    handle
        .insert_event(make_event(
            "q-1",
            1,
            "cam-1",
            EventSource::Camera,
            "person",
            t0 + 1_000,
            Some(t0 + 2_000),
        ))
        .unwrap();
    assert_eq!(
        handle
            .analytics_status("UTC", WeekStart::Monday)
            .unwrap()
            .state,
        RollupState::Ready
    );

    // Second event in a different hour exceeds quota -> canonical write SUCCEEDS, analytics enters Degraded!
    handle
        .insert_event(make_event(
            "q-2",
            1,
            "cam-1",
            EventSource::Camera,
            "vehicle",
            t0 + 3_600_000,
            Some(t0 + 3_605_000),
        ))
        .unwrap();
    assert!(handle.event_by_id("q-2").unwrap().is_some());
    assert_eq!(
        handle
            .analytics_status("UTC", WeekStart::Monday)
            .unwrap()
            .state,
        RollupState::Degraded
    );

    // Degraded state refuses long-range (> 7d) rollup queries rather than serving stale totals.
    let long_query = ActivitySummaryQuery::new(
        t0,
        t0 + 10 * 86_400_000,
        "UTC",
        BucketGranularity::Day,
        t0 + 11 * 86_400_000,
        cameras_map(&[("cam-1", "Front")]),
    );
    assert!(handle.query_activity_summary(long_query).is_err());

    // Raise quota and step rebuild while concurrently inserting and deleting events!
    handle.set_analytics_max_rollup_rows(1_000).unwrap();
    handle.start_analytics_rebuild().unwrap();
    assert!(!handle.step_analytics_rebuild(1).unwrap());
    // Concurrent insert & delete during active shadow rebuild.
    handle
        .insert_event(make_event(
            "q-3",
            1,
            "cam-1",
            EventSource::Camera,
            "person",
            t0 + 7_200_000,
            Some(t0 + 7_210_000),
        ))
        .unwrap();
    handle.delete_event("q-1", "retention").unwrap();
    while !handle.step_analytics_rebuild(1).unwrap() {}

    assert_eq!(
        handle
            .analytics_status("UTC", WeekStart::Monday)
            .unwrap()
            .state,
        RollupState::Ready
    );
    let summary = handle
        .query_activity_summary(ActivitySummaryQuery::new(
            t0,
            t0 + 86_400_000,
            "UTC",
            BucketGranularity::Hour,
            t0 + 86_400_000,
            cameras_map(&[("cam-1", "Front")]),
        ))
        .unwrap();
    assert_eq!(summary.totals.event_count, 2);
}

#[test]
fn authorization_principal_workflow_isolation_and_source_health_evidence() {
    let (_dir, catalog) = temp_catalog("auth-workflow-health");
    let handle = catalog.handle();

    let t0 = utc_ms(2026, 8, 1, 0, 0, 0);
    let t_end = utc_ms(2026, 8, 1, 4, 0, 0);
    let now_ms = utc_ms(2026, 8, 1, 5, 0, 0);

    handle
        .insert_event(make_event(
            "w-1",
            1,
            "cam-pub",
            EventSource::Camera,
            "person",
            t0 + 10_000,
            Some(t0 + 20_000),
        ))
        .unwrap();
    handle
        .insert_event(make_event(
            "w-2",
            1,
            "cam-secret",
            EventSource::Camera,
            "person",
            t0 + 20_000,
            Some(t0 + 30_000),
        ))
        .unwrap();

    // Principal Alice reviews w-1; Principal Bob has NOT reviewed w-1.
    handle
        .mutate_event_reviews(
            "alice",
            vec![ReviewChange {
                key: EventKey {
                    source_id: "cam-pub".to_owned(),
                    event_id: "w-1".to_owned(),
                },
                expected_revision: 0,
                reviewed: true,
                dismissed: false,
            }],
        )
        .unwrap();

    // Record real source-health observations (never project backward as fact).
    handle
        .record_source_health_transition("cam-pub", t0, ExpectedSourceState::Healthy)
        .unwrap();
    handle
        .record_source_health_transition("cam-healthy-quiet", t0, ExpectedSourceState::Healthy)
        .unwrap();
    handle
        .record_source_health_transition(
            "cam-outage",
            t0 + 3_600_000,
            ExpectedSourceState::MissingHealth,
        )
        .unwrap();
    handle
        .record_source_health_transition("cam-no-events", t0, ExpectedSourceState::NoExpectedSource)
        .unwrap();
    // cam-unknown has NO health observations at all -> UnknownHistory.

    let cams = cameras_map(&[
        ("cam-pub", "Public"),
        ("cam-healthy-quiet", "Quiet"),
        ("cam-outage", "Outage"),
        ("cam-no-events", "Plain RTSP"),
        ("cam-unknown", "Unknown History"),
    ]);

    // Restricted user sees ZERO data from cam-secret even if requested in filter.source_ids!
    let mut restricted_query = ActivitySummaryQuery::new(
        t0,
        t_end,
        "UTC",
        BucketGranularity::Hour,
        now_ms,
        cams.clone(),
    );
    restricted_query.filter.source_ids = vec!["cam-secret".to_owned()];
    let restricted_res = handle.query_activity_summary(restricted_query).unwrap();
    assert_eq!(restricted_res.totals.event_count, 0);
    assert!(restricted_res.by_camera.is_empty());

    // Source health breakdown distinguishes all 5 statuses accurately.
    let mut health_query = ActivitySummaryQuery::new(
        t0,
        t_end,
        "UTC",
        BucketGranularity::Hour,
        now_ms,
        cams.clone(),
    );
    health_query.authorized_groups.insert(
        "g-ent".to_owned(),
        AuthorizedGroup {
            group_id: "g-ent".to_owned(),
            display_name: "Entrances".to_owned(),
            camera_ids: std::iter::once("cam-pub".to_owned()).collect(),
        },
    );
    let health_res = handle.query_activity_summary(health_query).unwrap();
    assert_eq!(health_res.totals.event_count, 1);
    assert_eq!(health_res.source_health.reporting_cameras, 1);
    assert_eq!(health_res.source_health.healthy_no_reports_cameras, 1);
    assert_eq!(health_res.source_health.missing_health_cameras, 1);
    assert_eq!(health_res.source_health.no_expected_source_cameras, 1);
    assert_eq!(health_res.source_health.unknown_history_cameras, 1);
    assert!(health_res.buckets[1].source_health_gap);
    assert_eq!(
        health_res.buckets[1].completeness,
        CompletenessState::SourceGap
    );
    assert_eq!(health_res.by_group.len(), 1);
    assert_eq!(health_res.by_group[0].measures.event_count, 1);
    let status_map: BTreeMap<_, _> = health_res
        .source_health
        .cameras
        .iter()
        .map(|row| (row.camera_id.as_str(), row.status))
        .collect();
    assert_eq!(status_map["cam-pub"], CameraHealthStatus::Reporting);
    assert_eq!(
        status_map["cam-healthy-quiet"],
        CameraHealthStatus::HealthyNoReports
    );
    assert_eq!(status_map["cam-outage"], CameraHealthStatus::MissingHealth);
    assert_eq!(
        status_map["cam-no-events"],
        CameraHealthStatus::NoExpectedSource
    );
    assert_eq!(
        status_map["cam-unknown"],
        CameraHealthStatus::UnknownHistory
    );

    // Principal-local workflow filtering: Alice sees 1 reviewed, Bob sees 0 reviewed!
    let mut alice_query = ActivitySummaryQuery::new(
        t0,
        t_end,
        "UTC",
        BucketGranularity::Hour,
        now_ms,
        cams.clone(),
    );
    let mut wf_alice = workflow::Query::new("alice");
    wf_alice.review = ReviewFilter::Reviewed;
    alice_query.filter.workflow = Some(wf_alice);
    let alice_res = handle.query_activity_summary(alice_query).unwrap();
    assert_eq!(alice_res.totals.event_count, 1);

    let mut bob_query =
        ActivitySummaryQuery::new(t0, t_end, "UTC", BucketGranularity::Hour, now_ms, cams);
    let mut wf_bob = workflow::Query::new("bob");
    wf_bob.review = ReviewFilter::Reviewed;
    bob_query.filter.workflow = Some(wf_bob);
    let bob_res = handle.query_activity_summary(bob_query).unwrap();
    assert_eq!(bob_res.totals.event_count, 0);
}

#[test]
fn analytics_config_validates_timezone_and_rollup_quota() {
    let valid: Config = toml::from_str(
        r#"
        [analytics]
        enabled = true
        timezone = "America/Los_Angeles"
        week_start = "sunday"
        max_rollup_rows = 250000
        "#,
    )
    .unwrap();
    valid.analytics.validate().unwrap();
    assert_eq!(valid.analytics.timezone, "America/Los_Angeles");
    assert_eq!(valid.analytics.week_start, WeekStart::Sunday);
    assert_eq!(valid.analytics.max_rollup_rows, 250_000);

    let bad_tz = AnalyticsConfig {
        timezone: "Mars/Olympus".to_owned(),
        ..AnalyticsConfig::default()
    };
    assert!(bad_tz.validate().is_err());

    let bad_quota = AnalyticsConfig {
        max_rollup_rows: 10,
        ..AnalyticsConfig::default()
    };
    assert!(bad_quota.validate().is_err());

    // Confidence band edge cases and image filter enum coverage.
    assert_eq!(ConfidenceBand::from_option(None), ConfidenceBand::Unknown);
    assert_eq!(
        ConfidenceBand::from_option(Some(f64::NAN)),
        ConfidenceBand::Unknown
    );
    assert_eq!(
        ConfidenceBand::from_option(Some(-0.1)),
        ConfidenceBand::Unknown
    );
    assert_eq!(ConfidenceBand::from_option(Some(0.0)), ConfidenceBand::Low);
    assert_eq!(
        ConfidenceBand::from_option(Some(0.499)),
        ConfidenceBand::Low
    );
    assert_eq!(
        ConfidenceBand::from_option(Some(0.5)),
        ConfidenceBand::Medium
    );
    assert_eq!(
        ConfidenceBand::from_option(Some(0.799)),
        ConfidenceBand::Medium
    );
    assert_eq!(ConfidenceBand::from_option(Some(0.8)), ConfidenceBand::High);
    assert_eq!(ConfidenceBand::from_option(Some(1.0)), ConfidenceBand::High);
    assert_eq!(EventImageFilter::default(), EventImageFilter::Any);
}

#[test]
fn event_metadata_search_supports_confidence_bands_and_missing_zone_drilldown() {
    use keeppeek::storage::search::{EventMetadataQuery, EventSearch};
    let (_dir, catalog) = temp_catalog("metadata-drilldown");
    let handle = catalog.handle();
    let t0 = utc_ms(2026, 9, 1, 0, 0, 0);

    let mut e_unknown = make_event(
        "d-unk",
        1,
        "cam-1",
        EventSource::Camera,
        "motion",
        t0 + 1_000,
        Some(t0 + 2_000),
    );
    e_unknown.confidence = None;
    e_unknown.zone = None;
    handle.insert_event(e_unknown).unwrap();

    let mut e_med = make_event(
        "d-med",
        1,
        "cam-1",
        EventSource::Camera,
        "person",
        t0 + 2_000,
        Some(t0 + 3_000),
    );
    e_med.confidence = Some(0.65);
    e_med.zone = Some("Porch".to_owned());
    handle.insert_event(e_med).unwrap();

    let mut e_high = make_event(
        "d-high",
        1,
        "cam-1",
        EventSource::Camera,
        "person",
        t0 + 3_000,
        Some(t0 + 4_000),
    );
    e_high.confidence = Some(0.95);
    e_high.zone = Some("Driveway".to_owned());
    handle.insert_event(e_high).unwrap();

    let search = EventSearch::new(handle);

    let mut med_q = EventMetadataQuery::new("main", t0, t0 + 60_000);
    med_q.confidence_bands = vec![ConfidenceBand::Medium];
    let med_ids: Vec<_> = search
        .search_metadata(med_q)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.event_id)
        .collect();
    assert_eq!(med_ids, ["d-med"]);

    let mut unk_q = EventMetadataQuery::new("main", t0, t0 + 60_000);
    unk_q.confidence_bands = vec![ConfidenceBand::Unknown];
    unk_q.include_missing_zone = true;
    let unk_ids: Vec<_> = search
        .search_metadata(unk_q)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.event_id)
        .collect();
    assert_eq!(unk_ids, ["d-unk"]);

    let mut combo_q = EventMetadataQuery::new("main", t0, t0 + 60_000);
    combo_q.zones = vec!["porch".to_owned()];
    combo_q.include_missing_zone = true;
    let combo_ids: Vec<_> = search
        .search_metadata(combo_q)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.event_id)
        .collect();
    assert_eq!(combo_ids, ["d-med", "d-unk"]);
}

fn seeded_benchmark_event(idx: usize, base_ms: i64, camera_count: usize) -> TimelineEvent {
    let kinds = ["person", "vehicle", "motion", "package", "animal"];
    let zones = [Some("front"), Some("driveway"), Some("yard"), None];
    let cam = format!("cam-{:03}", idx % camera_count);
    let day_offset = (idx % 365) as i64 * 86_400_000;
    let hour_offset = ((idx / 365) % 24) as i64 * 3_600_000;
    let start = base_ms + day_offset + hour_offset + ((idx % 60) as i64 * 1_000);
    let mut ev = make_event(
        &format!("ev-{idx:06}"),
        1,
        &cam,
        if idx.is_multiple_of(2) {
            EventSource::Camera
        } else {
            EventSource::KeepPeek
        },
        kinds[idx % kinds.len()],
        start,
        Some(start + 15_000),
    );
    ev.confidence = Some(((idx % 100) as f64) / 100.0);
    ev.zone = zones[idx % zones.len()].map(str::to_owned);
    ev
}

fn measure_ingest_batch(
    handle: &keeppeek::storage::RecordingCatalogHandle,
    total_events: usize,
    base_ms: i64,
    camera_count: usize,
) -> (u128, Vec<u64>) {
    let mut ingest_us = Vec::with_capacity(total_events);
    let started = std::time::Instant::now();
    for idx in 0..total_events {
        let ev = seeded_benchmark_event(idx, base_ms, camera_count);
        let t = std::time::Instant::now();
        handle.insert_event(ev).unwrap();
        ingest_us.push(t.elapsed().as_micros() as u64);
    }
    ingest_us.sort_unstable();
    (started.elapsed().as_millis(), ingest_us)
}

/// Measures baseline event ingest (rollups bypassed) versus active incremental rollups.
#[test]
#[ignore]
fn benchmark_analytics_ingest_baseline_delta() {
    let base_ms = utc_ms(2025, 1, 1, 0, 0, 0);
    let events = 2_000usize;
    let cameras = 127usize;

    let (base_dir, base_catalog) = temp_catalog("perf-baseline");
    let base_handle = base_catalog.handle();
    // Force rollups bypassed (`degraded`) via a 1-row quota so canonical-only ingest is measured.
    base_handle.set_analytics_max_rollup_rows(1).unwrap();
    base_handle
        .insert_event(seeded_benchmark_event(events + 1, base_ms, cameras))
        .unwrap();
    assert_eq!(
        base_handle
            .analytics_status("UTC", WeekStart::Monday)
            .unwrap()
            .state,
        RollupState::Degraded
    );
    let (base_ms_total, base_us) = measure_ingest_batch(&base_handle, events, base_ms, cameras);
    let base_bytes = std::fs::metadata(base_dir.join("recordings.db"))
        .unwrap()
        .len();

    let (rollup_dir, rollup_catalog) = temp_catalog("perf-rollups");
    let (rollup_ms_total, rollup_us) =
        measure_ingest_batch(&rollup_catalog.handle(), events, base_ms, cameras);
    let rollup_bytes = std::fs::metadata(rollup_dir.join("recordings.db"))
        .unwrap()
        .len();

    println!(
        "BASELINE events={events} cameras={cameras} total_ms={base_ms_total} p50_us={} p95_us={} max_us={} db_bytes={base_bytes}",
        base_us[base_us.len() / 2],
        base_us[(base_us.len() * 95) / 100],
        base_us[base_us.len() - 1]
    );
    println!(
        "WITH_ROLLUPS events={events} cameras={cameras} total_ms={rollup_ms_total} p50_us={} p95_us={} max_us={} db_bytes={rollup_bytes} derived_delta_bytes={}",
        rollup_us[rollup_us.len() / 2],
        rollup_us[(rollup_us.len() * 95) / 100],
        rollup_us[rollup_us.len() - 1],
        rollup_bytes.saturating_sub(base_bytes)
    );
}

/// Reproducible release performance harness (127 cameras, 30 query runs across day/month/year/DST).
#[test]
#[ignore]
fn benchmark_analytics_scale_and_latency() {
    use std::time::Instant;
    let (dir, catalog) = temp_catalog("perf-bench");
    let handle = catalog.handle();
    let base_ms = utc_ms(2025, 1, 1, 0, 0, 0);
    let total_events = 25_000usize;
    let camera_count = 127usize;

    let (ingest_total_ms, ingest_us) =
        measure_ingest_batch(&handle, total_events, base_ms, camera_count);
    let cams: BTreeMap<String, AuthorizedCamera> = (0..camera_count)
        .map(|i| {
            let id = format!("cam-{i:03}");
            (
                id.clone(),
                AuthorizedCamera {
                    camera_id: id.clone(),
                    display_name: id,
                    group_ids: Vec::new(),
                },
            )
        })
        .collect();

    let rebuild_start = Instant::now();
    let status = handle
        .rebuild_analytics_blocking("UTC", WeekStart::Monday)
        .unwrap();
    let rebuild_ms = rebuild_start.elapsed().as_millis();
    let db_bytes = std::fs::metadata(dir.join("recordings.db")).unwrap().len();

    let windows = [
        (
            "day_utc",
            base_ms,
            base_ms + 86_400_000,
            "UTC",
            BucketGranularity::Hour,
        ),
        (
            "month_utc",
            base_ms,
            base_ms + 31 * 86_400_000,
            "UTC",
            BucketGranularity::Day,
        ),
        (
            "year_utc",
            base_ms,
            base_ms + 365 * 86_400_000,
            "UTC",
            BucketGranularity::Month,
        ),
        (
            "dst_ny",
            utc_ms(2025, 3, 1, 0, 0, 0),
            utc_ms(2025, 4, 1, 0, 0, 0),
            "America/New_York",
            BucketGranularity::Day,
        ),
    ];
    println!(
        "INGEST events={total_events} cameras={camera_count} total_ms={ingest_total_ms} p50_us={} p95_us={} max_us={}",
        ingest_us[ingest_us.len() / 2],
        ingest_us[(ingest_us.len() * 95) / 100],
        ingest_us[ingest_us.len() - 1]
    );
    println!(
        "REBUILD events={total_events} rebuild_ms={rebuild_ms} rollup_rows={} db_bytes={db_bytes}",
        status.rollup_row_count
    );
    for (label, s, e, tz, gran) in windows {
        let mut times_us = Vec::with_capacity(30);
        let mut last_count = 0u64;
        for _ in 0..30 {
            let mut q = ActivitySummaryQuery::new(s, e, tz, gran, e + 1_000, cams.clone());
            q.comparison = ComparisonMode::PreviousDuration;
            q.include_heatmap = label != "year_utc";
            let t = Instant::now();
            let res = handle.query_activity_summary(q).unwrap();
            times_us.push(t.elapsed().as_micros() as u64);
            last_count = res.totals.event_count;
        }
        times_us.sort_unstable();
        println!(
            "QUERY window={label} runs=30 events={last_count} p50_us={} p95_us={} max_us={}",
            times_us[15], times_us[28], times_us[29]
        );
    }
}
