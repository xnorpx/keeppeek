# Camera and stream health

This page covers KeepPeek's server-authoritative health model (contract version `1`), fleet and client health diagnostics on `/system-health`, single-camera diagnosis on `/system-health/camera/[cameraId]`, the foreground terminal dashboard (`--tui`), durable operational outage intervals, `HealthCommand`, and Prometheus `GET /metrics`. For historical footage coverage see [Recording and evidence](./recording-and-evidence.md); for alert delivery see [Notifications and integrations](./notifications-and-integrations.md).

KeepPeek computes camera and stream health once on the server. Peek, Cameras, Health, camera diagnosis, fleet counts, the foreground TUI, the control API, and `/metrics` all project that same state. Clients never infer health from display text, local video decode, or alert severity, and must map unknown future states or reason codes to `unknown`—never `healthy`.

## Video walkthrough

Watch triaging prioritized fleet findings on **Health** (`/system-health`), opening single-camera diagnosis (`/system-health/camera/[cameraId]`), and switching an affected camera to TCP transport:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.mp4">Download the camera health triage and diagnosis demo</a>.
</video>

## Investigate an unhealthy camera

1. Open **Cameras** (`/cameras`) and filter by **Not healthy** (every state except `healthy`) to narrow affected devices, or open **Health** (`/system-health`) for prioritized fleet findings and refresh on demand.
2. On `/system-health`, switch between the **Server** tab (fleet counts, camera/stream dimensions, host CPU/memory/disks, WebRTC sources including `live_audio`) and the **Client** tab (this browser's WebRTC session, ICE state, inbound tracks, receive bitrate, packet loss, jitter, RTT, and decoder implementation).
3. Select a camera finding to open **Camera diagnosis** (`/system-health/camera/[cameraId]`), which presents server-observed **TRANSPORT**, **FRAMES**, **DECODABLE**, and **RECORDING** evidence, stream drop/error counters, **Pre-recording** buffer history/memory state, and an ordered checklist:
   1. **Open the camera on this network** (`http://<ip>`) to verify power and reachability.
   2. Note that credentials are write-only (no authentication probe endpoint exists).
   3. **Test TCP** (available when configured transport is UDP and `keeppeek.runtime-config.v1` is supported) to switch RTSP transport to TCP without changing other settings.
   4. **Open logs** (`/settings/logs`) for redacted server diagnostics.
4. Compare transport, advancing frames, keyframes, and recording progress separately: a connected socket can carry stale/undecodable media, and a live browser picture never proves recording. Check **Recording integrity** (`/recordings`) for retained historical coverage.
5. Inspect native camera events and external analysis services separately—their failures never mark healthy camera media or recording offline.

Health findings are live snapshot evidence and clear automatically once positive evidence recovers; they have no dismiss action (alert acknowledgement belongs to the notification inbox). Health panels also follow role permissions—if a User can view video but cannot load fleet health, check [Authentication and access control](./authentication.md). A failed refresh marks the view unavailable without rewriting the last server state.

## Foreground terminal dashboard (`--tui`)

Running KeepPeek in the foreground supports a read-only, non-blocking terminal dashboard controlled by `--tui`:

| Mode           | Behavior                                                                                                                         |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `--tui auto`   | Default. Starts the TUI when stdin, stdout, and stderr are interactive terminals with screen support; otherwise logs plain text. |
| `--tui always` | Requests the TUI; prints a notice and continues with plain logs if no interactive terminal is attached.                          |
| `--tui never`  | Runs with plain console logs only.                                                                                               |

- **Bounded collection:** Samples server health single-flight every `2 s` and derives camera warnings from live camera state rather than querying stored event history. If a sample is `>= 5 s` old or collection fails, the header retains the last successful snapshot with a `STALE` badge and sample/failure age.
- **Columns and detail:** Displays host process CPU %, RSS MiB, active viewers, connected/recording video counts, KeepPeek storage usage/pressure, and per-stream rows (camera name plus source ID on terminals `>= 115` columns wide or on name collisions, Main/Sub video or audio kind, canonical state, decimal `kbit/s` bitrate retaining stale values with a stale label, and report/frame age). Selecting a row shows source ID, media type, recording progress age, counters, and health reason. `0` is a measured value; `—` means unavailable or not applicable.
- **Controls:** `Up`/`Down` move selection, `PageUp`/`PageDown` page rows, `q` exits to plain logs (server keeps running), and `Ctrl+C` stops the server.

## Authoritative health model

### Evidence dimensions

| Dimension               | Server-observed evidence                                                                         |
| ----------------------- | ------------------------------------------------------------------------------------------------ |
| Configured and expected | Camera configuration, recording policy, and expected stream IDs                                  |
| Transport               | Router lifecycle plus expected vs. connected stream IDs                                          |
| Report freshness        | Monotonic age of the latest periodic ingress report                                              |
| Frame freshness         | Monotonic age of the latest advancing frame counter                                              |
| Decodability            | Monotonic age of the latest advancing video keyframe counter                                     |
| Ingress quality         | Measured FPS plus per-window reconnect, drop, and error deltas                                   |
| Recording               | Requested stream IDs, writer attempts, successful MP4 appends, and bounded writer errors         |
| Battery sleep           | Fresh registration heartbeat with no pending wake request from the optional battery wake service |

### Canonical states

| State          | Meaning                                                                                                   |
| -------------- | --------------------------------------------------------------------------------------------------------- |
| `starting`     | Inside initial startup evidence grace period (`20 s`).                                                    |
| `healthy`      | Only healthy state. Required transport, report, frame, keyframe, FPS, and recording evidence is current.  |
| `degraded`     | Media is current, but partial transport, keyframes, recording, FPS, errors, drops, or reconnects failed.  |
| `stale`        | Stream report is missing/stale, or frame counters stopped advancing.                                      |
| `reconnecting` | Transport is reconnecting while recent media evidence remains within the offline threshold.               |
| `offline`      | Transport is disconnected/reconnecting without recent media evidence.                                     |
| `stopped`      | Media is not expected, shutting down, stopped, or a registered battery camera is sleeping.                |
| `unknown`      | Server or client lacks authoritative evidence (never counted as connected, fresh, decodable, or healthy). |

### Precedence order

The server evaluates rules in strict order; the first match supplies canonical `state` and primary `reason`:

1. Not expected, shutting down, or stopped → `stopped` (`not_expected`).
2. Fresh battery registration with no connected transport and no pending wake → `stopped` (`battery_sleeping`). An accepted wake stays pending until connected media clears it, so wake failures become `reconnecting` or `offline`.
3. Startup grace (`< 20 s` uptime) with incomplete or transiently unhealthy evidence → `starting` (`starting`); a complete healthy snapshot promotes immediately.
4. Reconnecting transport → `reconnecting` (`transport_reconnecting`) while report age `<= 90 s`, else `offline` (`transport_disconnected`).
5. Missing lifecycle and report → `unknown` (`evidence_unavailable`); missing report (`no_stream_report`), stale report (`stream_report_stale`), or non-advancing frames (`frames_not_arriving`) → `stale`.
6. Any degraded condition → `degraded`, choosing primary `reason` in order: `transport_partially_connected`, `keyframes_missing`, `recording_not_progressing`, `frames_below_expected`, `ingress_errors`, `ingress_drops`, `ingress_reconnects` (all matching codes appear in `reason_codes`).
7. Missing required evidence dimension → `unknown` (`evidence_unavailable`).
8. All required evidence current → `healthy` (`healthy`).

A connected socket never overrides stale frames, missing keyframes, or stalled recording; a decoded browser frame or fresh capability snapshot never promotes server health.

### Thresholds, jitter, and recovery

All evidence ages use **server monotonic clocks** (wall timestamps are for display only; clock jumps never make stale evidence fresh).

| Signal                     | Threshold / rule                                                                                                                                              |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Ingress report cadence     | Published every `10 s` (first report establishes baseline for cumulative reconnect/drop/error counters).                                                      |
| Startup grace              | `20 s` (two ingress report intervals).                                                                                                                        |
| Stream report stale        | `> 30 s` (tolerates one missed `10 s` report without flapping).                                                                                               |
| Reconnecting → `offline`   | Latest stream report age `> 90 s`.                                                                                                                            |
| Frame freshness            | `max(20 s, 3 / expected_fps)`, capped at `120 s`; also requires a fresh stream report.                                                                        |
| Keyframe freshness         | `3 × configured GOP` (or `3 × observed keyframe interval`), clamped to `30`–`120 s`.                                                                          |
| Recording progress fresh   | `short_term_buffer + flush_interval + 30 s`.                                                                                                                  |
| Frame-arrival jitter (P99) | Absolute deviation from configured frame interval over `10 s` window (not WebRTC jitter); diagnostic only. Max observed frame gap `> 2 s` remains actionable. |

**Asymmetric recovery:** Failing states wait through the bounded debounce thresholds above, while positive evidence recovers immediately—an advancing frame or keyframe counter refreshes age, a successful MP4 append clears a bounded writer error, per-report deltas clear prior drop/error/reconnect penalties, and a fresh report clears stale state.

## Operational outage intervals

KeepPeek projects four independent durable event kinds from the health dimensions:

| Event kind              | Scope         | Trigger evidence                                                     |
| ----------------------- | ------------- | -------------------------------------------------------------------- |
| `camera_offline`        | Camera        | No expected transport connected, or aggregate transport disconnected |
| `stream_stale`          | Camera stream | Stream report missing/stale, or frames stopped advancing             |
| `decode_unavailable`    | Camera stream | No recent decodable keyframe                                         |
| `recording_interrupted` | Camera stream | Recording requested by policy, but writer progress stopped           |

Candidate intervals begin immediately on evidence loss and use monotonic elapsed time for hold-downs and recovery debounce (defaults under `[operational_events]`: `warning_hold_down_secs = 15`, `outage_hold_down_secs = 60`, `recovery_debounce_secs = 10`, `record_short_flaps = false`; per-camera overrides by ID or IP inherit omitted globals; bounds `0 s`–`24 h` with `warning <= outage`). See [Operational events](./configuration-reference.md#operational-events).

- Recovery before warning hold-down discards the candidate unless `record_short_flaps = true`.
- Visible intervals keep one stable event ID; severity/cause changes and stable recovery increment `revision`, recording recovery time and total duration from initial evidence loss.
- Open intervals restore from the recording catalog after restart, render alongside recording coverage on the timeline, and link from Health findings. Catalog persistence precedes notification delivery, so provider failures never alter or close health intervals.

## Session durations, fleet counts, API, and `/metrics`

**Current-process session durations** reset on server restart or registry replacement and do not claim retained archive coverage:

- `session_duration_ms`: longest observed ingress-worker duration across the camera's expected video streams.
- `recorded_main_duration_ms` / `recorded_sub_duration_ms`: video sample durations accumulated only after successful MP4 appends during the current run.
- `recorded_total_duration_ms`: sum across requested streams (can exceed elapsed wall time when Main and Sub record concurrently).

**Fleet counts** aggregate expected video streams separately per dimension (`configured`, `connected`, `fresh`, `decodable`, `recording_requested`, `recording`, `unknown`). Audio freshness stays on each stream snapshot and is never folded into video totals.

**Stable API fields:** Every camera and stream exposes `state`, primary `reason`, ordered `reason_codes`, bounded human-readable `detail`, and raw `dimensions`. Automate on `state` and `reason`/`reason_codes`, not display text. `HealthCommand` (version `1`) returns the protobuf snapshot including `WebRtcSourceHealthSnapshot.live_audio` timing occupancy and drop counters; see [Calibrating live audio](./recording-and-evidence.md#calibrating-live-audio).

**Prometheus `GET /metrics`** exposes the identical projection (with no legacy `online` or `degraded` gauge—use `camera_info` state labels and explicit dimension gauges):

| Metric family                                                                                 | Purpose                                                                            |
| --------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `keeppeek_cameras_{connected,fresh,decodable,recording_requested,recording}`                  | Fleet camera counts per independent evidence dimension.                            |
| `keeppeek_video_streams_{configured,connected,fresh,decodable,recording_requested,recording}` | Fleet expected video stream counts per dimension.                                  |
| `keeppeek_camera_info`                                                                        | Per-camera info gauge carrying canonical `state` label.                            |
| `keeppeek_camera_health_dimension` / `_known` (+ per-stream equivalents)                      | Explicit dimension values paired with availability (`_known`) gauges.              |
| `keeppeek_operational_event_active`                                                           | Active operational outage intervals labeled by camera, stream, kind, and severity. |
