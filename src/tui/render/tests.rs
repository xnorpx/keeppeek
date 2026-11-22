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
        .draw(|frame| draw(frame, Some(snapshot), nav, Duration::from_secs(6)))
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
