# Reporting bugs

This chapter covers inspecting live logs, generating scrubbed diagnostics bundles, using Administrator in-app bug reporting, and filing reproducible defects through [`.github/ISSUE_TEMPLATE/bug.yml`](https://github.com/xnorpx/keeppeek/blob/main/.github/ISSUE_TEMPLATE/bug.yml). GitHub Issues are strictly for reproducible KeepPeek defects; route setup help, feature proposals, and security vulnerabilities to the appropriate channels below.

## Choose the right channel

| Need                                           | Destination                                                                           |
| ---------------------------------------------- | ------------------------------------------------------------------------------------- |
| Existing KeepPeek behavior reproducibly fails  | [Bug report](https://github.com/xnorpx/keeppeek/issues/new?template=bug.yml)          |
| Setup help, troubleshooting, or usage guidance | [Q&A](https://github.com/xnorpx/keeppeek/discussions/categories/q-a)                  |
| New behavior or a feature proposal             | [Suggestions](https://github.com/xnorpx/keeppeek/discussions/categories/suggestions)  |
| A security vulnerability                       | [Private security report](https://github.com/xnorpx/keeppeek/security/advisories/new) |

Search open and closed Issues before filing so evidence stays on one report. Duplicate, support, feature, or under-documented reports may be closed or marked `needs more info`.

## Inspect logs and download diagnostics (`/settings/logs`)

Open **Settings > Diagnostics > View logs** or **Settings > Logs & diagnostics > Open logs** (`/settings/logs`) to inspect live **Server** (`GET /logs` SSE stream) and **Browser / Svelte** logs, update the **Server capture filter**, pause/clear the view, or export redacted JSONL.

Immediately after reproducing a defect, select **Download diagnostics** (available in **Settings > Logs & diagnostics** and on `/settings/logs`). KeepPeek builds `keeppeek-diagnostics-*.json.gz` from `GET /logs/snapshot` (bounded retained server log buffer with a per-process `generation` epoch), current browser logs, health, runtime configuration, metrics, and collection metadata. The manifest marks `privacy: scrubbed`, but automated scrubbing can miss private data—always inspect the bundle before sharing. If a bundle cannot be generated, state the exact reason on the issue.

## In-app bug reporting (Administrators)

Administrators can start a bounded capture session from **Settings** (`/settings`) or **Settings > Logs** (`/settings/logs`) by selecting **Report a bug**:

1. Review what will be collected and choose **Collect logs only** or **Record screen**. Screen capture calls `getDisplayMedia({ video: true, audio: false })` and never requests a webcam, microphone, system audio, or tab audio.
2. Reproduce the defect normally inside KeepPeek. A persistent status bar displays elapsed time, recorded bytes, **Stop**, and **Cancel**. Capture stops automatically at **5 minutes** or **100 MiB** of screen media.
3. Review the scrubbed issue text, server/browser log counts, UTC window, evidence warnings (active filters, buffer eviction, stream gaps, or recorder restarts detected via `LogSnapshot.generation`), artifact sizes, retention window, and private video preview. You can exclude or discard the screen recording before sharing.
4. Confirm once more to submit when `[reporting]` is configured in `config.toml`, or download the scrubbed diagnostics package, optional screen recording, and notes locally. Local download stays available whenever remote submission is unconfigured or fails.

### Recorder reporting endpoints and bounds

When `[reporting]` is configured in `config.toml` (canonical HTTPS `origin`, optional `credential = "{secret:KEY}"` in `secrets.toml`, `retention_days` `1..=30`, default `7`), the recorder exposes three Administrator-only endpoints enforcing `Cache-Control: no-store` and the remote HTTPS policy (`426` on plaintext remote HTTP):

| Endpoint                 | Purpose & bounds                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `GET /reporting/status`  | Returns `{ configured, origin?, retention_days? }` without credentials or secret references (`{ configured: false }` when `[reporting]` is omitted).                                                                                                                                                                                                                                                                                                                     |
| `POST /reporting/grants` | Accepts `<= 128 KiB` `{ idempotencyKey, manifest }`, validates the v1 manifest (`diagnostics <= 28 MiB` gzip, optional `screen <= 100 MiB` WebM/MP4, combined `<= 128 MiB`, max 4 in-flight grants), resolves `{secret:KEY}` on the recorder, calls `POST {origin}/v1/reports` over verified HTTPS (no redirects, 10s timeout), registers the returned 5-minute single-report grant for log redaction, audits `bug_report_submit`, and returns `{ id, grant, expires }`. |
| `POST /reporting/audit`  | Accepts `<= 512 B` closed-enum lifecycle events (`start`, `cancel`, `local_download`, `complete`, `failed`) with optional `mode` and opaque `report_id`; writes non-sensitive entries to the access audit log.                                                                                                                                                                                                                                                           |

The browser uploads artifacts directly to the reporting service with the issued single-report bearer grant (`PUT /v1/reports/{id}/artifacts/{kind}`, `POST /v1/reports/{id}/finalize`, `GET`/`DELETE /v1/reports/{id}`). Recorder credentials never leave the recorder. See [Configuration reference](./configuration-reference.md) and [`docs/reporting-service.md`](https://github.com/xnorpx/keeppeek/blob/main/docs/reporting-service.md).

### Browser capability matrix

| Capability                                        | When supported                                                | When unavailable or denied                                                                          |
| ------------------------------------------------- | ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `navigator.mediaDevices.getDisplayMedia`          | Offers **Record screen** after explicit consent (video-only). | **Record screen** disabled, or denial falls back to logs-only collection.                           |
| `MediaRecorder` + supported VP8/WebM or MP4 codec | Encodes bounded chunks up to 5 minutes / 100 MiB.             | **Record screen** disabled; logs-only reporting stays available.                                    |
| `CompressionStream('gzip')`                       | Produces gzip-compressed `keeppeek-diagnostics-*.json.gz`.    | Discloses that the diagnostics package is unavailable while preserving report notes and recordings. |

### Manual real-browser capture smoke check

When qualifying a release in a real desktop browser:

1. Sign in as an Administrator over HTTPS or `localhost`, open **Settings**, and click **Report a bug**.
2. Choose **Record screen**, select the current KeepPeek tab in the browser picker, and confirm no microphone, webcam, or audio prompt appears.
3. Navigate to **Settings > Logs**, confirm the persistent bar stays visible with updating elapsed time and byte count, then end capture via **Stop** or the browser sharing banner.
4. Confirm the review dialog opens with a playable preview, download the recording, and run `ffprobe` to verify **one video stream and zero audio streams**.

## Video walkthrough

This walkthrough covers the Appearance, System, and Logs viewer (`/settings/logs`), downloading a scrubbed diagnostics bundle, and the redacted Administrator **Report a bug** flow.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.mp4">Download the logs and diagnostics bug-reporting demo</a>.
</video>

## Privacy-preserving usage reporting (separate from bug reports)

KeepPeek also provides optional, Administrator-controlled **Usage reporting** under **Settings > Usage reporting** (`[usage_reporting]` in `config.toml`) to help prioritize OS, CPU, camera compatibility, and feature work. It is completely separate from bug reports and **disabled by default** on every fresh install and upgrade (never prompted during onboarding, generates no UUID, and sends zero requests unless enabled).

- **Hierarchical opt-in**: **Basic system data** is required before enabling **Camera compatibility data** or **Feature adoption data**; disabled categories are omitted completely.
- **Privacy and payload bounds**: Uses a dedicated pseudonymous UUID v4 + server-only proof token (separate from access keys), caps uncompressed payloads at `64 KiB` of strictly allowlisted fields (never camera names, IPs, URLs, credentials, firmware, events, logs, or media), and lets Administrators **Preview exact JSON payload** locally before opting in.
- **Schedule, retention, and deletion**: Sends over HTTPS at most once every 24 hours after a `>= 15` minute startup delay. Raw rows expire within 60 days, source IPs are not stored, and public aggregates enforce `k >= 10` installations with complementary suppression. Disabling or selecting **Reset installation ID** sends bounded deletion (`DELETE /v1/installations/{installation_id}`), with **Retry deletion** if offline.

See [Configuration reference](./configuration-reference.md) and [`docs/usage-reporting.md`](https://github.com/xnorpx/keeppeek/blob/main/docs/usage-reporting.md).

## Redaction rules

Redact at the source before publishing any issue text, log excerpt, screenshot, or diagnostics package. Never publish:

- credentials, access keys, tokens, cookies, or authorization headers;
- camera IP addresses, hostnames, private filesystem prefixes, or stream/snapshot URLs;
- faces, license plates, private recordings, notification text, or location details;
- unsanitized logs, screenshots, packet captures, or unreviewed diagnostics bundles.

Do not attach evidence that cannot be confidently sanitized; describe how a maintainer can build an equivalent synthetic fixture instead.

## Complete the GitHub bug form (`bug.yml`)

Keep one issue focused on one observable defect and satisfy every required section of `.github/ISSUE_TEMPLATE/bug.yml`:

1. **Before submitting**: Confirm all 6 preflight checks truthfully (duplicate search, reproducible defect, current release or exact build, Health/diagnosis/log/console review, redaction, diagnostics package attached or unavailability explained).
2. **Affected area**: Select the primary failing user-visible workflow (one of: `Recording and ingest`, `Peek and live video`, `Events, search, and previews`, `Keep, timeline, and recorded playback`, `Export`, `Cameras, discovery, and configuration`, `Camera controls and vendor integration`, `Storage, retention, backup, and restore`, `Authentication, authorization, and privacy`, `Installation, upgrade, and service lifecycle`, `API, WebRTC, and external integrations`, `UI, mobile, and accessibility`, `Other`).
3. **Bug description & Reproduction steps**: Give the smallest deterministic sequence from a known starting state plus frequency (`always`, `intermittent`, or `N of M attempts`).
4. **Expected and actual behavior**: State observable differences (state, HTTP/error code, file, timing, or resource value)—not "it does not work".
5. **Regression, frequency, and impact**: Record prior working status (`yes`/`no`/`unknown`), last good / first bad builds if known, data-loss risk, and workarounds. Use `unknown` rather than guessing.
6. **Environment**: Fill all 9 fields (version/commit/build profile, OS/arch, install method, browser/client version, camera make/model/anonymized firmware, camera source, stream codec/resolution/FPS/audio, camera count & catalog scale, storage type & free space); use `N/A` only when inapplicable.
7. **Diagnostics package and sanitized evidence**: Drag in `keeppeek-diagnostics-*.json.gz` (or state why it is unavailable) plus correlated timestamps and focused excerpts.
8. **Fix acceptance criteria and verification**: Keep each criterion unchecked and pair one observable outcome with one concrete verification test or procedure:

```markdown
- [ ] AC-1: <the reported reproduction produces the correct observable outcome>
  - Expected outcome: <exact state or result>
  - Verification: <regression test, command, or reproducible procedure>
```

9. **Performance evidence**: For runtime, build, memory, storage, network, or latency defects, report metric + unit, baseline/current value, budget/target, workload/dataset, environment, exact command/harness, and run count + statistic (median/p95). For correctness-only defects, write `Performance: N/A` and state which performance paths cannot be affected.
