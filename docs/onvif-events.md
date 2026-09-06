# Generic ONVIF and RTSP events

KeepPeek consumes camera-native notifications through ONVIF PullPoint and ONVIF
metadata tracks carried by RTSP. Both paths use one camera-owned lifecycle and
the existing durable event store, recording-demand policy, live event subscription,
notification, and MQTT paths. Event failures do not restart healthy video or audio.

The implementation is plain Rust. The reusable ONVIF event module contains bounded
XML and metadata parsers, subscription requests, endpoint validation, and a blocking
HTTP adapter. The independent `test-hikvision` fixture exercises the wire protocol;
the `test-camera` fixture combines actual RTSP video, metadata, and recording.
Fixture results are not physical-device certification.

## Selection and configuration

Configure events in the existing camera entry. These fields are file-only and do
not change the protected protobuf camera-settings contract.

```toml
[cameras.front]
ip = "192.0.2.10"
username = "{secret:FRONT_USERNAME}"
password = "{secret:FRONT_PASSWORD}"
onvif_port = 8000
record_generic_motion_events = true

[cameras.front.events]
mode = "auto"
metadata_stream = "auto"
snapshots = true
source_tokens = ["source-2"]
include_topics = []
exclude_topics = []
```

| Mode              | Event behavior                                                                                                                                                           |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `auto`            | Prefer an eligible Reolink or ISAPI adapter; otherwise start generic PullPoint and permit RTSP metadata. Unsupported ISAPI HTTP 404/405 hands over after vendor cleanup. |
| `vendor`          | Use only the eligible vendor adapter; do not fall back.                                                                                                                  |
| `onvif-pullpoint` | Force generic PullPoint independently of the selected video backend. Metadata is also permitted unless disabled.                                                         |
| `rtsp-metadata`   | Do not contact an ONVIF Event Service; consume metadata from the existing RTSP workers.                                                                                  |
| `disabled`        | Stop native event workers and exclude native event capabilities. Media remains independent.                                                                              |

`metadata_stream` accepts `auto`, `enabled`, or `disabled`. Both enabled forms permit
metadata for generic modes; neither overrides `vendor` or `disabled`. There is no
vendor/generic reconciliation mode. Authentication failures are terminal rather
than an invitation to repeatedly try credentials or switch authentication policy.
ISAPI failures other than unsupported endpoints retain their existing retry policy;
automatic failover from a failing Baichuan media session is not implemented.

An optional `event_service_url` must use HTTP(S) and the exact configured IP, without
credentials, a query, or a fragment. Source tokens are exact, opaque identifiers;
never guess that the second profile or an analytics-module name means channel 2.
Use the source token actually supplied by the camera. The runtime is keyed by device
IP, so it creates one subscription, not one per profile. Multiple independent
logical camera entries at the same IP are not provisioned by this feature. Within
one configured entry, source/rule/object identities remain separate and
`source_tokens` selects the intended channel, including non-default channels.

Topic filters use expanded names, such as
`{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm`. Exclusions take
precedence. Up to 32 source tokens and 32 combined topic filters are accepted.
See [configuration management](configuration-management.md) for validation limits.

Ordinary camera API updates preserve the saved event table. Runtime replacement
that changes only events or generic-motion retention keeps the media workers;
an explicit restart with unchanged settings still restarts media. Direct file edits
need the existing configuration activation/restart path: no file watcher is added.

## Discovery and subscription

Generic camera discovery requests service capabilities where supported, identifies
the Event Service by namespace, and retains its validated endpoint and bounded topic
evidence internally. Background metadata updates are camera-generation fenced.
Transient evidence-query failures do not remove already retained evidence or prevent
media discovery. The worker caches successful discovery across reconnects.

The subscription owns its returned manager endpoint and opaque reference parameters.
It creates a 90-second requested lease, attempts synchronization, pulls all returned
notifications, renews before expiry, and best-effort unsubscribes on stop or replacement.
Camera current/termination differences determine the lease; network time is deducted.
Initial pulls request two seconds and 32 messages. Lower fault limits are negotiated
only when they actually reduce usable limits. Reconnect backoff is 1 to 30 seconds.
Authentication and unsupported-service failures are explicit terminal states.

SOAP requests always carry WS-Security UsernameToken; HTTP Digest is added when
challenged. Digest protection spaces, exact method/path/query/body signing, and stale
nonces are handled separately from the SOAP token. There is no Basic or token-free
downgrade. HTTPS uses platform trust and certificate verification. Some firmware may
need a different supported authentication policy before it can be certified.

Returned URLs cannot escape the configured camera host, redirect to another server,
or downgrade verified HTTPS. An unspecified returned address can be repaired to the
configured authority; arbitrary hostname aliases are not resolved or trusted.
Snapshot URLs are separately validated against the exact configured IP. Discovery
never writes detector regions, camera rules, schedules, outputs, or firmware.

## Event semantics

Recognized kinds are `motion`, `tamper`, `digital_input`, `audio_detected`,
`video_loss`, `line_crossing`, `intrusion`, `region_entry`, `region_exit`, `loitering`,
`person`, `vehicle`, `animal`, `face`, `license_plate`, `package`, `object_count`, and
`doorbell_press`. Mapping is namespace-aware and fixture-driven. Arbitrary vendor
namespaces, unknown topics, heartbeats, and PTZ status do not become motion.
Generic motion remains opt-in through `record_generic_motion_events`, default false.

`Initialized` establishes a baseline without a user notification. A baseline that is
already active needs an explicit false transition before opening a new interval.
Changed activity opens or refreshes the matching property; false/deleted closes it.
Point events are finite. Replay history survives transport reconnects, and matching
PullPoint/metadata observations share one event. Losing one transport preserves an
interval still backed by the other. Main/sub metadata uses one active consumer with
a five-second inactivity handoff; both RTSP sessions may still SETUP metadata tracks.

Valid camera UTC from the preceding five minutes supplies event time. Future,
out-of-window, missing, or malformed notification timestamps use receipt time with
explicit `timestamp_source`, `timestamp_reason`, and `observation_time_ms` payload
fields. Missing/invalid camera time is not fabricated as `cameraTime`. Camera-time
watermarks and receipt ordering remain separate. Without a stable camera timestamp,
identical notifications received at different times cannot prove historical replay.
Analytics frames still require valid frame UTC.

Profile T/M object handling applies frame/appearance transformations, validates
confidence and normalized boxes, uses explicit class evidence, and tracks partial
frames and explicit object deletion. Empty partial frames do not delete objects.
Properties expire after 30 seconds without fresh activity; objects after five.
Inferred endings use the last observation, not a claimed physical stop time.
Updates are coalesced to at most one per second, with final details retained.

Source token, rule, object/module identity, count, recognition text, and boxes remain
bounded structured event payloads. An object box is not overlaid on an independently
fetched JPEG: without proven coordinate/time association it remains `payload.object_box`,
not an attachment-bound public box. Recognition text is never a filename or log field.

## Snapshots and delivery

One optional snapshot worker accepts four queued jobs and one request in flight.
It uses initial or subsequently discovered profile snapshot URLs. A missing URL,
authentication failure, timeout, invalid JPEG, full queue, or failed image write does
not discard the event. JPEGs have a one-MiB limit, a two-second request budget, and
validated dimensions. Generic events advertise zero or one optional image; ISAPI
retains its separate zero-to-16 correlated-image capability.

Lifecycle batches are ordered, bounded, and acknowledged by committed prefix. Storage
precedes live fanout. Complete capability snapshots are queued before newly discovered
native event types. A subscriber that cannot accept the snapshot/event ordering is
shed. Late batches from retired workers cannot reopen events; committed intervals are
closed by the recorder, including batches larger than one cleanup tick.
Final cleanup is best effort under a five-second budget. Persistent disk failure is
reported and can leave an interval open; an in-flight synchronous storage operation
cannot be forcibly interrupted by that budget.

## Bounds and diagnostics

SOAP bodies are limited to 256 KiB, 32 XML levels, and 256 notifications. XML rejects
DTD/entity input, ambiguous attributes, excessive namespace storage, and oversized
names/items. Invalid individual notifications do not discard valid neighbors.
RTSP assembly is limited to 256 KiB, 1,024 packets, and ten seconds per document.
Standard `vnd.onvif.metadata+gzip` and legacy `.gzip` are accepted. Decompression
enforces a one-MiB output, ratio, chunk, and 100-ms work budget; trailing members
are rejected. EXI is explicitly unsupported.

Per-camera state is bounded to 128 active/deferred lifecycles, 128 baselines, and
1,024 replay entries. The input queue has 32 items and an eight-MiB byte budget.
Pending output has 1,024 entries, with reserved ending capacity and four MiB of
optional snapshot bytes. Queue loss establishes a cutoff outside the saturated data
queue so stale work cannot reopen disconnected activity.

`/metrics` exposes fixed-cardinality `keeppeek_camera_events_*` counters and gauges
for pulls, renewals, notification errors, replay, active intervals, metadata, queue
pressure, snapshots, mode, and state. Advertisement and working PullPoint evidence
are separate. No topic, URL, token, license plate, or raw XML is a metric label.
Media health is evaluated independently; metadata cannot keep stalled video healthy.

A plain RTSP camera without native evidence has no native event/analytics claim.
Configured external inference continues through the existing
[computer-vision publication contract](computer-vision.md), retaining KeepPeek origin
and model provenance. This feature adds neither an inference model nor provisioning.

## Verification and hardware

See [the implementation tracker](../tasks/onvif-events.md) and
[recording performance measurements](native-event-performance.md). Local fixtures
cover leases/authentication, XML/gzip limits, channel filtering, lifecycle races,
snapshots, durable delivery, live capability ordering, and actual dual-stream MP4s.

Axis, Dahua/Amcrest, Hikvision generic ONVIF, Reolink generic ONVIF, Uniview, Profile
T/M, gzip, broken-PullPoint, non-default NVR channel, and plain-RTSP physical cases
remain unverified for this implementation. Prior ONVIF example results and prior
ISAPI diagnostics are not this matrix. For each device, record model/firmware,
profile, auth mode, topic dialect, observed source token, subscription renewal and
cleanup, encoding, event start/clear, and uninterrupted video/recording. Use private
configuration-based credentials and do not publish subscription URLs or raw payloads.

ONVIF push is conditional future work only if real firmware requires it; no generic
push listener is enabled. [ISAPI HTTP callbacks](hikvision-isapi.md) are separate.
Browser microphone talkback remains outside this native-event implementation and is
not made complete by the ISAPI crate's audio transport. Issues #94 and #96 must not
be closed solely from fixture success; final-head CI and hardware evidence remain
release requirements.
