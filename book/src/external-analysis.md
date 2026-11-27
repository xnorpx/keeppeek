# External analysis

This chapter explains how independent computer-vision and analytics services subscribe to KeepPeek WebRTC streams and publish durable event revisions back through the MIT-licensed public API (`keeppeek.event-publication.v1`). KeepPeek does not install, host, or select inference products—model execution, decoding, sampling, deployment, and model licensing belong to the external service—while camera-native detections are covered in [Native camera events](./native-camera-events.md).

## System boundary

| Responsibility | KeepPeek owns                                                                                | External service owns                                                                           |
| -------------- | -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| **Media**      | Camera ingest, recording, WebRTC stream routing (`RTP` preferred, data-channel fallback).    | Frame decoding (`ffmpeg` or native decoder), sampling rate, keyframe gating, bounded queues.    |
| **Analysis**   | Source authorization, advertised event-type catalog, attachment limits.                      | Model weights, licensing, inference runtime, tracking, event lifecycle formation, JPEG encode.  |
| **Events**     | Atomic catalog + attachment commit, revision conflict checks, live/notification/MQTT fanout. | Globally unique `event_id` generation, strictly increasing revisions, idempotent commit retry.  |
| **Config**     | Named Administrator credentials and advertised `ServerCapabilities`.                         | Service-specific config files, env vars, and deployment lifecycle (no KeepPeek browser wizard). |

External services must publish strictly through the public API—never write KeepPeek's SQLite catalog or attachment directories directly, and do not invent undocumented KeepPeek `config.toml` sections.

## Prepare an external integration

1. Confirm the camera is online in KeepPeek and that its stream codec (`H.264` or `H.265`) can be decoded by the service. Select the low-resolution `sub` stream explicitly for continuous inference rather than continuously decoding `main`.
2. Copy the camera's stable `source_id` from camera details (`/camera`). Display names, transient `source_session_id` values, and Home Assistant entity IDs are not substitutes.
3. Create a dedicated named **Administrator** credential in **Settings > Access** ([Authentication and access control](./authentication.md)). Event publication currently requires Administrator authority—a **User** key can read media but cannot publish events, and per-source/per-event-type scopes are not implemented. Use a separate credential per service so it can be revoked independently.
4. Confirm `ServerCapabilities` advertises `keeppeek.event-publication.v1`, the target source (`source_id` and active `source_session_id`), a supported media variant, and every event type the service will emit. Unadvertised event types are rejected.
5. Start the service and verify one committed event in **Events** (`/events`) before attaching alert rules or automations.

## Publication contract and limits

| Aspect                   | Contract behavior                                                                                                                                                                                         |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Metadata-only events** | Use envelope-only `PublishEvent` when no attachments are included.                                                                                                                                        |
| **Attachment events**    | Use the two-phase transaction (`StartEventPublication` → `EventAttachmentChunk` over `RELIABLE_DATA` → `CommitEventPublication`). Files become visible only after `COMMITTED`.                            |
| **Failure & reaping**    | Abort unfinished work with `AbortEventPublication`; on process crash, KeepPeek expires and deletes staged bytes at `expires_at_ms` with no partial event visible.                                         |
| **Idempotency**          | Reuse the same `publication_id`, `event_id`, and `revision` on retry. Identical metadata and attachment bytes return `COMMITTED` without duplicate fanout; differing content returns a revision conflict. |
| **Session quotas**       | Atomic publications are bounded to **4 active** and **64 distinct IDs** per API session (**64 active** / **256 retained** server-wide). Reconnect after exhausting per-session IDs.                       |
| **Source restarts**      | Restarts change `source_session_id`. Rediscover the active session and re-subscribe while preserving the stable `source_id` for event history.                                                            |
| **Observability**        | Prometheus `/metrics` exports fixed-cardinality `keeppeek_external_analysis_*` gauges, counters, and bounded 256-sample `p50`/`p95` durable publication latency (ms).                                     |

## Reference Python example

[`examples/object_detection_service/`](https://github.com/xnorpx/keeppeek/tree/main/examples/object_detection_service) is a minimal Python reference service built for interoperability demonstration and CI. It is **not** a production detector product.

- **Prerequisites**: Python `3.12+` (installed directly into the interpreter—do not use virtual environments), `ffmpeg` on `PATH`, an active H.264 or H.265 source, and a KeepPeek server advertising `keeppeek.event-publication.v1`.
- **Bindings**: Run `generate_protos.py` to generate MIT-licensed bindings directly from `api/webrtc.proto`. Never copy wire types by hand.
- **Authentication**: Supply the key via `KEEPPEEK_ACCESS_KEY_FILE` (owner-only file) or `KEEPPEEK_ACCESS_KEY` (takes precedence). Never pass keys in command arguments, URLs, or committed files.
- **Settings**: Configure non-secret inputs via `KEEPPEEK_URL`, `KEEPPEEK_SOURCE_ID`, `KEEPPEEK_STREAM`, `KEEPPEEK_MODEL`, `KEEPPEEK_CONFIDENCE`, `KEEPPEEK_COOLDOWN_SECONDS` (5s default per class), and `KEEPPEEK_INFERENCE_FPS`.
- **Bounded queues**: Caps memory by dropping oldest work first—encoded queue: 4 frames / 8 MiB; decoded queue: 1 frame / 24 MiB; publication queue: 16 events / 16 KiB; single frame: `<= 8 MiB` and 256 fragments.
- **Model weights**: Loads Ultralytics `yolo11n.pt` only after auth and media subscription succeed (Ultralytics may download missing weights under its own AGPL-3.0 / enterprise license; KeepPeek does not distribute weights). Use `--detector fake` for weight-free CI/local checks.

## Verify results in KeepPeek

1. Open **Events** (`/events`) and select the source and UTC date.
2. Inspect event kind, confidence, text, revision, and canonical image. Higher revisions enrich or close the same logical event rather than creating a new detection.
3. Select **Open at this moment** to compare the event against recorded footage in **Keep** (`/keep`), and **Back to event** to return.

Review state and shared bookmarks behave identically for external and native events. An attached JPEG, story, or description is event evidence only—it does not prove continuous video was retained ([Recording and evidence](./recording-and-evidence.md)).

## Troubleshooting

| Symptom                                   | What to check                                                                                                                           |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Subscription rejected                     | Source visibility, active `source_session_id`, advertised `main`/`sub` H.264/H.265 variant, and client media capabilities.              |
| Publication denied                        | Administrator credential role, enabled/unexpired key, current source session, and advertised event type.                                |
| Processing falls behind                   | Service decode/inference rate and bounded queues; lower `KEEPPEEK_INFERENCE_FPS` or subscribe to `sub` instead of queuing stale frames. |
| Events appear without images              | Two-phase upload/commit completion, descriptor count/byte limits, and explicit attachment availability state.                           |
| Recording continues while detections stop | Inspect the external process and WebRTC session; inference failure never interrupts camera ingest or recording.                         |
| Reconnect produces duplicates             | Preserve logical `event_id`, increment `revision` strictly, and retry with the original `publication_id` on ambiguous commits.          |

Start client development with the MIT-licensed [public API](https://github.com/xnorpx/keeppeek/tree/main/api) and the [computer-vision scenario](https://github.com/xnorpx/keeppeek/blob/main/docs/computer-vision.md) (verify live `ServerCapabilities` before assuming a scenario workload is enabled on a given build).
