# Hikvision ISAPI integration notes

## Status and scope

KeepPeek provides a [plain-Rust ISAPI crate](../crates/isapi/README.md), a
continuous camera event worker, and a read-only Rust diagnostic. Request construction,
Digest state, multipart framing, and XML/JSON event parsing are Sans-I/O; a `ureq` adapter performs
HTTP requests. No Hikvision SDK binary, Python interpreter, or JavaScript runtime
is required. See [source provenance and references](../crates/isapi/UPSTREAM.md).

The normal KeepPeek process stores supported ISAPI events and delivers committed
revisions to live event subscribers, notifications, and MQTT. The standalone
diagnostic does not store events. An opt-in camera-to-server HTTP callback listener
shares the same event pipeline. Typed management supports explicit reads and writes;
neither event mode changes camera settings automatically. There is no HCNetSDK
backend. Camera media still uses SADP discovery, ONVIF, and RTSP; see
[camera integration paths](../book/src/users-and-design-choices.md#brand-integration-paths).

The seven topics below distinguish implemented behavior, tested firmware, and
unverified vendor capabilities. Update them alongside further ISAPI work.

Shared detector on/off, read-only device capability discovery, PTZ control and
the bounded two-way-audio transport are described in
[camera controls and vendor alignment](camera-controls.md). This includes the
exact differences from Reolink and `reo-proto`. Browser Talk is not connected:
protocol-level audio support is not an end-to-end microphone workflow.

## Runtime selection and event policy

KeepPeek starts one ISAPI event worker with each eligible non-Reolink camera.
Selection uses Hikvision, Annke, or HiWatch manufacturer information, or a
recognized `/Streaming/Channels/<id>` or `/ISAPI/Streaming/channels/<id>` RTSP
path. The configured camera IP and HTTP port are used for authentication, never
an address supplied by an event. Main and sub URLs must agree on the channel.
The file-only event policy can force generic ONVIF, metadata-only, vendor-only,
or disabled operation. In automatic mode, an unsupported ISAPI HTTP 404/405 hands
over to [generic ONVIF events](onvif-events.md) only after vendor cleanup; a healthy
vendor and generic worker are not started concurrently.
Recognized URLs map `101/102` to channel 1 and `201/202` to channel 2; a recognized
manufacturer without such a URL defaults to channel 1. Other channels are ignored.
Do not use that fallback for an NVR's non-default channel; configure its exact
main/sub URLs. Conflicting channel paths are not activated as an event source.

Basic VMD is retained only when the camera's existing
`record_generic_motion_events` setting is enabled. For a camera that reports VMD
without explicit classification, set this in its existing KeepPeek camera entry
or use the corresponding camera setting in the UI:

```toml
record_generic_motion_events = true
```

This is a server retention setting, not a camera detector setting. It does not
invent a person label from a camera's `human` filter. The integration accepts
explicit `human`/`person` targets as `person` and `vehicle` as `vehicle`.
Supported unclassified rules map to `motion`, `line_crossing`, `intrusion`,
`region_entry`, `region_exit`, `tamper`, and `alarm`. Unknown rules/states are
ignored by the worker but remain available through the crate's parsed data.

Repeated active messages coalesce by camera, channel, normalized rule, kind,
region and object identity when supplied. Named target collections and ANPR
notifications preserve explicit person/vehicle evidence, attributes, confidence
and license-plate text. See the supported structures below.
An explicit inactive message closes matching observations. A disconnect or 30
seconds without another active observation closes the span at its last evidence,
not at an invented physical stop time. A later active message starts a new span.
Intervals carry `interval_semantics=observed_activity` and `end_time_semantics`
metadata. Missing object/region IDs remain absent; KeepPeek does not infer tracks
or count distinct people from notification counts.

Recording timestamps use server receipt time, consistent with the native camera
pipeline; the original `dateTime` is retained as `camera_time` metadata. Active
notifications refresh event-triggered recording demand. Storage commits happen
before live delivery. Slow live subscribers are shed by the existing bounded
queue. Worker delivery attempts are bounded to 500 ms; a full queue retains up to
128 ordered transitions and pauses ingestion until they are delivered. Starts
and clears are not discarded merely because the receiver is temporarily slow.
If the receiver disconnects or shutdown completes before delivery recovers,
remaining transitions are counted and logged as dropped rather than silently lost.

The worker does not change detector regions, filters, schedules, passwords, or
other camera settings. Explicit image references associate original JPEGs with
events. Images may arrive before or after metadata; unrelated images are never
attached by proximity. Up to 16 matched images are stored with generated IDs,
and boxes reference only the matching image coordinate space. Files are durable
before metadata commit and live delivery. Live snapshot routes receive all
retained JPEGs; stored fetches can retrieve each native image by descriptor ID.
Events with missing images remain usable without a thumbnail.

### Supported analytics structures

- Direct `detectionTarget` and `targetType`: human/person or vehicle.
- `DetectionRegionList/DetectionRegionEntry`: region identity and explicit target,
  with optional `TargetList/Target` objects.
- Root `TargetList/Target` and JSON `detectionResult` collections: separate object
  identities, classification, `humanInfo`/`vehicleInfo` attributes and `targetRect`.
- `ANPR`: `licensePlate`, confidence, vehicle attributes and
  `pictureInfoList/pictureInfo` image references with pixel-based `plateRect`.
- `humanRecognition`, `humanDetection`, `vehicleDetection` and `targetCapture`
  events enter the same lifecycle pipeline as VMD and smart-region events.

Confidence scales are explicit: `confidenceLevel` is 0..100 and `confidence` is
0..1. `targetRect` defaults to 0..1000 units; explicit `coordinateSystem` supports
`pixel`/`pixels`, `normalized` or `1000`. A region polygon is not an object box.
The crate retains unknown XML/JSON fields in `Event::data()` but never recursively
guesses their meaning. Persisted per-object metadata remains within the existing
16 KiB event payload contract. Invalid/ambiguous objects and excessive metadata
are rejected, not truncated into misleading evidence.

Image references use `contentID`, `fileName` or `pictureName`. MIME Content-ID and
filename have precedence; the form name is a fallback only if neither exists.
This prevents generic names such as `image` from colliding across unrelated files.
Identifiers are opaque: no peer filename or URL is fetched or opened as a path.
Pending correlation is capped at 16 metadata bundles, 32 images, 8 MiB JPEG bytes
and 512 identifiers. The image deadline is five seconds; used identifiers remain
reserved for thirty seconds within the transport scope. Ambiguous filename reuse
fails closed. Callbacks get a separate correlation scope per HTTP request.

## Read-only Rust diagnostic

From the repository root on macOS:

```sh
cargo run --locked --bin keeppeek-camera -- events --protocol isapi \
  --credentials-from "$HOME/Library/Application Support/keeppeek/config.toml" \
  --camera "North Frontyard" --duration 15
```

Replace the camera name with a configured camera name, key, or IP. On other
platforms, point `--credentials-from` at the existing private KeepPeek config;
omitting it uses the platform's default config path. Credentials are resolved
inside the process, including camera defaults and secret references. There is no
password CLI option. `--protocol onvif` remains the default when omitted.

The command uses the configured camera HTTP port, defaulting to 80. The crate
supports verified HTTPS origins, but this diagnostic currently uses HTTP on the
camera LAN. It observes one stream for 1..300 seconds, with a 4,096-part and 64 MiB
aggregate payload budget. It does not record video or change camera settings.
`isapi_connected`, `isapi_event`, and `isapi_summary` JSON records expose selected
redacted fields and counters, not raw XML, images, or credential-bearing URLs.
XML and JSON notifications are interpreted; their counts are reported separately.
JPEG parts are counted but not interpreted or stored.

Normal completion reports `duration_elapsed` or `stream_closed`. Ctrl+C interrupts
read waits through the cancellation hook, checked every 100 ms. Connect/send
phases can take up to five seconds.
Authentication, framing, encoding, transport, and resource-limit errors exit with
failure. Reconnection is not automatic; do not interpret an ended stream as a
motion-clear notification.

## 1. Authentication and credentials

HTTP Digest authentication worked in the I91ET diagnostic. An initial `401` with a
`WWW-Authenticate: Digest` challenge is normal: a Digest-capable client computes
the response and retries. Basic authentication is not a substitute for that
exchange. Authentication policies can differ by firmware and device settings;
avoid universal claims that every ISAPI device permits only Digest.

Use a maintained Digest implementation. Document supported challenge algorithms,
nonce refresh, and authentication failure handling. Repeated `401` responses need
diagnosis, not repeated credential guesses or an automatic security downgrade.
Digest does not encrypt event payloads; use verified HTTPS when available or an
appropriately isolated camera network.

Resolve credentials inside the process from the private KeepPeek configuration
and [secret store](secrets.md). Diagnostic tools must use file-based credential
input such as `--credentials-from` when supported. Never put credentials in shell
arguments, environment variables, logs, examples, or reports during agent runs.
Do not forward authenticated requests to an unvalidated redirect target.

The Rust adapter uses `digest_auth`, with synthetic MD5 and SHA-256 challenge
coverage. It caches Digest state within one origin, refreshes explicitly stale
nonces, and attempts at most three requests per operation. It does not fall back
to Basic or disable certificate verification. Usernames used in quoted headers
reject control characters, quotes, and backslashes; passwords are hashed rather
than interpolated into URLs or headers. The library's diagnostic output redacts
credentials, authorization values, and payload contents.

## 2. Event delivery and connection lifetime

### Persistent alert stream

`GET /ISAPI/Event/notification/alertStream` opens a persistent HTTP response. It is
an event stream, not repeated polling and not a response to close after each
event. The tested I91ET returned multipart notifications over this connection.

Parse the response's advertised MIME boundary and each part's headers. XML and
optional JPEG parts may arrive across arbitrary network reads. Do not assume one
read equals one part, search JPEG bytes as XML, or hard-code a boundary. Bound
headers, part sizes, XML complexity, image sizes, and queued work.

Document heartbeat recognition, idle deadlines, bounded reconnect backoff,
shutdown, and state recovery after a gap. The tested camera sent periodic
`videoloss` notifications with state `inactive` during idle periods; those are not
motion alarms. Repeated active alarms are not necessarily separate detections,
and a connection loss does not prove motion stopped.

The implemented decoder accepts `multipart/mixed`, `multipart/x-mixed-replace`
and `multipart/form-data`, with or without per-part Content-Length. Feed and
drain limits, header/body limits, and rejected framing are listed in the
[crate documentation](../crates/isapi/README.md#bounds-and-semantics).
Transport connect/send phases are bounded to five seconds; total ordinary
requests are bounded to 15 seconds. Diagnostic streams have a selected total
lifetime; `Client::subscribe` and the runtime worker do not impose a periodic
reconnect. Each part must complete within the configured progress deadline
(30 seconds by default), even if a peer trickles partial bytes.

The worker additionally retains a 30-second notification deadline across image
or unknown MIME parts. Only a valid XML/JSON notification renews it, so unrelated
traffic cannot hide a lost heartbeat/event feed.

The runtime worker reconnects transient failures with 1, 2, 4, 8, 16, then 30
second backoff, reset by valid notifications. Authentication failures and HTTP
403/404/405 stop retries until the camera worker is restarted or its configuration
is reapplied. Camera restart/shutdown cancels the event worker with the media
workers. The crate itself never creates a retry thread.

XML `EventNotificationAlert` and JSON notification parts produce structured events. The parser
exposes type, state, channel ID, dynamic channel ID, timestamp string, active-post
count, detection target, and channel name. Unknown event types/states are kept;
only explicit `active`/`inactive` states map to Boolean state. Unknown XML
extensions remain in the validated event document as well as the original part.
The KeepPeek consumer applies the runtime policy above. `videoloss/inactive` and
`heartBeat/active` are recognized heartbeats, not timeline alarms. The supplied
[ISAPI guide](isapi.pdf) specifies a ten-second heartbeat cadence on newer
firmware and a 30-second heartbeat timeout (PDF pages 626-628).

Structured `camera.isapi.status` logs report connection state, channel, connection
count, notifications, heartbeats, ignored messages, image parts, failures,
transitions, delivery stalls, pending transitions, and dropped transitions. Failure logs distinguish permanent errors
from retryable ones. The existing event-subscription queue metrics also cover
native deliveries. No raw payloads, credentials, or authentication headers are
included in these reports.

### Camera-to-server HTTP push

Some devices can POST events to a configured HTTP listener. Availability, setup
menus, endpoint configuration, authentication, and payload details require
model/firmware verification. Multipart form data containing XML and optional
images is a possible payload, not a universal contract; honor the actual
`Content-Type` and document the tested format.

Push support must document camera-to-server reachability, firewall and TLS setup,
sender authentication, request limits, acknowledgments, retry behavior, and
duplicate handling. Do not expose an unauthenticated public event receiver.
The receiver is implemented and tested against the shared fake Hikvision camera.
It has not been enabled or verified on the physical I91ET.

Enable it explicitly in the private KeepPeek configuration and restart the server:

```toml
[isapi_callbacks]
bind = "192.0.2.10:8092"

[[isapi_callbacks.sources]]
ip = "192.0.2.20"
channel = 1
username = "gate-callback"
password = "{secret:GATE_CALLBACK_PASSWORD}"
```

The source IP must already identify a configured non-Reolink camera. Store the
password in the private secret store; inline callback passwords are rejected by
configuration loading. Use a separate receiver credential per camera, not an
administrator credential. Configuring callbacks suppresses that camera's pull
worker, so one physical event source does not produce duplicate push/pull events.
Removing or replacing the camera fences old uploads and closes old observations.

Configure the camera's callback destination as
`http://192.0.2.10:8092/ISAPI/Event/notification/callback/192.0.2.20`, with
`MD5digest`, XML or JSON parameters, and binary image upload. Query its callback
capabilities first. `management::CallbackHost` builds create/update/delete/test
requests, and `Capabilities::query(Endpoint::CallbackHosts)` exposes advertised
host counts, authentication options and field bounds. Back up the current host
list before writes; restore it or delete the explicitly created host to roll back.

The listener is HTTP-only. For an untrusted network, terminate verified HTTPS at
a reverse proxy and bind KeepPeek to a private/loopback address. Set
`trusted_proxy = "127.0.0.1"` only for that explicitly trusted proxy address.
Forward the original path and Authorization header, disable request-body
buffering, limit request bodies to 8 MiB, and enforce short header/body timeouts.
Do not expose this HTTP listener directly on the Internet. Without a configured
proxy, the TCP peer must match the configured camera IP; forwarded headers are
not trusted to bypass this check. A proxy must enforce its own source allowlist.

The receiver authenticates before consuming the body, checks the exact POST
target, rejects browser Origin headers, and uses one request per connection.
Its bounds are 16 connections, four handlers, 8 MiB per upload, 32 MiB aggregate
upload reservations, 128 MIME parts, 64 notifications and 128 transitions per
callback. Reads have a 15-second absolute deadline and one-second socket waits.
XML/JSON and JPEG limits from the crate still apply inside the multipart body.

HTTP 200 is returned only after every generated event transition is committed.
An unavailable/full commit queue or a two-second acknowledgment wait returns 503. Pending transitions and their committed prefix remain retained; retries do
not restart an in-flight batch. A lost acknowledgment is treated as an unknown
outcome and blocks blind resubmission until the source is diagnosed/restarted.
Catalog and filesystem operations use KeepPeek's existing synchronous storage
infrastructure; the HTTP acknowledgment wait is not a hard disk-I/O deadline.

Identical successful bodies are deduplicated within the last 1,024 successes and
five minutes per camera. This cache is process-local, not durable exactly-once
delivery across restarts or arbitrarily delayed camera retries. Digest nonces
expire after one minute, and nonce counts reject wire replay. Fresh credentials
do not make a stale event timestamp trustworthy. `camera.isapi.callback` logs
report peer, HTTP outcome and reserved bytes without event payloads or secrets.

`/ISAPI/Event/triggers` describes event configuration and linkage, not a live event
feed. Polling it does not replace either delivery mechanism.

## 3. ISAPI versus HCNetSDK

ISAPI uses HTTP and does not require a vendor-native SDK binary. This makes it a
candidate for KeepPeek's macOS, Linux, and Windows deployments, subject to the
HTTP client's capabilities and actual device compatibility.

HCNetSDK may expose device-specific functions or details unavailable through an
implemented ISAPI path. Verify the exact required smart-event, two-way audio, or
PTZ feature before adding SDK packaging and deployment dependencies. Do not assume
ISAPI lacks a feature merely because the current adapter does not implement it.

Check the current official SDK release for OS, CPU architecture, ABI, dependent
libraries, redistribution terms, and feature coverage. Blanket claims that all
official builds are x86-only or that ARM is always unofficial are not a verified
support matrix. Native macOS support must also be established from the actual
package, not assumed. No SDK platform matrix was verified in this investigation.

## 4. RTSP channel URLs and escaping

Common Hikvision stream paths use these channel identifiers; verify them on the
actual device, especially NVRs and multi-channel cameras:

- `rtsp://camera-address:554/Streaming/Channels/101`: channel 1 main stream.
- `rtsp://camera-address:554/Streaming/Channels/102`: channel 1 substream.
- `rtsp://camera-address:554/Streaming/Channels/201`: channel 2 main stream.

The examples deliberately omit credentials. Prefer separate credential fields.
If a client requires URL user information, use a URL library to percent-encode
the username and password components, not the whole URL. A literal `@` inside a
password must not become the host delimiter. Do not double-encode existing
escapes or print the resulting credential-bearing URL.

RTSP media and ISAPI events are separate connections. A working stream URL does
not prove event subscription or analytics support.

## 5. Smart-event rules and target filters

Capability discovery is not rule activation. Check the selected channel's event
enablement, detection grid or region, sensitivity, target filters, arming schedule,
and notification linkage. Line crossing, intrusion, and region entrance are
distinct rules; generic motion/VMD is not proof that any of them is enabled.
Device-specific `/ISAPI/Smart/...` resources require verified capabilities and
schemas before use.

Camera changes require explicit approval for the exact settings, a private
backup, and a tested rollback plan. Change only the approved fields, preserve
unrelated settings and XML namespaces, then verify both apply and restoration by
reading back the configuration. The tested I91ET rejected a motion-setting PUT
with generated namespace prefixes but accepted the original default namespace.

A configured `human` filter means the detector is configured to filter motion; it
does not establish that each delivered event contains a person classification.
Emit person or vehicle labels only from verified event semantics or payload data,
not from configuration alone. Document unavailable labels, boxes, confidence
scores, and images explicitly.

## 6. Firmware compatibility and observed behavior

Maintain compatibility by exact model and firmware, not just brand. Record safe
identity fields, enabled rules, tested endpoints, response formats, event fields,
and unsupported or inconclusive operations. Repeat the checks after a firmware
upgrade; identical model names do not guarantee identical protocol behavior.

Diagnostic observations on an Annke I91ET reporting firmware `V5.8.10`:

- The Rust `ureq` diagnostic authenticated, parsed two 549-byte XML heartbeat
  notifications, and exited normally after 15 seconds in a read-only test.
  No new motion or classified events occurred in that run. Earlier walking-test
  findings below came from the separate diagnostic investigation, not a new
  Rust motion validation.
- Digest authentication and the ISAPI alert stream succeeded.
- Motion was enabled with sensitivity `60` and target filter `human`. A temporary
  full-frame grid produced alarms where the original restricted grid did not.
  The original grid was restored and verified after testing.
- ISAPI delivered repeated `VMD` active alarms. The parallel ONVIF subscription
  delivered explicit motion start and clear states on two overlapping topics.
  Neither notification counts nor repeated active messages count distinct objects.
- Motion capability XML advertised `targetType` options `human,vehicle`. This is
  configuration capability, not evidence of per-event classification output.
- The filtered event reports did not establish explicit human/vehicle labels.
  They also did not preserve every raw field, so absence from a report is not
  proof that a richer payload is unavailable.
- Reads of `/ISAPI/Smart/LineDetection/1` and
  `/ISAPI/Smart/FieldDetection/1` returned `403`. Without validated error details,
  this does not distinguish permissions, unsupported features, or device mode.

Keep raw captures, credential-bearing XML, configuration backups, and images in
the private KeepPeek configuration directory, never the source checkout. Share
only redacted findings or sanitized synthetic fixtures.

## 7. XML encoding and parser behavior

Retain the original XML bytes until an encoding-aware parser processes them.
Check the XML declaration, byte-order mark, and MIME charset; document how
conflicts and unsupported encodings are handled. Do not silently replace invalid
bytes or decode every response as UTF-8 before examining its encoding.

Older-device GB2312 responses are a reported interoperability risk, not behavior
verified on the tested I91ET. Before claiming support, add representative
sanitized fixtures with non-ASCII channel names and the relevant encoding labels.
Test malformed input and size limits as well as successful decoding. Disable
DTD/external entity processing and bound parsing work regardless of encoding.

Rust support includes UTF-8/ASCII, UTF-16 LE/BE, GB2312/GBK and GB18030 using
`encoding_rs`, never lossy replacement. GB2312 labels use the codec's GBK mapping.
BOM, XML declaration and MIME charset must agree. Generic UTF-16 requires byte
order evidence; unsupported labels and malformed sequences fail closed. Encoded
XML/JSON remains capped at 256 KiB and decoded text at 1 MiB. Raw MIME bytes remain
available. Synthetic fixtures cover non-ASCII names, surrogate pairs, conflicting
declarations and fragmented network input. `dateTime` is
retained verbatim, including nonstandard firmware offsets; it is not silently
rewritten or replaced with server time.

## Troubleshooting

| Symptom                              | Check                                                                                                                                                                     |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Repeated authentication failure      | Verify stored credentials and account access locally. A first Digest `401` is expected; guessing credentials or enabling Basic is not a fix.                              |
| Heartbeats but no motion             | Check channel enablement, motion grid, target filter, schedule, and notification linkage. A supported capability is not an active rule.                                   |
| `403` on a smart resource            | Inspect device permissions, capability support, and mode. The HTTP status alone does not prove which condition failed.                                                    |
| Malformed or truncated protocol data | Check framing, encoding, namespaces, duplicate fields, and disconnects with a private capture. Do not relax bounds or share raw secrets to get a green result.            |
| Stream ends with `duration_elapsed`  | Expected diagnostic lifetime, not a camera failure or motion-clear signal.                                                                                                |
| PUT returns HTTP 200                 | Use `Client::command` to validate device `ResponseStatus`, then read back the setting. The raw `execute` escape hatch does not validate device success.                   |
| No events in the KeepPeek timeline   | The diagnostic does not persist events. In the normal server, check `camera.isapi.status`, the selected channel, and `record_generic_motion_events` for unclassified VMD. |

## Accuracy, event semantics, and server cost

The camera performs detection. Selecting ISAPI instead of ONVIF does not by
itself improve detector accuracy. Different exposed rules and payload fields can
still change what the server can classify or represent.

For comparable metadata-only events, a persistent ISAPI stream is expected to
have less request/envelope overhead than repeated ONVIF PullPoint requests.
ONVIF long polling returns when an event is available; a longer timeout does not
impose that entire timeout as delivery latency. Neither path requires video
decoding or server-side inference merely to receive events.

No equivalent CPU, memory, or wire-byte benchmark has established a winner.
Camera HTTP push is not automatically cheaper or more scalable than an idle
held-open stream. Compare equal workloads, including authentication, heartbeats,
reconnections, event bursts, and any JPEG attachments. Do not compare a temporary
Python probe's memory use to a Rust implementation as a protocol measurement.

ONVIF can also carry rich classification through capabilities such as Profile M;
support on this camera remains unverified. Use one primary event source per
physical camera in production and explicitly coalesce overlapping topics.

## Camera management

| Surface         | Typed operations                                                                                 |
| --------------- | ------------------------------------------------------------------------------------------------ |
| Device identity | `DeviceInfo::query`                                                                              |
| Capabilities    | `Capabilities::query` for device, events, motion, stream, rule, PTZ, audio and callback hosts    |
| Clock           | `Time::query`, validated mode/RFC 3339/timezone setters, explicit update                         |
| Streaming       | `Stream::list/query`, encoder dimension/bitrate/framerate setters and update                     |
| Motion          | `Motion::query`, enable/sensitivity setters and update preserving masks/extensions               |
| Smart rules     | `Rule::query`, enable/update for line, field, region entrance and region exit                    |
| PTZ             | Status, continuous/absolute/momentary commands, preset list/store/delete/recall                  |
| Two-way audio   | Channel list/query, codec options, session open/close, concurrent bounded raw G.711 send/receive |
| Snapshot        | `Client::snapshot`, original JPEG bytes with a 1 MiB limit                                       |
| Callback hosts  | Capabilities, list/create/update/delete/test with Digest receiver credentials                    |

These methods support the named resource families, not every endpoint in the
795-page general guide. Query the device's actual capabilities and preserve
unknown configuration fields. Model-specific values outside the typed setters
remain available in retained documents and the explicit raw request layer. No
camera is rebooted automatically when status 7 reports that a reboot is required.

## Fake camera and verification

All network-facing ISAPI tests share the independent
[fake Hikvision device](../crates/test-hikvision/README.md). It verifies Digest,
retains management writes, serves synthetic JPEGs, streams scripted XML/JSON,
and sends callbacks only to loopback. It supports delayed, fragmented, truncated
and failing responses for regression tests. Parser/lifecycle unit tests remain
Sans-I/O. For a local HTTP-only test camera:

```sh
cargo run --locked -p test-camera --bin test_camera -- hikvision
```

Run `cargo test --locked -p isapi` for authentication, framing, XML/JSON, deadlines,
and cancellation; run `cargo test --locked --lib isapi` for routing, reconnection,
retention, lifecycle, backpressure, storage ordering, and live delivery. The
core-only gate is `cargo test --locked -p isapi --no-default-features`.

The I91ET has passed the read-only Rust heartbeat probe with its original camera
settings. Synthetic tests cover motion and explicit classifications through the
stored/live pipeline. A new real-camera person/vehicle classification test has
not been completed. The fake-backed matrix covers callbacks, legacy encodings,
multi-image ANPR association, region/object lifecycles and the typed management
families above. This is protocol/integration evidence, not a firmware certification
or proof that a physical camera enables those features. No physical camera
settings were changed during the capability-completion tests.
No comparative CPU/memory benchmark has established an ISAPI efficiency advantage.

Motion/PTZ controls and two-way audio have fake-backed coverage, including gated
full-duplex progress and failure cleanup. They have not been exercised against a
physical camera. A reported speaker capability does not enable the browser Talk
action; see the [current application boundary](camera-controls.md#application-and-test-coverage).

## References

- [Supplied ISAPI General Application Developer Guide](isapi.pdf): PDF pages
  131-132 (arming stream), 192 (heartbeat recognition), and 626-628 (event fields
  and heartbeat timeout). This is protocol documentation, not an MIT code license.
- [loozhengyuan/hikvision-sdk, Go](https://github.com/loozhengyuan/hikvision-sdk/tree/master/hikvision):
  GPL-3.0 reference client for HTTP, Digest, and device/time APIs; no code copied
  into the MIT crate. It is an archived partial implementation, not an official SDK.
- [Hikvision Open Capabilities](https://tpp.hikvision.com/tpp/OpenCapabilities):
  official ISAPI and device integration overview, not a model-specific guarantee.
- [Hikvision developer downloads](https://open.hikvision.com/download): verify
  exact SDK packages and their release documentation before claiming support.
- [ONVIF Profile M](https://www.onvif.org/profiles/profile-m/): standardized
  analytics metadata capabilities, not proof of I91ET conformance.
