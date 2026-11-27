# Notifications and integrations

This chapter covers server-owned notification rules (browser alerts, Pushover, webhooks, and optional bounded Keeppeek Service mobile push signaling) and the MQTT 5 event forwarder, configured by Administrators in **Settings** (`/settings#notifications` and `/settings#integrations`). Camera and storage workers publish transitions only after committing state and never perform provider or broker I/O, so a slow or unreachable integration cannot disrupt camera ingest, recording, event storage, health projection, or other channels.

## Channels and runtimes at a glance

| Channel / runtime           | UI location                                                                             | Capability gate                   | Status                                  | Key boundary                                                                                            |
| --------------------------- | --------------------------------------------------------------------------------------- | --------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| **Browser alerts & inbox**  | **Settings > Notifications**                                                            | `keeppeek.rules.v1`               | Available                               | Process-local per-principal inbox; desktop popups need browser permission and an open frontend.         |
| **Push (Pushover)**         | **Settings > Notifications** (**Push**)                                                 | `keeppeek.rules.v1`               | Available                               | HTTPS Pushover Messages API; write-only secrets in `secrets.toml`; JPEG `<= 4 MiB`; priority `-2..2`.   |
| **Keeppeek Service**        | **Settings > Notifications** (**Keeppeek Service** + **Bounded mobile push signaling**) | `keeppeek.cloud-notifications.v1` | Optional (off by default)               | Exact 109-byte signed signal only; zero metadata/media sent to cloud; durable `cloud-notifications.db`. |
| **Webhook**                 | **Settings > Notifications** (**Webhook**)                                              | `keeppeek.rules.v1`               | Available                               | 5s timeout, no redirects, no custom headers, no URL credentials; second delivery on enrichment opt-in.  |
| **Forwarder (rule action)** | **Settings > Notifications** (**Forwarder**)                                            | `keeppeek.rules.v1`               | **Unavailable (`channel_unavailable`)** | Exposed in the rule editor but fails visibly as `channel_unavailable`. **Not** the MQTT forwarder.      |
| **MQTT 5 event forwarder**  | **Settings > Integrations**                                                             | `keeppeek.mqtt-forwarder.v1`      | Available                               | Separate MQTT 5 runtime (no 3.1/3.1.1 fallback); JSON snapshots (`schema_version = 1`), metadata only.  |

## Video walkthrough

This walkthrough demonstrates configuring notification rules, Pushover, bounded Keeppeek Service mobile push signaling, and MQTT 5 forwarder health in **Settings**.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.mp4">Download the notifications and MQTT setup demo</a>.
</video>

## Build and activate notification rules

Notification rule controls appear in **Settings > Notifications** only when the server advertises `keeppeek.rules.v1`.

1. Select **Add rule** and choose one or more triggers: **Event starts**, **Event enriched**, **Event ends**, **Camera outage** (`camera_offline`, `stream_stale`, `decode_unavailable`, `recording_interrupted`), **Camera recovery**, **Storage health**, or **Recording health**.
2. Narrow matching with filters: camera/source IDs, groups, event kinds, zones, minimum confidence, minimum duration, image predicate (**Any**, **Image required**, **No image**), severity, review state, and bookmark state.
3. Configure an IANA timezone, active windows, DST-aware **Quiet hours**, cooldowns, fixed-window rate limits, and optional **Bounded critical bypass**.
4. Add up to 8 ordered actions (**Browser**, **Push**, **Keeppeek Service**, **Webhook**; avoid **Forwarder**, which returns `channel_unavailable`).
5. Use **Save draft** to store changes without activating, or **Save & activate** to validate and atomically promote the draft. Expected-revision checks reject stale edits without altering the previous active revision.
6. Run the rule's test action, then verify one real matching event in **Events** (`/events`). A test send uses separate rate accounting and validates credentials/transport, not your camera filters or schedule.

Active rules and drafts persist under `[notifications]` in `config.toml` across restarts. Pending deliveries, retries, deduplication keys, cooldowns, rate windows, inbox receipts, and history are process-local in memory and reset on restart.

## Deduplication, inbox, webhooks, and retries

- **Stable collapse identity**: Each logical notification derives a `SHA-256` identity from rule ID, source ID, event or outage ID, and lifecycle. Revisions, retries, enrichment, and recovery share one collapse key while running; identical text never merges unrelated cameras or events.
- **Cooldowns and rate limits**: Cooldowns support **Event family**, **Camera + event kind**, **Group**, **Whole rule**, and **Outage interval**, starting at logical notification creation time. Rate limits apply per rule, channel, principal, or server. Both reset on restart.
- **Preliminary vs enriched delivery**: A canonical image or end revision arriving before the enrichment deadline can replace a preliminary alert. Missing imagery never blocks metadata alerts unless the action sets **Attachment** to **Required**. Late enrichment is recorded without waking the user unless permitted.
- **Inbox and History**: **Settings > Notifications** provides **Rules**, **Inbox**, and **History** tabs (refreshed every 5 seconds). Inbox actions (mark seen, acknowledge, clear, **Clear all** → **Confirm clear all**) apply per authenticated principal and stay separate from event review state, camera health, and Pushover emergency acknowledgements.
- **Browser alerts**: Add a **Browser** action and select **Enable browser alerts** to grant OS/browser permission. Browser notifications require an active frontend and are not an offline push service for a closed browser.
- **Webhooks**: Enforce a 5-second timeout, zero redirects, no arbitrary headers, and no credentials in the URL. Enable **Allow second delivery without replacement** only when the receiving endpoint expects an enriched follow-up POST.
- **Outbox and retry bounds**: The in-memory notification outbox holds at most **10,000** pending/retrying actions; overflow records `expired` (`outbox_full`). Timeouts, connection failures, and HTTP `408`, `425`, `429`, and `5xx` responses use bounded exponential backoff (clamping `Retry-After` to rule bounds). Invalid credentials and other `4xx` responses fail permanently without infinite retries. Disabling a rule or action expires its pending work immediately; restarting KeepPeek discards queued work.

## Pushover delivery

Pushover is the supported third-party push channel:

1. Register an application at <https://pushover.net/apps/build> and copy the 30-character application token plus your user or delivery-group key.
2. In **Settings > Notifications**, add a **Push** action to a rule and enter both credentials.
3. Configure optional fields: comma-separated **Devices**, **Sound**, **Priority** (`-2` through `2`), public **Deep-link base URL**, and JPEG **Attachment**. Emergency priority `2` requires retry (`>= 30s`) and expiry (`1..10,800s`) and polls receipts at `>= 5s` intervals.
4. Select **Save & activate**, run the test action, and inspect **History** for the provider request ID.

Pushover credentials are write-only: KeepPeek stores values in `secrets.toml` and references in `config.toml`, never returning them to the browser, health snapshots, logs, or delivery reasons. Editing non-secret fields preserves stored credentials; replacing credentials requires entering both values again. Titles cap at 250 characters, bodies at 1,024, deep links at 512, and JPEG attachments at **4 MiB** (below Pushover's 5 MiB limit). Templates support `{{source.name}}` and `{{source.id}}`. Requests use verified HTTPS, a 5-second timeout, zero redirects, and at least 5 seconds between retries.

## Bounded Keeppeek Service mobile push signaling

When the server advertises `keeppeek.cloud-notifications.v1`, Administrators can enable an optional `keeppeek_service` rule action for compact mobile push signaling. Both signaling and direct still-image previews default to off (`enabled = false`, `image_previews_enabled = false`) under `[cloud_notifications]` in `config.toml`. Pairing requires a canonical HTTPS `service_origin`, 16-byte `nvr_id`, `signing_key_version >= 1`, and an Ed25519 signing seed referenced solely as `signing_key = "{secret:KEY}"` in `secrets.toml`.

Manage the runtime in **Settings > Notifications** under **Bounded mobile push signaling**:

- Toggle **Enable Keeppeek Service signaling** and optionally **Allow private direct still-image previews** (requires signaling enabled and active pairing), then select **Save cloud settings**.
- Use **Send test signal (3/hr limit)** and **Refresh cloud status** to verify admission and inspect state (`DISABLED`, `UNPAIRED`, `READY`, `THROTTLED`, `BACKING OFF`, `UNAVAILABLE`) and rolling `1h / 24h` acceptance and suppression counters.
- Add a **Keeppeek Service** action to matching rules (**Attachment** cannot be set to **Required** on this channel).

### Privacy boundary and non-configurable limits

Outbound signals hit `POST /api/v1/nvr/events` (`application/octet-stream`) over verified HTTPS (5s timeout, 0 redirects, 4 KiB discarded response cap) with an **exact 109-byte binary payload**: protocol version (`1`), 16-byte opaque `nvr_id`, a random 128-bit `cloud_event_id` alias persisted once per logical event, issued-at Unix seconds, `signing_key_version`, and a 64-byte Ed25519 signature. Keeppeek Service **never** receives local event IDs, camera names or IDs, event kinds, detection payloads, free text, media URLs, attachment descriptors, or image bytes.

If **Allow private direct still-image previews** is enabled, an authenticated paired mobile credential bound to the current `nvr_id` and authorized for the event's camera resolves the 16-byte `cloud_event_id` directly against the private NVR and fetches static still images (`image/jpeg` or `image/png`) via existing event media delivery. Video, animated images, and audio previews are unsupported.

| Safety boundary          | Hard local limit (non-configurable)                                                                                           |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| Pending signal queue     | 1 claimed/in-flight + 1 newest-only pending intent                                                                            |
| Production event spacing | 30 seconds between distinct event intents                                                                                     |
| Attempt pacing (GCRA)    | 2 attempts/second, burst 4                                                                                                    |
| Production rolling cap   | Exact rolling 120/hour and 500/day                                                                                            |
| Test-signal cap          | Exact rolling 3/hour (separate accounting)                                                                                    |
| Retry envelope           | Max 3 attempts or 5-minute TTL (whichever first), exact 109-byte payload reuse, bounded `Retry-After` + jitter                |
| Durable local state      | `cloud-notifications.db` under long-term storage (preserves aliases, pending intent, budgets, and clock floor across restart) |

Signal acceptance confirms **Keeppeek Service admission only**, not APNs/FCM provider delivery or mobile display. Disabling the channel cancels pending work while preserving spent budgets and accepted-ID deduplication.

## MQTT 5 event forwarder

Configure, test, and monitor MQTT forwarding under **Settings > Integrations** (**Edit broker** → **Save MQTT settings** / **Test connection** / **Refresh status**) or under `[event_forwarder.mqtt]` in `config.toml`. This requires the Administrator role, `keeppeek.mqtt-forwarder.v1`, and an **MQTT 5** broker—MQTT 3.1 and 3.1.1 are rejected with no fallback.

- **Security**: Use `mqtts://` with system trust or a PEM `tls_ca_path` outside trusted local dev networks. Credentials embedded in `broker_url` are rejected; broker passwords are write-only and stored in `secrets.toml` (`{secret:MQTT_PASSWORD}`, overridable via `KEEPPEEK_SECRET_MQTT_PASSWORD`). **Test connection** uses a temporary client ID and non-retained status so it never evicts the live client.
- **Default topics** (dynamic segments are percent-encoded to prevent `/`, `+`, `#` injection):
  - Events: `keeppeek/{instance_id}/sources/{source_id}/events/{event_type}` (unretained by default)
  - Status: `keeppeek/{instance_id}/forwarders/{forwarder_id}/status` (retained by default; Last Will sets `disconnected`)
- **Payload & properties**: Every message is a complete JSON snapshot with `schema_version = 1` and MQTT 5 properties (`application/json`, UTF-8 indicator, Correlation Data = event or forwarder ID, User Property `schema-version=1`). Deduplicate consumers on `(instance_id, event_id, revision)`. Operational outage/recovery records (`camera_offline`, `stream_stale`, `decode_unavailable`, `recording_interrupted`) use the same envelope. Attachment metadata is published, never image bytes.
- **Delivery & bounds**: Default QoS `1` (`0`, `1`, `2` supported; QoS `2` does not survive KeepPeek restart), `clean_start = false` with 24-hour session expiry, 1 in-flight publish, 256-command ingestion queue, 100,000-key process-local receipt deduplication set, **64 MiB** default in-memory outbox (`outbox_full` fails visibly while recording/storage continue), and `250 ms` → `30s` reconnect backoff. Restarting KeepPeek discards queued publications and deduplication history (events are not replayed from `recordings.db`).
- **Observability**: Settings and Prometheus (`/metrics` `keeppeek_mqtt_forwarder_*`) expose state, redacted errors, pending items/bytes, retries, duplicates, last received/delivered times, and oldest unacknowledged work.

For Home Assistant, the [direct camera card](./home-assistant.md) displays live video only; configure MQTT topics and deduplication independently in Home Assistant automations.

## Troubleshooting

| Symptom                                          | What to check                                                                                                                       |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| No event appears in **Events**                   | Verify [Native camera events](./native-camera-events.md) or [External analysis](./external-analysis.md), camera scope, and UTC day. |
| Rule saved but never fires                       | Click **Save & activate** and resolve any draft revision conflict.                                                                  |
| `suppressed`, `collapsed`, or `rate_limited`     | Inspect active filters, **Quiet hours**, cooldown scope, and next eligible time in **History**.                                     |
| **Forwarder** action fails                       | Returns `channel_unavailable` by design; configure MQTT under **Settings > Integrations** instead.                                  |
| Pushover fails permanently                       | Verify both write-only credentials and destination with a test send; invalid credentials do not retry indefinitely.                 |
| Keeppeek Service shows `THROTTLED` or `UNPAIRED` | Check `[cloud_notifications]` pairing fields, `{secret:KEY}`, and the hard 30s / 120-per-hour / 500-per-day pacing counters.        |
| MQTT pending bytes grow continuously             | Verify MQTT 5 broker reachability, TLS trust, credentials, and acknowledgements before the bounded outbox fills.                    |
| Inbox or queued work gone after restart          | Notification outbox, inbox, cooldowns, and MQTT outbox are process-local (only `[cloud_notifications]` state persists in SQLite).   |

See the detailed references for [notification rules](https://github.com/xnorpx/keeppeek/blob/main/docs/notifications.md), [Pushover](https://github.com/xnorpx/keeppeek/blob/main/docs/pushover.md), [Keeppeek Service signaling](https://github.com/xnorpx/keeppeek/blob/main/docs/cloud-notifications.md), [MQTT 5 forwarding](https://github.com/xnorpx/keeppeek/blob/main/docs/event-forwarder.md), and [Camera and stream health](./camera-health.md).
