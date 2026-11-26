use super::snapshot::{Measurement, Snapshot, StreamRow};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Cell, Paragraph, Row, Table},
};
use std::time::{Duration, Instant};

const WIDE_COLUMNS: u16 = 100;
const SOURCE_COLUMNS: u16 = 115;
const STALE_SAMPLE_AGE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Navigation {
    pub selected: usize,
    pub page: usize,
    sample: Option<Instant>,
    key: Option<(String, &'static str, &'static str)>,
}

impl Navigation {
    fn update(&mut self, snapshot: &Snapshot) {
        if self.sample != Some(snapshot.collected) {
            if let Some((id, stream, medium)) = &self.key
                && let Some(index) = snapshot
                    .rows
                    .iter()
                    .position(|row| &row.id == id && row.stream == *stream && row.medium == *medium)
            {
                self.selected = index;
            }
            self.sample = Some(snapshot.collected);
        }
        self.selected = self.selected.min(snapshot.rows.len().saturating_sub(1));
        if let Some(row) = snapshot.rows.get(self.selected) {
            if !self.key.as_ref().is_some_and(|(id, stream, medium)| {
                id == &row.id && *stream == row.stream && *medium == row.medium
            }) {
                self.key = Some((row.id.clone(), row.stream, row.medium));
            }
        } else {
            self.key = None;
        }
    }

    pub fn step(&mut self, by: isize, rows: usize) {
        self.key = None;
        self.selected = self
            .selected
            .saturating_add_signed(by)
            .min(rows.saturating_sub(1));
    }
}

fn clipped(label: &str) -> &str {
    let end = label
        .char_indices()
        .nth(256)
        .map_or(label.len(), |(end, _)| end);
    &label[..end]
}

fn line(frame: &mut Frame<'_>, y: u16, label: &str, style: Style) {
    let area = frame.area();
    if y < area.height {
        frame.render_widget(
            Paragraph::new(clipped(label)).style(style),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    }
}

fn value<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "—".into(), |v| v.to_string())
}

fn age(ms: Option<u64>, elapsed: Duration) -> String {
    ms.map_or_else(
        || "—".into(),
        |n| {
            format!(
                "{}s",
                Duration::from_millis(n).saturating_add(elapsed).as_secs()
            )
        },
    )
}

fn mib(bytes: Option<u64>) -> String {
    value(bytes.map(|n| n / (1024 * 1024)))
}

const fn recording(m: &Measurement) -> &'static str {
    match (m.recording_requested, m.recording_progress) {
        (None, _) => "n/a",
        (Some(false), _) => "off",
        (Some(true), Some(true)) => "recording",
        (Some(true), Some(false)) => "stalled",
        (Some(true), None) => "unknown",
    }
}

pub(super) fn draw(
    frame: &mut Frame<'_>,
    snapshot: Option<&Snapshot>,
    nav: &mut Navigation,
    elapsed: Duration,
    failure_age: Option<Duration>,
) {
    let plain = Style::new();
    let strong = plain.add_modifier(Modifier::BOLD);
    let height = frame.area().height;
    let width = frame.area().width;
    let failure = failure_age.map(|age| format!("Health check failed {}s ago", age.as_secs()));
    let Some(s) = snapshot else {
        line(
            frame,
            0,
            failure
                .as_deref()
                .unwrap_or("KeepPeek   Waiting for server health"),
            strong,
        );
        line(
            frame,
            height.saturating_sub(1),
            "q  Plain logs    Ctrl+C  Stop server",
            plain,
        );
        return;
    };
    draw_sample_heading(frame, s, elapsed, failure);
    if height < 10 || width < 46 {
        line(frame, 1, "Enlarge terminal for stream details", plain);
        line(
            frame,
            height.saturating_sub(1),
            "q logs   Ctrl+C stop",
            plain,
        );
        return;
    }
    draw_server_summary(frame, s);
    draw_stream_page(frame, s, nav, elapsed, failure_age.is_some());
}

fn draw_sample_heading(
    frame: &mut Frame<'_>,
    s: &Snapshot,
    elapsed: Duration,
    failure: Option<String>,
) {
    let strong = Style::new().add_modifier(Modifier::BOLD);
    let stale = if elapsed >= STALE_SAMPLE_AGE {
        "  STALE"
    } else {
        ""
    };
    line(
        frame,
        0,
        &failure.map_or_else(
            || {
                format!(
                    "KeepPeek {}   {}   Uptime {}s   Sample {}s ago{stale}",
                    s.version,
                    s.health,
                    s.uptime_s,
                    elapsed.as_secs()
                )
            },
            |failure| {
                format!(
                    "{failure}   Last sample {}s ago{stale}   KeepPeek {}",
                    elapsed.as_secs(),
                    s.version
                )
            },
        ),
        strong,
    );
}

fn draw_server_summary(frame: &mut Frame<'_>, s: &Snapshot) {
    let plain = Style::new();
    line(
        frame,
        1,
        &format!(
            "Process CPU {}%   RSS {} MiB   Viewers {}   Video {}/{} connected   {} recording",
            value(s.cpu_percent.map(|v| format!("{v:.1}"))),
            mib(s.memory_bytes),
            s.viewer_sessions,
            s.connected_video,
            s.expected_video,
            s.recording_video
        ),
        plain,
    );
    line(
        frame,
        2,
        &format!(
            "KeepPeek storage {} MiB used   {} MiB free   {} / {}",
            mib(s.storage_used),
            mib(s.storage_free),
            s.storage_pressure,
            s.storage_state
        ),
        plain,
    );
}

fn draw_stream_page(
    frame: &mut Frame<'_>,
    s: &Snapshot,
    nav: &mut Navigation,
    elapsed: Duration,
    failed: bool,
) {
    let plain = Style::new();
    let strong = plain.add_modifier(Modifier::BOLD);
    let height = frame.area().height;
    let width = frame.area().width;
    let warnings = (usize::from(height) / 5).min(s.warnings.len()).min(6);
    let detail_lines = if height >= 15 { 4 } else { 1 };
    let table_top: u16 = 4;
    nav.page = usize::from(height)
        .saturating_sub(usize::from(table_top) + detail_lines + warnings + 2)
        .max(1);
    nav.update(s);
    let start = (nav.selected / nav.page) * nav.page;
    let end = (start + nav.page).min(s.rows.len());
    stream_table(
        frame,
        &s.rows[start..end],
        nav.selected.saturating_sub(start),
        elapsed,
    );
    let mut y =
        height.saturating_sub(u16::try_from(detail_lines + warnings + 2).unwrap_or_default());
    if let Some(row) = s.rows.get(nav.selected) {
        for detail in details(row, elapsed, width).into_iter().take(detail_lines) {
            line(frame, y, &detail, plain);
            y += 1;
        }
    } else {
        line(frame, y, "No configured or observed streams", plain);
        y += 1;
    }
    for warning in s.warnings.iter().take(warnings) {
        line(frame, y, warning, strong);
        y += 1;
    }
    draw_stream_footer(frame, s, start, end, warnings, elapsed, failed);
}

fn draw_stream_footer(
    frame: &mut Frame<'_>,
    s: &Snapshot,
    start: usize,
    end: usize,
    warnings: usize,
    elapsed: Duration,
    failed: bool,
) {
    let plain = Style::new();
    let height = frame.area().height;
    let extra = s
        .omitted_warnings
        .saturating_add(s.warnings.len().saturating_sub(warnings));
    let omitted = s.total_rows.saturating_sub(s.rows.len());
    let mut footer = format!(
        "{}–{} of {}   ↑↓ PgUp/PgDn   q logs   Ctrl+C stop",
        if end == 0 { 0 } else { start + 1 },
        end,
        s.total_rows
    );
    if failed {
        footer.push_str("   CHECK FAILED");
    }
    if elapsed >= STALE_SAMPLE_AGE {
        footer.push_str("   STALE");
    }
    if omitted > 0 {
        footer.push_str(&format!("   {omitted} beyond row limit"));
    }
    if extra > 0 {
        footer.push_str(&format!("   {extra} more warnings"));
    }
    line(frame, height.saturating_sub(1), &footer, plain);
}

fn table_columns(wide: bool, show_source: bool) -> (Vec<Constraint>, Vec<&'static str>) {
    let mut widths = vec![Constraint::Min(8)];
    let mut titles = vec!["Camera"];
    if show_source {
        widths.push(Constraint::Length(12));
        titles.push("Source");
    }
    widths.extend([Constraint::Length(10), Constraint::Length(12)]);
    titles.extend(["Stream", "Health"]);
    if wide {
        widths.extend([
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(9),
        ]);
        titles.extend(["Codec", "Resolution", "FPS"]);
    }
    widths.push(Constraint::Length(11));
    titles.push("Recv kbit/s");
    if wide {
        widths.push(Constraint::Length(12));
        titles.push("Recording");
    }
    widths.push(Constraint::Length(9));
    titles.push("Age");
    (widths, titles)
}

fn stream_table_row<'a>(
    row: &'a StreamRow,
    wide: bool,
    show_source: bool,
    selected: bool,
    elapsed: Duration,
) -> Row<'a> {
    let measurement = row.measurement.as_ref();
    let mut cells = vec![Cell::from(camera_cell(row, show_source))];
    if show_source {
        cells.push(Cell::from(row.id.as_str()));
    }
    cells.extend([
        Cell::from(format!("{}/{}", row.stream, row.medium)),
        Cell::from(row.health),
    ]);
    if wide {
        cells.extend([
            Cell::from(row.codec.as_deref().unwrap_or("—")),
            Cell::from(row.resolution.as_deref().unwrap_or("—")),
            Cell::from(fps_cell(row, measurement)),
        ]);
    }
    cells.push(Cell::from(value(
        measurement.and_then(|m| received_rate(m, elapsed)),
    )));
    if wide {
        cells.push(Cell::from(
            measurement.map_or_else(|| "—".into(), |m| recording_cell(m, elapsed)),
        ));
    }
    cells.push(Cell::from(
        measurement.map_or_else(|| "—".into(), |m| row_age(m, elapsed)),
    ));
    let table_row = Row::new(cells);
    if selected {
        table_row.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        table_row
    }
}

fn stream_table(frame: &mut Frame<'_>, rows: &[StreamRow], selected: usize, elapsed: Duration) {
    let area = frame.area();
    let wide = area.width >= WIDE_COLUMNS;
    let show_source = area.width >= SOURCE_COLUMNS;
    let (widths, titles) = table_columns(wide, show_source);
    let visible = rows
        .iter()
        .enumerate()
        .map(|(index, row)| stream_table_row(row, wide, show_source, index == selected, elapsed));
    let table = Table::new(visible, widths)
        .header(Row::new(titles).style(Style::new().add_modifier(Modifier::BOLD)));
    let height = u16::try_from(rows.len() + 1).unwrap_or(area.height);
    frame.render_widget(table, Rect::new(area.x, area.y + 3, area.width, height));
}

fn camera_cell(row: &StreamRow, show_source: bool) -> String {
    if row.camera.is_empty() {
        row.id.clone()
    } else if row.duplicate_name && !show_source {
        format!("{} [{}]", row.camera, row.id)
    } else {
        row.camera.clone()
    }
}

fn format_fps(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.05 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.1}")
    }
}

fn fps_cell(row: &StreamRow, m: Option<&Measurement>) -> String {
    match (m.and_then(|m| m.observed_fps), row.expected_fps) {
        (Some(observed), Some(expected)) => format!("{observed:.1}/{}", format_fps(expected)),
        (Some(observed), None) => format!("{observed:.1}"),
        (None, _) => "—".into(),
    }
}

fn row_age(m: &Measurement, elapsed: Duration) -> String {
    let report = Duration::from_millis(m.report_age_ms)
        .saturating_add(elapsed)
        .as_secs();
    if let Some(frame_ms) = m.frame_age_ms {
        let frame = Duration::from_millis(frame_ms)
            .saturating_add(elapsed)
            .as_secs();
        if frame != report {
            return format!("{report}s/{frame}s");
        }
    }
    format!("{report}s")
}

fn recording_cell(m: &Measurement, elapsed: Duration) -> String {
    match (
        m.recording_requested,
        m.recording_progress,
        m.recording_age_ms,
    ) {
        (Some(true), Some(true), Some(ms)) => format!(
            "rec {}s",
            Duration::from_millis(ms).saturating_add(elapsed).as_secs()
        ),
        (Some(true), Some(false), Some(ms)) => format!(
            "stalled {}s",
            Duration::from_millis(ms).saturating_add(elapsed).as_secs()
        ),
        _ => recording(m).into(),
    }
}

fn received_rate(m: &Measurement, elapsed: Duration) -> Option<String> {
    let kbit = m.kbit_per_second?;
    let stale = Duration::from_millis(m.report_age_ms).saturating_add(elapsed)
        > Duration::from_millis(m.report_freshness_ms);
    Some(if stale {
        format!("{kbit:.0} stale")
    } else {
        format!("{kbit:.0}")
    })
}

fn details(row: &StreamRow, elapsed: Duration, width: u16) -> [String; 4] {
    let m = row.measurement.as_ref();
    let state = m
        .and_then(|m| m.connected)
        .map_or("unknown", |v| if v { "connected" } else { "disconnected" });
    let codec = row.codec.as_deref().unwrap_or("—");
    let resolution = row.resolution.as_deref().unwrap_or("—");
    let observed = value(m.and_then(|m| m.observed_fps).map(|v| format!("{v:.1}")));
    let expected = value(row.expected_fps.map(format_fps));
    let record = m.map_or("—", recording);
    let progress = age(m.and_then(|m| m.recording_age_ms), elapsed);
    let media = if width < WIDE_COLUMNS {
        format!(
            "Recording {record}   Progress {progress}   {codec} {resolution}   FPS {observed}/{expected}"
        )
    } else {
        format!(
            "Codec {codec}   Resolution {resolution}   Ingest FPS {observed}/{expected} expected   Recording {record}   Progress {progress}"
        )
    };
    [
        format!(
            "{}   {}/{}   {state}   {}   {}",
            row.id, row.stream, row.medium, row.health, row.reason
        ),
        media,
        format!(
            "Frames {}   Bytes {}   Drops {}   Errors {}   Reconnects {}",
            value(m.and_then(|m| m.frames)),
            value(m.and_then(|m| m.bytes)),
            value(m.and_then(|m| m.drops)),
            value(m.and_then(|m| m.errors)),
            value(m.and_then(|m| m.reconnects))
        ),
        format!(
            "Report {}   Frame {}   Keyframe {} / {} Hz   Session {}",
            age(m.map(|m| m.report_age_ms), elapsed),
            age(m.and_then(|m| m.frame_age_ms), elapsed),
            age(m.and_then(|m| m.keyframe_age_ms), elapsed),
            value(
                m.and_then(|m| m.keyframes_per_second)
                    .map(|v| format!("{v:.2}"))
            ),
            age(m.map(|m| m.session_ms), Duration::ZERO)
        ),
    ]
}

#[cfg(test)]
mod tests;
