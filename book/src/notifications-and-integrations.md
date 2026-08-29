# Notifications and integrations

KeepPeek evaluates notification policy on the server after an event or operational transition has
been persisted. Camera workers never call a notification provider or MQTT broker directly. A slow
or unavailable integration therefore cannot stop camera ingest, recording, event storage, health
projection, or another delivery channel. See [Camera and stream health](./camera-health.md) for the
evidence that creates operational transitions.

Notification controls are available only when the server advertises `keeppeek.rules.v1`.

## Build and activate a rule

Open **Settings > Notifications** to create a rule. A rule can match:

- event creation, enrichment, and completion;
- camera offline, stale stream, unavailable decode, and interrupted recording transitions;
- storage or recording health changes;
- an explicit test send.

Filters can narrow a rule by camera or source, group, event kind, zone, confidence, image
availability, duration, severity, review state, and bookmark state. Schedules use an IANA timezone
and support active windows and quiet hours, including daylight-saving transitions.

Editing creates a draft. **Activate** validates and atomically promotes the complete draft; a
conflict or invalid draft leaves the previous active revision unchanged. Use the test action before
depending on a new destination. Test sends have separate rate accounting but still use the saved
credentials and provider configuration.

## Avoid duplicate and noisy alerts

KeepPeek gives one logical notification a stable identity derived from its rule, source, event or
outage, and lifecycle. Event revisions, retries, enrichment, process restart, and recovery retain
that identity. Equal message text never collapses unrelated cameras or events.

Rules can apply cooldowns per event family, camera and event kind, group, whole rule, or outage
interval. The cooldown starts when the logical notification is created and survives restart. Rate
limits can apply to the rule, channel, principal, or whole server. Critical bypass is explicit,
bounded, and audited.

A preliminary alert may be replaced by an enriched revision when a canonical image or final
duration becomes available within the configured deadline. Missing imagery does not suppress a
metadata alert unless the action explicitly requires an attachment. Late enrichment stays in
history without waking the user unless the rule permits it.

The inbox keeps unread, seen, acknowledged, and cleared state per authenticated principal. Provider
delivery and acknowledgement remain separate from KeepPeek review state. Delivery history explains
whether work was created, replaced, suppressed, collapsed, rate-limited, retried, expired,
delivered, or failed.

## Deliver through Pushover

Pushover is the supported push channel. To configure it:

1. Register an application at <https://pushover.net/apps/build>.
2. Obtain the application token and a user or delivery-group key.
3. Add a **Push** action to a notification rule.
4. Enter the token and key directly in **Settings > Notifications**.
5. Save, activate, and run the rule's test action.

The two credentials are write-only. KeepPeek stores them in server-owned notification state and
never returns them to the browser, health snapshots, logs, or delivery reasons. Editing non-secret
settings preserves the configured values; replacing credentials requires entering both again.

Push actions support optional device names, sound, priorities `-2` through `2`, a public base URL
for deep links, and one bounded JPEG attachment. Emergency priority `2` also requires retry and
expiry values and records provider acknowledgement separately from KeepPeek acknowledgement.

Timeouts, connection failures, HTTP 408, 425, 429, and 5xx responses use the rule's bounded durable
retry policy. Invalid credentials and other permanent provider failures do not retry indefinitely.
Disabling a rule or Push action stops new work and expires its pending attempts without deleting
delivery history.

## Forward events through MQTT

Open **Settings > Integrations** to configure, test, observe, or disable the MQTT event forwarder.
It requires an MQTT 5 broker; MQTT 3.1 and 3.1.1 are not fallback protocols.

Configure a stable client ID, KeepPeek instance ID, topic prefix, optional username and write-only
password, QoS, retention policy, and optional CA certificate for `mqtts://`. Use TLS outside an
explicitly trusted local development network. Credentials embedded in the broker URL are rejected.

The default topics are:

```text
keeppeek/{instance_id}/sources/{source_id}/events/{event_type}
keeppeek/{instance_id}/forwarders/{forwarder_id}/status
```

Every event publication is a complete JSON snapshot with `schema_version = 1`. Consumers should
deduplicate with `(instance_id, event_id, revision)`. Camera outage and recovery records use the
same envelope and carry bounded cause, affected streams, recording consequence, duration, and
recovery evidence. This profile publishes attachment metadata, not image bytes.

QoS 1 is the default and provides at-least-once delivery. A crash around broker acknowledgement can
redeliver the same stable identity, so consumers must still deduplicate. QoS 0 has no broker
acknowledgement; QoS 2 does not remove the need for application-level identity.

The durable MQTT outbox defaults to 64 MiB and reconnect delay grows from 250 ms to a 30-second
ceiling. Broker outage, restart, or configuration replacement does not reconnect camera streams.
When the outbox is full, forwarding fails visibly while event persistence and recording continue.

Settings and Prometheus expose connection state, bounded error detail, pending items and bytes,
retry and duplicate counts, last received and delivered times, and oldest unacknowledged work.

See the detailed [notification rule](https://github.com/xnorpx/keeppeek/blob/master/docs/notifications.md),
[Pushover](https://github.com/xnorpx/keeppeek/blob/master/docs/pushover.md), and
[MQTT 5 forwarder](https://github.com/xnorpx/keeppeek/blob/master/docs/event-forwarder.md)
references for every field, payload, metric, and failure state.
