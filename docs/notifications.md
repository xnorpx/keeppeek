# Notification rules and delivery

KeepPeek owns notification policy and delivery state on the server. Camera and storage workers
publish normalized lifecycle transitions after their primary state mutation succeeds; they do not
perform provider I/O. The notification runtime evaluates rules on its own bounded worker, persists
the resulting logical notification and outbox work, and uses one isolated delivery worker per
channel.

The runtime is advertised as `keeppeek.rules.v1`. Clients must keep notification controls disabled
when that capability is absent.

## Durable state

`notifications.db` is stored beside `config.toml`. It is independent of the recording catalog so
notification rules and unread state remain available when recording is disabled or recording paths
move.

Each rule has two revisions: the active revision used for evaluation and the user's current draft.
Draft writes, activation, and deletion require expected revisions. A conflict returns both current
revisions in a `google.protobuf.Any` detail with type URL
`type.keeppeek.dev/notification-rule-conflict.v1`.

Activation validates the complete draft and commits the immutable active version, audit record, and
active pointer in one transaction. A failed activation leaves the previous active version and the
current draft unchanged, including a draft that fails validation.

## Inputs and identity

Rules can match event create, enrichment, and end revisions; camera outage and recovery intervals;
recording write failure and recovery; global storage write failure and recovery; and explicit test
sends. Event filters include source, group, event kind, zone, confidence, attachment availability,
duration, severity, reviewed state, and bookmarked state.

A logical notification ID is the SHA-256 digest of length-delimited rule ID, source ID, source event
or outage identity, and lifecycle. Text is not part of the identity. Revisions, retries, preliminary
delivery, enrichment, service restart, and recovery retain one collapse key, while different rules,
cameras, events, and lifecycles cannot collapse based on equal text.

## Schedules and suppression

Schedules use an IANA timezone and weekly local-time windows. Quiet-hour evaluation converts each
transition instant through that timezone, including daylight-saving gaps and repeated hours.
Critical bypass is opt-in and has its own persisted maximum and time window.

Cooldown scopes are event family, camera and event kind, group, whole rule, and outage interval.
The documented default is **logical notification creation time**. Cooldown rows are committed with
logical creation, so restart does not reopen a cooldown. Recovery of an existing outage updates the
original logical notification before new-notification cooldown checks.

Fixed-window rate limits can apply to the rule, channel, principal, or server-wide delivery. Test
sends use separate rate keys but still validate and execute the saved channel actions.

Every matched candidate records a visible outcome such as `created`, `replaced`, `suppressed`,
`collapsed`, `rate_limited`, `retried`, `expired`, `delivered`, or `failed`, plus a bounded reason
and next eligible time when applicable.

## Preliminary and enriched delivery

The first matching event revision creates a preliminary logical notification. A later canonical
image or end revision can replace it before the rule's enrichment deadline. Source revisions,
enrichment attempts, attachment bytes, and deadline behavior are bounded by the active rule.

Late enrichment is recorded without waking the user unless `wake_after_deadline` is enabled.
Missing or unreadable imagery does not block a metadata alert unless that action explicitly requires
an attachment. Privacy-active candidates treat imagery as unavailable.

Channel behavior is explicit:

| Channel   | Replacement                                        | Current adapter policy                                                                                   |
| --------- | -------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Browser   | Same collapse key replaces the browser item        | Delivered to the server inbox; a permitted browser uses the logical ID as its notification tag           |
| Push      | Same logical ID and stage retain one history group | Delivered through Pushover with device, sound, priority, deep-link, image, and emergency receipt support |
| Webhook   | No replacement guarantee                           | Enriched second delivery occurs only when the action opts in                                             |
| Forwarder | No replacement guarantee                           | Fails visibly as `channel_unavailable` until forwarding from issue #65 is configured                     |

Webhook requests have a five-second global timeout, no redirects, no arbitrary headers, and no URL
credentials. Provider payloads may contain bounded base64 JPEG bytes; local paths are never sent.
Delivery history stores a SHA-256 target hash rather than the destination.

## Outbox and retries

Logical creation and outbox enqueue happen in one database transaction. The pending/retrying outbox
is bounded to 10,000 entries. Full queues record `expired` with reason `outbox_full` rather than
allocating an unbounded in-memory queue.

Each action persists maximum attempts, maximum retry interval, expiry, priority, and replacement
key. Retryable HTTP status codes are 408, 425, 429, and 5xx. `Retry-After` seconds are respected but
clamped to the rule maximum; other transient failures use bounded exponential backoff. Work that
cannot retry before expiry becomes `expired`. A process restart returns an interrupted `delivering`
row to `retrying`.

Provider workers have separate database connections and queues. A slow or unavailable channel does
not block camera ingest, event storage, recording, health projection, or another channel.

See [Pushover notifications](pushover.md) for provider setup, supported fields, write-only secret
handling, privacy, limits, failure behavior, and emergency acknowledgement tracking.

## Inbox and acknowledgement

Unread, seen, KeepPeek acknowledgement, and cleared state belong to the authenticated principal and
are separate from provider delivery attempts. Opening a deep link first marks that logical
notification seen through the authenticated control channel, then follows the normal authorized UI
route. Clearing one receipt cannot affect another. Bulk clear requires an explicit all, rule, or
before-time scope and writes an audit record.

The server returns the principal's authoritative unread count with every inbox query. Connected
browser clients refresh it and grouped delivery history every five seconds. Provider emergency
acknowledgement is not inferred from KeepPeek review acknowledgement.

## Control API

`NotificationRuleCommand` in [`api/webrtc.proto`](../api/webrtc.proto) provides:

- list, save draft, activate, delete, and test rule operations;
- inbox and grouped history queries;
- mark seen, acknowledge, clear one, and explicitly scoped clear operations.

Rule definitions are bounded JSON objects inside the protobuf command. Rust deserializes them into a
closed enum/struct model, validates allowlisted template fields, and never evaluates rule text as
code. Responses omit attachment paths and destination values.

## Performance evidence

The camera and storage ingress path uses a nonblocking bounded-channel publish call. Reproduce its
release-mode overhead measurement with:

```sh
cargo test --release --lib notification_publish_latency -- --ignored --nocapture
```

On macOS on the final issue #134 PR branch, 20,000 calls measured clone-only baseline p50/p95 of
208/1,209 ns and clone-plus-publish p50/p95 of 166/1,625 ns. The p95 delta was 416 ns against the
100,000 ns budget. The harness uses `hdrhistogram`, prints the run count and p50/p95 values, and
fails when the publish p95 exceeds that budget.
