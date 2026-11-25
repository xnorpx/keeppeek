use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn fixture(count: usize) -> Snapshot {
    let rows = (0..count)
        .map(|index| StreamRow {
            camera: format!("Camera {index}"),
            id: format!("source-{index}"),
            stream: if index % 4 < 2 { "main" } else { "sub" },
            medium: if index % 2 == 0 { "video" } else { "audio" },
            health: "healthy",
            reason: "healthy".into(),
            codec: None,
            resolution: None,
            expected_fps: None,
            measurement: None,
        })
        .collect();
    Snapshot {
        collected: Instant::now(),
        version: "test",
        uptime_s: 400,
        health: "healthy".into(),
        cpu_percent: None,
        memory_bytes: None,
        storage_used: None,
        storage_free: None,
        storage_pressure: "normal",
        storage_state: "active",
        connected_video: 254,
        expected_video: 254,
        recording_video: 127,
        viewer_sessions: 3,
        rows,
        total_rows: count,
        warnings: Vec::new(),
        omitted_warnings: 0,
    }
}

fn screen(width: u16, height: u16, snapshot: &Snapshot, nav: &mut Navigation) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, Some(snapshot), nav, Duration::from_secs(6), None))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

#[test]
fn fleet_paging_retains_identity_after_inventory_changes() {
    let mut snapshot = fixture(127 * 4);
    let mut nav = Navigation::default();
    let first = screen(80, 24, &snapshot, &mut nav);
    assert!(first[0].contains("STALE"));
    assert!(first[23].contains("of 508"));
    assert!(first[4].contains("main/video"));
    nav.step(isize::MAX, snapshot.rows.len());
    let last = screen(120, 40, &snapshot, &mut nav);
    assert!(last.iter().any(|line| line.contains("source-507")));
    assert!(last[39].contains("508 of 508"));
    snapshot.collected += Duration::from_secs(2);
    snapshot.rows.remove(0);
    snapshot.total_rows -= 1;
    let moved = screen(80, 24, &snapshot, &mut nav);
    assert!(moved.iter().any(|line| line.contains("source-507")));
    assert_eq!(nav.selected, 506);
}

#[test]
fn wide_camera_labels_leave_health_columns_visible_and_overflow_explicit() {
    let mut snapshot = fixture(super::super::snapshot::MAX_ROWS);
    snapshot.total_rows += 5;
    snapshot.rows[0].camera = "門".repeat(256);
    snapshot.rows[0].health = "degraded";
    snapshot.warnings = vec!["warning  Recording stopped".into()];
    let mut nav = Navigation::default();
    let wide = screen(120, 40, &snapshot, &mut nav);
    assert!(wide[4].contains("main/video") && wide[4].contains("degraded"));
    assert!(wide.iter().any(|line| line.contains("Recording stopped")));
    assert!(wide[39].contains("5 beyond row limit"));
    let compact = screen(80, 24, &snapshot, &mut nav);
    assert!(compact[4].contains("main/video") && compact[4].contains("degraded"));
}

#[test]
fn compact_video_details_show_recording_state_and_progress_age() {
    let mut snapshot = fixture(1);
    let row = &mut snapshot.rows[0];
    row.codec = Some("H264".into());
    row.resolution = Some("1920x1080".into());
    row.expected_fps = Some(25.0);
    row.measurement = Some(Measurement {
        connected: Some(true),
        observed_fps: Some(25.0),
        kbit_per_second: 1500.0,
        report_freshness_ms: 1000,
        frames: Some(200),
        bytes: Some(2048),
        drops: Some(0),
        errors: Some(0),
        reconnects: Some(0),
        keyframe_age_ms: Some(1000),
        keyframes_per_second: Some(1.0),
        session_ms: 8100,
        report_age_ms: 0,
        frame_age_ms: Some(40),
        recording_requested: Some(true),
        recording_progress: Some(true),
        recording_age_ms: Some(2000),
    });
    let compact = screen(80, 24, &snapshot, &mut Navigation::default());
    let detail = compact
        .iter()
        .find(|line| line.contains("Recording recording"))
        .unwrap();
    assert!(detail.contains("Progress 8s"));
    assert!(detail.contains("H264 1920x1080") && detail.contains("FPS 25.0/25"));
}

#[test]
fn small_and_empty_terminals_keep_exit_controls_visible() {
    let mut nav = Navigation::default();
    let empty = fixture(0);
    let small = screen(36, 7, &empty, &mut nav);
    assert!(small[1].contains("Enlarge terminal"));
    assert!(small[6].contains("Ctrl+C"));
    let normal = screen(80, 24, &empty, &mut nav);
    assert!(
        normal
            .iter()
            .any(|line| line.contains("No configured or observed streams"))
    );
    assert!(normal[23].contains("0–0 of 0"));
}

#[test]
fn failed_collection_keeps_last_sample_and_reports_its_own_age() {
    use crate::tui::Observation;
    use std::sync::Arc;
    use std::time::Instant;

    let initial = Arc::new(fixture(1));
    let now = Instant::now();
    let mut observation = Observation::default();
    observation.update(Some(initial.clone()), now);
    observation.update(None, now + Duration::from_secs(4));
    observation.update(None, now + Duration::from_secs(5));
    assert_eq!(observation.failed_at, Some(now + Duration::from_secs(4)));
    assert!(Arc::ptr_eq(
        observation.snapshot.as_ref().unwrap(),
        &initial
    ));

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            draw(
                frame,
                observation.snapshot.as_deref(),
                &mut Navigation::default(),
                Duration::from_secs(6),
                Some(Duration::from_secs(2)),
            );
        })
        .unwrap();
    let row = |y| {
        (0..80)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect::<String>()
    };
    assert!(row(0).contains("Health check failed 2s ago   Last sample 6s ago"));
    assert!((1..24).any(|y| row(y).contains("Camera 0")));

    observation.update(Some(Arc::new(fixture(2))), now + Duration::from_secs(8));
    assert_eq!(observation.snapshot.unwrap().rows.len(), 2);
    assert!(observation.failed_at.is_none());
}

#[test]
fn renderer_consumes_one_snapshot_and_a_new_publish_replaces_an_unread_one() {
    use crate::tui::Observation;
    use std::sync::Arc;
    use std::time::Instant;

    let now = Instant::now();
    let mut slot = Observation::default();
    assert!(!slot.update(Some(Arc::new(fixture(1))), now));
    assert!(slot.unread_snapshot);
    assert!(slot.update(Some(Arc::new(fixture(2))), now));
    assert_eq!(slot.read_for_render().snapshot.unwrap().rows.len(), 2);
    assert!(!slot.unread_snapshot);
    assert!(!slot.update(Some(Arc::new(fixture(3))), now));
    assert!(slot.unread_snapshot);
}

#[test]
fn zero_row_viewport_renders_without_underflow() {
    let snapshot = fixture(2);
    let mut nav = Navigation::default();
    let empty = screen(0, 0, &snapshot, &mut nav);
    assert!(empty.is_empty());

    let mut terminal = Terminal::new(TestBackend::new(80, 0)).unwrap();
    terminal
        .draw(|frame| draw(frame, None, &mut nav, Duration::ZERO, None))
        .unwrap();
}
