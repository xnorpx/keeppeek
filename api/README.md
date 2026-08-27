<!-- SPDX-License-Identifier: MIT -->

# KeepPeek API

This directory defines the initial public HTTP API used by viewer clients and services.

- `openapi.yaml` is the source of truth for this HTTP contract.
- Scenario walkthroughs live in [docs/](../docs/).

The in-band WebRTC data-channel contract is defined by [webrtc.proto](webrtc.proto) and
[webrtc.md](webrtc.md). The SDP data-channel topology required to establish a session is defined
in [sdp-offer.md](sdp-offer.md).

## License

All API definitions and documentation in this directory are licensed under the MIT License. See
[LICENSE](LICENSE). This includes generated bindings derived solely from these files; using those
bindings does not impose KeepPeek's repository-wide AGPL license on an API client.

## Status

This API is a pre-1.0 draft. It is not locked and may change freely, including incompatible
changes to endpoints, messages, fields, enum values, channel IDs, and SDP rules. The stable
compatibility guarantees begin with the 1.0 release.

## Scenarios

- [Viewer application](../docs/viewer.md) shows connection setup, live subscriptions, indexed stored-media
  search and scrubbing, MID assignments, quality preferences, limited connection health, and
  optional HTTP server logs and metrics.
- [Event forwarder](../docs/event-forwarder.md) shows a data-only service that proxies structured events,
  text, motion snapshots, and multi-JPEG stories to MQTT or another durable sink with backfill
  and deduplication.
- [Computer vision service](../docs/computer-vision.md) shows multi-stream decode and inference, durable
  event-plus-image publication, event storage, and commit-only fanout through KeepPeek's event
  router.
- [Transcoding service](../docs/transcoding.md) shows multi-stream decode and re-encoding, discoverable
  derived variants, lineage and loop prevention, publisher control, and viewer selection of
  alternate codecs or quality levels.
- [Shared state store](../docs/state-store.md) shows durable revisioned desired state, watches, leases,
  and media orchestration intent without confusing coordination with live media truth.
- [Group clients](../docs/groups.md) shows server-defined groups that bundle static camera streams and
  optionally host live participants, with group discovery, optional passwords, individual
  participant subscriptions, full-duplex audio with client-side push to talk, and per-medium
  recording policy.
- [Notification rules](../docs/notifications.md) describes revisioned rules, deterministic collapse
  identity, cooldowns, staged enrichment, durable delivery history, principal-scoped unread state,
  and [Pushover configuration](../docs/pushover.md).
- [Access control](../docs/access-control.md) defines local network trust, remote credentials,
  Administrator/User authorization, session revocation, trusted proxies, and audit evidence.
- [Home Assistant card](../docs/home-assistant.md) shows a direct browser-to-KeepPeek Lovelace card with
  one named credential, direct live media/events/timeline review, and no Home Assistant proxy.

## HTTP API

The initial API has five operations:

1. `POST /create` sends a gzip-compressed SDP offer and returns a gzip-compressed SDP answer
   with a session ID.
2. `POST /delete` closes the session identified by the session ID in its JSON body.
3. `GET /logs` reads server logs through Server-Sent Events.
4. `GET /logs/snapshot` returns the complete bounded retained log buffer as JSON.
5. `GET /metrics` exposes Prometheus text metrics.

## Access control

KeepPeek resolves one principal before a protected HTTP or WebRTC operation runs. Direct clients
inside the configured local networks act as Administrator without signing in. Remote clients send
one named UUID credential through the `Authorization` header:

```http
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
```

Credential values are never accepted from a query parameter. Direct remote requests require HTTPS
when `require_secure_remote` is enabled. A configured trusted proxy is treated as the TLS boundary;
the proxy is responsible for accepting HTTPS from its client and forwarding over its protected
link to KeepPeek.

### Network policy

The default local CIDRs cover IPv4 loopback, RFC 1918, IPv4 link-local, IPv6 loopback, IPv6 unique
local, and IPv6 link-local addresses. IPv4-mapped IPv6 addresses are normalized before matching.
Container bridge addresses in `172.16.0.0/12` are local by default. Carrier-grade NAT
`100.64.0.0/10`, documentation ranges, and unknown addresses are remote.

```toml
[access]
local_networks = [
  "127.0.0.0/8",
  "10.0.0.0/8",
  "172.16.0.0/12",
  "192.168.0.0/16",
  "169.254.0.0/16",
  "::1/128",
  "fc00::/7",
  "fe80::/10",
]
trusted_proxies = []
require_secure_remote = true
failed_authentication_limit = 5
failed_authentication_window_secs = 60
session_idle_timeout_secs = 1800
session_absolute_timeout_secs = 86400
max_sessions_per_principal = 64
max_sessions_per_address = 128
```

Forwarding headers are ignored for an untrusted immediate peer and force remote classification.
A trusted immediate peer must send exactly one `X-Forwarded-For` field. KeepPeek rejects duplicate,
empty, malformed, overlong, or mixed forwarding contracts, including `Forwarded`, `X-Real-IP`,
`X-Forwarded-Host`, and `X-Forwarded-Proto`. It walks the comma-separated chain from right to left
and selects the first address not covered by `trusted_proxies`; an untrusted public hop therefore
cannot prepend a private address and become local.

Unix-domain HTTP listeners are not implemented. The server currently listens on IPv4 or IPv6 TCP.
Unknown classifications fail closed as remote.

### Roles

Administrator includes every User operation and all camera, recording, storage, integration,
notification, logging, identity, audit, health, deletion, and server configuration operations.
User permits live and stored viewing, event queries and media fetches, camera PTZ and preset
operation, group/publication operations, shared-state operations subject to namespace policy, and
personal notification inbox state. The server checks the centralized command policy before every
control command. UI visibility is not an authorization boundary.

### Credentials

First start creates a random remote Administrator UUID. The protected legacy value remains in
owner-only `secrets.toml` for compatibility, while `access.toml` stores its SHA-256 verifier and
identity metadata. The local setup flow can retrieve that initial value once. Named credentials
created afterward store only their verifier and return the raw value only in the successful create
or rotate response.

`access.toml` is written atomically with owner-only permissions. Each credential has a stable UUID,
name, optional description, fixed role, created/rotated/last-used/expiry/revocation timestamps,
enabled state, and monotonically increasing revision. Last-used persistence is coalesced to at most
one write per minute. The catalog holds at most 128 credentials and 1,024 audit events.

Administrator control-channel operations are `list_access_credentials`,
`create_access_credential`, `rotate_access_credential`, `set_access_credential_enabled`, and
`revoke_access_credential`. Create and rotate responses contain the new key exactly once. Listing,
disable, enable, revoke, audit, capabilities, logs, and errors never contain a raw key or verifier.

Remote authentication failures are limited per normalized effective address to five attempts per
60-second window by default. The address tracker itself is bounded to 1,024 entries. Failure
responses do not distinguish unknown, disabled, revoked, or expired credentials. Bounded audit
records retain the internal failure category without storing an Authorization header.

## Session lifecycle

To create a session, encode an `offer` object with `type: "offer"` and its SDP string as the
UTF-8 JSON `CreateRequest` body. Gzip the complete body, then send it to `POST /create`:

```http
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
Content-Type: application/json
Content-Encoding: gzip
```

KeepPeek rejects a request with a missing or different content encoding with `415` and the
reason `offer_not_gzip_encoded`. Invalid gzip, JSON, or SDP uses `400`; an SDP offer that
violates the required topology uses `422`. Each offer-validation error includes a nonempty
human-readable `error` string that explains the specific rejected input, a machine-readable
`reason`, and, when relevant, the invalid `mid`.

If KeepPeek cannot accept a decoded offer after topology checks, it returns `400` with
`reason: "sdp_offer_rejected"`. `error` is a nonempty human-readable explanation of that
rejection. It may include text from the SDP stack and is not a stable machine contract.

The full [SDP offer contract](sdp-offer.md) defines the required SCTP transport, the three
pre-negotiated data channels, the client's initial-only RTP `StreamId` allocation, opaque MIDs, ICE Lite
offer/answer, and simulcast rules. There is no SDP renegotiation or trickle ICE. The client
always offers and KeepPeek always answers. The offerer does not need ICE candidates; the
answer supplies ICE Lite host candidates. The offer chooses every RTP send and receive `StreamId`
the session will have, up to 256 RTP media sections. Each MID is the exact opaque string assigned
by the browser or native offerer; KeepPeek never assigns meaning to its value. A successful `201`
response is gzip-compressed JSON with `Content-Encoding: gzip`. After decompression it contains
the opaque `session_id` to retain for cleanup and an `answer` object with `type: "answer"` and its
SDP string.

The session ID is a random non-zero 64-bit value reserved across all live WebRTC threads. A session
record binds it to the principal, role, effective client address, classification reason, credential
revision, creation time, last activity, and absolute expiry. `ServerCapabilities.access_session`
reports the current non-secret identity metadata.

Delete the session with `POST /delete` and a JSON body containing that `session_id` when the viewer
or service is finished. The authenticated principal and effective address must match its owner. An
unknown or previously deleted session is an idempotent success; a known session owned by another
caller returns `404`. The default successful response is `204`. Browser clients may send
`Prefer: return=representation` to receive a complete `200 text/plain` response before the
associated WebRTC transport closes.

Sessions expire after 30 minutes idle or 24 hours absolute by default, no later than credential
expiry. The server limits active sessions to 64 per principal and 128 per effective address.
Administrator operations can list and revoke sessions. Credential rotation, disable, revoke, or
expiry invalidates its revision, removes matching authorization records, cancels authenticated log
streams, and requests WebRTC shutdown. The server sweeps expiry every 100 ms; log readers wake for
cancellation at most every 100 ms. WebRTC shutdown allows up to one second for graceful close and
then reports a warning; normal session cleanup stops PTZ ownership, event/discovery work, stored
cursors, and queued data.

Audit events record credential lifecycle, remote login, session lifecycle, denied commands, and
proxy/authentication failures with principal, role, action, target, result, classification, and UTC
timestamp. They never contain keys, Authorization fields, SDP, cookies, or media. Prometheus access
metrics use fixed label-free counters and gauges. Audit entries are visible in memory immediately
and are flushed atomically to `access.toml` within one second and again during graceful shutdown.

## Log stream

`GET /logs` returns `text/event-stream`. Each `log` event has a JSON log entry in its
`data` field and uses that entry's sequence number as its SSE `id`. The stream requires the
same Authorization header as the session endpoints.

`GET /logs/snapshot` returns one `application/json` `LogSnapshot` containing every entry still
retained by the bounded in-memory log hub, plus sequence, truncation, eviction, byte, and entry
limits. The response uses `Cache-Control: no-store`, requires the same authorization as the live
stream, and is intended for the scrubbed diagnostics-package workflow rather than polling.

## Metrics

`GET /metrics` returns Prometheus text exposition metrics. Prometheus sends the same
`Authorization: Bearer <GUID>` header used by the other endpoints. Any configured GUID may
scrape metrics.

Camera and server health numbers belong here, not on `ServerCapabilities`: process CPU and
RSS, host memory and load, recording-disk bytes, per-camera online/degraded gauges, ingress
fps and bitrate, reconnects, drops, and WebRTC session counts. `ServerCapabilities.cameras`
carries identity and `ptz.supported`; it is not a metrics feed.
