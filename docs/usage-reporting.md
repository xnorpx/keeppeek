# Usage reporting

KeepPeek supports optional, opt-in **Usage reporting** to give maintainers aggregate evidence about active versions, deployment platforms, camera compatibility, and feature adoption.

Usage reporting is **disabled by default** on every fresh installation and upgrade. Installations that do not opt in generate no telemetry installation ID, run no recurring telemetry collection task, and send zero requests to the receiver.

## Privacy classification

Raw per-installation reports stored on the receiver are **pseudonymous**, not anonymous, because each report carries a persistent random `installation_id` UUID v4 so subsequent reports can replace the previous row and Administrators can delete or reset their installation's stored row.

Public outputs are **aggregate only**. No raw row, `installation_id`, or source IP address is published or persisted in public outputs.

## Published artifacts

- **Wire schema (outside protected `api/`)**: [`schemas/usage-reporting/v1.schema.json`](../schemas/usage-reporting/v1.schema.json)
- **Receiver reference source**: [`examples/usage_reporting_receiver/`](../examples/usage_reporting_receiver/README.md)
- **Representative public aggregate dataset**: [`examples/usage_reporting_receiver/public/aggregate-example.json`](../examples/usage_reporting_receiver/public/aggregate-example.json)

## Consent categories and dependencies

All categories default to `false` in `config.toml` under `[usage_reporting]`. Only an Administrator can read, preview, update, reset, or delete usage reporting state from **Settings**. KeepPeek never prompts during onboarding and never preselects any category.

**Basic system data** is required before **Camera compatibility data** or **Feature adoption data** can be enabled. Disabled categories are omitted completely from the serialized JSON (never sent as `null` or empty placeholders).

### 1. Basic system data (`basic`)

| Field                | Type / Bounds                                                                                                    | Purpose                                                     |
| -------------------- | ---------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| `keeppeek_version`   | SemVer string (`<= 64` chars)                                                                                    | Active release adoption and upgrade tracking                |
| `release_channel`    | `stable`, `beta`, `alpha`, `nightly`, `dev`, `unknown`                                                           | Release channel distribution                                |
| `deployment_type`    | `native`, `container`, `other`                                                                                   | Packaging and container runtime support                     |
| `os_family`          | `linux`, `macos`, `windows`, `bsd`, `other`, `unknown`                                                           | Platform prioritization                                     |
| `os_version`         | Normalized `major`, `major.minor`, `rolling`, or `unknown`                                                       | OS support window planning (never kernel build or hostname) |
| `architecture`       | `x86_64`, `aarch64`, `arm`, `riscv64`, `other`, `unknown`                                                        | Target binary and build prioritization                      |
| `cpu_vendor`         | `intel`, `amd`, `apple`, `arm`, `qualcomm`, `ampere`, `other`, `unknown`                                         | CPU family distribution                                     |
| `cpu_model_family`   | Finite reviewed family enum (never raw CPU description)                                                          | Hardware decode and performance planning                    |
| `physical_cores`     | Optional integer `1..=1024`                                                                                      | Sizing defaults for worker pools                            |
| `logical_cores`      | Integer `1..=2048`                                                                                               | Sizing defaults for worker pools                            |
| `memory_bucket`      | `lt_2gb`, `2_4gb`, `4_8gb`, `8_16gb`, `16_32gb`, `32_64gb`, `64_128gb`, `gte_128gb`, `unknown`                   | Memory budget planning                                      |
| `video_acceleration` | Unique array (`<= 8`) of `vaapi`, `nvenc`, `qsv`, `videotoolbox`, `v4l2`, `d3d11va`, `vulkan`, `none`, `unknown` | Hardware acceleration investment                            |

### 2. Camera compatibility data (`camera_compatibility`, optional)

| Field                | Type / Bounds                                              | Purpose                       |
| -------------------- | ---------------------------------------------------------- | ----------------------------- |
| `configured_cameras` | Integer `0..=10000`                                        | Typical deployment size       |
| `connected_cameras`  | Integer `0..=10000`                                        | Connectivity reliability      |
| `decodable_cameras`  | Integer `0..=10000`                                        | Decode compatibility          |
| `recording_cameras`  | Integer `0..=10000`                                        | Active recording scale        |
| `groups`             | Array (`<= 256`) of normalized camera compatibility groups | Hardware compatibility matrix |

Each group contains only: `manufacturer` (finite allowlisted slug or `unknown`), `model` (finite allowlisted slug for that manufacturer or `unknown`), `backend` (`auto`, `retina`, `reo_proto`, `unknown`), `transport` (`tcp`, `udp`, `unknown`), `video_codec` (`h264`, `h265`, `mjpeg`, `vp8`, `vp9`, `av1`, `unknown`), `resolution_bucket` (`sd`, `hd_720p`, `fhd_1080p`, `qhd_1440p`, `uhd_4k`, `above_4k`, `unknown`), `capabilities` (sorted unique subset of `ptz`, `audio`, `events`, `recording`, `analytics`, `imaging`, `two_way_audio`, `quick_replies`, `chimes`), and `count` (`1..=10000`).

Camera firmware is explicitly excluded in v1.

### 3. Feature adoption data (`feature_adoption`, optional)

| Field                        | Type / Bounds                                                                           | Purpose                             |
| ---------------------------- | --------------------------------------------------------------------------------------- | ----------------------------------- |
| `recording_modes`            | Array (`<= 8`) of `{ mode, count }` (`continuous`, `event_only`, `disabled`, `unknown`) | Storage and pre-roll prioritization |
| `storage_capacity_bucket`    | `lt_250gb`, `250gb_1tb`, `1tb_4tb`, `4tb_16tb`, `16tb_64tb`, `gte_64tb`, `unknown`      | Storage tier sizing                 |
| `projected_retention_bucket` | `lt_1d`, `1_7d`, `7_30d`, `30_90d`, `90_365d`, `gte_365d`, `unknown`                    | Retention default validation        |
| `enabled_integrations`       | Unique array (`<= 16`) of closed integration IDs (no endpoints or accounts)             | Integration maintenance priority    |
| `event_source_types`         | Unique array (`<= 12`) of closed event-source IDs                                       | Event ingestion compatibility       |

## Explicitly denied data

KeepPeek never collects or transmits:

- Camera IDs, user camera names, IP addresses, URLs, hostnames, ports, MAC addresses, serial numbers, hardware IDs, camera firmware (v1), or raw unknown manufacturer/model strings.
- Access keys, credentials, session IDs, user names, notification destinations, broker addresses, topic prefixes, filesystem paths, or file names.
- Events, event times, zones, alerts, motion counts, viewer history, exports, media, thumbnails, logs, diagnostics, crash dumps, `/metrics` output, public IP addresses, derived location, arbitrary labels, or raw error messages.

## Normalization guarantees

CPU brand strings and camera manufacturer/model metadata are mapped through finite reviewed tables in [`src/usage_reporting/normalize.rs`](../src/usage_reporting/normalize.rs) and verified identically by both sender and receiver against [`schemas/usage-reporting/v1.schema.json`](../schemas/usage-reporting/v1.schema.json). Any unmatched, ambiguous, or user-controlled string maps to `unknown`.

## Schedule, payload bounds, and failure isolation

- **Uncompressed payload limit**: strictly capped at `65,536` UTF-8 bytes (`64 KiB`). Payloads exceeding this limit are rejected locally before network access and rejected by the receiver (`413 Payload Too Large`).
- **Snapshot-only projection**: payloads are projected once from existing authoritative configuration, camera inventory, and health snapshots. Usage reporting never probes cameras, storage, integrations, or the network.
- **Schedule**: when enabled, the first report waits at least **15 minutes** after startup; subsequent reports run at most once every **24 hours** plus bounded non-negative random jitter. Last-attempt timestamps are persisted so rapid restarts or clock rollbacks cannot accelerate sends.
- **Bounded network work**: HTTPS only, native TLS with platform root verifier, zero redirects, `5s` timeout, `4 KiB` response body cap, one in-flight request, and zero retry storms (after a failure, the sender waits until the next scheduled interval).
- **Failure isolation**: receiver outage, DNS/TLS error, timeout, rate limit, rejection, or deletion failure never affects ingest, recording, playback, or event processing. Status surfaces only closed non-sensitive failure categories; payloads, installation UUIDs, tokens, and raw receiver bodies are never logged.

## Installation identity, receiver ordering, and deletion

- Explicit opt-in generates a dedicated random UUID v4 (`installation_id`) and a separate 256-bit per-installation proof token (`installation_token`). Neither reuses any KeepPeek access key, credential, camera ID, or session ID.
- `installation_token` is stored only in KeepPeek's internal durable telemetry state and is sent only in `Authorization: Bearer <installation_token>` to the configured HTTPS receiver. It never appears in the JSON payload, Settings preview, or logs.
- The receiver stores `SHA-256(installation_token)` and verifies mutations in constant time so each installation can replace or delete only its own UUID.
- Every report carries a monotonic `report_sequence` (`>= 1`). The receiver replaces the single row for `installation_id` (no activity history) and rejects stale lower sequences (`409 Conflict`).
- Turning off all categories immediately stops future sends and attempts a bounded `DELETE /v1/installations/{installation_id}`. Resetting identity rotates `(installation_id, installation_token)`, fences the prior `installation_id` with a bounded deletion tombstone, and attempts immediate deletion. Any late in-flight `PUT` for a deleted or reset `installation_id` is rejected with `410 Gone` and cannot resurrect the row.
- If the receiver is offline when disabling or resetting, KeepPeek records the pending deletion (bounded to 16 entries; further resets are refused rather than silently dropping undeleted rows) and allows explicit Administrator retry from Settings.

## Retention and public aggregation

- **Maximum raw retention**: active per-installation rows and deletion tombstones expire no later than **60 days** after the last successful report or deletion.
- **No source IP retention**: the receiver never stores client IP addresses, forwarded headers, or derived location in durable storage, logs, or aggregates.
- **k-anonymity threshold (`k >= 10`) and complementary suppression**: every published public aggregate cell counts distinct non-expired installations and requires at least **10** installations. When any cell in a mutually exclusive dimension is suppressed and the suppressed residual is below `10`, the receiver also suppresses the smallest published cell(s) until the suppressed residual is at least `10` (or hides the dimension) so rare installations cannot be inferred by subtracting published cells from totals.
- **Renewed consent**: any future schema or disclosure revision that adds a category or materially increases detail bumps `disclosure_revision` and keeps new fields disabled until an Administrator explicitly accepts the updated disclosure in Settings.
