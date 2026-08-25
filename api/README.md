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
- [Home Assistant card](../docs/home-assistant.md) shows a direct browser-to-KeepPeek Lovelace card with
  one configured token, direct live media/events/timeline review, and no Home Assistant proxy.

## HTTP API

The initial API has five operations:

1. `POST /create` sends a gzip-compressed SDP offer and returns a gzip-compressed SDP answer
   with a session ID.
2. `POST /delete` closes the session identified by the session ID in its JSON body.
3. `GET /logs` reads server logs through Server-Sent Events.
4. `GET /logs/snapshot` returns the complete bounded retained log buffer as JSON.
5. `GET /metrics` exposes Prometheus text metrics.

## Access key

KeepPeek uses one operator GUID as the HTTP Bearer secret. The value is a 128-bit integer. On the
wire it is the usual hyphenated UUID string; `config.toml` stores
`{secret:KEEPPEEK_ACCESS_KEY}`. The integer `0` is reserved: it means unset, and tests may use
`access_key = 0` / `AccessKey(0)` without minting a real secret.

Direct same-host loopback requests skip the key and act as Administrator. Every non-loopback
peer, including private LAN and link-local clients, must send Bearer. Requests carrying reverse
proxy headers also require Bearer even when the immediate peer is loopback. The first-party UI
may later receive the same key in an HttpOnly SameSite cookie when KeepPeek serves the origin for
a remote browser.

### How the key is chosen

Resolution order at process start:

1. `keeppeek --access-key <guid>` when the value is not `0`
2. `access_key` in the loaded `config.toml` when the value is not `0`, including a secret reference
3. `KEEPPEEK_SECRET_KEEPPEEK_ACCESS_KEY` or `KEEPPEEK_ACCESS_KEY` in owner-only `secrets.toml`
4. Otherwise generate a random non-zero master GUID in owner-only `secrets.toml`

CLI and existing inline config values are migrated into `secrets.toml`; `config.toml` retains only
the reference. A later start reuses that secret and never prints its value.

```toml
host = "0.0.0.0"
port = 8081
access_key = "{secret:KEEPPEEK_ACCESS_KEY}"
```

```text
keeppeek --access-key 550e8400-e29b-41d4-a716-446655440000
keeppeek --config /path/to/config.toml
```

`--access-key` overrides the file for that process. If the override is a real key, KeepPeek
writes it back to `access_key` so a restart without the flag still authenticates. Settings
updates that do not set a key must leave the stored value alone. The settings HTTP JSON must
never include the raw key.

No HTTP endpoint creates, rotates, or lists keys. A loopback Administrator session may explicitly
reveal or rotate the shared key through the in-band control channel. Remote sessions cannot use
either command. Rotation atomically replaces `KEEPPEEK_ACCESS_KEY` in the owner-only secret file,
updates future Bearer authentication immediately, and closes sessions authenticated with the old
key. Debug logs must not print either GUID.

Until per-key scopes exist, every configured GUID has the same rights to `/create`, `/delete`,
`/logs`, `/logs/snapshot`, and `/metrics`. A Home Assistant card token is therefore also a
metrics and log credential.

Every remote request sends its configured key in the Authorization header:

```http
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
```

There are no users, roles, scopes, or per-client resource counters in this API.

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

Delete the session with `POST /delete` and a JSON body containing that `session_id` when
the viewer or service is finished. KeepPeek records the key used to create each session and
accepts deletion only from that same key. An unknown session or a session created by a different
key returns `404`.

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
