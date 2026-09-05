# Notification rules and delivery

KeepPeek owns notification policy and delivery state on the server. Camera and storage workers
publish normalized lifecycle transitions after their primary state mutation succeeds; they do not
perform provider I/O. The notification runtime evaluates rules on its own bounded worker and uses
one isolated delivery worker per channel.

The runtime is advertised as `keeppeek.rules.v1`. Clients must keep notification controls disabled
when that capability is absent.

## Configuration and runtime state

Notification rules, drafts, and active revisions are stored under `[notifications]` in
`config.toml`. Rule updates atomically replace that file and use expected revisions to reject stale
edits. A failed activation preserves the previous active rule and the current draft.

Pending deliveries, retries, deduplication records, cooldowns, rate windows, inbox receipts, and
history exist only in bounded process memory. Rule match and delivery timestamps are also
process-local. Restarting KeepPeek clears them. A restart can lose a pending delivery, allow a
replayed event to notify again, reset a cooldown or rate window, and clear the browser inbox.
KeepPeek does not promise durable or exactly-once notification delivery.

On the first start after upgrading, KeepPeek imports rule drafts and active revisions from a legacy
`notifications.db` when `[notifications]` is absent. It writes the rules and managed secret
references to `config.toml` and `secrets.toml`, discards legacy operational state, and then removes
the database and its sidecars. A failed migration leaves the legacy database intact.

A conflict returns both current rule revisions in a `google.protobuf.Any` detail with type URL
`type.keeppeek.dev/notification-rule-conflict.v1`.

## Inputs and identity

Rules can match event create, enrichment, and end revisions; `camera_offline`,
`stream_stale`, `decode_unavailable`, and `recording_interrupted` start, update, and recovery
revisions; global storage write failure and recovery; and explicit test sends. Event filters include
source, group, event kind, zone, confidence, attachment availability, duration, severity, reviewed
state, and bookmarked state.

A logical notification ID is the SHA-256 digest of length-delimited rule ID, source ID, source event
or outage identity, and lifecycle. Text is not part of the identity. Revisions, retries, preliminary
delivery, and enrichment retain one collapse key while the process remains running. Different
rules, cameras, events, and lifecycles cannot collapse based on equal text.

## Schedules and suppression

Schedules use an IANA timezone and weekly local-time windows. Quiet-hour evaluation converts each
transition instant through that timezone, including daylight-saving gaps and repeated hours.
Critical bypass is opt-in and has its own in-memory maximum and time window.

Cooldown scopes are event family, camera and event kind, group, whole rule, and outage interval.
The documented default is **logical notification creation time**. Recovery of an existing outage
updates the original logical notification before new-notification cooldown checks while the process
remains running.

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

Operational event revisions are lifecycle evidence, not image enrichment. Meaningful cause or
severity updates and recovery can replace the original logical notification after the enrichment
deadline and beyond the image-revision limit. Replaying an already processed event ID and revision
in the same process is collapsed without creating another action. Duration filters use elapsed
interval time on starts and updates and total interval duration on recovery. Webhook payloads include nested
source and event records with the stable event ID, revision, kind, lifecycle, stage, duration,
severity, recovery state, and bounded operational evidence.

Channel behavior is explicit:

| Channel   | Replacement                                        | Current adapter policy                                                                                   |
| --------- | -------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Browser   | Same collapse key replaces the browser item        | Delivered to the server inbox; a permitted browser uses the logical ID as its notification tag           |
| Push      | Same logical ID and stage retain one history group | Delivered through Pushover with device, sound, priority, deep-link, image, and emergency receipt support |
| Webhook   | No replacement guarantee                           | Enriched second delivery occurs only when the action opts in                                             |
| Forwarder | No replacement guarantee                           | Fails visibly as `channel_unavailable` until forwarding from issue #65 is configured                     |

Webhook requests have a five-second global timeout, no redirects, no arbitrary headers, and no URL
credentials. Provider payloads may contain bounded base64 JPEG bytes; local paths are never sent.
Structured delivery logs store a SHA-256 target hash rather than the destination.

## Outbox and retries

The pending and retrying in-memory outbox is bounded to 10,000 entries. Full queues record `expired`
with reason `outbox_full` rather than allocating unbounded memory.

Each queued action carries maximum attempts, maximum retry interval, expiry, priority, and a
replacement key. Retryable HTTP status codes are 408, 425, 429, and 5xx. `Retry-After` seconds are respected but
clamped to the rule maximum; other transient failures use bounded exponential backoff. Work that
cannot retry before expiry becomes `expired`. A process restart discards queued work.

Provider workers have separate in-memory queues. A slow or unavailable channel does
not block camera ingest, event storage, recording, health projection, or another channel.

See [Pushover notifications](pushover.md) for provider setup, supported fields, write-only secret
handling, privacy, limits, failure behavior, and emergency acknowledgement tracking.

## Inbox and acknowledgement

Unread, seen, KeepPeek acknowledgement, and cleared state belong to the authenticated principal and
are separate from provider delivery attempts. Opening a deep link first marks that logical
notification seen through the authenticated control channel, then follows the normal authorized UI
route. Clearing one receipt cannot affect another. Bulk clear requires an explicit all, rule, or
before-time scope and writes an audit record.

The server returns the principal's process-local unread count with every inbox query. Connected
browser clients refresh it and grouped in-memory delivery history every five seconds. Provider emergency
acknowledgement is not inferred from KeepPeek review acknowledgement.

KeepPeek emits structured outcome logs and Prometheus counters for candidate acceptance and drops,
logical notification outcomes, pending work, delivery attempts, retries, successes, and failures.

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
cargo test --locked --release --lib notification_publish_latency -- --ignored --nocapture
```

On macOS on the final issue #134 PR branch, 20,000 calls measured clone-only baseline p50/p95 of
208/1,209 ns and clone-plus-publish p50/p95 of 166/1,625 ns. The p95 delta was 416 ns against the
100,000 ns budget. The harness uses `hdrhistogram`, prints the run count and p50/p95 values, and
fails when the publish p95 exceeds that budget.
