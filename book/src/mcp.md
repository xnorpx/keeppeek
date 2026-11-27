# MCP server and LLM configuration

KeepPeek includes built-in Model Context Protocol (MCP) support so AI assistants and MCP clients
(such as Claude, Codex, Cursor, and Gemini) can inspect recorder health, logs, events, storage,
and recording coverage, and preview or apply validated configuration changes.

MCP is disabled by default for network access. You can connect locally through the `keeppeek mcp`
`stdio` bridge or enable the embedded Streamable HTTP endpoint (`POST /mcp`) in `config.toml`.

```mermaid
flowchart LR
    A[Local AI client] -->|stdio JSON-RPC| B[keeppeek mcp bridge]
    B -->|Bearer auth over HTTP or HTTPS| K[KeepPeek POST /mcp]
    R[Remote MCP client] -->|HTTPS Bearer auth| P[Trusted reverse proxy or VPN]
    P --> K
```

## Use the bundled agent skill

KeepPeek ships a canonical agent skill (`skills/keeppeek-mcp/SKILL.md`) that teaches LLM
assistants how to locate `config.toml` and `secrets.toml`, use `{secret:KEY}` references, run
`plan_configuration_change` before requesting writes, and respect privacy and role boundaries.

Print the skill from any installed KeepPeek binary or export it to a directory:

```sh
keeppeek mcp skill
keeppeek mcp skill --output ./skills/keeppeek-mcp
```

Release artifacts and container images also include `SKILL.md` directly:

| Package                       | Shipped skill path                                             |
| ----------------------------- | -------------------------------------------------------------- |
| Linux `.tar.gz` archive       | `keeppeek-<version>-<platform>/skills/keeppeek-mcp/SKILL.md`   |
| Windows `.zip` archive        | `keeppeek-<version>-<platform>\skills\keeppeek-mcp\SKILL.md`   |
| macOS `.dmg` (`KeepPeek.app`) | `KeepPeek.app/Contents/Resources/skills/keeppeek-mcp/SKILL.md` |
| Container image               | `/usr/local/share/keeppeek/skills/keeppeek-mcp/SKILL.md`       |

To guide an assistant (Claude, Codex, Cursor, or Gemini), copy `SKILL.md` into the assistant's
skills folder or point the assistant at the exported file before asking it to inspect or configure
KeepPeek.

## Enable and configure `[mcp]`

All MCP server settings live in the existing `config.toml` under `[mcp]`. Every capability that
opens network access, mutates configuration, or captures camera frames defaults to `false`.

Stop KeepPeek before editing `config.toml` manually, add `[mcp]`, and restart the server:

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

| Field                           | Type    | Default  | Allowed range     | Meaning                                                                                               |
| ------------------------------- | ------- | -------- | ----------------- | ----------------------------------------------------------------------------------------------------- |
| `enabled`                       | `bool`  | `false`  | `true`/`false`    | Enable the embedded Streamable HTTP endpoint at `POST /mcp`.                                          |
| `allow_writes`                  | `bool`  | `false`  | `true`/`false`    | Permit Administrator write tools after explicit plan approval.                                        |
| `allow_snapshots`               | `bool`  | `false`  | `true`/`false`    | Permit privacy-gated single-frame snapshots via `get_camera_snapshot`.                                |
| `max_concurrent_requests`       | `usize` | `4`      | `1..=32`          | Maximum simultaneous in-flight MCP requests.                                                          |
| `max_response_bytes`            | `usize` | `65536`  | `4096..=262144`   | Maximum serialized JSON-RPC response size in bytes (`get_camera_snapshot` uses `max_snapshot_bytes`). |
| `request_timeout_seconds`       | `u64`   | `15`     | `1..=60`          | Server-side deadline per MCP request, in seconds.                                                     |
| `max_snapshot_bytes`            | `usize` | `262144` | `16384..=1048576` | Maximum encoded JPEG size for `get_camera_snapshot`, in bytes (base64 + metadata overhead is added).  |
| `max_snapshot_dimension`        | `u32`   | `1280`   | `160..=1920`      | Maximum width or height of returned snapshot frames, in pixels.                                       |
| `snapshot_min_interval_seconds` | `u64`   | `5`      | `1..=300`         | Minimum interval between snapshot captures per camera, in seconds.                                    |

See [Configuration reference](./configuration-reference.md#mcp-server) for validation rules and
file ownership details.

## Connect an MCP client

Create a dedicated named credential in **Settings > Access & roles** for each MCP client so its
activity appears separately in audit records and can be rotated or revoked independently. Use the
**User** role for camera-scoped read-only monitoring, or **Administrator** for full diagnostics and
configuration management. See [Authentication and access control](./authentication.md).

### Local `stdio` bridge

Most desktop AI assistants launch MCP servers over `stdio`. KeepPeek provides a built-in `stdio`
JSON-RPC bridge that talks to your KeepPeek server over `POST /mcp` (with `[mcp].enabled = true`):

```sh
keeppeek mcp [--server <URL>] [--config <PATH>] [--credential-file <PATH>] [--read-only]
```

- `--server <URL>` targets a specific KeepPeek server origin (for example `http://127.0.0.1:8081`
  or `https://keeppeek.example.net`).
- `--config <PATH>` selects an alternate `config.toml` when resolving local server settings.
- `--credential-file <PATH>` reads the Bearer access key from an owner-only file instead of an
  environment variable.
- `--read-only` sends `X-KeepPeek-MCP-Read-Only: true`, blocking all write tools even if the server
  has `[mcp].allow_writes = true`.
- `stdout` is reserved strictly for newline-delimited JSON-RPC messages; warnings and logs go to
  `stderr`.

Example MCP client configuration (Claude Desktop, Cursor, or other `mcpServers` clients) using an
environment variable:

```json
{
  "mcpServers": {
    "keeppeek": {
      "command": "keeppeek",
      "args": ["mcp", "--server", "http://127.0.0.1:8081", "--read-only"],
      "env": {
        "KEEPPEEK_ACCESS_KEY": "replace-with-dedicated-access-key"
      }
    }
  }
}
```

Example using an owner-only credential file (`chmod 600` on Unix) so the access key is not stored
inside the MCP client's JSON file:

```json
{
  "mcpServers": {
    "keeppeek": {
      "command": "keeppeek",
      "args": [
        "mcp",
        "--server",
        "https://keeppeek.example.net",
        "--credential-file",
        "/path/to/keeppeek-mcp.key",
        "--read-only"
      ]
    }
  }
}
```

Remove `"--read-only"` only when you intentionally allow the assistant to apply approved changes
against a server configured with `[mcp].allow_writes = true`.

### Remote Streamable HTTP (`POST /mcp`)

Clients that support MCP Streamable HTTP directly can send JSON-RPC requests to `POST /mcp` once
`[mcp].enabled = true` is configured and KeepPeek has restarted:

```http
POST /mcp HTTP/1.1
Host: keeppeek.example.net
Authorization: Bearer replace-with-dedicated-access-key
Content-Type: application/json
X-KeepPeek-MCP-Read-Only: true
```

Example remote MCP client configuration:

```json
{
  "mcpServers": {
    "keeppeek": {
      "url": "https://keeppeek.example.net/mcp",
      "headers": {
        "Authorization": "Bearer replace-with-dedicated-access-key",
        "X-KeepPeek-MCP-Read-Only": "true"
      }
    }
  }
}
```

Omit `X-KeepPeek-MCP-Read-Only` when the client needs to call approved write tools and the server
enables `[mcp].allow_writes = true`.

When exposing `POST /mcp` beyond loopback, keep `[access].require_secure_remote = true` and
terminate HTTPS at a trusted reverse proxy listed in `[access].trusted_proxies`, or reach KeepPeek
over a private VPN. Direct plain HTTP from a remote-classified address is rejected with HTTP `426`.

## Available MCP tools

### Read-only and dry-run tools

These tools do not modify persisted configuration or runtime settings. `plan_configuration_change`
always runs as a dry-run preview and remains available even when `allow_writes = false`.

| Tool                        | Minimum role  | Purpose                                                                                             |
| --------------------------- | ------------- | --------------------------------------------------------------------------------------------------- |
| `get_server_health`         | User          | Recorder status, stream health, and disk pressure (filtered to permitted cameras for Users).        |
| `search_events`             | User          | Search stored events across permitted cameras (bounded `limit`, default 25, max 100).               |
| `get_recording_coverage`    | User          | Inspect recorded intervals and coverage gaps for permitted cameras over a bounded time window.      |
| `search_logs`               | Administrator | Query redacted server logs filtered by level, target, camera, time, and limit.                      |
| `get_logging_settings`      | Administrator | Inspect active logging destination and diagnostic filter settings.                                  |
| `get_configuration`         | Administrator | Read current configuration with `{secret:KEY}` references preserved and raw secrets redacted.       |
| `list_secret_keys`          | Administrator | Return sorted key names from `secrets.toml` without ever returning secret values.                   |
| `plan_configuration_change` | Administrator | Validate a proposed configuration change and return `plan_id`, revision, redacted diff, and impact. |
| `get_storage_status`        | Administrator | Inspect storage configuration, disk pressure, named volumes, archive status, and maintenance state. |
| `get_notification_status`   | Administrator | Summarize notification rules, cloud push status, and MQTT delivery health without raw secrets.      |
| `discover_cameras`          | Administrator | Run a bounded camera discovery and connection probe.                                                |
| `get_runtime_configuration` | Administrator | Read runtime storage, retention, and listener settings and revision.                                |
| `list_runtime_profiles`     | Administrator | List saved runtime profiles and active selection.                                                   |

### Opt-in write tools

Write tools require all three conditions:

1. `[mcp].allow_writes = true` in `config.toml`.
2. An **Administrator** credential (or trusted-local Administrator access).
3. No `--read-only` flag and no `X-KeepPeek-MCP-Read-Only: true` header.

| Tool                            | Minimum role  | Purpose                                                                                                                                          |
| ------------------------------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `apply_configuration_plan`      | Administrator | Apply a previously reviewed `plan_configuration_change` plan with revision verification.                                                         |
| `manage_camera`                 | Administrator | Add, update, or remove a camera by IP address using `{secret:KEY}` / `{secret:KEY                                                                | url}` references only. |
| `update_runtime_configuration`  | Administrator | Update runtime storage, retention, or listener settings (always requires `expected_configuration_revision`).                                     |
| `manage_configuration_template` | Administrator | Save, duplicate, or delete reusable camera templates using `{secret:KEY}` references.                                                            |
| `manage_notification_rule`      | Administrator | List (read-only), save draft, activate, or delete notification rules (`push`/`webhook`/`forwarder` require `{secret:KEY}` or `destination_ref`). |
| `set_runtime_profile`           | Administrator | Activate or update runtime profile selection.                                                                                                    |
| `set_logging_filter`            | Administrator | Update the live diagnostic log filter atomically.                                                                                                |

### Opt-in snapshot tool

| Tool                  | Minimum role | Purpose                                                                                                      |
| --------------------- | ------------ | ------------------------------------------------------------------------------------------------------------ |
| `get_camera_snapshot` | User         | Capture one privacy-gated, downscaled JPEG frame for a permitted camera when `[mcp].allow_snapshots = true`. |

## Safe configuration workflow

KeepPeek enforces plan-before-apply so an assistant never modifies cameras or storage blindly:

1. Inspect the current configuration (`get_configuration`) and available secret key names
   (`list_secret_keys`).
2. Add any new camera or integration passwords directly to `secrets.toml` on the server (or via
   `KEEPPEEK_SECRET_<KEY>` in the server environment) and reference them as `{secret:KEY}` or
   `{secret:KEY|url}`. Never paste raw passwords into an LLM prompt.
3. Run `discover_cameras` (read-only LAN probe) if adding new devices, then call
   `plan_configuration_change` (or preview a `manage_camera` / `manage_notification_rule` payload).
4. Review the returned redacted diff, validation errors, revision, and live-apply versus restart
   summary.
5. Approve the plan explicitly, then allow the assistant to call `apply_configuration_plan` (or the
   approved `manage_camera` / `manage_notification_rule` write action).
6. Check `get_server_health` and `search_logs` to confirm activation, or restart KeepPeek when the
   plan reports that a restart is required.

See [Visual configuration management](./configuration-management.md) for revision conflicts,
inheritance, and template rules.

## Security, privacy, and resource boundaries

### Authentication, RBAC, and audit

`POST /mcp` reuses KeepPeek's standard `[access]` authentication and authorization pipeline:

- Remote requests require an active Bearer credential in the `Authorization` header. Query-string
  tokens are rejected, and failed authentication rate limits apply.
- **User** credentials can call only `get_server_health`, `search_events`,
  `get_recording_coverage`, and (when enabled) `get_camera_snapshot`, strictly scoped to their
  granted cameras.
- **Administrator** credentials are required for logs, configuration, secret key listing, discovery,
  storage, notifications, profiles, and write tools.
- Privileged reads, configuration mutations, and snapshot requests record bounded entries in
  KeepPeek's security audit without logging raw credentials or image bytes.

Remember that KeepPeek's `[access].local_networks` policy grants Administrator access to trusted
local addresses. Narrow `local_networks` if a local MCP client must be restricted to a **User**
credential.

### Secret redaction

Every MCP response, configuration snapshot, dry-run diff, log search result, and error passes
through serialization-layer secret redaction before leaving KeepPeek:

- Raw values loaded from `secrets.toml` (including URL percent-encoded variants) are scrubbed.
- Fields with credential-like names (`password`, `secret`, `token`, `access_key`, `verifier`,
  `cookie`, `authorization`, `api_key`, `private_key`) are redacted unless they are already
  `{secret:KEY}` or `{secret:KEY|url}` references.
- Embedded URL credentials (`scheme://user:pass@host`) are scrubbed while `{secret:KEY}` and
  `{secret:KEY|url}` tokens remain visible for safe editing.

### DNS rebinding and cross-origin protection

`POST /mcp` validates the `Host` header and enforces an explicit browser origin allowlist:

- Non-browser MCP clients that omit `Origin` are permitted.
- Unauthenticated local requests (when `[access].local_networks` grants local trust) require `Host`
  to be `localhost` or an IP literal (`127.0.0.1`, `[::1]`) so DNS-rebound domain names are
  rejected with `403`.
- Requests carrying a browser `Origin` header are rejected with `403` unless the exact origin is
  listed in `[direct_card].allowed_origins`, and require `Authorization: Bearer <key>` even on local
  networks (returning `401` if unauthenticated).
- Unsupported HTTP methods on `/mcp` return `405 Method Not Allowed`, and a disabled MCP server
  returns `404 Not Found`.

### Fail-closed privacy and snapshot limits

`get_camera_snapshot` enforces four sequential gates before returning any image bytes:

1. `[mcp].allow_snapshots` must be `true`.
2. The authenticated principal must have view permission for the requested camera.
3. The per-camera rate limit (`snapshot_min_interval_seconds`) must not be exceeded.
4. Camera privacy state (`[privacy]` schedules and runtime profile overrides) must be verified as
   inactive. If privacy mode is active or cannot be verified, KeepPeek fails closed and refuses the
   snapshot.

Decoded frames use bounded image decoder limits, downscale to at most `max_snapshot_dimension`
pixels per side while preserving aspect ratio, and encode JPEG within `max_snapshot_bytes`.
KeepPeek does not apply server-side pixel masks; only camera-side (hardware) privacy masks already
rendered into the camera stream appear in the returned frame.

### Concurrency and payload caps

KeepPeek caps simultaneous MCP executions at `max_concurrent_requests`, enforces
`request_timeout_seconds`, and rejects oversized JSON-RPC responses above `max_response_bytes`
(`get_camera_snapshot` uses `max_snapshot_bytes` plus base64 and metadata envelope overhead) so an
automated assistant cannot starve camera ingest, recording, or live playback.

## Troubleshooting

| Symptom                                     | Check                                                                                                                         |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `POST /mcp` returns `404`                   | Confirm `[mcp].enabled = true` in `config.toml` and restart KeepPeek.                                                         |
| `POST /mcp` returns `403`                   | Check whether `Origin` is missing from `[direct_card].allowed_origins`, `Host` is non-local for local trust, or role is User. |
| `POST /mcp` returns `426`                   | Remote plain HTTP is blocked by `[access].require_secure_remote = true`. Use HTTPS via a trusted proxy or a private VPN.      |
| Write tool is denied                        | Confirm `[mcp].allow_writes = true`, an Administrator credential, and that `--read-only` is not set.                          |
| `get_camera_snapshot` is denied             | Confirm `[mcp].allow_snapshots = true`, camera view permission, inactive privacy mode, and the per-camera interval limit.     |
| Request returns `429` or `503` or times out | Too many simultaneous MCP calls (`max_concurrent_requests`) or slow probe exceeding `request_timeout_seconds`.                |
| `apply_configuration_plan` fails conflict   | Configuration changed after the plan was created. Call `get_configuration` and generate a fresh plan.                         |
| `keeppeek mcp` fails to connect remotely    | Confirm `KEEPPEEK_ACCESS_KEY` or `--credential-file` is set and readable, and verify `--server` points to the HTTPS origin.   |
