use std::net::SocketAddr;
use std::time::{Duration, Instant};

use isapi::blocking::{AlertStream, Client};
use isapi::{Credentials, PartKind};
use keeppeek::config;
use keeppeek::logging::LogHub;
use keeppeek::shutdown::Shutdown;
use serde_json::json;

const PART_COUNT_MAX: u64 = 4096;
const PAYLOAD_SIZE_BYTES_MAX: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct Counts {
    parts: u64,
    payload_bytes: u64,
    events: u64,
    json_events: u64,
    motion_active: u64,
    motion_inactive: u64,
    jpeg_parts: u64,
    opaque_parts: u64,
}

pub fn run(cli: super::Cli) -> anyhow::Result<()> {
    let configured = config::load_cameras(&cli.credentials_from)
        .map_err(|_| anyhow::anyhow!("could not load camera credentials from the configuration"))?;
    let camera = super::super::stream_test::select_camera_config(&configured, &cli.camera)?;
    let report = LogHub::default();
    report.set_sensitive_values(
        configured
            .values()
            .flatten()
            .flat_map(|camera| [camera.username.clone(), camera.password.clone()]),
    );
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.cancel())?;
    let authority = SocketAddr::new(camera.ip, camera.http_port.unwrap_or(80));
    let cancellation = shutdown.clone();
    let mut client = Client::builder(
        format!("http://{authority}"),
        Credentials::new(camera.username.clone(), camera.password.clone()),
    )
    .cancelled(move || cancellation.is_cancelled())
    .build()?;
    let lifetime = Duration::from_secs(cli.duration);
    let started = Instant::now();
    let deadline = started + lifetime;
    let mut stream = client.alert_stream(lifetime)?;
    super::emit(
        &report,
        "isapi_connected",
        json!({
            "camera": camera.display_name(), "ip": camera.ip, "http_port": authority.port(),
            "protocol": "isapi", "duration_seconds": cli.duration,
        }),
    );
    let mut counts = Counts::default();
    let result = observe(&mut stream, &shutdown, &report, deadline, &mut counts);
    drop(stream);
    let ended = result.as_ref().map_or("error", |reason| *reason);
    super::emit(
        &report,
        "isapi_summary",
        json!({
            "ended": ended, "elapsed_ms": started.elapsed().as_millis(),
            "parts": counts.parts, "payload_bytes": counts.payload_bytes,
            "events": counts.events, "xml_events": counts.events - counts.json_events,
            "json_events": counts.json_events, "motion_active_notifications": counts.motion_active,
            "motion_inactive_notifications": counts.motion_inactive,
            "jpeg_parts": counts.jpeg_parts, "opaque_parts": counts.opaque_parts,
        }),
    );
    result.map(|_| ())
}

fn observe(
    stream: &mut AlertStream,
    shutdown: &Shutdown,
    report: &LogHub,
    deadline: Instant,
    counts: &mut Counts,
) -> anyhow::Result<&'static str> {
    for _ in 0..PART_COUNT_MAX {
        if shutdown.is_cancelled() {
            return Ok("cancelled");
        }
        if Instant::now() >= deadline {
            return Ok("duration_elapsed");
        }
        let part = match stream.next_part() {
            Ok(Some(part)) => part,
            Ok(None) => return Ok("stream_closed"),
            Err(error) if error.is_timeout() && Instant::now() >= deadline => {
                return Ok("duration_elapsed");
            }
            Err(error) if error.is_cancelled() => return Ok("cancelled"),
            Err(error) => return Err(error.into()),
        };
        counts.parts += 1;
        counts.payload_bytes += u64::try_from(part.body().len())?;
        if counts.payload_bytes > PAYLOAD_SIZE_BYTES_MAX {
            anyhow::bail!("ISAPI diagnostic payload budget exceeded");
        }
        if let Some(event) = part.event()? {
            counts.events += 1;
            if part.kind() == PartKind::Json {
                counts.json_events += 1;
            }
            if event.is_motion() {
                match event.active() {
                    Some(true) => counts.motion_active += 1,
                    Some(false) => counts.motion_inactive += 1,
                    None => {}
                }
            }
            super::emit(
                report,
                "isapi_event",
                json!({
                    "event_type": event.event_type(), "state": event.state(),
                    "channel_id": event.channel_id(), "dynamic_channel_id": event.dynamic_channel_id(),
                    "camera_time": event.date_time(), "active_post_count": event.active_post_count(),
                    "detection_target": event.detection_target(),
                }),
            );
        } else if part.kind() == PartKind::Jpeg {
            counts.jpeg_parts += 1;
        } else {
            counts.opaque_parts += 1;
        }
    }
    anyhow::bail!("ISAPI diagnostic notification budget exceeded")
}
