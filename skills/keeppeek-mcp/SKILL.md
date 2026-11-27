---
name: keeppeek-mcp
description: Set up the KeepPeek MCP server, inspect live recorder health and logs, and configure cameras, storage, profiles, and notifications safely through MCP.
---

# KeepPeek MCP

Use this skill to connect an AI assistant or MCP client to KeepPeek, inspect recorder health and logs, and make validated configuration changes without exposing credentials.

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

To print or export this skill directly from any KeepPeek binary:

```sh
keeppeek mcp skill
keeppeek mcp skill --output ./skills/keeppeek-mcp
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

### Local `stdio` bridge (recommended for local clients)

Run the built-in `stdio` JSON-RPC bridge:

```sh
keeppeek mcp [--server <URL>] [--config <PATH>] [--credential-file <PATH>] [--read-only]
```

- `stdout` carries strict newline-delimited JSON-RPC frames. Warnings and diagnostic output go to `stderr`.
- Pass `--read-only` to enforce read-only inspection even when the server allows MCP writes. The bridge sends `X-KeepPeek-MCP-Read-Only: true`.
- Authenticate with the `KEEPPEEK_ACCESS_KEY` environment variable or `--credential-file <PATH>`. On loopback, the server allows unauthenticated access only when its `[access].local_networks` policy permits local trust.

### Embedded Streamable HTTP (`POST /mcp`, opt-in)

The embedded `POST /mcp` endpoint is disabled by default and returns `404` until enabled. To enable it, stop KeepPeek, add `[mcp]` to `config.toml`, and start KeepPeek again:

```toml
[mcp]
enabled = true
allow_writes = false
allow_snapshots = false
max_concurrent_requests = 4
max_response_bytes = 65536
request_timeout_seconds = 15
max_snapshot_bytes = 262144
max_snapshot_dimension = 1280
snapshot_min_interval_seconds = 5
```

- Keep `allow_writes = false` and `allow_snapshots = false` until the operator explicitly requests write operations or snapshots.
- For remote access, keep `[access].require_secure_remote = true` and place KeepPeek behind a trusted HTTPS reverse proxy (`[access].trusted_proxies`) or a private VPN.
- Authenticate HTTP requests with `Authorization: Bearer <key>`. Never place access keys in query parameters.
- Browser `Origin` headers are rejected unless explicitly listed in `[direct_card].allowed_origins`, and any request carrying an `Origin` header requires `Authorization: Bearer <key>` even on local networks. Non-browser clients omit `Origin`, and unauthenticated local requests must use `localhost` or an IP literal `Host` header to block DNS rebinding.

## 3. Handle Credentials and Secrets Safely

1. Ask the operator to create a dedicated named credential in **Settings > Access & roles** for the MCP client:
   - Use the **User** role with scoped camera access for read-only event, coverage, and health monitoring.
   - Use the **Administrator** role only when the task requires server logs, storage or notification diagnostics, camera discovery, or configuration changes.
2. Store the MCP client access key in the `KEEPPEEK_ACCESS_KEY` environment variable or an owner-only credential file (`chmod 600` on Unix) passed via `--credential-file <PATH>`. Never pass raw access keys on the command line or paste them into chat.
3. Call `list_secret_keys` before referencing secrets in camera, MQTT, or notification settings. This tool returns sorted key names from `secrets.toml` and never returns values.
4. Store camera passwords, webhook URLs, and integration credentials in `secrets.toml` (keys matching `[A-Z_][A-Z0-9_]*`, or override via `KEEPPEEK_SECRET_<KEY>` in the server environment).
5. Reference secrets in `config.toml` and MCP write tools using:
   - `{secret:KEY}` for direct string fields such as camera `username`, camera `password`, camera `uid`, MQTT `password`, or notification action `destination`.
   - `{secret:KEY|url}` for credentials embedded in a URL component such as `main_rtsp_url` or `sub_rtsp_url`.
   - Raw credentials passed to `manage_camera`, `manage_notification_rule`, `plan_configuration_change`, or `manage_configuration_template` are rejected.
6. If a required secret key does not exist yet, ask the operator to add the key locally to `secrets.toml` outside chat, then call `list_secret_keys` again to verify that the key name is available.

## 4. Configure KeepPeek Safely (Plan -> Review -> Approve -> Apply)

Follow this exact sequence for any configuration change:

1. **Inspect current state:** Call `get_configuration`, `get_runtime_configuration`, `list_runtime_profiles`, `manage_notification_rule` (`action: "list"`), and `list_secret_keys`.
2. **Discover devices (when adding cameras):** Call `discover_cameras` to run a read-only bounded LAN probe.
3. **Create a dry-run plan (for existing cameras, defaults, or templates):** Call `plan_configuration_change` with the proposed change. Dry-run planning works even when `[mcp].allow_writes = false` or `--read-only` is active.
4. **Present the plan or proposed mutation to the operator:** Show the returned `plan_id` (if applicable), expected revision, validation errors (if any), redacted diff, affected cameras or rules, and which changes apply live versus require a server restart.
5. **Obtain explicit operator approval:** Wait for the operator to confirm the exact change before calling any write tool.
6. **Apply the approved change:** Only when `[mcp].allow_writes = true`, the credential has the **Administrator** role, and the client is not in `--read-only` mode:
   - Call `apply_configuration_plan` with the approved `plan_id` and revision for camera, default, and template plans.
   - Use `manage_camera` (`action: "upsert"` or `"remove"`) to add, update, or remove a camera by IP address (credentials must use `{secret:KEY}` or `{secret:KEY|url}` references only).
   - Use `update_runtime_configuration` for runtime storage, retention, or listener settings (always supply `expected_configuration_revision`).
   - Use `manage_configuration_template` to save, duplicate, or delete camera templates (use `{secret:KEY}` references only; never inline credentials).
   - Use `manage_notification_rule` (`list`, `save_draft`, `activate`, `delete`) to manage notification rules; `push`, `webhook`, and `forwarder` destinations must use `{secret:KEY}` references or preserve `destination_configured: true` with `destination_ref`.
   - Use `set_runtime_profile` to switch or manage runtime profiles.
   - Use `set_logging_filter` to change the live diagnostic tracing filter.
7. **Verify activation:** Call `get_server_health`, `get_configuration`, and `search_logs` after applying changes. If the plan reported that a restart is required, tell the operator to restart KeepPeek before expecting runtime activation.

### Example Tool Payloads

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

Add a camera with secret references only (`manage_camera`):

```json
{
  "action": "upsert",
  "ip": "192.0.2.50",
  "expected_configuration_revision": "<revision-from-get_configuration>",
  "display_name": "Side Gate",
  "username": "{secret:CAMERA_USER}",
  "password": "{secret:CAMERA_PASSWORD}",
  "main_rtsp_url": "rtsp://192.0.2.50:554/main"
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

## 5. Troubleshoot a Running Recorder

Use read-only diagnostic tools in order from broad health to specific evidence:

1. Call `get_server_health` to inspect recorder status, camera connection states, stream progress, and disk pressure. (For **User** credentials, results are filtered to permitted cameras.)
2. Call `search_logs` and `get_logging_settings` (Administrator) to inspect redacted server logs by level, target, camera, time window, and limit.
3. Call `get_recording_coverage` to check recorded segments and gaps for authorized cameras over a bounded time window.
4. Call `search_events` to query stored events with a bounded `limit` (default 25, max 100) across authorized cameras.
5. Call `get_storage_status` (Administrator) to inspect storage paths, disk usage, named volumes, archive destinations, and recording maintenance status.
6. Call `get_notification_status` and `manage_notification_rule` (`action: "list"`) (Administrator) to inspect notification rules, cloud push status, and MQTT delivery health without exposing secrets.
7. Call `get_camera_snapshot` only when visual verification is needed and `[mcp].allow_snapshots = true`:
   - Requires camera view permission and respects `snapshot_min_interval_seconds` per camera.
   - Privacy is fail-closed: KeepPeek refuses capture if a `[privacy]` schedule or runtime profile override is active or cannot be verified.
   - Frames are bounded-decoded, downscaled to `max_snapshot_dimension`, and JPEG-encoded within `max_snapshot_bytes`. KeepPeek does not apply server-side pixel masks; only camera-side (hardware) privacy masks already rendered by the camera appear in the frame.

## 6. Strict Safety Rules

- **Never bypass security gates:** Do not try to work around `[mcp].allow_writes = false`, `--read-only`, `[mcp].allow_snapshots = false`, RBAC roles, camera grants, or fail-closed privacy checks.
- **Never overwrite on revision conflict:** If a write fails because the configuration revision changed, re-read the current configuration, generate a fresh `plan_configuration_change` preview, and ask the operator to approve the new plan.
- **Never edit `config.toml` under a running server:** Use the MCP tools while KeepPeek is running. Stop KeepPeek before making manual file edits to `config.toml` (for example, to change `[mcp]` settings).
- **Never expose raw credentials:** Never ask for raw passwords in chat, never echo secrets or access keys in responses or logs, and never write resolved secrets into `config.toml` or camera templates.
- **Respect server resource limits:** Keep requests within `max_concurrent_requests`, `max_response_bytes`, `request_timeout_seconds`, and `snapshot_min_interval_seconds`. Narrow log, event, or coverage queries instead of retrying aggressively when responses are truncated or rate-limited.
