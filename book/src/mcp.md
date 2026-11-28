# MCP server and LLM configuration

KeepPeek includes built-in Model Context Protocol (MCP) support so AI assistants and MCP clients
(such as Claude, Codex, Cursor, and Gemini) can inspect recorder health, logs, events, storage,
and recording coverage, onboard and tune cameras without exposing plaintext credentials, and
preview or apply validated configuration changes.

MCP is disabled by default for network access. You can connect locally through the `keeppeek mcp`
`stdio` bridge or enable the embedded Streamable HTTP endpoint (`POST /mcp`) in `config.toml`.

```mermaid
flowchart LR
    A[Local AI client] -->|stdio JSON-RPC| B[keeppeek mcp bridge]
    B -->|Bearer auth over HTTP or HTTPS| K[KeepPeek POST /mcp]
    R[Remote MCP client] -->|HTTPS Bearer auth| P[Trusted reverse proxy or VPN]
    P --> K
    U[Local operator browser] -->|Loopback single-use token| S[GET/POST /mcp/secret-entry]
    S --> K
```

## Use the bundled agent skills

KeepPeek ships two canonical agent skills:

- `skills/keeppeek-mcp/SKILL.md` teaches LLM assistants how to locate `config.toml` and `secrets.toml`, collect missing secrets out-of-band via `request_secret_entry` or `keeppeek secret`, probe cameras with `{secret:KEY}` references, run `plan_configuration_change` or dry-run hardware previews before requesting writes, and respect privacy and role boundaries.
- `skills/keeppeek-camera-debug/SKILL.md` teaches assistants how to opt in safely to `[mcp].allow_camera_debugging`, stream live remote SSE traces via `keeppeek mcp logs`, capture decrypted PCAPNG, raw elementary bitstreams, and correlated `traces.jsonl` via `camera_debugging`, pull captures remotely (`keeppeek mcp pull-capture` or chunked `read_artifact`), triage with `tshark`/`ffprobe`/`ffmpeg`, export sanitized offline fixtures for `ReplayCameraServer` and `keeppeek-camera replay`, and hot-swap live dev builds with `keeppeek-supervisor update --channel local`.

List, print, or export bundled skills from any installed KeepPeek binary:

```sh
keeppeek mcp skill --list
keeppeek mcp skill
keeppeek mcp skill --output ./skills/keeppeek-mcp
keeppeek mcp skill camera-debug
keeppeek mcp skill camera-debug --output ./skills/keeppeek-camera-debug
```

Release artifacts and container images also include both `SKILL.md` files directly:

| Package                       | Shipped skill paths                                                                                                                     |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Linux `.tar.gz` archive       | `keeppeek-<version>-<platform>/skills/keeppeek-mcp/SKILL.md`, `keeppeek-<version>-<platform>/skills/keeppeek-camera-debug/SKILL.md`     |
| Windows `.zip` archive        | `keeppeek-<version>-<platform>\skills\keeppeek-mcp\SKILL.md`, `keeppeek-<version>-<platform>\skills\keeppeek-camera-debug\SKILL.md`     |
| macOS `.dmg` (`KeepPeek.app`) | `KeepPeek.app/Contents/Resources/skills/keeppeek-mcp/SKILL.md`, `KeepPeek.app/Contents/Resources/skills/keeppeek-camera-debug/SKILL.md` |
| Container image               | `/usr/local/share/keeppeek/skills/keeppeek-mcp/SKILL.md`, `/usr/local/share/keeppeek/skills/keeppeek-camera-debug/SKILL.md`             |

To guide an assistant (Claude, Codex, Cursor, or Gemini), copy the relevant skill folder into the assistant's skills directory or point the assistant at the exported `SKILL.md` file.

## Enable and configure `[mcp]`

All MCP server settings live in the existing `config.toml` under `[mcp]`. Every capability that opens network access, mutates configuration, writes on-camera hardware settings, captures camera frames, or captures raw camera traffic defaults to `false`.

A **human Administrator** can enable or disable MCP access live without restarting KeepPeek from
**Settings > Integrations > MCP server** (trusted local network access or an interactive
OIDC/identity-proxy browser session; explicit confirmation is required to enable — remote browser
sessions authenticated with a Bearer access key cannot enable MCP or silence warnings). To change
policy limits (`allow_writes`, `allow_snapshots`, `allow_camera_device_writes`, or payload/rate
caps), stop KeepPeek before editing `config.toml` manually, add `[mcp]`, and restart the server:

```toml
[mcp]
enabled = true
allow_writes = false
allow_snapshots = false
allow_camera_device_writes = false
allow_camera_debugging = false
debug_camera_ids = []
max_debug_capture_bytes = 67108864
max_debug_storage_bytes = 536870912
max_concurrent_requests = 4
max_response_bytes = 65536
request_timeout_seconds = 15
max_snapshot_bytes = 262144
max_snapshot_dimension = 1280
snapshot_min_interval_seconds = 5
```

> **Security warning:** Setting `[mcp].allow_camera_debugging = true` grants Administrator MCP clients and AI assistants **full raw access to live video bitstreams, audio streams, decrypted network captures (PCAPNG), and camera credentials** for `debug_camera_ids` (or all configured cameras when empty). KeepPeek emits a prominent `tracing::warn!` on startup and adds a `warning` issue to `ServerHealthResponse` whenever both `enabled` and `allow_camera_debugging` are `true`. Enable it only during active protocol debugging and set `allow_camera_debugging = false` afterward.

| Field                           | Type           | Default     | Allowed range           | Meaning                                                                                                                                  |
| ------------------------------- | -------------- | ----------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`                       | `bool`         | `false`     | `true`/`false`          | Enable the embedded Streamable HTTP endpoint at `POST /mcp` (and loopback `/mcp/secret-entry`). Live via Web UI or `disable_mcp_access`. |
| `enabled_at_unix_secs`          | Optional `u64` | Absent      | Unix epoch secs         | Server-managed timestamp recorded when `enabled` becomes `true`; used for the 7-day enabled warning and cleared when disabled.           |
| `long_enabled_warning_silenced` | `bool`         | `false`     | `true`/`false`          | Human Administrator acknowledgement silencing the 7-day enabled warning for the current enabled stretch; cleared when disabled.          |
| `allow_writes`                  | `bool`         | `false`     | `true`/`false`          | Permit Administrator KeepPeek configuration write tools after explicit approval.                                                         |
| `allow_snapshots`               | `bool`         | `false`     | `true`/`false`          | Permit privacy-gated single-frame snapshots via `get_camera_snapshot`.                                                                   |
| `allow_camera_device_writes`    | `bool`         | `false`     | `true`/`false`          | Permit live on-camera hardware writes via `update_camera_device_settings` (`dry_run: false`).                                            |
| `allow_camera_debugging`        | `bool`         | `false`     | `true`/`false`          | Permit Administrator live camera packet/bitstream capture and repro-fixture export via `camera_debugging`.                               |
| `debug_camera_ids`              | `Vec<String>`  | `[]`        | `<= 256` IDs            | Optional camera ID allowlist for `camera_debugging` (empty allows all cameras; each trimmed non-empty ASCII ID `<= 128` bytes).          |
| `max_debug_capture_bytes`       | `usize`        | `67108864`  | `1048576..=268435456`   | Maximum capture artifact size in bytes (1 MiB..=256 MiB; default 64 MiB).                                                                |
| `max_debug_storage_bytes`       | `u64`          | `536870912` | `16777216..=4294967296` | Total disk budget under `<config-dir>/camera-debug/` in bytes (16 MiB..=4 GiB; default 512 MiB, 24h TTL, FIFO eviction).                 |
| `max_concurrent_requests`       | `usize`        | `4`         | `1..=32`                | Maximum simultaneous in-flight MCP requests.                                                                                             |
| `max_response_bytes`            | `usize`        | `65536`     | `4096..=262144`         | Maximum serialized JSON-RPC response size in bytes (`get_camera_snapshot` uses `max_snapshot_bytes`).                                    |
| `request_timeout_seconds`       | `u64`          | `15`        | `1..=60`                | Server-side deadline per MCP request, in seconds.                                                                                        |
| `max_snapshot_bytes`            | `usize`        | `262144`    | `16384..=1048576`       | Maximum encoded JPEG size for `get_camera_snapshot`, in bytes (base64 + metadata overhead is added).                                     |
| `max_snapshot_dimension`        | `u32`          | `1280`      | `160..=1920`            | Maximum width or height of returned snapshot frames, in pixels.                                                                          |
| `snapshot_min_interval_seconds` | `u64`          | `5`         | `1..=300`               | Minimum interval between snapshot captures per camera, in seconds.                                                                       |

### Human-only enable, agent self-disable, and 7-day warning

MCP access is designed as a temporary maintenance surface rather than an unmonitored permanent open
door:

- **Agent self-disable (`disable_mcp_access`):** An authenticated Administrator agent can turn off
  MCP access itself when finished by calling `disable_mcp_access` with `{ "confirm": true }` (and an
  optional `reason`). This works even when `[mcp].allow_writes = false` (though it is blocked under
  `--read-only` / `X-KeepPeek-MCP-Read-Only: true`), persists `[mcp].enabled = false` in
  `config.toml` (and fails safe by still turning MCP off live if `config.toml` persistence fails),
  drops any pending `/mcp/secret-entry` handoffs, and immediately switches new `/mcp` and
  `/mcp/secret-entry` requests to `404` without a restart (already in-flight `/mcp` calls finish
  within `request_timeout_seconds`).
- **Human-only enable and warning silence:** No MCP tool can enable MCP or silence the 7-day
  warning. Enabling MCP or silencing the warning over the WebRTC control plane
  (`keeppeek.mcp-access.v1`) requires a **human Administrator** (trusted local network or
  interactive OIDC/identity-proxy browser session) and rejects Bearer access-key credentials with
  `403` (including remote browser sessions signed in with an access key). Bearer-authenticated
  configuration restores (`POST /config/apply`) are also rejected with `403` if they attempt to
  enable `[mcp]`, silence the warning, advance or omit `enabled_at_unix_secs`, escalate `[mcp]`
  write/snapshot flags, or alter `[access]`, `[external_auth]`, or `[external_identities]` trust
  policy.
- **7-day enabled warning:** When `[mcp].enabled` stays `true` for 7 days or longer (`>= 604,800`
  seconds) without being silenced, KeepPeek logs a startup warning, adds an Administrator-only
  warning to server health (`scope: "mcp"`), and displays an Administrator warning banner in the Web
  UI with direct **Disable MCP** and **Silence warning** actions. Silencing persists
  `long_enabled_warning_silenced = true` for the current enabled stretch; disabling MCP and later
  re-enabling it resets the timer and clears the silence flag.

See [Configuration reference](./configuration-reference.md#mcp-server) for validation rules and
file ownership details.

## Connect an MCP client

Create a dedicated named credential in **Settings > Access & roles** for each MCP client so its
activity appears separately in audit records and can be rotated or revoked independently. Use the
**User** role for camera-scoped read-only monitoring, or **Administrator** for full diagnostics,
camera onboarding, hardware tuning, and configuration management. See
[Authentication and access control](./authentication.md).

### Local `stdio` bridge and remote diagnostic CLI

Most desktop AI assistants launch MCP servers over `stdio`. KeepPeek provides a built-in `stdio`
JSON-RPC bridge plus remote log-streaming and capture-download utilities over `POST /mcp`,
`GET /logs`, and `GET /logs/snapshot` (with `[mcp].enabled = true`):

```sh
keeppeek mcp [--server <URL>] [--config <PATH>] [--credential-file <PATH>] [--read-only]
keeppeek mcp [--server <URL>] [--credential-file <PATH>] logs [--level <LEVEL>] [--target <SUBSTR>] [--camera <ID>] [--query <SUBSTR>] [--filter <DIRECTIVE>] [--follow] [--limit <N>]
keeppeek mcp [--server <URL>] [--credential-file <PATH>] pull-capture <CAPTURE_ID> --output <DIR> [--artifact <NAME>...]
```

- `--server`, `--config`, `--credential-file`, and `--read-only` are top-level `keeppeek mcp` options and must appear **before** `logs`, `pull-capture`, or `skill`.
- `keeppeek mcp logs` fetches `GET /logs/snapshot` (default, up to `--limit`, default `100`) or streams live `GET /logs` SSE events (`--follow`, tail `1..=1000`), filtering by `--level` (`trace | debug | info | warn | error`), `--target`, `--camera`, and `--query`, and printing compact JSONL `LogEntry` lines to `stdout`. Passing `--filter <DIRECTIVE>` temporarily applies an `EnvFilter` via `get_logging_settings` + `set_logging_filter` (requires Administrator + `[mcp].allow_writes = true`, without `--read-only`) and restores the previous filter automatically on exit or `Ctrl-C`.
- `keeppeek mcp pull-capture` downloads every artifact (or specific `--artifact <NAME>` files) from `camera_debugging` (`list_captures` + chunked `read_artifact`), writes files atomically inside `--output <DIR>`, verifies SHA-256 digests, and prints `<artifact>\t<bytes>\t<sha256>`.

- `--server <URL>` targets a specific KeepPeek server origin (for example `http://127.0.0.1:8081`
  or `https://keeppeek.example.net`).
- `--config <PATH>` selects an alternate `config.toml` when resolving local server settings.
- `--credential-file <PATH>` reads the Bearer access key from an owner-only file instead of an
  environment variable.
- `--read-only` sends `X-KeepPeek-MCP-Read-Only: true`, blocking all configuration and on-camera
  hardware write tools even if the server enables `[mcp].allow_writes` or
  `[mcp].allow_camera_device_writes`.
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
against a server configured with `[mcp].allow_writes = true` or
`[mcp].allow_camera_device_writes = true`.

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
enables `[mcp].allow_writes = true` or `[mcp].allow_camera_device_writes = true`.

When exposing `POST /mcp` beyond loopback, keep `[access].require_secure_remote = true` and
terminate HTTPS at a trusted reverse proxy listed in `[access].trusted_proxies`, or reach KeepPeek
over a private VPN. Direct plain HTTP from a remote-classified address is rejected with HTTP `426`.

### Fast local development and agentic MCP loop

Because `POST /mcp` is stateless, AI agents testing local KeepPeek changes can stay connected
across live supervised binary swaps without restarting the agent or editing MCP client config:

1. Start the supervised host once with local channel opt-in and a short probation window:

   ```sh
   # Standalone CLI supervisor
   keeppeek-supervisor run --root <dev-supervisor-root> --config <path/to/config.toml> --allow-local --probation-ms 500
   # Or Windows service / KeepPeek Desktop dev hosts:
   keeppeek-service.exe --allow-local
   keeppeek-desktop --manage-local-server --allow-local
   ```

2. Point the MCP client directly at Streamable HTTP (`http://127.0.0.1:8081/mcp`). On Windows,
   prefer Streamable HTTP (or a copied `keeppeek.exe` outside `target/`) over running
   `target/debug/keeppeek.exe mcp` directly so Cargo can relink `target/debug/keeppeek.exe`
   without a file lock.
3. After editing Rust code, rebuild and hot-swap the running child in one command (use `target/debug/keeppeek.exe` on Windows):

   ```sh
   cargo build --bin keeppeek
   keeppeek-supervisor update --root <dev-supervisor-root> --channel local --binary target/debug/keeppeek --apply --wait --json
   ```

4. Confirm the new child PID/executable via `get_server_health` (`system.process.pid` and
   `system.process.executable`) or `keeppeek-supervisor status --root <dev-supervisor-root> --json`.
   If the new binary crashes or fails probation, the supervisor automatically rolls back to the
   previous working slot and reports `rolled_back: true`.

## Manage secrets without plaintext chat (`keeppeek secret` and loopback UI handoff)

KeepPeek never accepts raw camera passwords or webhook secrets through MCP tool arguments, and
assistants should never ask you to paste passwords into chat. Store secrets in `secrets.toml`
out-of-band using either the local CLI or the loopback secret-entry handoff, then reference them as
`{secret:KEY}` or `{secret:KEY|url}`.

### `keeppeek secret` CLI

Manage `secrets.toml` entries directly from a local shell without ever placing secret values in
command-line arguments:

```sh
keeppeek secret set <KEY> [--config <PATH>] [--server <URL>] [--stdin]
keeppeek secret list [--config <PATH>]
keeppeek secret remove <KEY> [--config <PATH>]
```

- Keys must match `^[A-Z_][A-Z0-9_]*$` (1–128 bytes); values must be non-empty, at most 4 KiB, and
  contain no NUL bytes.
- `keeppeek secret set` reads values only from a no-echo interactive terminal prompt or `--stdin`
  (never from positional arguments or flags).
- `keeppeek secret list` prints sorted key names only and never prints values.
- Writes use owner-only file permissions (`0600` on Unix / owner-only DACL on Windows), atomic
  replacement under the shared configuration lock, and zeroized temporary buffers.
- A running KeepPeek server automatically detects `secrets.toml` updates so newly added keys work
  immediately in `probe_camera_streams` and `manage_camera` without restarting.

### Loopback-only secret-entry UI handoff (`GET` / `POST /mcp/secret-entry`)

When an assistant discovers that a secret key is missing from `list_secret_keys`, it can call
`request_secret_entry` to create a short-lived, single-use handoff (`60..=300` seconds, default 180,
capped at 16 pending handoffs). KeepPeek returns a loopback URL:

```text
http://127.0.0.1:<port>/mcp/secret-entry?token=<256-bit-single-use-token>
```

- Open the link locally on the recorder host, enter the values in the self-contained HTML form, and
  submit. KeepPeek writes `secrets.toml` atomically, reloads secrets live without a restart,
  invalidates the token immediately, emits a value-free audit event, and never echoes secret values.
- The assistant polls `get_secret_entry_status`, which returns only `status` (`"pending"`,
  `"completed"`, `"expired"`, or `"failed"`), `configured_keys`, and `expires_at_ms`.
- `/mcp/secret-entry` is strictly **loopback-only**: non-loopback connections, proxy-forwarded
  requests, non-loopback `Host` headers, and cross-origin `POST` requests are rejected with `403`.
  When `[mcp].enabled = false`, `/mcp/secret-entry` returns `404`.
- Form responses enforce `Cache-Control: no-store`, `Referrer-Policy: same-origin`,
  `X-Frame-Options: DENY`, and a strict Content Security Policy.

## Available MCP tools (29 total)

### Read-only, dry-run, onboarding, and secret-handoff tools (18)

These tools do not modify persisted KeepPeek configuration or on-camera hardware settings.
`plan_configuration_change` always runs as a dry-run preview and remains available even when
`allow_writes = false`.

| Tool                         | Minimum role  | Purpose                                                                                                            |
| ---------------------------- | ------------- | ------------------------------------------------------------------------------------------------------------------ |
| `get_server_health`          | User          | Recorder status, stream health, and disk pressure (filtered to permitted cameras for Users).                       |
| `search_events`              | User          | Search stored events across permitted cameras (bounded `limit`, default 25, max 100).                              |
| `get_recording_coverage`     | User          | Inspect recorded intervals and coverage gaps for permitted cameras over a bounded time window.                     |
| `search_logs`                | Administrator | Query redacted server logs filtered by level, target, camera, time, and limit.                                     |
| `get_logging_settings`       | Administrator | Inspect active logging destination and diagnostic filter settings.                                                 |
| `get_configuration`          | Administrator | Read current configuration with `{secret:KEY}` references preserved and raw secrets redacted.                      |
| `list_secret_keys`           | Administrator | Return sorted key names from `secrets.toml` without ever returning secret values.                                  |
| `plan_configuration_change`  | Administrator | Validate a proposed configuration change and return `plan_id`, revision, redacted diff, and impact.                |
| `get_storage_status`         | Administrator | Inspect storage configuration, disk pressure, named volumes, archive status, and maintenance state.                |
| `get_notification_status`    | Administrator | Summarize notification rules, cloud push status, and MQTT delivery health without raw secrets.                     |
| `discover_cameras`           | Administrator | Run a bounded LAN camera discovery probe.                                                                          |
| `search_camera_catalog`      | Administrator | Search KeepPeek's embedded camera catalog (`limit` `1..=25`) and attach credential-free RTSP `stream_hints` by IP. |
| `probe_camera_streams`       | Administrator | Probe ONVIF/RTSP streams using single `{secret:KEY}` refs or `credential_candidates` (max 5); returns refs only.   |
| `request_secret_entry`       | Administrator | Create a single-use loopback `/mcp/secret-entry` handoff URL and CLI instructions for missing secret keys.         |
| `get_secret_entry_status`    | Administrator | Check whether a secret-entry handoff is `pending`, `completed`, `expired`, or `failed` (never returns values).     |
| `get_camera_device_settings` | Administrator | Read live on-camera hardware settings and supported/unsupported sections for one configured camera.                |
| `get_runtime_configuration`  | Administrator | Read runtime storage, retention, and listener settings and revision.                                               |
| `list_runtime_profiles`      | Administrator | List saved runtime profiles and active selection.                                                                  |

### Opt-in KeepPeek configuration write tools (7)

Configuration write tools require all three conditions:

1. `[mcp].allow_writes = true` in `config.toml`.
2. An **Administrator** credential (or trusted-local Administrator access).
3. No `--read-only` flag and no `X-KeepPeek-MCP-Read-Only: true` header.

| Tool                            | Minimum role  | Purpose                                                                                                                                          |
| ------------------------------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `apply_configuration_plan`      | Administrator | Apply a previously reviewed `plan_configuration_change` plan with revision verification.                                                         |
| `manage_camera`                 | Administrator | Add, update, or remove a camera by IP address using `{secret:KEY}` or `{secret:KEY\|url}` references only.                                       |
| `update_runtime_configuration`  | Administrator | Update runtime storage, retention, or listener settings (always requires `expected_configuration_revision`).                                     |
| `manage_configuration_template` | Administrator | Save, duplicate, or delete reusable camera templates using `{secret:KEY}` references.                                                            |
| `manage_notification_rule`      | Administrator | List (read-only), save draft, activate, or delete notification rules (`push`/`webhook`/`forwarder` require `{secret:KEY}` or `destination_ref`). |
| `set_runtime_profile`           | Administrator | Activate or update runtime profile selection.                                                                                                    |
| `set_logging_filter`            | Administrator | Update the live diagnostic log filter atomically.                                                                                                |

### Opt-in on-camera hardware write tool (1)

`update_camera_device_settings` defaults to `dry_run: true`, which validates camera bounds and
returns a redacted before/after diff even when `[mcp].allow_camera_device_writes = false`. Live
application (`dry_run: false`) requires all three conditions:

1. `[mcp].allow_camera_device_writes = true` in `config.toml`.
2. An **Administrator** credential (or trusted-local Administrator access).
3. No `--read-only` flag and no `X-KeepPeek-MCP-Read-Only: true` header.

| Tool                            | Minimum role  | Purpose                                                                                                                |
| ------------------------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `update_camera_device_settings` | Administrator | Dry-run preview (`dry_run: true`) or apply (`dry_run: false`) on-camera hardware settings with read-back verification. |

### Opt-in snapshot tool (1)

| Tool                  | Minimum role | Purpose                                                                                                      |
| --------------------- | ------------ | ------------------------------------------------------------------------------------------------------------ |
| `get_camera_snapshot` | User         | Capture one privacy-gated, downscaled JPEG frame for a permitted camera when `[mcp].allow_snapshots = true`. |

### Opt-in camera debugging tool (1)

`camera_debugging` is **Administrator-only** and fails closed unless `[mcp].allow_camera_debugging = true`. When `[mcp].debug_camera_ids` is non-empty, requests for cameras outside the allowlist are rejected. Mutating/capture actions (`capture`, `compare_transports`, `delete_capture`) reject `--read-only` sessions (`X-KeepPeek-MCP-Read-Only: true`).

| Tool               | Minimum role  | Purpose                                                                                                                                                  |
| ------------------ | ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `camera_debugging` | Administrator | Inspect camera ingest pipelines, capture decrypted PCAPNG and raw bitstreams, compare transports, export sanitized repro fixtures, or clean up captures. |

Supported `camera_debugging` actions:

- `inspect_pipeline`: Inspect active camera backend, stream state, health, and profile configuration from live `ServerState`.
- `capture`: Run a bounded capture (`duration_seconds` `1..=60`, `max_bytes` `65536..=268435456`, `stream`: `"main" | "sub" | "both"`) collecting requested `artifacts` (`"pcapng"`, `"video_bitstream"`, `"audio_bitstream"`, `"muxed_mp4"`, `"protocol_transcript"`, `"traces"`; all six enabled by default when omitted) across `protocols` (`"rtsp"`, `"audio"`, `"onvif"`, `"isapi"`, `"reolink_http"`, `"reo_proto"`). Optional `trace_filter` (`1..=512` bytes, `EnvFilter` directive) temporarily boosts tracing under an RAII guard when `"traces"` is enabled (defaulting to full protocol `TRACE` targets) and restores the previous filter on completion.
- `compare_transports`: Compare `mode: "media_transports"` or `mode: "event_channels"` side-by-side for a camera (also accepts `trace_filter` when `"traces"` is enabled).
- `read_artifact`: Read any saved capture artifact in bounded chunks (`capture_id`, single-segment `artifact` filename `<= 128` bytes, `offset` default `0`, `length` `1..=131072` default `65536`, `encoding`: `"auto" | "base64" | "utf8"`) returning `total_bytes`, full-file `sha256`, `eof`, `next_offset`, `encoding`, and `data`. Works in `--read-only` mode.
- `export_repro_fixture`: Export a `keeppeek.camera-repro.v1` JSON fixture (`strip_credentials` default `true`, `anonymize_network` default `true`, `trim_media`: `"headers_and_minimal_failing_slice" | "synthetic_payloads" | "keep_raw_frames"`) with a `sharing_safety_report`.
- `list_captures`: List saved captures under `<config-dir>/camera-debug/<capture-id>/` with byte sizes, filenames, and `created_at_unix_secs` timestamps (24-hour TTL entries are pruned on list).
- `delete_capture`: Delete one `capture_id` or all saved captures (`delete_all: true`).

### Self-disable governance tool (1)

`disable_mcp_access` is available to authenticated **Administrator** callers even when
`[mcp].allow_writes = false`, unless `--read-only` or `X-KeepPeek-MCP-Read-Only: true` is active.
Re-enabling MCP always requires a human Administrator.

| Tool                 | Minimum role  | Purpose                                                                                                                            |
| -------------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `disable_mcp_access` | Administrator | Requires `confirm: true` (optional `reason`). Persists `[mcp].enabled = false`, clears pending handoffs, and disables `/mcp` live. |

## Multi-candidate stream probing and on-camera hardware support

### Pre-populated `secrets.toml` multi-candidate probing

If a site uses a few known camera credential pairs already stored in `secrets.toml`, an assistant
can pass up to **5** `{secret:KEY}` pairs in `probe_camera_streams.credential_candidates`:

- Raw credential fields and inline RTSP userinfo or query-string credentials are rejected.
- Targets are SSRF-bounded: unspecified, multicast, broadcast, link-local (`169.254.0.0/16`),
  public IPs, unconfigured loopback addresses, and mismatched RTSP URL hosts are blocked.
- Probing stops immediately on the first matching credential pair or if the camera reports lockout,
  HTTP `429`, or becomes unreachable.
- Responses return only `matched_credentials` (`{secret:KEY}` references), `failed_candidates`
  indexes, `recommended_backend`, verified RTSP URLs, and redacted diagnostics.

### Honest on-camera hardware capability matrix

`get_camera_device_settings` and `update_camera_device_settings` normalize hardware controls into
seven sections (`encoder`, `imaging`, `lighting`, `motion`, `osd`, `time`, `ptz`) and report
unsupported sections honestly per protocol:

| Camera protocol | Supported sections and capabilities                                                                                                                                                                                                         |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Reolink         | `encoder` (read), `imaging` (brightness, contrast, saturation, sharpness, day/night, HDR), `lighting` (IR mode, white LED), `motion` (MD & AI sensitivity), `osd` (channel name, max 32 chars), `time` (clock/NTP), `ptz` (presets, guard). |
| Hikvision ISAPI | `encoder` (stream resolution, bitrate, framerate), `motion` (motion + smart rules: `line`, `field`, `region_entrance`, `region_exit`), `time` (`manual`/`ntp`), `ptz` (`store_preset` 1–300).                                               |
| ONVIF           | `encoder` (video encoder settings validated against camera `GetVideoEncoderConfigurationOptions`), `osd` (plain-text OSD up to 64 chars).                                                                                                   |
| RTSP-only       | None (`supported_sections: []`, all 7 sections listed in `unsupported_sections`; live writes return `501`).                                                                                                                                 |

**Hard security exclusions:** `update_camera_device_settings` permanently refuses firmware upload or
flashing, factory reset or default restore, camera reboot, password/user/credential modifications,
and IP/network/DHCP/gateway/DNS/port reassignment with HTTP `403`, even when `dry_run: true`.

## Deep camera protocol debugging and offline replay

When standard health and log tools are not enough to diagnose an RTSP, RTP, ONVIF, ISAPI, or Reolink Baichuan (`reo_proto`) interoperability defect, enable `[mcp].allow_camera_debugging = true` temporarily and export the `keeppeek-camera-debug` skill (`keeppeek mcp skill camera-debug`). Remote HTTPS servers and local servers use the exact same workflow:

1. **Stream live traces over SSE (optional):** Run `keeppeek mcp --server <URL> logs --follow --camera <id> --filter "info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug"` to observe real-time span-correlated events (`camera_id`, `stream`) and auto-restore the filter on exit.
2. **Capture on the running server:** Run `camera_debugging` (`inspect_pipeline` -> `capture` -> `compare_transports`). KeepPeek writes bounded artifacts under `<config-dir>/camera-debug/<capture-id>/` (`capture.pcapng` with Ethernet/IP/TCP/UDP framing around decrypted payloads, raw `.h264`/`.h265` and audio elementary streams, `rtp_packets.bin` when raw RTP dump is enabled, `muxed.mp4`, `sdp.txt`, `protocol_transcript.json`, capture-correlated `traces.jsonl` up to 1 MiB, and atomically written `capture.json`) with a 24-hour TTL and FIFO eviction at `max_debug_storage_bytes`.
3. **Pull artifacts and triage locally:** Download via `keeppeek mcp --server <URL> pull-capture <capture-id> --output ./debug-out` (with automatic SHA-256 verification; pure-MCP agents without CLI access can loop `camera_debugging` `action: "read_artifact"` chunks). Inspect `capture.pcapng` with `tshark` (`-Y "rtsp || rtp || http || xml"`, `-z follow,tcp,ascii,0`, `-T fields -e rtp.seq -e rtp.timestamp -e h264.nal_unit_type`), `ffprobe -show_packets -show_frames muxed.mp4`, `ffmpeg -v error -i video.h264 -f null -`, and `traces.jsonl`.

   | Filter tier | Directive                                                                                                                                                                                                                           | Scope                                                                                                        |
   | ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
   | `INFO`      | `info,keeppeek=info`                                                                                                                                                                                                                | Sparse connect/disconnect, negotiated stream summary, failover, warnings/errors.                             |
   | `DEBUG`     | `info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug`                                                                                                                                                                      | Resolves ~75% of issues (auth retries, capability/profile selection, SDP, SPS/PPS/VPS, RTP gaps, A/V drift). |
   | `TRACE`     | `retina=trace,reo_proto=trace,onvif=trace,keeppeek::rtsp=trace,keeppeek::reolink=trace,keeppeek::isapi=trace,keeppeek::cameras=trace,keeppeek::camera_events=trace,keeppeek::media_time=trace,keeppeek::storage::medium_term=trace` | Resolves 100% of protocol/depacketizer/muxer issues (RTSP/RTCP, FU-A/STAP-A, Baichuan headers, ISAPI, SOAP). |

4. **Export a sanitized fixture, reproduce offline, and hot-swap live:** Call `camera_debugging` (`action: "export_repro_fixture"`, `strip_credentials: true`, `anonymize_network: true`) and verify `sharing_safety_report.safe_to_share_publicly` is `true` (RFC 5737 `192.0.2.x` addresses and stripped credentials). Reproduce deterministically using `ReplayCameraServer` in `crates/test-camera/src/replay.rs` (`keeppeek.camera-repro.v1`) or interactively via `keeppeek-camera replay --fixture <PATH> [--bind 127.0.0.1:0] [--once | --loop]`. After offline `cargo nextest` passes, swap your build onto a `--allow-local` supervised host without dropping your MCP client using [Fast local development and agentic MCP loop](#fast-local-development-and-agentic-mcp-loop) (`keeppeek-supervisor update --channel local --binary ... --apply --wait --json`) and immediately re-verify on the real camera.
5. **Clean up and re-lock:** Delete server captures with `camera_debugging` (`action: "delete_capture"`), reset `set_logging_filter` if changed manually, stop KeepPeek, set `allow_camera_debugging = false` in `config.toml`, restart KeepPeek, and optionally call `disable_mcp_access` (`{"confirm": true}`) to disable `/mcp` live.

## Safe configuration workflow

KeepPeek enforces plan-before-apply so an assistant never modifies cameras, hardware settings, or
storage blindly:

1. Inspect the current configuration (`get_configuration`) and available secret key names
   (`list_secret_keys`).
2. Add missing camera or integration passwords out-of-band via `keeppeek secret set <KEY>` or the
   loopback `request_secret_entry` UI handoff (or `KEEPPEEK_SECRET_<KEY>` in the server
   environment), and reference them as `{secret:KEY}` or `{secret:KEY|url}`. Never paste raw
   passwords into an LLM prompt.
3. Run `discover_cameras` and `search_camera_catalog` for new devices, verify streams with
   `probe_camera_streams`, then call `plan_configuration_change` (or preview a `manage_camera` /
   `manage_notification_rule` payload).
4. Review the returned redacted diff, validation errors, revision, and live-apply versus restart
   summary.
5. Approve the plan explicitly, then allow the assistant to call `apply_configuration_plan` (or the
   approved `manage_camera` / `manage_notification_rule` write action).
6. If tuning on-camera settings, inspect with `get_camera_device_settings`, preview with
   `update_camera_device_settings` (`dry_run: true`), approve explicitly, and apply with
   `dry_run: false` (requires `[mcp].allow_camera_device_writes = true`).
7. Check `get_server_health` and `search_logs` to confirm activation, or restart KeepPeek when the
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
- **Administrator** credentials are required for logs, configuration, secret key listing, secret
  entry handoffs, catalog search, stream probing, discovery, storage, notifications, profiles,
  hardware settings, and write tools.
- Privileged reads, configuration mutations, secret-entry submissions (key names only), on-camera
  hardware updates, and snapshot requests record bounded entries in KeepPeek's security audit
  without logging raw credentials or image bytes.

Remember that KeepPeek's `[access].local_networks` policy grants Administrator access to trusted
local addresses. Narrow `local_networks` if a local MCP client must be restricted to a **User**
credential.

### Secret redaction

Every MCP response, configuration snapshot, dry-run diff, probe diagnostic, log search result, and error passes through serialization-layer secret redaction before leaving KeepPeek:

- Raw values loaded from `secrets.toml` (including URL percent-encoded variants) are scrubbed.
- Fields with credential-like names (`password`, `secret`, `token`, `access_key`, `verifier`, `cookie`, `authorization`, `api_key`, `private_key`) are redacted unless they are already `{secret:KEY}` or `{secret:KEY|url}` references.
- Embedded URL credentials (`scheme://user:pass@host`) are scrubbed while `{secret:KEY}` and `{secret:KEY|url}` tokens remain visible for safe editing.
- **Explicit `camera_debugging` boundary:** When `[mcp].allow_camera_debugging = true`, `SecretRedactor` is bypassed by default on `capture` and `compare_transports` (both default `include_raw_auth: true` — pass `include_raw_auth: false` to redact), bypassed on authorized `read_artifact` responses (so `traces.jsonl`, `capture.pcapng`, and `protocol_transcript.json` download intact with SHA-256 parity), bypassed on `inspect_pipeline` only when `include_raw_auth: true` is passed (defaults to `false`), and bypassed on `export_repro_fixture` only when `strip_credentials: false` is passed (defaults to `true`). `SecretRedactor` remains strictly enforced on all other 28 MCP tools, error messages, disallowed cameras, and whenever `export_repro_fixture` runs with `strip_credentials: true`.

### DNS rebinding and cross-origin protection

`POST /mcp` validates the `Host` header and enforces an explicit browser origin allowlist:

- Non-browser MCP clients that omit `Origin` are permitted.
- Unauthenticated local requests (when `[access].local_networks` grants local trust) require `Host`
  to be `localhost` or an IP literal (`127.0.0.1`, `[::1]`) so DNS-rebound domain names are
  rejected with `403`.
- Requests carrying a browser `Origin` header are rejected with `403` unless the exact origin is
  listed in `[direct_card].allowed_origins`, and require `Authorization: Bearer <key>` even on local
  networks (returning `401` if unauthenticated).
- `/mcp/secret-entry` is stricter still: it allows loopback peers only, rejects proxy-forwarded
  headers, and requires an exact same-origin loopback `Origin` on `POST`.
- Unsupported HTTP methods on `/mcp` or `/mcp/secret-entry` return `405 Method Not Allowed`, and a
  disabled MCP server returns `404 Not Found`.

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

| Symptom                                          | Check                                                                                                                                                       |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `POST /mcp` or `/mcp/secret-entry` returns `404` | Confirm MCP is enabled by a human Administrator in **Settings > Integrations** (or `[mcp].enabled = true` in `config.toml` + restart).                      |
| `POST /mcp` returns `403`                        | Check whether `Origin` is missing from `[direct_card].allowed_origins`, `Host` is non-local for local trust, or role is User.                               |
| `/mcp/secret-entry` returns `403` or `410`       | Open the link directly from loopback without a reverse proxy, and verify the single-use token has not expired or been consumed.                             |
| `POST /mcp` returns `426`                        | Remote plain HTTP is blocked by `[access].require_secure_remote = true`. Use HTTPS via a trusted proxy or a private VPN.                                    |
| Configuration write tool is denied               | Confirm `[mcp].allow_writes = true`, an Administrator credential, and that `--read-only` is not set.                                                        |
| On-camera hardware write is denied (`403`)       | Confirm `[mcp].allow_camera_device_writes = true`, `--read-only` is not set, and no excluded security field was requested.                                  |
| `probe_camera_streams` rejects input             | Replace any raw password or inline RTSP credentials with `{secret:KEY}` refs and keep `credential_candidates` to at most 5.                                 |
| `get_camera_snapshot` is denied                  | Confirm `[mcp].allow_snapshots = true`, camera view permission, inactive privacy mode, and the per-camera interval limit.                                   |
| `camera_debugging` is denied                     | Confirm `[mcp].allow_camera_debugging = true`, an Administrator credential, `debug_camera_ids` scope, and no `--read-only` flag for capture/delete actions. |
| Request returns `429` or `503` or times out      | Too many simultaneous MCP calls (`max_concurrent_requests`), camera rate-limit/lockout, or slow probe exceeding timeout.                                    |
| `apply_configuration_plan` fails conflict        | Configuration changed after the plan was created. Call `get_configuration` and generate a fresh plan.                                                       |
| `keeppeek mcp` fails to connect remotely         | Confirm `KEEPPEEK_ACCESS_KEY` or `--credential-file` is set and readable, and verify `--server` points to the HTTPS origin.                                 |
