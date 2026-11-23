use super::snapshot::{Measurement, Snapshot, StreamRow};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Cell, Paragraph, Row, Table},
};
use std::time::{Duration, Instant};

const WIDE_COLUMNS: u16 = 100;

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
    let stale = if elapsed >= Duration::from_secs(5) {
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
    let extra = s.omitted_warnings + s.warnings.len() - warnings;
    let omitted = s.total_rows - s.rows.len();
    let mut footer = format!(
        "{}–{} of {}   ↑↓ PgUp/PgDn   q logs   Ctrl+C stop",
        if end == 0 { 0 } else { start + 1 },
        end,
        s.total_rows
    );
    if omitted > 0 {
        footer.push_str(&format!("   {omitted} beyond row limit"));
    }
    if extra > 0 {
        footer.push_str(&format!("   {extra} more warnings"));
    }
    line(frame, height - 1, &footer, plain);
}

fn stream_table(frame: &mut Frame<'_>, rows: &[StreamRow], selected: usize, elapsed: Duration) {
    let area = frame.area();
    let wide = area.width >= WIDE_COLUMNS;
    let mut widths = vec![
        Constraint::Min(8),
        Constraint::Length(10),
        Constraint::Length(13),
    ];
    let mut titles = vec!["Camera", "Stream", "Health"];
    if wide {
        widths.extend([
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(6),
        ]);
        titles.extend(["Codec", "Resolution", "FPS"]);
    }
    widths.push(Constraint::Length(13));
    titles.push("Recv kbit/s");
    if wide {
        widths.push(Constraint::Length(9));
        titles.push("Recording");
    }
    let visible = rows.iter().enumerate().map(|(index, row)| {
        let measurement = row.measurement.as_ref();
        let mut cells = vec![
            Cell::from(row.camera.as_str()),
            Cell::from(format!("{}/{}", row.stream, row.medium)),
            Cell::from(row.health),
        ];
        if wide {
            cells.extend([
                Cell::from(row.codec.as_deref().unwrap_or("—")),
                Cell::from(row.resolution.as_deref().unwrap_or("—")),
                Cell::from(value(
                    measurement
                        .and_then(|m| m.observed_fps)
                        .map(|f| format!("{f:.1}")),
                )),
            ]);
        }
        cells.push(Cell::from(value(
            measurement.map(|m| received_rate(m, elapsed)),
        )));
        if wide {
            cells.push(Cell::from(measurement.map_or("—", recording)));
        }
        let row = Row::new(cells);
        if index == selected {
            row.style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            row
        }
    });
    let table = Table::new(visible, widths)
        .header(Row::new(titles).style(Style::new().add_modifier(Modifier::BOLD)));
    let height = u16::try_from(rows.len() + 1).unwrap_or(area.height);
    frame.render_widget(table, Rect::new(area.x, area.y + 3, area.width, height));
}

fn received_rate(m: &Measurement, elapsed: Duration) -> String {
    let stale = Duration::from_millis(m.report_age_ms).saturating_add(elapsed)
        > Duration::from_millis(m.report_freshness_ms);
    if stale {
        format!("{:.0} stale", m.kbit_per_second)
    } else {
        format!("{:.0}", m.kbit_per_second)
    }
}

fn details(row: &StreamRow, elapsed: Duration, width: u16) -> [String; 4] {
    let m = row.measurement.as_ref();
    let state = m
        .and_then(|m| m.connected)
        .map_or("unknown", |v| if v { "connected" } else { "disconnected" });
    let codec = row.codec.as_deref().unwrap_or("—");
    let resolution = row.resolution.as_deref().unwrap_or("—");
    let observed = value(m.and_then(|m| m.observed_fps).map(|v| format!("{v:.1}")));
    let expected = value(row.expected_fps);
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
