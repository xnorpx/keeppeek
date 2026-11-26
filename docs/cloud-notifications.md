# Keeppeek Service Mobile Push Signaling

KeepPeek supports an optional, bounded `keeppeek_service` notification channel under the existing
notification runtime (`keeppeek.rules.v1`). The channel is capability-gated behind
`keeppeek.cloud-notifications.v1` and defaults to off (`enabled = false`,
`image_previews_enabled = false`).

## Privacy and Media Boundary

- Outbound requests go only to the paired canonical HTTPS Keeppeek Service origin at
  `POST /api/v1/nvr/events` with `Content-Type: application/octet-stream` and an exact 109-byte
  body (`256`-byte pre-parse ceiling).
- The signed body contains only:
  - Byte `0`: protocol version (`1`)
  - Bytes `1..17`: 16-byte service-assigned opaque NVR ID
  - Bytes `17..33`: random 128-bit `cloud_event_id` alias persisted once per logical event
  - Bytes `33..41`: issued-at Unix whole seconds (unsigned big-endian)
  - Bytes `41..45`: signing key version (unsigned big-endian, `>= 1`)
  - Bytes `45..109`: Ed25519 signature over `keeppeek.nvr-event.v1\0` followed by bytes `0..45`
- Local event IDs, camera IDs/names, event kinds, detection results, free text, attachment
  descriptors, media URLs, image bytes, video, audio, and bearer credentials never enter Keeppeek
  Service, logs, metrics, or status responses.
- Still-image previews default off and remain authoritative on the private NVR. Only an
  authenticated paired-mobile credential bound to the current NVR ID and authorized for the event's
  camera can resolve a 16-byte `cloud_event_id` via `ResolveCloudNotification` and fetch static
  still-image bytes (`image/jpeg` or `image/png`) through existing authenticated event media
  delivery (`EventSearchCommand.fetch_media`). Video, animated image, and audio previews are
  unsupported.

## Non-Configurable Local Safety Limits

| Boundary                  | Hard Limit                                                                                                                          |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Pending signal queue      | At most 1 claimed/in-flight + 1 newest-only pending intent                                                                          |
| Outbound event spacing    | 30 seconds between distinct production event intents                                                                                |
| Advisory attempt GCRA     | 2 attempts/second, burst 4                                                                                                          |
| Rolling production output | Exact rolling 120/hour and 500/day                                                                                                  |
| Test signals              | Exact rolling 3/hour (separate from production accounting)                                                                          |
| Retry envelope            | At most 3 attempts or 5 minutes TTL (whichever comes first), exact canonical byte reuse, bounded integer `Retry-After`, full jitter |
| HTTP client bounds        | 5-second timeout, 0 redirects, 4 KiB discarded response cap                                                                         |

## Configuration and Durable State

Settings are stored under `[cloud_notifications]` in `config.toml`, with the Ed25519 signing seed
stored exclusively in `secrets.toml` and referenced via `{secret:KEY}`. Durable runtime state
(aliases, pending/claimed intents, exact rolling windows, and monotonic clock floor) lives in
`cloud-notifications.db` under the configured long-term storage directory. Disabling the channel
cancels pending work while preserving spent capacity and accepted-ID deduplication across restart.

## External Conformance Boundary

This repository implements the NVR-side signaling runtime, local control API (`Request.command = 34`,
`Ok.result = 52`), durable state recovery, Settings UI, and deterministic local fakes/simulators.
External Keeppeek Service cloud infrastructure, APNs/FCM provider delivery, and physical mobile
device qualification remain outside this repository.
