# Native camera events

This chapter covers ingesting detections and alarm transitions reported directly by cameras over Reolink Baichuan, Hikvision/Annke/HiWatch ISAPI, generic ONVIF PullPoint, and RTSP ONVIF metadata tracks without an external inference service. Native event processing runs independently of media ingest—an event failure never restarts healthy video or audio—and KeepPeek never modifies camera detection rules, regions, or schedules automatically.

Supported detections enter the same catalog as external events and flow to **Events** (`/events`), **Keep** (`/keep`), notification rules, and MQTT. Compare native event status separately from [Camera and stream health](./camera-health.md).

## Supported adapters and capabilities

| Adapter                                 | Transport & auth                                                                                                                                      | Recognized event kinds                                                                                                                                                                                                                                                        | Images & channel scope                                                                                                                          |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| **Reolink (Baichuan / `reo-proto`)**    | Native Baichuan alarm push (`StartMotionAlarm` / `AlarmEventList`) over the camera session.                                                           | `person`, `vehicle`, `animal`, `doorbell_press` (point event from `visitor`), plus `motion` when `record_generic_motion_events = true`.                                                                                                                                       | Scoped to the camera's configured Baichuan channel. No automatic failover from a failing Baichuan media session.                                |
| **Hikvision / Annke / HiWatch (ISAPI)** | Persistent `GET /ISAPI/Event/notification/alertStream` (multipart XML/JSON, HTTP Digest; no Basic downgrade) or opt-in `[isapi_callbacks]` HTTP push. | `person`, `vehicle`, `line_crossing`, `intrusion`, `region_entry`, `region_exit`, `tamper`, `alarm`, ANPR, named target collections, plus opt-in `motion` (VMD).                                                                                                              | Up to 16 explicitly correlated JPEGs per event. Channel derived from matching `101/102` (ch 1) or `201/202` (ch 2) URLs; conflicting URLs fail. |
| **Generic ONVIF PullPoint**             | ONVIF Event Service subscription (WS-Security UsernameToken + HTTP Digest challenge; no Basic/token-free downgrade; 90s lease, 1–30s backoff).        | 18 namespace-aware kinds: `motion`, `tamper`, `digital_input`, `audio_detected`, `video_loss`, `line_crossing`, `intrusion`, `region_entry`, `region_exit`, `loitering`, `person`, `vehicle`, `animal`, `face`, `license_plate`, `package`, `object_count`, `doorbell_press`. | 0–1 optional JPEG snapshot (`<= 1 MiB`, 2s budget). One subscription per device IP; narrow multi-channel devices with opaque `source_tokens`.   |
| **RTSP metadata (`rtsp-metadata`)**     | ONVIF metadata tracks on existing RTSP workers (supports `vnd.onvif.metadata+gzip`; EXI unsupported). Does not contact an ONVIF Event Service.        | Same namespace-aware ONVIF event and Profile T/M object mapping as PullPoint.                                                                                                                                                                                                 | Uses one active main/sub metadata consumer with a 5-second inactivity handoff.                                                                  |

Unknown topics, arbitrary vendor namespaces, PTZ status, and heartbeats (`videoloss/inactive`, `heartBeat/active`) never become invented motion events. Hardware verification across non-fixture ONVIF models and smart-rule firmware dialects remains model- and firmware-dependent; fixture tests are not firmware certification.

## Enable and verify events

1. Add the camera and verify live video and recording independently in [Camera and stream health](./camera-health.md).
2. In the camera's own web or app interface, enable the detection rule, schedule, detection region, and notification linkage. KeepPeek discovery never writes camera detector settings.
3. Keep the default `[cameras.<name>.events]` `mode = "auto"` unless forcing a specific transport.
4. If the camera emits only unclassified motion/VMD, enable **Store generic motion events** (`record_generic_motion_events = true`, default `false`) in its KeepPeek camera settings. Explicit `person` or `vehicle` classifications remain distinct.
5. Trigger one known event, open **Events** (`/events`), select the camera and UTC date, and inspect kind, source, timestamp, and image. Verify matching footage in **Keep** (`/keep`) before enabling alert rules.

A detector-enabled switch on the camera is not an activity indicator, and a camera motion rule with a human-only filter does not guarantee notifications carry an explicit `person` label. KeepPeek classifies strictly from explicit payload evidence.

## Video walkthrough

This walkthrough demonstrates verifying camera-origin events in **Events** (`/events`) and **Activity** (`/activity`).

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4">Download the Events and Activity review demo</a>.
</video>

## Transport modes and file-only configuration

Configure per-camera event overrides under `[cameras.<name>.events]` in `config.toml`. These fields are file-only—ordinary UI camera edits preserve them—and require the normal configuration apply/restart workflow. Replace `<name>` with the existing camera key; never duplicate a camera entry just to change its event path.

```toml
[cameras.front.events]
mode = "onvif-pullpoint"
metadata_stream = "auto"
snapshots = true
```

| `mode`            | Behavior                                                                                                                                                                                          |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `auto` (default)  | Prefer an eligible Reolink or ISAPI adapter; otherwise use generic ONVIF PullPoint and permit RTSP metadata. Unsupported ISAPI HTTP `404`/`405` hands over to generic ONVIF after vendor cleanup. |
| `vendor`          | Use only the eligible vendor adapter without generic fallback.                                                                                                                                    |
| `onvif-pullpoint` | Force generic ONVIF PullPoint regardless of video backend; RTSP metadata remains permitted unless disabled.                                                                                       |
| `rtsp-metadata`   | Consume metadata from existing RTSP workers without contacting an ONVIF Event Service.                                                                                                            |
| `disabled`        | Stop native event workers and exclude native event capabilities while leaving camera media running.                                                                                               |

- `metadata_stream`: `auto` (default), `enabled`, or `disabled`. Does not override `vendor` or `disabled`.
- `event_service_url`: optional HTTP(S) URL restricted to the exact configured camera IP with no credentials, query, or fragment.
- `source_tokens`: up to 32 exact camera-supplied opaque identifiers (`<= 256` bytes each); never guess profile indexes.
- `include_topics` / `exclude_topics`: up to 32 combined namespace-expanded XML topic paths (`<= 1,024` bytes each), such as `{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm`; exclusions take precedence.
- `snapshots`: `true` by default; controls optional ONVIF snapshot fetches.
- `[isapi_callbacks]`: optional camera-to-server HTTP Digest callback listener in `config.toml` for eligible non-Reolink cameras. Configuring callbacks suppresses that camera's pull worker to prevent duplicate events; store callback passwords in `secrets.toml` via `{secret:KEY}`.

Authentication failures are reported as terminal errors rather than downgraded or retried across policies. See [Configuration reference](./configuration-reference.md#native-camera-events) for complete field limits and secret-reference rules.

## Event lifecycle, timestamps, and images

| Behavior               | Rule                                                                                                                                                                                           |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Coalescing**         | Repeated active observations update one logical event; an explicit clear closes it. Matching ONVIF PullPoint and RTSP metadata observations deduplicate into one event.                        |
| **ONVIF baselines**    | `Initialized` notifications establish a baseline without alerting; an already-active baseline must clear before opening a new interval.                                                        |
| **Timestamps (ONVIF)** | Valid camera UTC from the preceding 5 minutes supplies event time; future, stale, missing, or invalid times fall back to receipt time with `timestamp_source` and `timestamp_reason` metadata. |
| **Timestamps (ISAPI)** | Recording timeline uses server receipt time; the camera's original `dateTime` is retained as `camera_time` metadata. Sync device clocks when investigating drift.                              |
| **Inferred endings**   | ISAPI disconnects or a 30-second active-observation timeout (ONVIF: 30s properties, 5s objects) close the span at its last evidence—not proof of physical motion stop.                         |
| **Snapshots & boxes**  | Missing or failed snapshots never discard event metadata. Bounding boxes draw only on their explicitly matched image; unrelated footage is never substituted.                                  |
| **Observability**      | `/metrics` exports fixed-cardinality `keeppeek_camera_events_*` counters and gauges; topics, URLs, tokens, and raw XML are never metric labels.                                                |

## Troubleshooting

| Symptom                         | What to check                                                                                                                             |
| ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Video works, events do not      | Native event status, device event service, credentials, rule schedule, and channel selection (`101/102` vs `201/202` or `source_tokens`). |
| Only heartbeats arrive          | Enable the camera's rule and notification linkage; enable **Store generic motion events** for unclassified VMD.                           |
| Generic service is unsupported  | Confirm the device exposes ONVIF Event Service or RTSP metadata; an ONVIF video profile alone is insufficient.                            |
| Repeated authentication failure | Fix saved credentials and device permissions; KeepPeek does not downgrade Digest/WS-Security or repeatedly guess passwords.               |
| Events have no thumbnail        | Check snapshot support and attachment state; metadata delivery succeeds independently of image capture.                                   |
| Duplicate-looking cards         | Compare source, event ID, rule, object, and revision; similar text can represent distinct observations.                                   |

For wire-level protocol limits and hardware diagnostics, see [ONVIF events](https://github.com/xnorpx/keeppeek/blob/main/docs/onvif-events.md) and [Hikvision ISAPI](https://github.com/xnorpx/keeppeek/blob/main/docs/hikvision-isapi.md). For model-based detections, see [External analysis](./external-analysis.md); for alerting and MQTT, see [Notifications and integrations](./notifications-and-integrations.md).
