---
name: keeppeek-mcp
description: Set up the KeepPeek MCP server, inspect live recorder health and logs, and configure cameras, storage, profiles, and notifications safely through MCP.
---

# KeepPeek MCP

Use this skill to connect an AI assistant or MCP client to KeepPeek, onboard and tune cameras with zero plaintext credentials, inspect recorder health and logs, and make validated configuration changes safely.

## 1. Locate KeepPeek and Configuration

Check that the `keeppeek` binary is available on the host:

```sh
keeppeek --help
```

Common binary and bundled skill locations:

| Environment | Binary location                                              | Bundled `SKILL.md` location                                                  |
| ----------- | ------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| Linux       | `keeppeek` in the extracted archive or `PATH`                | `<archive>/skills/keeppeek-mcp/SKILL.md`                                     |
| macOS       | `/Applications/KeepPeek.app/Contents/Resources/keeppeek`     | `/Applications/KeepPeek.app/Contents/Resources/skills/keeppeek-mcp/SKILL.md` |
| Windows     | `keeppeek.exe` in the extracted archive or install directory | `<archive>\skills\keeppeek-mcp\SKILL.md`                                     |
| Container   | `/keeppeek`                                                  | `/usr/local/share/keeppeek/skills/keeppeek-mcp/SKILL.md`                     |

To print, list, or export bundled skills directly from any KeepPeek binary:

```sh
keeppeek mcp skill
keeppeek mcp skill --list
keeppeek mcp skill --output ./skills/keeppeek-mcp
keeppeek mcp skill camera-debug --output ./skills/keeppeek-camera-debug
```

Locate the single application configuration file (`config.toml`) and its companion `secrets.toml` in the same directory:

| Environment | Default `config.toml` path                               |
| ----------- | -------------------------------------------------------- |
| macOS       | `~/Library/Application Support/keeppeek/config.toml`     |
| Linux       | `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/config.toml` |
| Windows     | `%APPDATA%\keeppeek\config.toml`                         |
| Container   | `/config/keeppeek/config.toml`                           |

When `--config <PATH>` is passed to KeepPeek, `secrets.toml` always resides in the same directory as that configuration file. Keep all settings in `config.toml` and reusable private strings in `secrets.toml`. Do not create a second settings file.

## 2. Configure the MCP Transport

Choose the local `stdio` bridge for local assistants, or enable the embedded Streamable HTTP endpoint for remote or networked MCP clients.

### Local `stdio` bridge and remote diagnostic utilities

Run the built-in `stdio` JSON-RPC bridge, or invoke remote log-streaming and capture-download utilities (pass `--server`, `--config`, `--credential-file`, and `--read-only` **before** `logs` or `pull-capture`):

```sh
keeppeek mcp [--server <URL>] [--config <PATH>] [--credential-file <PATH>] [--read-only]
keeppeek mcp [--server <URL>] [--credential-file <PATH>] logs [--level <LEVEL>] [--target <SUBSTR>] [--camera <ID>] [--query <SUBSTR>] [--filter <DIRECTIVE>] [--follow] [--limit <N>]
keeppeek mcp [--server <URL>] [--credential-file <PATH>] pull-capture <CAPTURE_ID> --output <DIR> [--artifact <NAME>...]
```

- `stdout` carries strict newline-delimited JSON-RPC frames (or JSONL `LogEntry` lines for `logs`, and `<artifact>\t<bytes>\t<sha256>` lines for `pull-capture`). Warnings and diagnostic output go to `stderr`.
- Pass `--read-only` to enforce read-only inspection even when the server allows MCP writes or camera device writes. The bridge sends `X-KeepPeek-MCP-Read-Only: true` (note `logs --filter` requires write access to call `set_logging_filter`).
- Authenticate with the `KEEPPEEK_ACCESS_KEY` environment variable or `--credential-file <PATH>`. On loopback, the server allows unauthenticated access only when its `[access].local_networks` policy permits local trust.

### Embedded Streamable HTTP (`POST /mcp`, opt-in)

The embedded `POST /mcp` endpoint is disabled by default and returns `404` until enabled by a human Administrator — either live in **Settings > Integrations > MCP server** (trusted local network or interactive OIDC/identity-proxy browser session, with explicit confirmation; not a Bearer access key) or by stopping KeepPeek, adding `[mcp]` to `config.toml`, and starting KeepPeek again:

```toml
[mcp]
enabled = true
allow_writes = false
allow_snapshots = false
allow_camera_device_writes = false
max_concurrent_requests = 4
max_response_bytes = 65536
request_timeout_seconds = 15
max_snapshot_bytes = 262144
max_snapshot_dimension = 1280
snapshot_min_interval_seconds = 5
```

- Keep `allow_writes = false`, `allow_snapshots = false`, and `allow_camera_device_writes = false` until the operator explicitly requests those capabilities.
- For remote access, keep `[access].require_secure_remote = true` and place KeepPeek behind a trusted HTTPS reverse proxy (`[access].trusted_proxies`) or a private VPN.
- Authenticate HTTP requests with `Authorization: Bearer <key>`. Never place access keys in query parameters.
- Browser `Origin` headers are rejected unless explicitly listed in `[direct_card].allowed_origins`, and any request carrying an `Origin` header requires `Authorization: Bearer <key>` even on local networks. Non-browser clients omit `Origin`, and unauthenticated local requests must use `localhost` or an IP literal `Host` header to block DNS rebinding.

## 3. Handle Credentials and Secrets Safely (Zero-Plaintext Policy)

1. Ask the operator to create a dedicated named credential in **Settings > Access & roles** for the MCP client:
   - Use the **User** role with scoped camera access for read-only event, coverage, and health monitoring.
   - Use the **Administrator** role only when the task requires server logs, storage or notification diagnostics, camera onboarding, hardware tuning, or configuration changes.
2. Store the MCP client access key in the `KEEPPEEK_ACCESS_KEY` environment variable or an owner-only credential file (`chmod 600` on Unix) passed via `--credential-file <PATH>`. Never pass raw access keys on the command line or paste them into chat.
3. Call `list_secret_keys` before referencing secrets in camera, probe, MQTT, or notification settings. This tool returns sorted key names from `secrets.toml` and never returns values.
4. When a required secret key is missing, NEVER ask the operator to paste the raw password into chat. Use one of these zero-plaintext paths instead:
   - **Loopback secret-entry handoff (browser UI):** Call `request_secret_entry` with the needed key names and purposes. Give the operator the returned `local_ui_url` (`http://127.0.0.1:<port>/mcp/secret-entry?token=<token>`), which is strictly loopback-only, single-use, and time-limited (`60..=300` seconds). Poll `get_secret_entry_status` until `status` is `"completed"`.
   - **Local CLI (`keeppeek secret`):** Ask the operator to store the secret out-of-band using a hidden TTY prompt or `--stdin` (secret values are never accepted in CLI arguments):
     ```sh
     keeppeek secret set CAMERA_PASSWORD
     printf '%s' "$SECRET_VALUE" | keeppeek secret set CAMERA_PASSWORD --stdin
     keeppeek secret list
     keeppeek secret remove OLD_CAMERA_PASSWORD
     ```
   - KeepPeek writes `secrets.toml` atomically with owner-only permissions (`0600` on Unix / owner-only DACL on Windows) and reloads updated secrets live without requiring a server restart.
5. Reference secrets in `config.toml` and MCP tools using:
   - `{secret:KEY}` for direct string fields such as camera `username`, camera `password`, camera `uid`, `username_secret_reference`, `password_secret_reference`, `uid_secret_reference`, MQTT `password`, or notification action `destination`.
   - `{secret:KEY|url}` for credentials embedded in a URL component such as `main_rtsp_url` or `sub_rtsp_url`.
   - Raw credentials passed to `probe_camera_streams`, `manage_camera`, `manage_notification_rule`, `plan_configuration_change`, or `manage_configuration_template` are rejected.

## 4. Zero-Plaintext Camera Onboarding and Hardware Tuning

Follow this exact end-to-end sequence when adding or tuning cameras:

1. **Discover devices and look up catalog hints:**
   - Call `discover_cameras` to run a bounded LAN probe.
   - Call `search_camera_catalog` with the brand/model `query` and optional `ip` to get credential-free RTSP `stream_hints`.
2. **Verify or collect secrets out-of-band:**
   - Call `list_secret_keys` to check which `secrets.toml` keys already exist.
   - If keys are missing, call `request_secret_entry` (loopback `GET`/`POST /mcp/secret-entry`) and wait via `get_secret_entry_status`, or ask the operator to run `keeppeek secret set <KEY>` (`--stdin` or hidden TTY prompt).
3. **Probe streams without raw credentials:**
   - Call `probe_camera_streams` using either single `{secret:KEY}` references (`username_secret_reference` + `password_secret_reference`, optional `uid_secret_reference`) or pre-populated `credential_candidates` (up to **5** pairs).
   - KeepPeek stops immediately on the first authentication match or on camera lockout, HTTP `429`, or an unreachable target.
   - Use the returned `matched_credentials` (`{secret:KEY}` references only), `recommended_backend`, and verified stream URLs for camera registration.
4. **Configure KeepPeek (Plan -> Review -> Approve -> Apply):**
   - Call `get_configuration` to read the current `configuration_revision`.
   - For a new camera, show the operator the proposed `manage_camera` (`action: "upsert"`) payload with `{secret:KEY}` references and wait for explicit approval before calling `manage_camera`.
   - For existing cameras, defaults, or templates, call `plan_configuration_change` (works even when `allow_writes = false`), show the redacted diff and live-vs-restart summary, wait for explicit approval, then call `apply_configuration_plan`.
5. **Inspect and tune on-camera hardware settings (optional):**
   - Call `get_camera_device_settings` to inspect live hardware sections (`encoder`, `imaging`, `lighting`, `motion`, `osd`, `time`, `ptz`), `supported_sections`, and `unsupported_sections` for the camera's protocol (`reolink`, `isapi`, `onvif`, or `rtsp_only`).
   - Call `update_camera_device_settings` with `dry_run: true` (the default; works even when `allow_camera_device_writes = false`) to validate ranges and produce a redacted `diff`.
   - Show the operator the `before`/`after` diff and `unsupported` list, and wait for explicit approval.
   - Only when `[mcp].allow_camera_device_writes = true`, the credential is **Administrator**, and `--read-only` is not set, call `update_camera_device_settings` with `dry_run: false`. Inspect `verified_settings` and `restart_or_reconnect_expected` from read-back verification.
   - Never request excluded security operations (firmware update, factory reset, reboot, password/user changes, or IP/network/port reassignment); KeepPeek always rejects them with `403`.
6. **Verify live operation:**
   - Call `get_server_health` and `search_logs` (and optionally `get_camera_snapshot` if `[mcp].allow_snapshots = true`) to confirm the camera connects and records cleanly.

## 5. General KeepPeek Configuration Workflow

For storage, templates, profiles, notifications, and logging changes:

1. **Inspect current state:** Call `get_configuration`, `get_runtime_configuration`, `list_runtime_profiles`, `manage_notification_rule` (`action: "list"`), and `list_secret_keys`.
2. **Preview and confirm:** Call `plan_configuration_change` for camera/default/template changes, or show the exact payload for runtime, notification, profile, or log-filter updates. Show the operator the expected revision, validation results, redacted diff, and live-vs-restart impact.
3. **Obtain explicit operator approval** before calling any write tool.
4. **Apply the approved change** (requires `[mcp].allow_writes = true`, **Administrator** role, and no `--read-only`):
   - `apply_configuration_plan`: apply a reviewed `plan_id` and expected revision.
   - `manage_camera`: add, update, or remove a camera by IP address using `{secret:KEY}` / `{secret:KEY|url}` references only.
   - `update_runtime_configuration`: update runtime storage, retention, or listener settings (always supply `expected_configuration_revision`).
   - `manage_configuration_template`: save, duplicate, or delete reusable camera templates (`{secret:KEY}` references only).
   - `manage_notification_rule`: `list`, `save_draft`, `activate`, or `delete` notification rules (`push`, `webhook`, and `forwarder` destinations require `{secret:KEY}` or preserved `destination_configured: true` with `destination_ref`).
   - `set_runtime_profile`: switch or update runtime profiles.
   - `set_logging_filter`: change the live diagnostic tracing filter.
5. **Verify activation:** Call `get_server_health`, `get_configuration`, and `search_logs`. If a restart is required, tell the operator clearly rather than claiming live activation succeeded.

### Example Tool Payloads

Search the camera catalog with IP stream hints (`search_camera_catalog`):

```json
{
  "query": "Reolink RLC-810A",
  "ip": "192.0.2.50",
  "limit": 5
}
```

Request loopback secret entry for missing credentials (`request_secret_entry`), then poll `get_secret_entry_status`:

```json
{
  "keys": [
    { "key": "FRONT_CAM_USER", "purpose": "Front door camera username" },
    { "key": "FRONT_CAM_PASS", "purpose": "Front door camera password" }
  ],
  "ttl_seconds": 180
}
```

```json
{
  "handoff_id": "<handoff-id-from-request_secret_entry>"
}
```

Probe a camera against up to 5 pre-populated secret candidates (`probe_camera_streams`):

```json
{
  "ip": "192.0.2.50",
  "onvif_port": 8000,
  "transport": "tcp",
  "query_onvif": true,
  "timeout_seconds": 8,
  "credential_candidates": [
    {
      "username_secret_reference": "{secret:FRONT_CAM_USER}",
      "password_secret_reference": "{secret:FRONT_CAM_PASS}"
    },
    {
      "username_secret_reference": "{secret:CAMERA_USERNAME}",
      "password_secret_reference": "{secret:CAMERA_PASSWORD}"
    }
  ]
}
```

Add a verified camera with secret references only (`manage_camera`):

```json
{
  "action": "upsert",
  "ip": "192.0.2.50",
  "expected_configuration_revision": "<revision-from-get_configuration>",
  "display_name": "Side Gate",
  "username": "{secret:FRONT_CAM_USER}",
  "password": "{secret:FRONT_CAM_PASS}",
  "main_rtsp_url": "rtsp://192.0.2.50:554/h264Preview_01_main"
}
```

Dry-run camera patch (`plan_configuration_change`), then apply with `apply_configuration_plan`:

```json
{
  "expected_configuration_revision": "<revision-from-get_configuration>",
  "targets": { "camera_ids": ["192.0.2.10"] },
  "change": {
    "patch": {
      "display_name": "Front Door",
      "recording_mode": "event_boost",
      "password_secret_reference": "{secret:CAMERA_PASSWORD}"
    }
  }
}
```

Inspect live on-camera hardware settings (`get_camera_device_settings`):

```json
{
  "camera_id": "192.0.2.50"
}
```

Preview and apply on-camera hardware settings (`update_camera_device_settings`, dry-run first, then `dry_run: false` after approval):

```json
{
  "camera_id": "192.0.2.50",
  "dry_run": true,
  "settings": {
    "imaging": { "brightness": 128, "contrast": 128 },
    "lighting": { "ir_mode": "auto" },
    "osd": { "channel_name": "Side Gate" }
  }
}
```

Update runtime storage retention (`update_runtime_configuration`):

```json
{
  "expected_configuration_revision": "<revision-from-get_runtime_configuration>",
  "storage": { "short_term_secs": 30, "long_term_max_gb": 500 }
}
```

Save a webhook notification draft using a secret reference (`manage_notification_rule`):

```json
{
  "action": "save_draft",
  "definition": {
    "id": "front-door-person",
    "name": "Front Door Person",
    "enabled": true,
    "revision": 0,
    "triggers": ["event_created"],
    "schedule": {
      "timezone": "UTC",
      "active_windows": [],
      "quiet_hours": null
    },
    "enrichment": {
      "deadline_ms": 5000,
      "maximum_revisions": 2,
      "maximum_attempts": 1,
      "maximum_attachment_bytes": 1048576,
      "wake_after_deadline": false
    },
    "actions": [
      {
        "enabled": true,
        "channel": "webhook",
        "destination": "{secret:WEBHOOK_URL}",
        "template": {
          "title": "Person detected",
          "body": "Camera {{source.name}}"
        },
        "attachment": "never",
        "allow_second_delivery": false
      }
    ],
    "failure": {
      "maximum_attempts": 3,
      "maximum_retry_interval_ms": 5000,
      "expiry_ms": 60000
    }
  }
}
```

## 6. Troubleshoot a Running Recorder

Use read-only diagnostic tools in order from broad health to specific evidence:

1. Call `get_server_health` to inspect recorder status, camera connection states, stream progress, and disk pressure. (For **User** credentials, results are filtered to permitted cameras.)
2. Call `search_logs` and `get_logging_settings` (Administrator) to inspect redacted server logs by level, target, camera, time window, and limit — or stream live SSE traces over HTTPS/HTTP with `keeppeek mcp --server <URL> logs --follow --camera <ID> --filter "..."` (which restores the previous filter via `set_logging_filter` on exit). Standard 3-tier filter directives:
   - `INFO`: `info,keeppeek=info` (or default `info,keeppeek=debug`)
   - `DEBUG`: `info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug`
   - `TRACE`: `retina=trace,reo_proto=trace,onvif=trace,keeppeek::rtsp=trace,keeppeek::reolink=trace,keeppeek::isapi=trace,keeppeek::cameras=trace,keeppeek::camera_events=trace,keeppeek::media_time=trace,keeppeek::storage::medium_term=trace`
3. Call `get_recording_coverage` to check recorded segments and gaps for authorized cameras over a bounded time window.
4. Call `search_events` to query stored events with a bounded `limit` (default 25, max 100) across authorized cameras.
5. Call `get_storage_status` (Administrator) to inspect storage paths, disk usage, named volumes, archive destinations, and recording maintenance status.
6. Call `get_notification_status` and `manage_notification_rule` (`action: "list"`) (Administrator) to inspect notification rules, cloud push status, and MQTT delivery health without exposing secrets.
7. Call `get_camera_device_settings` (Administrator) to verify on-camera encoder, imaging, lighting, motion, OSD, time, or PTZ configuration when stream quality or alerts look wrong.
8. Call `get_camera_snapshot` only when visual verification is needed and `[mcp].allow_snapshots = true`:
   - Requires camera view permission and respects `snapshot_min_interval_seconds` per camera.
   - Privacy is fail-closed: KeepPeek refuses capture if a `[privacy]` schedule or runtime profile override is active or cannot be verified.
   - Frames are bounded-decoded, downscaled to `max_snapshot_dimension`, and JPEG-encoded within `max_snapshot_bytes`. KeepPeek does not apply server-side pixel masks; only camera-side (hardware) privacy masks already rendered by the camera appear in the frame.
9. For deep protocol or bitstream debugging (decrypted PCAPNG captures, raw H.264/H.265/audio elementary streams, capture-correlated `traces.jsonl` with RAII `trace_filter` boost, remote `keeppeek mcp pull-capture` / chunked `read_artifact` downloads, transport comparisons, sanitized offline `ReplayCameraServer` fixtures via `camera_debugging`, and the `keeppeek-supervisor update --channel local` live binary hot-swap loop), switch to the bundled [`keeppeek-camera-debug`](../keeppeek-camera-debug/SKILL.md) skill (`keeppeek mcp skill camera-debug`). It requires explicit `[mcp].allow_camera_debugging = true` opt-in.

## 7. Strict Safety Rules

- **Never bypass security gates:** Do not try to work around `[mcp].allow_writes = false`, `[mcp].allow_camera_device_writes = false`, `--read-only`, `[mcp].allow_snapshots = false`, RBAC roles, camera grants, or fail-closed privacy checks.
- **Turn off MCP when finished if requested (`disable_mcp_access`):** When an Administrator task is complete (or the operator asks to lock down MCP), call `disable_mcp_access` with `{ "confirm": true, "reason": "..." }`. This works even when `[mcp].allow_writes = false` (blocked only under `--read-only`), persists `[mcp].enabled = false`, and immediately rejects new `/mcp` and `/mcp/secret-entry` requests with `404` live (already in-flight `/mcp` calls complete within `request_timeout_seconds`). Never assume you can re-enable MCP or silence the 7-day enabled warning yourself — both strictly require a human Administrator.
- **Never ask for or pass raw credentials:** Never ask the operator for raw passwords in chat, never pass raw secrets to MCP tools or `keeppeek secret set` CLI arguments, never echo secrets or access keys in responses or logs, and never write resolved secrets into `config.toml` or camera templates.
- **Never attempt excluded device operations:** Never ask `update_camera_device_settings` to upload firmware, factory-reset, reboot, change passwords/users, or reassign camera IP/network/ports.
- **Never overwrite on revision conflict:** If a write fails because the configuration revision changed, re-read the current configuration, generate a fresh `plan_configuration_change` preview, and ask the operator to approve the new plan.
- **Never edit `config.toml` under a running server:** Use the MCP tools while KeepPeek is running. Stop KeepPeek before making manual file edits to `config.toml` (for example, to change `[mcp]` settings).
- **Respect server and camera limits:** Keep requests within `max_concurrent_requests`, `max_response_bytes`, `request_timeout_seconds`, `snapshot_min_interval_seconds`, and the 5-candidate cap on `probe_camera_streams`. Stop probing on lockout or HTTP `429` instead of retrying aggressively.
