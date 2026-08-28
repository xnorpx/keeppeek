# Camera and stream health

KeepPeek computes camera and stream health on the server. Peek, Cameras, Health, camera
diagnosis, fleet counts, the control API, and Prometheus metrics consume that same projection.
Clients do not infer health from display text, local video playback, or issue severity.

The current health contract version is `1`. A client that cannot understand a state or reason must
treat it as `unknown`, never as healthy.

## Evidence dimensions

Health keeps these dimensions separate:

| Dimension               | Evidence                                                                                         |
| ----------------------- | ------------------------------------------------------------------------------------------------ |
| Configured and expected | Camera configuration, recording policy, and expected stream IDs                                  |
| Transport               | Router lifecycle plus the expected and connected stream IDs                                      |
| Report freshness        | Monotonic age of the latest periodic ingress report                                              |
| Frame freshness         | Monotonic age of the latest advancing frame counter                                              |
| Decodability            | Monotonic age of the latest advancing video keyframe counter                                     |
| Ingress quality         | Measured frame rate and recent reconnect, drop, and error deltas                                 |
| Recording               | Requested stream IDs, writer attempts, successful MP4 appends, and bounded writer errors         |
| Battery sleep           | Fresh registration heartbeat plus no pending wake request from the optional battery wake service |

Native events, external detectors, and other optional services have their own health. Their failure
does not make otherwise healthy media or recording appear offline.

## Canonical states

| State          | Meaning                                                                                                      |
| -------------- | ------------------------------------------------------------------------------------------------------------ |
| `starting`     | KeepPeek is inside the initial evidence grace period.                                                        |
| `healthy`      | Required transport, frame, keyframe, frame-rate, and recording evidence is current.                          |
| `degraded`     | Media is current, but one or more quality, decodability, transport, ingress, or recording dimensions failed. |
| `stale`        | A report is missing or old, or frame counters stopped advancing.                                             |
| `reconnecting` | Transport is reconnecting and recent media evidence still exists.                                            |
| `offline`      | Transport is disconnected without recent evidence.                                                           |
| `stopped`      | Media is not expected, the camera is shutting down, or a registered battery camera is sleeping.              |
| `unknown`      | The server or client lacks enough authoritative evidence.                                                    |

`healthy` is the only healthy state. `starting`, `unknown`, and `stopped` are not included in healthy
counts. A health request failure makes the current view unavailable; it does not rewrite the last
server camera state.

## Precedence

The server applies this order. The first matching rule supplies the canonical state and primary
reason code.

1. Not expected, shutting down, or stopped: `stopped`.
2. Fresh battery registration without a connected transport or pending wake request: `stopped`
   with `battery_sleeping`. An accepted wake remains pending until connected media clears it;
   failures therefore become `reconnecting` or `offline`, not sleeping.
3. During initial grace, incomplete or transiently unhealthy evidence remains `starting`; a complete
   healthy snapshot may promote immediately.
4. Reconnecting transport: `reconnecting`, or `offline` after recent evidence expires.
5. No report, an old report, or frames that stopped advancing: `stale`.
6. Missing keyframes, stalled requested recording, low frame rate, partial transport, recent errors,
   drops, or reconnects: `degraded`. The primary degraded reason is ordered as partial transport,
   missing keyframes, recording failure, low frame rate, errors, drops, then reconnects; all matching
   codes remain in `reason_codes`.
7. Required evidence unavailable: `unknown`.
8. All required evidence current: `healthy`.

A connected transport cannot override stale frames, missing keyframes, or failed recording.
A decoded browser frame cannot promote server state. A fresh capability snapshot is not media
evidence.

## Thresholds

Ingress publishes every 10 seconds.

- A stream report is stale after 30 seconds.
- Reconnecting becomes offline when the latest stream evidence is older than 90 seconds.
- Frame freshness is
  $\max(20\text{ s}, 3 / \text{expected fps})$, capped at 120 seconds, and also requires a fresh
  stream report.
- Keyframe freshness is three configured GOP intervals, or three observed keyframe intervals when
  configuration is unavailable. It is clamped to 30 through 120 seconds.
- Recording progress is fresh for the configured short-term buffer plus flush interval plus 30
  seconds.
- Initial startup grace is two ingress report intervals, currently 20 seconds.

The first ingress report establishes the baseline for cumulative reconnect, drop, and error
counters. An initial connection is therefore not reported as a recent reconnect. During startup
grace, partial-window frame rates and other transient evidence remain `starting`; if they have not
recovered when grace expires, the canonical degraded or offline reason becomes visible.

These thresholds are the bounded debounce window. One missed 10-second ingress report does not
change a camera to `stale`; report evidence must age beyond 30 seconds. Recovery is intentionally
asymmetric: a new advancing frame or keyframe counter can recover the state immediately. The API
still returns the actual evidence ages, and clients do not add another timer or rewrite the state.

These ages use monotonic clocks inside the server. Wall-clock timestamps are included for display,
but wall-clock changes do not make stale evidence fresh.

## Operational intervals

KeepPeek projects four durable event kinds from the independent health dimensions:

| Event kind              | Scope         | Evidence                                                                   |
| ----------------------- | ------------- | -------------------------------------------------------------------------- |
| `camera_offline`        | Camera        | No expected transport is connected, or aggregate transport is disconnected |
| `stream_stale`          | Camera stream | The stream report is missing or stale, or frames stopped advancing         |
| `decode_unavailable`    | Camera stream | No recent decodable keyframe exists                                        |
| `recording_interrupted` | Camera stream | Recording was requested but writer progress stopped                        |

These event kinds remain independent. One stream can be stale and undecodable while another stays
healthy, and recording failure does not rewrite transport evidence. Native event and external
analysis failures are not camera, stream, or recording outages.

A candidate interval starts as soon as evidence fails. Monotonic elapsed time controls warning,
critical, and recovery thresholds; server wall time supplies the displayed start and end
timestamps. The default policy is:

```toml
[operational_events]
warning_hold_down_secs = 15
outage_hold_down_secs = 60
recovery_debounce_secs = 10
record_short_flaps = false

[operational_events.cameras.front-door]
warning_hold_down_secs = 30
```

Camera overrides may use the stable camera ID or IP address and inherit omitted global values. A
zero duration requests an immediate transition. Durations cannot exceed 24 hours, and the warning
hold-down cannot exceed the outage hold-down.

Recovery before the warning hold-down removes the candidate unless `record_short_flaps` is true.
Once visible, an interval keeps one event ID. Cause or severity changes increment its revision;
stable recovery increments the same event again, records the recovery timestamp, and reports total
duration from the original evidence-loss instant. Open intervals are restored from the recording
catalog after restart. Stable revision replay makes notification processing at least once while
the notification store collapses duplicate revisions.

The stored timeline renders open and recovered intervals beside recording availability. Event
payloads include severity, bounded cause and explanation, affected streams, recording consequence,
evidence source, duration, and recovery state. Health findings link to the corresponding camera and
timeline instant.

## Session durations

Camera health reports current-process session durations separately from historical archive
coverage:

- `session_duration_ms` is the longest observed ingress-worker duration for the camera's expected
  video streams;
- `recorded_main_duration_ms` and `recorded_sub_duration_ms` accumulate video sample durations only
  after those samples are successfully appended to MP4 during the current process run;
- `recorded_total_duration_ms` is the sum of all requested stream durations for that camera session.

The total can exceed camera session elapsed time when Main and Sub record concurrently. These
counters reset when KeepPeek restarts or the corresponding recording-health registry is replaced;
they do not claim lifetime or retained archive coverage. A recording indicator uses writer progress
state, not a comparison between these counters and wall time.

## Reasons and detail

Every camera and stream includes:

- `state`: the canonical state;
- `reason`: the primary stable reason code;
- `reason_codes`: all applicable ordered reason codes;
- `detail`: bounded human-readable text;
- `dimensions`: the independent raw evidence and counts.

Stable reasons include `transport_disconnected`, `transport_reconnecting`,
`transport_partially_connected`, `no_stream_report`, `stream_report_stale`,
`frames_not_arriving`, `frames_below_expected`, `keyframes_missing`, `ingress_reconnects`,
`ingress_drops`, `ingress_errors`, and `recording_not_progressing`.

Use reason codes for automation. Display text may change.

## Fleet counts

Fleet summaries name the dimension they count:

- configured cameras and expected video streams;
- cameras and streams with connected transports;
- cameras and streams with fresh frames;
- cameras and streams with recent keyframes;
- cameras and streams requested for recording;
- cameras and streams with current writer progress;
- cameras whose evidence is unknown.

Fleet aggregates count expected video streams. Audio freshness remains available on each stream
snapshot and is not folded into the video decodability or recording totals.

Unknown evidence is not counted as connected, fresh, decodable, recording, or healthy. The Cameras
`Not healthy` filter counts every state except `healthy`.

## API and metrics

`HealthCommand` returns the versioned protobuf health snapshot. Version 1 includes the canonical
`state`, stable reasons, camera and stream dimensions, media progress timestamps, and explicit fleet
counts. Unknown future state or reason strings are presented as `unknown`.

`GET /metrics` exposes the same projection. Important gauges include:

- `keeppeek_cameras_connected`, `keeppeek_cameras_fresh`,
  `keeppeek_cameras_decodable`, and `keeppeek_cameras_recording`;
- corresponding `keeppeek_video_streams_*` gauges;
- `keeppeek_camera_info` with the canonical state label;
- `keeppeek_camera_health_dimension` and
  `keeppeek_camera_health_dimension_known`;
- corresponding per-stream dimension and known gauges;
- `keeppeek_operational_event_active`, labeled by camera, stream, event kind, and severity.

There is no separate `online` or `degraded` gauge. Consumers use the canonical state label and
explicit dimensions.

## Recovery

Recovery requires new positive evidence:

- a newly advancing frame counter refreshes frame age;
- a newly advancing keyframe counter refreshes decodability;
- a successful writer append clears the previous bounded recording error;
- drop, error, and reconnect reasons use per-report deltas, so old cumulative counters do not keep a
  recovered stream degraded;
- a fresh report can recover stale state, while cached metadata cannot.

Operational interval recovery uses this canonical projection. Delivery failure cannot change or
close an interval because catalog persistence happens before notification publication and provider
work runs independently from camera media.
