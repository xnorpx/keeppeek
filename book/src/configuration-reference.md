# Configuration reference

This chapter is the canonical reference for every supported on-disk section, serialized Rust struct, field, type, default, limit, ownership boundary, apply mode, and secret-reference rule in `config.toml` and its companion `secrets.toml`. KeepPeek stores all durable settings—including layouts, notification rules, credential records, templates, runtime profiles, and saved searches—in `config.toml` rather than separate settings files; media files and catalog databases are data, not settings stores.

Use [visual configuration](./configuration-management.md) for routine UI administration and [configuration exchange](./configuration-export-import.md) for validated export and import.

## Quick section index

| TOML section                                | Rust type                                | Purpose                                                     | Ownership                            | Apply mode                              | Reference                                                                       |
| ------------------------------------------- | ---------------------------------------- | ----------------------------------------------------------- | ------------------------------------ | --------------------------------------- | ------------------------------------------------------------------------------- |
| Root fields                                 | `Config`                                 | HTTP listener and initial access key                        | Operator                             | Restart                                 | [Root settings](#root-settings)                                                 |
| `[access]`                                  | `AccessConfig`                           | Network trust, rate limits, session bounds                  | Operator                             | Restart                                 | [Access policy](#access-policy)                                                 |
| `[direct_card]`                             | `DirectCardConfig`                       | Exact browser-origin allowlist                              | Operator                             | Restart                                 | [Direct card origins](#direct-card-origins)                                     |
| `[desktop]`                                 | `DesktopConfig`                          | Default server URL for KeepPeek Desktop                     | Operator                             | Desktop launch                          | [Desktop client settings](#desktop-client-settings)                             |
| `[mcp]`                                     | `McpConfig`                              | Opt-in MCP server, write, snapshot, and camera debug policy | Operator                             | Restart                                 | [MCP server](#mcp-server)                                                       |
| `[sharing]`                                 | `sharing::SharingConfig`                 | Isolated guest review deployment policy                     | Operator (server revision)           | Live (UI/API) / Restart (file)          | [Guest review deployment policy](#guest-review-deployment-policy)               |
| `[external_auth]`                           | `access::external::Config`               | OIDC and identity-proxy login policy                        | Operator                             | Live (plan/apply) / Restart (file)      | [External authentication configuration](#external-authentication-configuration) |
| `[external_identities]`                     | `access::identities::Directory`          | Provisioned external user records                           | Server-managed                       | Live (runtime)                          | [Durable external identities](#durable-external-identities)                     |
| `[camera_defaults]`                         | `CameraCredentialDefaults`               | Shared camera credentials and policy defaults               | Operator                             | Live (plan/apply) / Restart (file)      | [Shared camera defaults](#shared-camera-defaults)                               |
| `[<namespace>.<camera-key>]`                | `CameraConfig`                           | Per-camera connection, streams, recording                   | Operator (server `profile_identity`) | Live (UI/API) / Restart (file)          | [Camera entries](#camera-entries)                                               |
| `[<namespace>.<camera-key>.live_audio]`     | `LiveAudioSettings`                      | Live listener A/V timing offset                             | Operator                             | Live (no ingress restart)               | [Live audio timing](#live-audio-timing)                                         |
| `[<namespace>.<camera-key>.recorded_audio]` | `RecordedAudioSettings`                  | Recorded playback and export audio offset                   | Operator                             | Live (no ingress restart)               | [Recorded-audio timing](#recorded-audio-timing)                                 |
| `[<namespace>.<camera-key>.events]`         | `EventConfig`                            | File-only camera-native event transport/filters             | Operator (file-only)                 | Restart                                 | [Native camera events](#native-camera-events)                                   |
| `[isapi_callbacks]`                         | `isapi::callbacks::Config`               | Dedicated Hikvision ISAPI callback listener                 | Operator                             | Restart                                 | [ISAPI event callbacks](#isapi-event-callbacks)                                 |
| `[storage]`                                 | `StorageToml`                            | Recording paths, budgets, safety, pre-roll                  | Operator                             | Restart (paths/budgets) / Live (policy) | [Recording storage](#recording-storage)                                         |
| `[storage.retention]`                       | `storage::retention::settings::Settings` | Time-based recording lifetime rules                         | Operator                             | Restart / activation sweep              | [Recording retention rules](#recording-retention-rules)                         |
| `[storage.named_volumes]`                   | `storage::volumes::VolumeConfiguration`  | Named storage roots and placement rules                     | Operator                             | Restart                                 | [Named storage volumes](#named-storage-volumes)                                 |
| `[storage.metadata]`                        | `MetadataBinding`                        | Authoritative catalog and export-history binding            | Server-managed                       | Confirmed restart handoff               | [Named storage volumes](#named-storage-volumes)                                 |
| `[storage.metadata_pending]`                | `config::metadata::pending::Pending`     | Staged metadata relocation handoff                          | Server-managed                       | Startup migration                       | [Named storage volumes](#named-storage-volumes)                                 |
| `[storage.archive]`                         | `storage::archive::settings::Settings`   | Fourth-tier local archive replica target                    | Operator                             | Restart / runtime worker                | [Fourth-tier local archive](#fourth-tier-local-archive)                         |
| `[battery_wake]`                            | `BatteryWakeConfig`                      | Reolink battery-camera wake middleman                       | Operator                             | Restart                                 | [Battery wake](#battery-wake)                                                   |
| `[logging]`                                 | `LoggingConfig`                          | Windows service log destination                             | Operator                             | Restart                                 | [Logging](#logging)                                                             |
| `[reporting]`                               | `reporting::Settings`                    | Optional HTTPS bug reporting service                        | Operator                             | Restart                                 | [Bug reporting service](#bug-reporting-service)                                 |
| `[usage_reporting]`                         | `usage_reporting::UsageReportingConfig`  | Opt-in privacy-preserving usage reporting                   | Operator                             | Live (UI/API) / Restart (file)          | [Usage reporting](#usage-reporting)                                             |
| `[cloud_notifications]`                     | `cloud_notifications::Settings`          | Optional Keeppeek Service mobile push signaling             | Operator (server revision)           | Live (UI/API) / Restart (file)          | [Cloud notifications](#cloud-notifications)                                     |
| `[operational_events]`                      | `OperationalEventsConfig`                | Global and per-camera health-event timing                   | Operator                             | Restart                                 | [Operational events](#operational-events)                                       |
| `[privacy]`                                 | `PrivacyConfig`                          | Default and per-camera recurring privacy schedules          | Operator                             | Live (plan/apply) / Restart (file)      | [Privacy schedules](#privacy-schedules)                                         |
| `[analytics]`                               | `AnalyticsConfig`                        | Activity summary defaults and rollup quota                  | Operator                             | Restart                                 | [Activity analytics](#activity-analytics)                                       |
| `[event_forwarder.mqtt]`                    | `MqttForwarderConfig`                    | MQTT broker connection and forwarding policy                | Operator (server revision)           | Live (UI/API) / Restart (file)          | [MQTT event forwarding](#mqtt-event-forwarding)                                 |
| `[access_credentials]`                      | `PersistedAccessCatalog`                 | Named bearer credential records and grants                  | Server-managed                       | Live (UI/API)                           | [Credential records](#credential-records)                                       |
| `[saved_event_searches]`                    | `server::saved_searches::StoredRegistry` | Owner-scoped saved event search ASTs                        | Server-managed                       | Live (UI/API)                           | [Saved event searches](#saved-event-searches)                                   |
| `[peek_layouts]`                            | `server::peek_layouts::StoredRegistry`   | Live-wall dashboards, display settings, selections          | Server-managed                       | Live (UI/API)                           | [Peek layout registry](#peek-layout-registry)                                   |
| `[configuration_templates]`                 | `StoredTemplateDocument`                 | Versioned camera configuration templates                    | Server-managed                       | Live (UI/API)                           | [Camera templates](#camera-templates)                                           |
| `[notifications]`                           | `NotificationConfiguration`              | Notification rule drafts and active rules                   | Server-managed                       | Live (UI/API)                           | [Notification rules](#notification-rules)                                       |
| `[runtime_profiles]`                        | `ProfileRegistry`                        | Runtime camera policy profiles and selection                | Server-managed                       | Live (UI/API)                           | [Runtime profiles](#runtime-profiles)                                           |
| `[state_store]`                             | `server::state_store_settings`           | Settings-backed `StateStore` adapter namespaces             | Server-managed                       | Live (API)                              | [Settings-backed state store](#settings-backed-state-store)                     |
| `[storage_migration]`                       | `StorageMigration`                       | Staged legacy storage-path move journal                     | Server-managed                       | Startup migration                       | [Pending storage migration](#pending-storage-migration)                         |
| `[homekit]`                                 | None (reserved)                          | Reserved placeholder; not an implemented schema             | Reserved (unimplemented)             | N/A                                     | —                                                                               |

**Namespace and parsing rules:**

- Every top-level table **not** listed among the 28 reserved keys above (`access`, `sharing`, `reporting`, `usage_reporting`, `cloud_notifications`, `external_auth`, `external_identities`, `access_credentials`, `storage`, `battery_wake`, `direct_card`, `desktop`, `mcp`, `homekit`, `logging`, `operational_events`, `analytics`, `event_forwarder`, `notifications`, `peek_layouts`, `saved_event_searches`, `configuration_templates`, `runtime_profiles`, `isapi_callbacks`, `camera_defaults`, `privacy`, `state_store`, `storage_migration`) is parsed as a **camera namespace** (`cameras` is conventional, not required). Do not use `homekit` as a camera namespace. A typo in a reserved section name is therefore parsed as a camera namespace.
- Some structs ignore unknown fields while strict sections (`#[serde(deny_unknown_fields)]`, noted per section) reject them. Successful TOML parsing alone does not prove an unrecognized key has an effect.
- `Config.source` and `Config.secrets_snapshot` are in-memory `#[serde(skip)]` fields; neither is a writable file key.
- Sources: [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs), [src/cameras/mod.rs](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/mod.rs), and the subsystem sources linked in each section.

## Location and editing

| Environment | Default `config.toml` path                               |
| ----------- | -------------------------------------------------------- |
| macOS       | `~/Library/Application Support/keeppeek/config.toml`     |
| Linux       | `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/config.toml` |
| Windows     | `%APPDATA%\keeppeek\config.toml`                         |
| Docker      | `/config/keeppeek/config.toml`                           |

- **CLI selection:** `--config <path>`, `--config=<path>`, or `-c <path>` selects another config file; its companion secrets file is always `secrets.toml` in the same directory. `--access-key <uuid>` (or `--access-key=<uuid>`) supplies or updates the initial Administrator key stored into `secrets.toml`.
- **Service account paths:** Default paths belong to the process user account (a Windows service can resolve a different `%APPDATA%` from interactive runs; the macOS package runs a per-user launch agent). Verify effective paths before migrating; see [Upgrades and migrations](./upgrades-and-migrations.md).
- **Safe editing:** Use the UI or typed API while KeepPeek runs. Stop the server before manual edits so concurrent updates do not overwrite them. Server writers validate full candidates, preserve unrelated sections and secret references, and replace the file atomically under the shared configuration-update lock.
- **File permissions:** On Unix, KeepPeek creates default directories with `0700` and private files with `0600`. Restrict the directory to the service account on every platform. Values in `secrets.toml` are plaintext at rest, not encrypted by TOML.
- **Table conventions:** `Absent` in the Default column means omit the key when unset (TOML has no `null`). Integer types refer to the Rust model and must also fit TOML's signed 64-bit integer range (`i64`). `Vec<T>` is a TOML array; maps are TOML tables keyed by the noted identity. Byte limits and character limits differ. Place all root fields before the first `[table]` header.

## Passwords and secret references

`Secrets` (`secrets.toml`) is a flat `BTreeMap<String, String>`. Keys must match `[A-Z_][A-Z0-9_]*`. Nested tables, arrays, numbers, and booleans are invalid. Enter real secrets directly on the host; never commit them to source control or place them in logs, screenshots, or shell arguments.

Example `secrets.toml` (replace placeholders locally):

```toml
CAMERA_USERNAME = "replace-with-camera-user"
CAMERA_PASSWORD = "replace-with-camera-password"
FRONT_CAMERA_PASSWORD = "replace-with-front-camera-password"
MQTT_PASSWORD = "replace-with-broker-password"
```

Example `config.toml` referencing those keys:

```toml
host = "0.0.0.0"
port = 8081

[camera_defaults]
username = "{secret:CAMERA_USERNAME}"
password = "{secret:CAMERA_PASSWORD}"

[cameras.front_door]
ip = "192.0.2.10"
password = "{secret:FRONT_CAMERA_PASSWORD}"
main_rtsp_url = "rtsp://{secret:CAMERA_USERNAME|url}:{secret:FRONT_CAMERA_PASSWORD|url}@192.0.2.10:554/stream1"

[cameras.back_door]
ip = "192.0.2.11"

[event_forwarder.mqtt]
username = "keeppeek"
password = "{secret:MQTT_PASSWORD}"
```

- `{secret:KEY}` substitutes the string verbatim (full value or substring).
- `{secret:KEY|url}` percent-encodes a URL component (all bytes except RFC 3986 unreserved `A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, `~`, including `@`, `:`, `/`, spaces, and non-ASCII UTF-8). Use for embedded credentials or query values, not entire URLs.
- `KEEPPEEK_SECRET_<KEY>` in the server environment overrides the matching key in `secrets.toml`. Literal config values never consult either source.
- Missing keys, malformed references, unknown modifiers, or invalid `secrets.toml` fail loading or the settings operation; diagnostics name the key without exposing its value.
- Resolution substitutes strings only (never TOML numbers/tables) and is non-recursive.
- Rotating a secret in `secrets.toml` requires restarting KeepPeek to reconnect with the new value.

### Reference boundaries

| Section / fields                                                                                                                                                                                                                                  | Secret-reference behavior                                                                                                                                                                                                                                                             |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Root settings, `[camera_defaults]`, camera entries (including `ip` and `.events`), `[storage]` (including `named_volumes` and `archive`), `[sharing].public_origin`, `[external_auth]`, and other ordinary sections                               | Strings (including inside arrays/tables) are resolved before typed deserialization; resolved values must still satisfy field types and bounds. `[external_auth]` secrets (`client_secret`, `shared_secret`) **must** be complete `{secret:...}` references (inline secrets rejected). |
| `[isapi_callbacks].sources[].password`                                                                                                                                                                                                            | **Must** contain a `{secret:...}` reference in `config.toml` (inline passwords rejected before resolution). Other strings in `[isapi_callbacks]` also resolve before deserialization.                                                                                                 |
| `[event_forwarder.mqtt].password`                                                                                                                                                                                                                 | Accepts literal or reference; the MQTT writer stores supplied passwords in `secrets.toml` under `MQTT_PASSWORD`.                                                                                                                                                                      |
| `[reporting].credential` & `[cloud_notifications].signing_key`                                                                                                                                                                                    | Complete `{secret:KEY}` only (`<= 128`-byte key name, no `\|url`). Syntax-checked at load without resolving; resolved lazily only when issuing a report grant or signing a cloud signal.                                                                                              |
| `[notifications]` `active.actions[].destination` & `draft.actions[].destination`                                                                                                                                                                  | Explicitly resolved by the notification loader. Non-browser destinations saved via the store are moved into managed `KEEPPEEK_NOTIFICATION_...` keys.                                                                                                                                 |
| `[configuration_templates]` `username_secret_reference` & `password_secret_reference`                                                                                                                                                             | Complete reference tokens only (no inline credentials or surrounding text); resolved when applied to a camera.                                                                                                                                                                        |
| `[access_credentials]`, `[external_identities]`, `[peek_layouts]`, `[saved_event_searches]`, `[runtime_profiles]`, `[state_store]`, other notification/template fields, `[storage_migration]`, numeric audio offsets, numeric retention durations | Not blanket-expanded as secret templates. Dedicated loaders or numeric types apply.                                                                                                                                                                                                   |

Settings responses and unrelated writes preserve existing reference tokens instead of writing resolved secrets back to `config.toml`. Inline camera credentials remain supported and are not auto-migrated; replacing a camera credential in the editor can replace a reference with an inline value. Ordinary layout exports exclude credentials, verifiers, and unrelated user state; full recovery archives have a separate security contract (see [Backup and restore](./backup-and-restore.md)).

First-run startup generates `KEEPPEEK_ACCESS_KEY` in `secrets.toml` and references it from root `access_key`. Named credentials created later store only SHA-256 verifiers in `[access_credentials]` and show raw keys once. See [Authentication](./authentication.md) and [docs/secrets.md](https://github.com/xnorpx/keeppeek/blob/main/docs/secrets.md).

## Root settings

**Type:** `Config` | **Section:** root (before any `[table]` header) | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

| Field        | Type        | Default                     | Limits and notes                                                                                                                                                                  |
| ------------ | ----------- | --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `host`       | `String`    | `"0.0.0.0"`                 | HTTP listener bind address.                                                                                                                                                       |
| `port`       | `u16`       | `8081`                      | HTTP listener port.                                                                                                                                                               |
| `access_key` | `AccessKey` | Unset; generated on startup | Initial compatibility Administrator key: hyphenated UUID string, `{secret:KEY}` resolving to a UUID, or integer `0` as the unset marker. `0` does **not** disable authentication. |

## Access policy

**Type:** `AccessConfig` | **Section:** `[access]` | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

| Field                               | Type         | Default                          | Limits and notes                                                                                                                                  |
| ----------------------------------- | ------------ | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `local_networks`                    | `Vec<IpNet>` | 8 private/link-local CIDRs below | Max 64 CIDRs. Matching peers are trusted-local Administrators without sign-in. Setting this array replaces all defaults.                          |
| `trusted_proxies`                   | `Vec<IpNet>` | `[]`                             | Max 64 CIDRs, checked separately from `local_networks`. Only these peers may supply the forwarded-client header (`X-Forwarded-For`, max 16 hops). |
| `require_secure_remote`             | `bool`       | `true`                           | Requires the declared secure transport boundary for remote traffic.                                                                               |
| `failed_authentication_limit`       | `u32`        | `5`                              | Nonzero failed attempts per address per window before rate limiting.                                                                              |
| `failed_authentication_window_secs` | `u64`        | `60`                             | Nonzero authentication rate-limit window, in seconds.                                                                                             |
| `session_idle_timeout_secs`         | `u64`        | `1800`                           | Nonzero; must be `<= session_absolute_timeout_secs`.                                                                                              |
| `session_absolute_timeout_secs`     | `u64`        | `86400`                          | Nonzero maximum session lifetime, in seconds.                                                                                                     |
| `max_sessions_per_principal`        | `u32`        | `64`                             | Nonzero concurrent session cap per identity.                                                                                                      |
| `max_sessions_per_address`          | `u32`        | `128`                            | Nonzero concurrent session cap per client address.                                                                                                |

Default `local_networks`: `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.0.0/16`, `::1/128`, `fc00::/7`, `fe80::/10`. Container bridges or VPNs inside those ranges are trusted unless narrowed. Forwarding headers and CORS never replace authentication; review [Authentication and access control](./authentication.md) before exposing the listener.

### Direct card origins

**Type:** `DirectCardConfig` | **Section:** `[direct_card]` | **Ownership:** Operator | **Apply:** Restart

| Field             | Type          | Default | Limits and notes                                                                                                                                                    |
| ----------------- | ------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `allowed_origins` | `Vec<String>` | `[]`    | Unique canonical exact HTTP(S) origins (e.g., `"https://ha.example"`). No credentials, path, query, fragment, wildcard, trailing slash, control chars, or port `0`. |

### Desktop client settings

**Type:** `DesktopConfig` (`deny_unknown_fields`) | **Section:** `[desktop]` | **Ownership:** Operator / KeepPeek Desktop | **Apply:** Desktop launch

| Field                 | Type              | Default | Limits and notes                                                                                                                                                                                                                                         |
| --------------------- | ----------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `server_url`          | Optional `String` | Absent  | Canonical exact HTTP(S) origin (`1..=512` UTF-8 bytes, e.g., `"http://127.0.0.1:8081"`, `"https://nvr.example"`). Rejects credentials, path, query, fragment, trailing slash, control chars, port `0`, and unspecified bind addresses (`0.0.0.0`, `::`). |
| `manage_local_server` | `bool`            | `false` | Opt-in flag enabling `keeppeek-desktop` to embed `keeppeek-supervisor` and manage a local loopback `keeppeek` child process. Omitted from `config.toml` when `false` to preserve compatibility with older `keeppeek` servers.                            |
| `supervisor_root`     | Optional `String` | Absent  | Optional directory override (`1..=1,024` UTF-8 bytes, non-blank, no control chars) for the desktop supervisor's `supervisor-state.json` and A/B slots. Defaults to `<config_dir>/supervisor`.                                                            |

## Guest review deployment policy

**Type:** `sharing::SharingConfig` (`deny_unknown_fields`) | **Section:** `[sharing]` | **Ownership:** Operator (server-owned `revision`) | **Apply:** Live (Admin control command) / Restart (manual file edit) | **Source:** [src/config/sharing.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/sharing.rs)

Guest sharing is disabled by default. These fields do not configure a tunnel, relay, DNS, TLS certificate, or network exposure—the operator must provide a reachable HTTPS origin.

| Field           | Type              | Default | Limits and notes                                                                                                                                                                                                                                                                                           |
| --------------- | ----------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`       | `bool`            | `false` | Enables guest handoff only when origin, evidence, private metadata, and transport checks succeed. Requires `public_origin` when `true`.                                                                                                                                                                    |
| `public_origin` | Optional `String` | Absent  | Canonical HTTPS origin (`1..=512` UTF-8 bytes, e.g., `"https://review.example"`). No credentials, path, query, fragment, trailing slash, or control chars. Accepts `{secret:KEY}`; saving an unchanged effective origin preserves the raw reference. Visible to Administrators because guest links use it. |
| `revision`      | `u64`             | `1`     | Server-owned optimistic-write revision (`1..=i64::MAX`). Successful Admin saves increment it; stale writes are rejected.                                                                                                                                                                                   |

**Lifecycle and operational bounds:**

- Saving policy invalidates active guest sessions and stops active guest transfers while preserving durable view/download counters. If an atomic write returns an indeterminate persistence error after commit, guest access halts until an Administrator saves successfully; reloading camera config cannot silently re-enable it.
- Grants, hashed token verifiers, and audit counters live in operational SQLite storage under the private, identity-pinned metadata owner (`guest-review/grants.db`), never in a second settings file. Guest sessions and active transfers do not survive restart, and guest cookies never elevate to a KeepPeek principal, role, or control-channel credential.
- **Fixed limits:** max `256` active grants, `32` per creator, `<= 24` hours (bounded by export expiry); evidence max `2` minutes and `512 MiB`; display name `<= 128` UTF-8 bytes, notes `<= 512` bytes; max `8` concurrent media sessions globally and `2` per grant. Sharing creates no retention hold. Expired/revoked records drop from management lists after `30` days. Audit history caps at `4,096` records (new grants rejected at capacity); startup purges overdue rows, and runtime maintenance deletes up to `256` rows per pass (re-scheduling immediately on a full batch, otherwise waiting 1 minute).

## External authentication configuration

**Type:** `access::external::Config` (`deny_unknown_fields`) | **Section:** `[external_auth]` (optional) | **Ownership:** Operator | **Apply:** Live (plan/apply) / Restart (file) | **Source:** [src/access/external.rs](https://github.com/xnorpx/keeppeek/blob/main/src/access/external.rs)

Omitting `[external_auth]` preserves default local and bearer-key behavior. OIDC providers use browser authorization code + PKCE; identity proxies assert identities from immediate trusted transport peers. Both issue revocable browser sessions and enforce server-side camera grants. All structs in this section reject unknown fields.

| Field                        | Type            | Default  | Limits and notes                                                                                                                                      |
| ---------------------------- | --------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `allowed_origins`            | `Vec<String>`   | Required | `1..=16` unique canonical exact HTTPS origins (`<= 2,048` B each; no credentials, path, query, fragment, wildcard, trailing slash, or control chars). |
| `providers`                  | `Vec<Provider>` | Required | `1..=4` `[[external_auth.providers]]` entries with unique `id`s.                                                                                      |
| `bearer_enabled`             | `bool`          | `false`  | Enables mixed bearer + external transition mode; requires `bearer_transition_until_ms`.                                                               |
| `bearer_transition_until_ms` | Optional `i64`  | Absent   | Positive Unix ms; must be present **if and only if** `bearer_enabled = true`.                                                                         |

**Provider (`[[external_auth.providers]]`) & Mapping (`[[external_auth.providers.mappings]]`):**

| Table / Field           | Type                    | Default                                            | Limits and notes                                                                                                                                                                                                                 |
| ----------------------- | ----------------------- | -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `provider.id`           | `String`                | Required                                           | Unique; `1..=64` ASCII `[A-Za-z0-9_-]`.                                                                                                                                                                                          |
| `provider.name`         | `String`                | Required                                           | Nonblank `1..=64` bytes, no control chars.                                                                                                                                                                                       |
| `provider.mappings`     | `Vec<Mapping>`          | Required                                           | `>= 1` per provider; max `128` mappings across all providers.                                                                                                                                                                    |
| `provider.method`       | `Method`                | Required                                           | Tagged table `[external_auth.providers.method]` with `kind = "oidc"` or `kind = "proxy"`.                                                                                                                                        |
| `mapping.claim`         | `String`                | Required                                           | Nonblank `1..=64` bytes, no control chars.                                                                                                                                                                                       |
| `mapping.value`         | `String`                | Required                                           | Nonblank `1..=256` bytes, no control chars.                                                                                                                                                                                      |
| `mapping.role`          | `AccessRole`            | Required                                           | `"administrator"` or `"user"`.                                                                                                                                                                                                   |
| `mapping.camera_access` | Optional `CameraAccess` | Required for `user`; forbidden for `administrator` | Same schema as [Credential records](#credential-records) (`all_cameras` + `camera_ids` explicit; `group_ids` defaults to `[]`; max `128` combined camera + group IDs). With `all_cameras = false`, empty lists grant no cameras. |

Unmapped identities and identities matching more than one rule are denied. Evaluated claims must be a string or a list of `<= 128` strings (`<= 256` B each); serialized claims JSON is bounded to `16 KiB`.

**OIDC method (`[external_auth.providers.method]`, `kind = "oidc"`):**

| Field                | Type              | Default      | Limits and notes                                                                                                                                   |
| -------------------- | ----------------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `issuer`             | `String`          | Required     | HTTPS URL (`<= 2,048` B); no user-info, query, or fragment.                                                                                        |
| `client_id`          | `String`          | Required     | Nonblank `1..=256` bytes, no control chars.                                                                                                        |
| `client_secret`      | Optional `String` | Absent       | Complete `{secret:KEY}` or `{secret:KEY\|url}` reference only (inline secrets rejected). Resolved value: nonblank `1..=4,096` B, no control chars. |
| `redirect_uri`       | `String`          | Required     | Allowed HTTPS origin with exact path `/auth/callback`; no query or fragment.                                                                       |
| `scopes`             | `Vec<String>`     | `["openid"]` | Max `16` unique OAuth scope tokens (`1..=64` B each); must include `"openid"`.                                                                     |
| `display_name_claim` | `String`          | `"name"`     | Nonblank `1..=64` bytes, no control chars.                                                                                                         |
| `endpoint_origins`   | `Vec<String>`     | `[]`         | Max `16` unique canonical HTTPS origins for additional provider endpoints.                                                                         |
| `private_networks`   | `Vec<IpNet>`      | `[]`         | Max `64` explicitly permitted private issuer networks.                                                                                             |
| `logout_uri`         | Optional `String` | Absent       | HTTPS URL (`<= 2,048` B); no user-info, query, or fragment.                                                                                        |

All external-auth URLs are bounded to `2,048` bytes and never authorize arbitrary discovery redirects or unconfigured destinations.

**Identity-proxy method (`[external_auth.providers.method]`, `kind = "proxy"`):**

| Field            | Type              | Default  | Limits and notes                                                                                                                                                                                                 |
| ---------------- | ----------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted_peers`  | `Vec<IpNet>`      | Required | `1..=64` immediate transport-peer CIDRs, independent of `[access].trusted_proxies`. Networks across different proxy providers must not overlap. IPv4-mapped IPv6 peers normalize to IPv4 (configure IPv4 CIDRs). |
| `subject_header` | `String`          | Required | Exact identity assertion header.                                                                                                                                                                                 |
| `role_header`    | `String`          | Required | Exact role assertion header (still requires explicit mapping).                                                                                                                                                   |
| `name_header`    | Optional `String` | Absent   | Optional display-name header.                                                                                                                                                                                    |
| `secret_header`  | Optional `String` | Absent   | Present **if and only if** `shared_secret` is present.                                                                                                                                                           |
| `shared_secret`  | Optional `String` | Absent   | Same complete-reference and resolved-value rules as `client_secret`.                                                                                                                                             |

All proxy header names must be distinct (case-insensitive), `<= 64` ASCII `[A-Za-z0-9-]`, start with `X-`, and **not** start with `X-Forwarded-`. Secret references stay unexpanded in `config.toml`.

### Durable external identities

**Type:** `access::identities::Directory` (`deny_unknown_fields`) | **Section:** `[external_identities]` | **Ownership:** Server-managed | **Apply:** Live (runtime) | **Source:** [src/access/identities.rs](https://github.com/xnorpx/keeppeek/blob/main/src/access/identities.rs)

Top-level fields: `version: u32` (must be `1`) and `records: Vec<Identity>` (`[[external_identities.records]]`, max `1,024` records).

| Field                          | Type              | Default  | Limits and notes                                                                                                                                                                                                                                              |
| ------------------------------ | ----------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`                           | `Uuid`            | Required | Unique non-nil UUID.                                                                                                                                                                                                                                          |
| `provider_id`                  | `String`          | Required | Nonblank `1..=64` B, no control chars.                                                                                                                                                                                                                        |
| `subject_fingerprint`          | `String`          | Required | Unique 64 lowercase hex SHA-256 digest of namespace + subject (raw subjects/claims/tokens are never stored).                                                                                                                                                  |
| `admission_policy_fingerprint` | Optional `String` | Absent   | 64 lowercase hex SHA-256 digest of resolved provider policy at last successful login. Absent/stale digests cannot satisfy retained-Administrator lockout checks until the identity re-authenticates under the current policy; legacy records remain readable. |
| `display_name`                 | `String`          | Required | Nonblank `1..=64` B, no control chars.                                                                                                                                                                                                                        |
| `role`                         | `AccessRole`      | Required | `"administrator"` or `"user"`.                                                                                                                                                                                                                                |
| `camera_access`                | `CameraAccess`    | Required | Unrestricted for Administrators; validated policy (`<= 128` combined IDs) for Users.                                                                                                                                                                          |
| `revision`                     | `u64`             | Required | Positive monotonic revision.                                                                                                                                                                                                                                  |
| `enabled`                      | `bool`            | Required | `false` revokes the identity.                                                                                                                                                                                                                                 |
| `created_at_ms`                | `i64`             | Required | Nonnegative Unix milliseconds.                                                                                                                                                                                                                                |

## Camera defaults and camera entries

**Sources:** [src/cameras/mod.rs](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/mod.rs) and [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs).

### Shared camera defaults

**Type:** `CameraCredentialDefaults` | **Section:** `[camera_defaults]` | **Ownership:** Operator | **Apply:** Live (plan/apply) / Restart (file)

| Field                               | Type                            | Default                                | Limits and notes                                                                                           |
| ----------------------------------- | ------------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `username`                          | `String`                        | `""`                                   | Shared fallback when a camera's resolved username is empty or omitted. Accepts secret refs.                |
| `password`                          | `String`                        | `""`                                   | Shared fallback when a camera's resolved password is empty or omitted. Accepts secret refs.                |
| `backend`                           | Optional `CameraBackend`        | Absent (model default `"auto"`)        | Inherited only when omitted on the camera.                                                                 |
| `transport`                         | Optional `CameraTransport`      | Absent (model default `"tcp"`)         | Inherited only when omitted on the camera.                                                                 |
| `record_generic_motion_events`      | Optional `bool`                 | Absent (model default `false`)         | Inherited only when omitted on the camera.                                                                 |
| `recording_mode`                    | Optional `CameraRecordingMode`  | Absent (model default `"event-boost"`) | Inherited only when omitted on the camera.                                                                 |
| `event_recording_duration_secs`     | Optional `u64`                  | Absent (model default `60`)            | Inherited only when omitted on the camera; `1..=3600` when `event-only` or pre-roll-enabled `event-boost`. |
| `event_pre_recording_duration_secs` | Optional `u64`                  | Absent (model default `0`)             | Inherited only when omitted on the camera; `0..=30`.                                                       |
| `event_recording_stream`            | Optional `EventRecordingStream` | Absent (model default `"main"`)        | Inherited only when omitted on the camera; `"main"` or `"sub"`.                                            |

Ports, RTSP URLs, audio offsets, and `.events` are **not** fields of `[camera_defaults]`.

### Camera entries

**Type:** `CameraConfig` | **Section:** `[<namespace>.<camera-key>]` (e.g., `[cameras.front_door]`) | **Ownership:** Operator (server-managed `profile_identity`) | **Apply:** Live (UI/API reconnect) / Restart (file)

| Field                               | Type                      | Default                           | Limits and notes                                                                                                                                                                              |
| ----------------------------------- | ------------------------- | --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ip`                                | `IpAddr`                  | Required                          | Camera IP address (not a DNS hostname). Supports `{secret:KEY}`; resolved value must parse as an IP.                                                                                          |
| `profile_identity`                  | `String` (server-managed) | Assigned at startup/create        | Unique canonical 36-char UUID v4; no operator default or secret ref. Preserved across ordinary edits (including IP changes); deleting and re-adding assigns a new UUID. Do not edit manually. |
| `name`                              | Optional `String`         | Camera table key                  | Replaced by the table key on load. Use `display_name` to rename in UI without changing storage identity.                                                                                      |
| `display_name`                      | Optional `String`         | Falls back to `name`              | Human-readable UI label.                                                                                                                                                                      |
| `manufacturer`                      | Optional `String`         | Absent                            | Nonempty trimmed value overrides discovered brand.                                                                                                                                            |
| `username`                          | `String`                  | Shared default or `""`            | Literal or secret reference.                                                                                                                                                                  |
| `password`                          | `String`                  | Shared default or `""`            | Literal or secret reference.                                                                                                                                                                  |
| `onvif_port`                        | Optional `u16`            | `8000` when unset                 | ONVIF port; typed operations require `1..=65535`.                                                                                                                                             |
| `http_port`                         | Optional `u16`            | `80` when unset                   | Direct camera HTTP control port; typed operations require `1..=65535`.                                                                                                                        |
| `main_rtsp_url`                     | Optional `String`         | Discover via ONVIF                | Explicit main-stream RTSP URL (blank values ignored). Use `{secret:KEY\|url}` in URL credentials.                                                                                             |
| `sub_rtsp_url`                      | Optional `String`         | Discover via ONVIF                | Explicit sub-stream RTSP URL.                                                                                                                                                                 |
| `uid`                               | Optional `String`         | Absent                            | Reolink P2P UID for direct BCUDP discovery.                                                                                                                                                   |
| `backend`                           | `CameraBackend`           | Shared default or `"auto"`        | `"auto"`, `"retina"`, or `"reo-proto"`.                                                                                                                                                       |
| `transport`                         | `CameraTransport`         | Shared default or `"tcp"`         | `"tcp"` or `"udp"`.                                                                                                                                                                           |
| `record_generic_motion_events`      | `bool`                    | Shared default or `false`         | Opt-in for supported generic motion events (independent of `events.mode`).                                                                                                                    |
| `recording_mode`                    | `CameraRecordingMode`     | Shared default or `"event-boost"` | `"off"`, `"sub"`, `"main"`, `"both"`, `"event-boost"`, or `"event-only"`.                                                                                                                     |
| `event_recording_duration_secs`     | `u64`                     | Shared default or `60`            | Event recording duration in seconds; `1..=3600` required for typed ops and TOML load when `event-only` or pre-roll-enabled `event-boost`.                                                     |
| `event_pre_recording_duration_secs` | `u64`                     | Shared default or `0`             | `0..=30` seconds; `0` disables pre-roll allocation.                                                                                                                                           |
| `event_recording_stream`            | `EventRecordingStream`    | Shared default or `"main"`        | `"main"` or `"sub"` for `event-only`; `event-boost` always boosts to main.                                                                                                                    |
| `events`                            | `EventConfig`             | Defaults below                    | File-only native camera event policy; preserved by API edits.                                                                                                                                 |
| `recorded_audio`                    | `RecordedAudioSettings`   | All inherit (`0`)                 | Recorded playback/export audio offset table.                                                                                                                                                  |
| `live_audio`                        | `LiveAudioSettings`       | All inherit (`0`)                 | Live listener audio offset table.                                                                                                                                                             |

**Camera identity and discovery notes:**

- `profile_identity` migration runs once atomically at startup before media starts, converting legacy IP targets in `[runtime_profiles]` to UUIDs (or `missing-ip:<address>` for absent targets so later devices at that IP cannot bind), advancing changed revisions once, and stamping `identity_schema = 1`. Camera `ip` secret references stay raw in `config.toml`; identity maps and camera configs use the same `secrets.toml` snapshot. Invalid/duplicate UUIDs and ambiguous active IP targets fail validation without writing. Keep `profile_identity` and `identity_schema = 1` in full backups; downgrading to a pre-migration binary requires restoring a pre-migration config. Other camera controls and media source IDs continue using IP identities.
- Stored fields do not guarantee a camera supports every backend, transport, or recording capability. Discovery outputs (`CameraCapabilities`, `CameraPorts`, `MediaProfile`, `VideoConfig`, `AudioConfig`) are runtime metadata, not editable config tables.

### Live audio timing

**Type:** `LiveAudioSettings` (`deny_unknown_fields`) | **Section:** `[<namespace>.<camera-key>.live_audio]` | **Apply:** Live (current listeners; no ingest or recording restart) | **Source:** [src/cameras/recorded_audio.rs](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/recorded_audio.rs)

| Field               | Type           | Default                      | Limits and notes                                         |
| ------------------- | -------------- | ---------------------------- | -------------------------------------------------------- |
| `default_offset_ms` | Optional `i32` | `0` when absent              | Signed whole ms in `-200..=200`. No secret refs.         |
| `main_offset_ms`    | Optional `i32` | Inherits `default_offset_ms` | `-200..=200`; explicit `0` disables correction for Main. |
| `sub_offset_ms`     | Optional `i32` | Inherits `default_offset_ms` | `-200..=200`; explicit `0` disables correction for Sub.  |

Positive values delay audio; negative values delay video and add `|offset|` to live video latency (including for muted listeners). In-page previews affect only the active viewer/stream.

```toml
[cameras.front_door.live_audio]
default_offset_ms = 80
sub_offset_ms = 0
```

### Recorded-audio timing

**Type:** `RecordedAudioSettings` (`deny_unknown_fields`) | **Section:** `[<namespace>.<camera-key>.recorded_audio]` | **Apply:** Live (new playback opens & new exports; no ingest or recording restart) | **Source:** [src/cameras/recorded_audio.rs](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/recorded_audio.rs)

Independent of native events and live audio. No `[camera_defaults]` equivalent; no secret refs; rejects unknown keys, floats, and out-of-range values.

| Field               | Type           | Default                      | Limits and notes                                                  |
| ------------------- | -------------- | ---------------------------- | ----------------------------------------------------------------- |
| `default_offset_ms` | Optional `i32` | `0` when absent              | Signed whole ms in `-5000..=5000` (editor warns above `1000` ms). |
| `main_offset_ms`    | Optional `i32` | Inherits `default_offset_ms` | `-5000..=5000`; explicit `0` disables correction for Main.        |
| `sub_offset_ms`     | Optional `i32` | Inherits `default_offset_ms` | `-5000..=5000`; explicit `0` disables correction for Sub.         |

```toml
[cameras.front_door.recorded_audio]
default_offset_ms = 125
main_offset_ms = 0
# Sub stream inherits 125 ms.
```

- **Direction & update rules:** Positive delays audio; negative advances audio (may clip complete leading audio samples; partial encoded-sample trimming is unsupported). Typed camera updates replace the nested table when present (omission preserves; empty message resets all overrides to inheritance). Source recordings, catalog times, identity, secret references, and profile state stay unchanged.
- **Playback & export constraints:** Applies without re-encoding to the stream actually stored (EventBoost main-stream review uses the Sub recording and its offset). Nonzero correction supports AAC with zero audio composition offset (nonzero AAC composition offsets fail explicitly); signed video composition offsets are supported. Export retries retain their original timing snapshot and reject a changed recording stream. Edge lookup is bounded to `32` extra periods, `64 MiB` indexed edge ranges, and `512` total source periods; nonzero corrected exports reject overlapping source periods or conflicting metadata. Timing diagnostics inspect container timestamps and do not prove camera/RTP clock accuracy.
- **Calibration:** Compare offsets in **Keep → Recorded audio preview** (cursor-local, does not alter saved settings or exports), save permanently as an Administrator in **Camera / Audio**, and reopen the recording.

### Native camera events

**Type:** `EventConfig` | **Section:** `[<namespace>.<camera-key>.events]` | **Ownership:** Operator (file-only; not exposed by fixed protobuf camera form; preserved by API edits) | **Apply:** Restart | **Source:** [src/cameras/events.rs](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/events.rs)

| Field               | Type              | Default           | Limits and notes                                                                                                                                                                                                |
| ------------------- | ----------------- | ----------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mode`              | `EventMode`       | `"auto"`          | `"auto"`, `"vendor"`, `"onvif-pullpoint"`, `"rtsp-metadata"`, or `"disabled"`.                                                                                                                                  |
| `metadata_stream`   | `MetadataMode`    | `"auto"`          | `"auto"`, `"enabled"`, or `"disabled"`. Does not override `mode = "vendor"` or `"disabled"`.                                                                                                                    |
| `event_service_url` | Optional `String` | Absent (discover) | HTTP(S) URL (`<= 4,096` UTF-8 B) using the exact configured camera IP. Rejects hostnames, wildcard/multicast IPs, port `0`, credentials, query, fragment, backslash, whitespace, and control chars.             |
| `source_tokens`     | `Vec<String>`     | `[]`              | Max `32` exact opaque tokens, each `1..=256` UTF-8 B (nonblank, no control chars).                                                                                                                              |
| `include_topics`    | `Vec<String>`     | `[]`              | Expanded `{namespace}Local/Path` syntax (e.g., `{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm`). Max `32` combined with `exclude_topics`, each `<= 1,024` UTF-8 B. Empty allows recognized topics. |
| `exclude_topics`    | `Vec<String>`     | `[]`              | Same syntax and shared `32`-entry / `1,024`-B limits; exclusions take precedence.                                                                                                                               |
| `snapshots`         | `bool`            | `true`            | Allow optional event snapshots; missing/failed snapshots do not drop events.                                                                                                                                    |

Unstable XML prefixes such as `tns1:VideoSource/MotionAlarm` are rejected; filters require nonempty namespace and slash-separated path segments without whitespace, embedded prefixes, or control chars. Secret references (`{secret:KEY}`, `{secret:KEY|url}`) resolve before validation without relaxing endpoint restrictions. No automatic file watcher exists. See [docs/onvif-events.md](https://github.com/xnorpx/keeppeek/blob/main/docs/onvif-events.md).

### Event recording pre-roll

- `event-only` stores only accepted event windows from `event_recording_stream` (`main` or `sub`). `event-boost` records `sub` continuously and promotes to `main` during event windows.
- Setting `event_pre_recording_duration_secs > 0` (`1..=30`) allocates bounded encoded pre-roll history (governed globally by `[storage].pre_recording_stream_max_bytes` and `pre_recording_global_max_bytes`). In `event-boost`, persistence is delayed by up to the pre-roll duration so an event can replace pending `sub` footage with `main` footage; a crash can lose this uncommitted window. `0` keeps immediate admission.
- Camera pre-roll changes use per-source restart when available (otherwise report restart required); `[storage]` memory-budget changes require an application restart. Whole-GOP eviction can shorten coverage under duration or memory pressure; privacy activation, stream discontinuities, and storage failures invalidate buffered pre-roll history.

## ISAPI event callbacks

**Type:** `isapi::callbacks::Config` & `Source` (`deny_unknown_fields`) | **Section:** `[isapi_callbacks]` (optional) | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/isapi/callbacks.rs](https://github.com/xnorpx/keeppeek/blob/main/src/isapi/callbacks.rs)

Starts a dedicated HTTP listener accepting Hikvision ISAPI alert `POST` requests at `/ISAPI/Event/notification/callback/<camera-ip>` authenticated via MD5 Digest (`qop="auth"`, realm `keeppeek-isapi`).

| Field           | Type              | Default  | Limits and notes                                                                                                            |
| --------------- | ----------------- | -------- | --------------------------------------------------------------------------------------------------------------------------- |
| `bind`          | `SocketAddr`      | Required | Dedicated listener `IP:port` (e.g., `"0.0.0.0:8082"`).                                                                      |
| `trusted_proxy` | Optional `IpAddr` | Absent   | Optional proxy IP permitted to deliver callbacks instead of the camera's own peer IP; must not be unspecified or multicast. |
| `sources`       | `Vec<Source>`     | Required | `1..=64` `[[isapi_callbacks.sources]]` entries.                                                                             |

Each `[[isapi_callbacks.sources]]` entry:

| Field      | Type     | Default  | Limits and notes                                                                                                                                  |
| ---------- | -------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ip`       | `IpAddr` | Required | Unique camera IP; must not be unspecified or multicast.                                                                                           |
| `channel`  | `u32`    | Required | Positive (`>= 1`) ISAPI channel ID; non-heartbeat payloads must match this channel.                                                               |
| `username` | `String` | Required | Unique across sources; `1..=1,024` bytes, Digest quoted-value safe (no control chars or unsafe quotes).                                           |
| `password` | `String` | Required | **Must** use a `{secret:...}` reference in `config.toml` (inline passwords rejected before resolution). Resolved value must be `1..=1,024` bytes. |

Removing a camera through configuration APIs automatically removes matching `isapi_callbacks.sources` entries for that IP and removes `[isapi_callbacks]` when no sources remain. Runtime bounds: max `8 MiB` per callback body, `32 MiB` total concurrent ingress, `128` multipart parts, `64` events per request, `15`-second read deadline, and `Origin` headers rejected (`403`).

## Recording storage

**Type:** `StorageToml` | **Section:** `[storage]` | **Ownership:** Operator (except server-managed `.metadata` / `.metadata_pending`) | **Apply:** Restart for paths and memory budgets; live/plan for eligible policies | **Sources:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs), [src/storage/engine.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/engine.rs)

When optional paths are omitted, `medium_term_path` and `long_term_path` default to `recordings/` under the default OS KeepPeek config directory (even when `--config` points elsewhere), `recording_catalog_path` defaults to `<long_term_path>/recordings.db`, and `event_thumbnail_path` defaults to `<long_term_path>/.event-thumbnails`. When `[storage.named_volumes]` and `[storage.metadata]` are active, effective media/thumbnail roots come from named volume defaults and catalog/history paths come from the metadata binding (shown read-only in the general storage editor).

| Field                            | Type              | Default               | Limits and notes                                                                                                                              |
| -------------------------------- | ----------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `medium_term_path`               | Optional `String` | Runtime-selected      | Legacy/fallback medium-term recording directory.                                                                                              |
| `long_term_path`                 | Optional `String` | Runtime-selected      | Legacy/fallback long-term recording directory.                                                                                                |
| `recording_catalog_path`         | Optional `String` | Runtime-selected      | Legacy catalog path; incompatible with `[storage.metadata]`.                                                                                  |
| `event_thumbnail_path`           | Optional `String` | Runtime-selected      | Legacy/fallback event-thumbnail directory.                                                                                                    |
| `event_thumbnail_max_mb`         | `u64`             | `1024`                | Thumbnail quota in MiB (applies only to catalog-referenced files; unindexed/temp files are untouched; missing files keep catalog references). |
| `short_term_secs`                | `u64`             | `120`                 | Short-term buffer retention, in seconds.                                                                                                      |
| `medium_term_secs`               | `u64`             | `1800`                | Target segment duration in seconds; rotation waits for a video keyframe.                                                                      |
| `flush_interval_secs`            | `u64`             | `60`                  | Recording flush interval, in seconds.                                                                                                         |
| `write_buffer_bytes`             | `usize`           | `8192`                | Recording write buffer size, in bytes.                                                                                                        |
| `long_term_max_gb`               | `u64`             | `1024`                | Long-term storage budget, in GiB.                                                                                                             |
| `minimum_free_gb`                | `u64`             | `10`                  | Minimum free-space floor, in GiB.                                                                                                             |
| `maximum_used_percent`           | Optional `u8`     | Absent                | When set, `1..=99`; filesystem-used percentage ceiling.                                                                                       |
| `warning_free_gb`                | `u64`             | `20`                  | Warning free-space threshold, in GiB.                                                                                                         |
| `critical_free_gb`               | `u64`             | `10`                  | Critical free-space threshold, in GiB.                                                                                                        |
| `cleanup_hysteresis_gb`          | `u64`             | `5`                   | Cleanup recovery margin, in GiB.                                                                                                              |
| `pre_recording_stream_max_bytes` | `u64`             | `67108864` (64 MiB)   | Positive per-stream encoded pre-roll cap; must be `<= pre_recording_global_max_bytes`.                                                        |
| `pre_recording_global_max_bytes` | `u64`             | `268435456` (256 MiB) | Positive global encoded pre-roll cap across all streams (including pending replay); must fit `usize` and `i64`.                               |

**Validation and runtime notes:**

- Effective critical threshold = `max(critical_free_gb, minimum_free_gb)`. If `warning_free_gb == 0` while effective critical `> 0`, warning derives as `critical + cleanup_hysteresis_gb`; otherwise `warning_free_gb >= effective critical` is required.
- Each recording writer buffers at most `16 MiB` between video keyframes; exceeding it commits the decodable prefix, reports a health gap, and resumes on the next keyframe.
- Configuration ZIP apply preserves target storage paths (it never moves recording archives). See [Move storage deliberately](./upgrades-and-migrations.md#move-storage-deliberately) and [Recording archive recovery](./recording-archive-recovery.md).

### Recording retention rules

**Type:** `storage::retention::settings::Settings` (`deny_unknown_fields`) | **Section:** `[storage.retention]` (optional) | **Ownership:** Operator | **Apply:** Restart / activation sweep | **Source:** [src/storage/retention/settings.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/retention/settings.rs)

Omitting `[storage.retention]` keeps capacity-based cleanup only. When present, at least `default` or one camera override is required, and every effective policy must contain at least one rule. Numeric durations do not accept secret references.

```toml
[storage.retention.default]
continuous_days = 1.0
motion_days = 7.0

[storage.retention.default.events]
person = 30.0

[storage.retention.cameras."192.0.2.10"]
continuous_days = 0.0
```

| Field                       | Type                          | Default | Limits and notes                                                                                                                                                                  |
| --------------------------- | ----------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `default`                   | Optional `Lifetimes`          | Absent  | Global rule defaults (`[storage.retention.default]`).                                                                                                                             |
| `cameras`                   | `BTreeMap<String, Lifetimes>` | `{}`    | Max `127` overrides keyed by exact catalog camera ID (camera IP for RTSP; `1..=256` nonblank bytes, no NUL). Omitted fields inherit `default`; explicit `0.0` disables that rule. |
| `Lifetimes.continuous_days` | Optional `f64`                | Absent  | Finite nonnegative days resolving to whole ms `< i64::MAX`.                                                                                                                       |
| `Lifetimes.motion_days`     | Optional `f64`                | Absent  | Same numeric bounds; matches canonical motion events.                                                                                                                             |
| `Lifetimes.events`          | `BTreeMap<String, f64>`       | `{}`    | Exact canonical event `kind` keys (`1..=128` ASCII `[A-Za-z0-9_.-]`). Max `16` effective rules per camera (including `continuous_days` and `motion_days`).                        |

- Stream-specific events match their logical stream; streamless events apply to all streams on that camera. The longest matching rule sets a whole-file deadline from recording end (so unrelated footage in the same file stays until expiry). Protection blocks deletion; shorter settings, removed rules, or edited events never shorten an already committed deadline. Files exceeding bounded evidence-evaluation limits are quarantined from automatic deletion until evaluable.
- Activation runs a bounded catalog sweep (fencing new cleanup admissions until done); expiry deletes eligible whole files even when disk space is free. Diagnose via structured logs `retention_started` and `retention_activation_ready` (correlated by `run_id`), which omit serialized configs and event payloads.

### Named storage volumes

**Type:** `storage::volumes::VolumeConfiguration` (`deny_unknown_fields`) | **Section:** `[storage.named_volumes]` | **Ownership:** Operator (server-managed `[storage.metadata]` & `[storage.metadata_pending]`) | **Apply:** Restart | **Sources:** [src/storage/volumes.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/volumes.rs), [src/storage/volumes/validation.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/volumes/validation.rs), [src/config/metadata.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/metadata.rs)

Fresh installs bootstrap four private roots under `storage/` beside `config.toml`: `media` (active/archive recordings, initial cap from `long_term_max_gb`), `exports`, `images` (thumbnails, initial cap from `event_thumbnail_max_mb`), and `metadata` (catalog + export history). Edit per-volume byte caps directly afterward. Saved volume/placement changes take effect after restart for future placement; existing objects keep their catalog owner. Missing bound roots remain unavailable and are never auto-recreated.

**Section limits:** `volumes` (`0..=32` entries) and `placement` (`0..=256` entries), both default empty. Omitting `named_volumes` in ordinary updates preserves it; an explicit empty section requests removal (subject to ownership/metadata checks). String fields (`id`, `root`, `sources`, `groups`, `source`, `group`, `candidates`) accept secret references; validation uses resolved values while persisted config preserves raw references. Paths are redacted in debug output.

Each `[[storage.named_volumes.volumes]]` entry (`deny_unknown_fields`):

| Field                 | Type                  | Default         | Limits and notes                                                                                                                                                                                                                            |
| --------------------- | --------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`                  | `VolumeId` (`String`) | Required        | Unique; `1..=64` lowercase `[a-z0-9_-]`; `legacy-` prefix reserved.                                                                                                                                                                         |
| `root`                | `PathBuf`             | Required        | Absolute UTF-8 directory (`<= 4,096` B, `<= 64` components); no `..`, control chars, duplicate, or nested/overlapping roots. Windows requires a local drive path on NTFS (rejects ReFS, FAT, network shares, device names, reserved chars). |
| `roles`               | `Vec<VolumeRole>`     | Required        | `1..=5` distinct roles: `"active"`, `"archive"`, `"export"`, `"thumbnail"`, `"metadata"`. Only one bound metadata owner is active; other metadata volumes can be relocation targets.                                                        |
| `state`               | `VolumeState`         | `"enabled"`     | `"enabled"` (admits writes), `"read_only"` / `"draining"` (reads only, no new writes), `"disabled"` (root not opened).                                                                                                                      |
| `priority`            | `u16`                 | `0`             | Lower ranks first under `"priority"` strategy.                                                                                                                                                                                              |
| `capacity_bytes`      | Optional `u64`        | Absent (no cap) | Positive owned-data byte cap (`1..=i64::MAX`).                                                                                                                                                                                              |
| `minimum_free_bytes`  | `u64`                 | `0`             | Minimum filesystem free bytes to retain (`<= i64::MAX`).                                                                                                                                                                                    |
| `critical_free_bytes` | `u64`                 | `0`             | Placement enforces `max(minimum_free_bytes, critical_free_bytes)` (`<= i64::MAX`).                                                                                                                                                          |
| `warning_free_bytes`  | `u64`                 | `0`             | Must be `>= max(minimum_free_bytes, critical_free_bytes)` and `<= i64::MAX`.                                                                                                                                                                |
| `sources`, `groups`   | `Vec<String>`         | `[]`            | Max `256` unique exact selectors each (`1..=256` B, no control or surrounding whitespace). Empty allows all; when populated, matching either permits placement. Must be empty on `metadata` volumes.                                        |

Each `[[storage.named_volumes.placement]]` entry (`deny_unknown_fields`):

| Field             | Type                | Default      | Limits and notes                                                                                                                    |
| ----------------- | ------------------- | ------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| `role`            | `VolumeRole`        | Required     | One of the five volume roles.                                                                                                       |
| `source`, `group` | Optional `String`   | Absent       | At most one may be set (same selector limits as above); both absent = role default. No duplicate `(role, source, group)` selectors. |
| `candidates`      | `Vec<VolumeId>`     | Required     | `1..=8` distinct configured volume IDs supporting `role`.                                                                           |
| `strategy`        | `PlacementStrategy` | `"priority"` | `"priority"` (smaller `priority` first) or `"free_space"` (most usable free bytes after reserve/cap). Ties break by `id`.           |
| `allow_fallback`  | `bool`              | `false`      | `false` restricts selection to `candidates[0]`; `true` ranks all eligible candidates.                                               |

Precedence is `source` > `group` > role default; failed overrides never fall through to broader rules. `metadata` placement requires one global candidate (`source`/`group` absent, `candidates.len() == 1`, `allow_fallback = false`). Placement is a pure proposal and never reserves capacity or moves existing objects.

**Runtime controls and removal** (**Settings → Storage → Named storage volumes**):

- Root probes inspect existing directories without creating them (and do not prove writability). Move controls preview up to `16` objects per page with exact file/byte totals before creating durable move jobs (cancellable under **Move jobs**; duplicate confirmation replies deduplicate by job identity). Operator drain stops new writes, survives restart, lets in-flight writes finish, and does not override saved `draining`/`read_only` state.
- Removing a bound volume requires stopped writes, zero owned/reserved bytes, and completed cleanup receipts (removal checks inspect up to `1,024` pending archive policies); an offline enabled volume must first be operator-drained or disabled and restarted. Remove archive-policy references and restart before removing their destination. Removal preserves catalog history and deletes no files.

**Server-managed metadata binding (`[storage.metadata]` & `[storage.metadata_pending]`):**

- `[storage.metadata]` (`MetadataBinding`, `deny_unknown_fields`): `volume_id`, `catalog_file` (`catalog-<uuid>.db`), `history_file` (`exports-<uuid>.json`, sharing the same canonical UUID), `catalog_id` (UUID), `generation` (`1..=i64::MAX`), `filesystem` and `root_identity` (`1..=256` B each, no control chars). The target volume must exist, be `enabled`, and have role `metadata`; incompatible with `recording_catalog_path`. Startup verifies directory and catalog authority and never fabricates an empty replacement catalog.
- `[storage.metadata_pending]` (`Pending`, `deny_unknown_fields`): `source_digest` (32-byte SHA-256 of current `StorageToml`) and `target` (`MetadataBinding`). Created via **Preview metadata move** / **Confirm metadata move** against a saved disabled `metadata`-only volume; ordinary settings cannot edit `[storage.metadata]` or alter storage settings while a handoff is pending. On restart, KeepPeek rechecks space/caps/reserves, validates export history (`<= 8 MiB`, unique owned jobs), copies and verifies catalog + history before committing atomically (and switching volume states). Interrupted transfers retain source files and pending state for retry; returning to an old volume uses a new generation.

### Fourth-tier local archive

**Type:** `storage::archive::settings::Settings` (`deny_unknown_fields`) | **Section:** `[storage.archive]` (optional) | **Ownership:** Operator | **Apply:** Restart / worker probe | **Source:** [src/storage/archive/settings.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/archive/settings.rs)

Configures one verified local replica target (distinct from named-volume `archive` placement, which relocates the authoritative copy). Local recording and live viewing never block on archive availability. Cloud provider fields are unsupported and rejected.

```toml
[storage.archive]
target_id = "local-archive"
root = "{secret:ARCHIVE_ROOT}"
enabled = false
automatic = false
source_ids = []
minimum_age_secs = 0
evict_after_archive = false
maximum_object_bytes = 4294967296
maximum_pending_count = 256
maximum_pending_bytes = 68719476736
operation_timeout_secs = 120
maximum_attempts = 3
```

| Field                    | Type           | Default                | Limits and notes                                                                                                                                                                                       |
| ------------------------ | -------------- | ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `target_id`              | `String`       | Required               | Stable `VolumeId` (`1..=64` lowercase `[a-z0-9_-]`; `legacy-` reserved). Keep paired with its catalog root binding.                                                                                    |
| `root`                   | `PathBuf`      | Required               | Absolute local path (`<= 4,096` B, `<= 64` components, named-volume path safety rules). Must already exist; cannot overlap named-volume or legacy media/thumbnail/metadata roots. Accepts secret refs. |
| `enabled`                | `bool`         | `false`                | Enables archive worker (does not auto-create or bind `root`; test destination after startup).                                                                                                          |
| `automatic`              | `bool`         | `false`                | Auto-admits finalized recordings when enabled.                                                                                                                                                         |
| `source_ids`             | `Vec<String>`  | `[]`                   | Auto-selection filter; empty = all sources. Max `127` distinct nonblank IDs (`<= 256` B, no control or surrounding whitespace).                                                                        |
| `minimum_age_secs`       | `u64`          | `0`                    | Minimum finalized recording age for auto-selection (`0..=315360000`, i.e., 10 years).                                                                                                                  |
| `evict_after_archive`    | `bool`         | `false`                | Permits policy-driven primary eviction after verification (holds, leases, and durable indexes still enforced).                                                                                         |
| `retention_days`         | Optional `u32` | Absent (indefinite)    | `1..=4294967295` days after verification before eligible for Admin-confirmed batch deletion (requires surviving primary verification).                                                                 |
| `maximum_object_bytes`   | `u64`          | `4294967296` (4 GiB)   | `1..=68719476736` (64 GiB) and `<= maximum_pending_bytes`.                                                                                                                                             |
| `maximum_pending_count`  | `u32`          | `256`                  | `1..=4096` retained pending jobs.                                                                                                                                                                      |
| `maximum_pending_bytes`  | `u64`          | `68719476736` (64 GiB) | Positive and `<= i64::MAX`.                                                                                                                                                                            |
| `operation_timeout_secs` | `u64`          | `120`                  | `1..=3600` seconds per archive operation.                                                                                                                                                              |
| `maximum_attempts`       | `u32`          | `3`                    | `1..=10` attempts per durable job.                                                                                                                                                                     |

String settings support secret references (preserved on rewrites/imports); older runtime settings updates omitting `archive` preserve it. Job journals, manifests, root bindings, and integrity metadata live in catalog state. Available local archive files play directly on the standard timeline without full-file restore; unavailable or corrupt files return explicit errors.

## Battery wake

**Type:** `BatteryWakeConfig` | **Section:** `[battery_wake]` | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

Reolink battery-camera wake middleman service (not Wake-on-LAN or privacy scheduling).

| Field              | Type                | Default            | Limits and notes                            |
| ------------------ | ------------------- | ------------------ | ------------------------------------------- |
| `enabled`          | `bool`              | `false`            | Enables the wake service.                   |
| `bind`             | Optional `Ipv4Addr` | Absent (automatic) | Local IPv4 bind address.                    |
| `middleman_port`   | `u16`               | `9999`             | Nonzero; must differ from `register_port`.  |
| `register_port`    | `u16`               | `58200`            | Nonzero; must differ from `middleman_port`. |
| `heartbeat_secs`   | `u64`               | `20`               | Nonzero heartbeat interval, in seconds.     |
| `stale_after_secs` | `u64`               | `80`               | Must be `>= heartbeat_secs`.                |

## Logging

**Type:** `LoggingConfig` | **Section:** `[logging]` | **Ownership:** Operator | **Apply:** Restart | **Sources:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs), [src/logging.rs](https://github.com/xnorpx/keeppeek/blob/main/src/logging.rs)

| Field     | Type                    | Default  | Limits and notes                                                        |
| --------- | ----------------------- | -------- | ----------------------------------------------------------------------- |
| `service` | `ServiceLogDestination` | `"file"` | `"file"` or `"event_log"` (Windows Event Log for Windows service mode). |

**Diagnostic log filter (`log-filter` sibling file):** Live tracing filter directives are stored in a sibling file named `log-filter` beside `config.toml`, not inside TOML. Startup precedence is `log-filter` > `RUST_LOG` > built-in `info,keeppeek=debug` (invalid values report an error and fall back). `log-filter` is outside the two-file configuration ZIP; back it up separately with the config directory if needed.

## Bug reporting service

**Type:** `reporting::Settings` (`deny_unknown_fields`) | **Section:** `[reporting]` (optional) | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/config/reporting.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/reporting.rs)

Omit `[reporting]` to keep bug reports local-only. When configured, enables Admin-only endpoints `GET /reporting/status`, `POST /reporting/grants`, and `POST /reporting/audit` (subject to secure remote-access classification).

| Field            | Type              | Default  | Limits and notes                                                                                                                                                                                                                         |
| ---------------- | ----------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `origin`         | `String`          | Required | Canonical HTTPS origin (`<= 512` B, optional port; no credentials, path, query, or fragment).                                                                                                                                            |
| `credential`     | Optional `String` | Absent   | Complete `{secret:KEY}` only (`<= 128`-byte key name, no `\|url`). Syntax-checked at load without resolving; resolved only when forwarding `POST {origin}/v1/reports` and registered (along with issued grant tokens) for log redaction. |
| `retention_days` | `u8`              | `7`      | `1..=30` days requested for private report evidence.                                                                                                                                                                                     |

## Usage reporting

**Type:** `usage_reporting::UsageReportingConfig` (`deny_unknown_fields`) | **Section:** `[usage_reporting]` (optional) | **Ownership:** Operator | **Apply:** Live (UI/API) / Restart (file) | **Sources:** [src/config/usage_reporting.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/usage_reporting.rs), [docs/usage-reporting.md](https://github.com/xnorpx/keeppeek/blob/main/docs/usage-reporting.md)

All categories default to `false`; fresh installs and upgrades create no telemetry ID, run no worker, and make zero network requests until an Administrator opts in.

| Field                           | Type              | Default | Limits and notes                                                                                     |
| ------------------------------- | ----------------- | ------- | ---------------------------------------------------------------------------------------------------- |
| `basic`                         | `bool`            | `false` | Enables basic system metrics; required before enabling any other category.                           |
| `camera_compatibility`          | `bool`            | `false` | Normalized camera compatibility counts; requires `basic = true`.                                     |
| `feature_adoption`              | `bool`            | `false` | Coarse recording-mode, storage/retention bucket, and integration summaries; requires `basic = true`. |
| `consented_disclosure_revision` | `u32`             | `0`     | Accepted disclosure revision (`1` required when `basic = true`; must be `<= 1`).                     |
| `receiver_origin`               | Optional `String` | Absent  | Canonical HTTPS origin (`1..=512` B; no credentials, path, query, fragment, or control chars).       |

Telemetry installation UUIDs, proof tokens, send schedules, and pending deletions (`<= 16` entries) live in `.keeppeek-usage-reporting-state.json` beside `config.toml` only after opt-in and are excluded from `config.toml` backups and `StateStore` so restores never clone proof tokens across hosts.

## Cloud notifications

**Type:** `cloud_notifications::Settings` (`deny_unknown_fields`) | **Section:** `[cloud_notifications]` (optional) | **Ownership:** Operator (server-incremented `revision`) | **Apply:** Live (UI/API) / Restart (file) | **Source:** [src/config/cloud_notifications.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/cloud_notifications.rs)

Configures bounded Keeppeek Service mobile push signaling behind capability `keeppeek.cloud-notifications.v1`.

| Field                    | Type              | Default | Limits and notes                                                                                                                                                                          |
| ------------------------ | ----------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `revision`               | `u64`             | `0`     | Optimistic concurrency revision incremented on each mutation.                                                                                                                             |
| `enabled`                | `bool`            | `false` | Enables Keeppeek Service signal emission when paired.                                                                                                                                     |
| `image_previews_enabled` | `bool`            | `false` | Authoritative NVR gate allowing paired mobile clients to resolve still-image previews; requires `enabled = true` and complete pairing.                                                    |
| `service_origin`         | Optional `String` | Absent  | Canonical HTTPS origin (`1..=512` B; no credentials, path, query, or fragment).                                                                                                           |
| `nvr_id`                 | Optional `String` | Absent  | Service-assigned 16-byte opaque NVR UUID/hex ID.                                                                                                                                          |
| `signing_key_version`    | Optional `u32`    | Absent  | Nonzero (`>= 1`) Ed25519 signing key version.                                                                                                                                             |
| `signing_key`            | Optional `String` | Absent  | Complete `{secret:KEY}` only (`<= 128`-byte key name, no `\|url`) holding a 32-byte Ed25519 seed (base64, URL-safe base64, or 64-char hex). Never resolved during config parse/serialize. |

The four pairing fields (`service_origin`, `nvr_id`, `signing_key_version`, `signing_key`) must be **all absent** (unpaired) or **all present** (paired). `service_origin`, `nvr_id`, and signing keys are never exposed in settings responses, status, logs, or Prometheus metrics. Durable runtime state (aliases, newest-only pending/claimed intents, rolling `120`/hour and `500`/day limits, `3`/hour test limits, monotonic clock floor) lives in `cloud-notifications.db` under `storage.long_term_path`.

## Operational events

**Type:** `OperationalEventsConfig` & `OperationalEventOverride` | **Sections:** `[operational_events]` & `[operational_events.cameras."<camera-id-or-ip>"]` | **Ownership:** Operator | **Apply:** Restart | **Source:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

| Field                    | Type                                         | Default | Limits and notes                                                                                                                                                                                                                                                                                             |
| ------------------------ | -------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `warning_hold_down_secs` | `u64`                                        | `15`    | Delay before recording a warning; must be `<= outage_hold_down_secs`.                                                                                                                                                                                                                                        |
| `outage_hold_down_secs`  | `u64`                                        | `60`    | Delay before recording an outage (`<= 86400`).                                                                                                                                                                                                                                                               |
| `recovery_debounce_secs` | `u64`                                        | `10`    | Stable-recovery debounce (`<= 86400`).                                                                                                                                                                                                                                                                       |
| `record_short_flaps`     | `bool`                                       | `false` | Retain supported brief-flap transitions.                                                                                                                                                                                                                                                                     |
| `cameras`                | `BTreeMap<String, OperationalEventOverride>` | `{}`    | Keys are `1..=256` nonblank bytes (stable camera ID takes precedence over IP). Each override accepts optional `warning_hold_down_secs`, `outage_hold_down_secs`, `recovery_debounce_secs`, and `record_short_flaps` (inheriting global defaults when absent; effective values must satisfy the same bounds). |

## Privacy schedules

**Type:** `PrivacyConfig`, `PrivacySchedule`, `PrivacyWindow`, `PrivacyOverride` | **Sections:** `[privacy.default]` & `[privacy.cameras."<camera-id-or-ip>"]` | **Ownership:** Operator | **Apply:** Live (plan/apply) / Restart (file) | **Sources:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs), [src/privacy.rs](https://github.com/xnorpx/keeppeek/blob/main/src/privacy.rs)

`[privacy.default]` applies to cameras without a per-camera entry. `[privacy.cameras]` supports at most `127` entries (keys `1..=256` nonblank bytes; stable camera ID preferred, IP accepted for migration). Invalid schedules fail validation before media starts.

| Field                   | Type                       | Default  | Limits and notes                                                                                                                                                                                                                                                                             |
| ----------------------- | -------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`               | `bool`                     | `true`   | `false` retains the schedule while disabling enforcement.                                                                                                                                                                                                                                    |
| `timezone`              | `String`                   | Required | Valid IANA timezone (e.g., `"America/New_York"`).                                                                                                                                                                                                                                            |
| `windows`               | `Vec<PrivacyWindow>`       | `[]`     | Max `64` half-open weekly windows. Each window: `weekdays` (nonempty ISO `1..=7`, Mon=1..Sun=7), `start` and `end` (`"HH:MM"`, must differ; `end < start` = overnight). Repeated DST local hours are protected.                                                                              |
| `temporary_override`    | Optional `PrivacyOverride` | Absent   | Persisted override suspending windows: `actor` and `reason` (`1..=256` nonblank B each), RFC3339 `accepted_at` and `expires_at` (`expires_at > accepted_at`, span `<= 24` hours). Control-channel requests overwrite `actor` and `accepted_at` with the authenticated Admin and server time. |
| `keep_camera_connected` | `bool`                     | `true`   | Keeps camera ingress warm while blocking media delivery.                                                                                                                                                                                                                                     |

## Activity analytics

**Type:** `AnalyticsConfig` | **Section:** `[analytics]` | **Ownership:** Operator | **Apply:** Restart | **Sources:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs), [src/storage/analytics/mod.rs](https://github.com/xnorpx/keeppeek/blob/main/src/storage/analytics/mod.rs)

Controls site defaults and derived rollup bounds for bounded event activity summaries. Derived rollup tables live in the recording catalog DB and can be rebuilt from canonical events without affecting recordings.

| Field             | Type        | Default    | Limits and notes                                                                 |
| ----------------- | ----------- | ---------- | -------------------------------------------------------------------------------- |
| `enabled`         | `bool`      | `true`     | Enables activity summary queries and rollup maintenance.                         |
| `timezone`        | `String`    | `"UTC"`    | Valid IANA timezone for default civil-time buckets and comparisons.              |
| `week_start`      | `WeekStart` | `"monday"` | `"monday"` or `"sunday"`.                                                        |
| `max_rollup_rows` | `u64`       | `500000`   | `1000..=5000000`. Exceeding quota degrades analytics without blocking recording. |

## MQTT event forwarding

**Type:** `EventForwarderConfig` (`mqtt: MqttForwarderConfig`) | **Section:** `[event_forwarder.mqtt]` | **Ownership:** Operator (server-owned `revision`) | **Apply:** Live (UI/API) / Restart (file) | **Source:** [src/event_forwarder/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/event_forwarder/config.rs)

| Field           | Type               | Default                   | Limits and notes                                                                                             |
| --------------- | ------------------ | ------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `revision`      | `u64`              | `1`                       | Server-managed settings revision.                                                                            |
| `enabled`       | `bool`             | `false`                   | Enables MQTT event/health forwarding.                                                                        |
| `broker_url`    | `String`           | `"mqtt://127.0.0.1:1883"` | `mqtt` or `mqtts` URL with host; no user-info, non-root path, query, or fragment.                            |
| `client_id`     | `String`           | `"keeppeek"`              | `1..=128` B; no NUL, `/`, `+`, or `#`.                                                                       |
| `instance_id`   | `String`           | `"home-nvr"`              | `1..=128` B; no NUL, `/`, `+`, or `#`.                                                                       |
| `forwarder_id`  | `String`           | `"mqtt"`                  | `1..=128` B; no NUL, `/`, `+`, or `#`.                                                                       |
| `topic_prefix`  | `String`           | `"keeppeek"`              | `1..=512` B; no leading/trailing `/`, NUL, `+`, or `#` (interior `/` allowed).                               |
| `username`      | Optional `String`  | Absent                    | Broker username (accepts secret refs).                                                                       |
| `password`      | Optional `String`  | Absent                    | Requires `username`; prefer `{secret:MQTT_PASSWORD}` (UI writer stores `1..=4,096` B under `MQTT_PASSWORD`). |
| `tls_ca_path`   | Optional `PathBuf` | Absent (system trust)     | Custom CA bundle; valid only with `mqtts`.                                                                   |
| `qos`           | `u8`               | `1`                       | `0`, `1`, or `2`.                                                                                            |
| `retain_events` | `bool`             | `false`                   | Retain event publications.                                                                                   |
| `retain_health` | `bool`             | `true`                    | Retain health publications.                                                                                  |
| `outbox_max_mb` | `u64`              | `64`                      | `1..=65536` MiB in-memory outbox budget (does not enable disk persistence; resets on restart).               |
| `retry_min_ms`  | `u64`              | `250`                     | Nonzero; `<= retry_max_ms`.                                                                                  |
| `retry_max_ms`  | `u64`              | `30000`                   | `<= 3600000` ms (1 hour).                                                                                    |

Outbox and deduplication state live in memory and reset on restart; the retired `mqtt-forwarder.db` is deleted at startup without replay. See [Notifications and integrations](./notifications-and-integrations.md).

## MCP server

**Type:** `McpConfig` | **Section:** `[mcp]` | **Ownership:** Operator | **Apply:** Restart | **Sources:** [src/config/mcp.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config/mcp.rs), [src/server/mcp.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/mcp.rs)

The embedded Streamable HTTP endpoint (`POST /mcp`), configuration writes, on-camera hardware writes, single-frame camera snapshots, and live camera protocol debugging default to disabled. The canonical writer omits `[mcp]` when all fields stay at defaults.

| Field                           | Type          | Default     | Limits and notes                                                                                                                 |
| ------------------------------- | ------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`                       | `bool`        | `false`     | Enable `POST /mcp` (and loopback `/mcp/secret-entry`); returns `404` when `false`.                                               |
| `allow_writes`                  | `bool`        | `false`     | Enable opt-in Administrator configuration mutation tools unless overridden by a read-only client.                                |
| `allow_snapshots`               | `bool`        | `false`     | Enable opt-in privacy-gated single-frame snapshots via `get_camera_snapshot` (camera hardware masks only).                       |
| `allow_camera_device_writes`    | `bool`        | `false`     | Enable opt-in live on-camera hardware writes via `update_camera_device_settings` (`dry_run: false`) when writable.               |
| `allow_camera_debugging`        | `bool`        | `false`     | Enable opt-in Administrator live camera capture, PCAPNG/bitstream inspection, and repro export via `camera_debugging`.           |
| `debug_camera_ids`              | `Vec<String>` | `[]`        | Optional camera ID allowlist for `camera_debugging` (empty = all cameras; max `256` non-empty trimmed ASCII IDs `<= 128` bytes). |
| `max_debug_capture_bytes`       | `usize`       | `67108864`  | `1048576..=268435456` bytes (1 MiB..=256 MiB; default 64 MiB) maximum size per `camera_debugging` capture.                       |
| `max_debug_storage_bytes`       | `u64`         | `536870912` | `16777216..=4294967296` bytes (16 MiB..=4 GiB; default 512 MiB) total disk budget under `<config-dir>/camera-debug/` (24h TTL).  |
| `max_concurrent_requests`       | `usize`       | `4`         | `1..=32` concurrent in-flight MCP requests.                                                                                      |
| `max_response_bytes`            | `usize`       | `65536`     | `4096..=262144` bytes serialized JSON-RPC payload (`get_camera_snapshot` uses `max_snapshot_bytes`).                             |
| `request_timeout_seconds`       | `u64`         | `15`        | `1..=60` seconds server deadline per request.                                                                                    |
| `max_snapshot_bytes`            | `usize`       | `262144`    | `16384..=1048576` bytes encoded JPEG snapshot size (response adds base64 and metadata envelope overhead).                        |
| `max_snapshot_dimension`        | `u32`         | `1280`      | `160..=1920` px maximum snapshot width or height.                                                                                |
| `snapshot_min_interval_seconds` | `u64`         | `5`         | `1..=300` seconds minimum interval between snapshots per camera.                                                                 |

> **Security warning:** When both `[mcp].enabled = true` and `[mcp].allow_camera_debugging = true`, Administrator MCP clients have **full raw access to live video bitstreams, audio streams, decrypted network captures (PCAPNG), and camera credentials** for `debug_camera_ids` (or all cameras when empty). KeepPeek emits a startup warning log and a `ServerHealthResponse` warning issue while enabled; keep `allow_camera_debugging = false` outside active debugging sessions.

Out-of-range values fail startup and configuration-planning validation. `POST /mcp` enforces `[access]` policy, Bearer header authentication, `localhost`/IP `Host` checks for unauthenticated local requests, `[direct_card].allowed_origins` plus mandatory Bearer auth for browser `Origin` headers, serialization-layer secret redaction (with the explicit `allow_camera_debugging` exception limited to `camera_debugging`), and fail-closed camera privacy schedules for snapshots. See [MCP server and LLM configuration](./mcp.md).

## Credential records

**Type:** `PersistedAccessCatalog` | **Section:** `[access_credentials]` | **Ownership:** Server-managed (use Admin credential controls) | **Apply:** Live (UI/API) | **Source:** [src/access.rs](https://github.com/xnorpx/keeppeek/blob/main/src/access.rs)

Top-level fields: `version: u32` (must be `1`) and `credentials: Vec<StoredCredential>` (`[[access_credentials.credentials]]`, max `128` records with unique IDs).

| Field                    | Type                    | Default                | Limits and notes                                                                                                                |
| ------------------------ | ----------------------- | ---------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `id`                     | `Uuid`                  | Required               | Unique stable credential identity.                                                                                              |
| `name`                   | `String`                | Required               | `1..=64` printable bytes after trimming (no control chars).                                                                     |
| `description`            | Optional `String`       | Absent                 | Max `256` printable bytes; empty normalizes to absent.                                                                          |
| `role`                   | `AccessRole`            | Required               | `"administrator"` or `"user"`.                                                                                                  |
| `camera_access`          | Optional `CameraAccess` | Absent (unrestricted)  | Per-user camera/group policy below. New credentials explicitly initialize unrestricted; Administrators are always unrestricted. |
| `verifier`               | `AccessKeyFingerprint`  | Required               | 32-byte SHA-256 fingerprint array (not raw key). Treat as sensitive auth material.                                              |
| `created_at_ms`          | `i64`                   | Required               | Creation time, Unix ms.                                                                                                         |
| `rotated_at_ms`          | Optional `i64`          | Absent                 | Last rotation time, Unix ms.                                                                                                    |
| `last_used_at_ms`        | Optional `i64`          | Absent                 | Accepted on load for compatibility, cleared on load and omitted on write (activity is runtime-only).                            |
| `expires_at_ms`          | Optional `i64`          | Absent                 | Expiry time, Unix ms; expired credentials are denied.                                                                           |
| `disabled`               | `bool`                  | Required               | `true` denies authentication.                                                                                                   |
| `revoked_at_ms`          | Optional `i64`          | Absent                 | Permanent revocation time, Unix ms.                                                                                             |
| `revision`               | `u64`                   | Required               | Nonzero monotonic revision; changes invalidate stale sessions.                                                                  |
| `legacy`                 | `bool`                  | Required               | Compatibility initial-credential marker.                                                                                        |
| `initial_secret_pending` | `bool`                  | Required               | Initial-key retrieval still pending.                                                                                            |
| `endpoint_kind`          | `EndpointKind`          | `"standard"` (omitted) | `"standard"` or `"paired_mobile"`.                                                                                              |
| `paired_nvr_id`          | Optional `String`       | Absent                 | Required 16-byte NVR UUID/hex when `endpoint_kind = "paired_mobile"`; forbidden when `"standard"`.                              |

**Nested `CameraAccess` (`deny_unknown_fields`):**

| Field         | Type          | Default  | Limits and notes                                                                                                                               |
| ------------- | ------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `all_cameras` | `bool`        | Required | Grants all current and future cameras/groups; requires both `group_ids` and `camera_ids` empty.                                                |
| `group_ids`   | `Vec<String>` | `[]`     | Camera namespace names (e.g., `"outdoor"` for `[outdoor.front_door]`). Max `128` unique nonblank entries (`<= 256` UTF-8 B, no control chars). |
| `camera_ids`  | `Vec<String>` | Required | Server-advertised camera IDs (not display names). Max `128` unique nonblank entries (`<= 256` UTF-8 B, no control chars).                      |

- With `all_cameras = false`, effective access is `group_ids ∪ camera_ids` (both empty grants zero cameras). Existing restrictions are never broadened on upgrade. No secret substitution occurs inside grant records.
- Saving grants increments `revision`, rejects stale writes, and invalidates sessions; runtime group-membership changes close affected group-scoped sessions and cancel queued work. `available_group_ids` (max `128`), `AccessCatalog.audit`, sessions, and last-use timestamps are runtime state, not persisted fields. Legacy `access.toml` is a one-time migration input only. Grid audiences never grant camera permissions, custom roles are unsupported, and trusted-local clients follow `[access].local_networks`.

## Saved event searches

**Type:** `StoredRegistry` & `StoredSearch` (`deny_unknown_fields`) | **Section:** `[saved_event_searches]` | **Ownership:** Server-managed (`keeppeek.event-search.advanced.v1` CAS commands) | **Apply:** Live (UI/API) | **Source:** [src/server/saved_searches.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/saved_searches.rs)

Top-level fields: `schema_version: u32` (must be `1`) and `owners: BTreeMap<String, Vec<StoredSearch>>` (keyed by principal or local workspace ID; max `64` per owner, `1,024` total).

Each `StoredSearch` under `[saved_event_searches.owners."<owner-id>"]`:

| Field                | Type     | Default  | Limits and notes                                                                   |
| -------------------- | -------- | -------- | ---------------------------------------------------------------------------------- |
| `id`                 | `String` | Required | Unique UUID per owner (`1..=128` B).                                               |
| `name`               | `String` | Required | Nonblank display name (`1..=128` B, no control chars).                             |
| `revision`           | `u64`    | Required | Positive monotonic CAS revision.                                                   |
| `expression_version` | `u32`    | Required | Positive AST version (`1` editable; newer versions remain readable and read-only). |
| `expression_json`    | `String` | Required | Serialized AST draft (`<= 32 KiB`); contains no credentials or secret refs.        |
| `updated_at_ms`      | `i64`    | Required | UTC update timestamp, Unix ms.                                                     |

Config-only backups validate and include `[saved_event_searches]` without copying the recording catalog DB or semantic embedding vectors.

## Peek layout registry

**Type:** `StoredRegistry` (`deny_unknown_fields`) | **Section:** `[peek_layouts]` | **Ownership:** Server-managed (`keeppeek.peek-layouts.v1` CAS commands) | **Apply:** Live (UI/API) | **Sources:** [src/server/peek_layouts.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/peek_layouts.rs), [src/server/peek_layouts/display.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/peek_layouts/display.rs)

Stores saved [live-wall](./live-wall.md) dashboards, per-dashboard display settings, and per-user active selection. Browser permissions and active media/wake-lock handles remain runtime state. All structs below reject unknown fields; no field resolves secret references.

| Type / Section                      | Field            | Type                                   | Default  | Limits and notes                                                                                                                                                                                   |
| ----------------------------------- | ---------------- | -------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `StoredRegistry` (`[peek_layouts]`) | `schema_version` | `u32`                                  | Required | Must be `1`.                                                                                                                                                                                       |
| `StoredRegistry`                    | `revision`       | `u64`                                  | Required | Nonzero registry CAS revision; initial value `1`.                                                                                                                                                  |
| `StoredRegistry`                    | `shared_layouts` | `Vec<Layout>`                          | Required | Nonempty server-owned layouts (initial default: `id = "default"`, name `"All cameras"`, `activity_focus = true`, up to `64` cameras).                                                              |
| `StoredRegistry`                    | `users`          | `BTreeMap<String, StoredUserRegistry>` | Required | Keyed by principal ID. Each entry has `active_layout_id: String` (missing values auto-repaired) and `layouts: Vec<Layout>` (legacy private layouts migrated to restricted shared layouts on open). |

**`Layout` (`shared_layouts[]` or legacy `users.<id>.layouts[]`):**

| Field            | Type                       | Default                                    | Limits and notes                                                                                                                                                                                                                                       |
| ---------------- | -------------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `id`             | `String`                   | Required                                   | Unique in registry view; nonblank `<= 128` chars.                                                                                                                                                                                                      |
| `name`           | `String`                   | Required                                   | Nonblank `<= 80` chars.                                                                                                                                                                                                                                |
| `scope`          | `LayoutScope`              | Required                                   | `"shared"` (canonical) or `"private"` (legacy migration).                                                                                                                                                                                              |
| `owner_id`       | `String`                   | Required                                   | `"server"` for shared layouts; principal ID for legacy private records.                                                                                                                                                                                |
| `audience`       | `LayoutAudience`           | `{ everyone = true, credential_ids = [] }` | `everyone: bool` (when `true`, `credential_ids` must be empty); `credential_ids: Vec<String>` (max `128` unique canonical UUIDs verified against known identities; `everyone = false` + empty hides from Users while Admins retain management access). |
| `activity_focus` | `bool`                     | Required                                   | Enables activity-driven focus on this dashboard.                                                                                                                                                                                                       |
| `display`        | Optional `DisplaySettings` | Absent (uses defaults below)               | Omission by older clients preserves saved settings; only Administrators may modify.                                                                                                                                                                    |
| `tiles`          | `Vec<LayoutTile>`          | Required                                   | Max `64` tiles; no duplicate `camera_id` or grid overlaps. Each tile: `camera_id` (stable ID), `column`/`row` (1-based `u32`), `column_span`/`row_span` (positive `u32` fitting the `12×12` grid), `pinned: bool`.                                     |

**`DisplaySettings` (nested `display` table; rejects unknown fields and positional arrays):**

| Field              | Type            | Default     | Limits and notes                                                                    |
| ------------------ | --------------- | ----------- | ----------------------------------------------------------------------------------- |
| `version`          | `u32`           | `1`         | Required when object is present; only `1` supported.                                |
| `tile_shape`       | `TileShape`     | `"16:9"`    | `"16:9"`, `"4:3"`, or `"native"`.                                                   |
| `media_fit`        | `MediaFit`      | `"contain"` | `"contain"` or `"cover"` (never stretch).                                           |
| `streaming_mode`   | `StreamingMode` | `"smart"`   | `"smart"` or `"continuous"`.                                                        |
| `stream_limit`     | `u32`           | `12`        | Whole number `1..=12` (also bounded by browser capacity).                           |
| `keep_awake`       | `bool`          | `false`     | Saved wake-lock intent (browser gesture/visibility/policy still apply).             |
| `gap_px`           | `u32`           | `10`        | Whole pixels `0..=24`.                                                              |
| `corner_radius_px` | `u32`           | `10`        | Whole pixels `0..=24` (`0` = square corners). Preset names are derived, not stored. |

**Registry boundaries:** `256 KiB` serialized ceiling; max `32` layouts per validated view. Stored tile camera references survive temporary camera removal; additions validate against known cameras. The `All cameras` dashboard keeps immutable coordinates/ID while its `display` object is editable and preserved across inventory sync. Opening the store migrates legacy `peek-layouts.json` (not an active write target). Client exchange `LayoutRegistry` (`schema_version`, `active_layout_id`, `layouts`) is **not** the on-disk `StoredRegistry`. Grid audiences control dashboard visibility only—never camera authorization; the server intersects User tile views with camera grants without mutating the canonical layout.

## Camera templates

**Type:** `StoredTemplateDocument` (`deny_unknown_fields`) | **Section:** `[configuration_templates]` | **Ownership:** Server-managed | **Apply:** Live (UI/API) | **Source:** [src/server/configuration.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/configuration.rs)

Top-level fields: `document_version: u32` (must be `1`) and `templates: Vec<StoredTemplate>` (`[[configuration_templates.templates]]`, max `64`; `16 KiB` serialized document limit; absent section = empty). Legacy `configuration-templates.json` is imported on startup and removed.

Each `StoredTemplate` (`deny_unknown_fields`): `template_id` (unique `1..=64` ASCII `[A-Za-z0-9_-]`), `version: u64`, `name` (case-insensitive unique, nonblank `<= 128` B, no control chars), `description` (default `""`, omitted when empty, `<= 1,024` B, no control chars), `values: StoredTemplateValues` (`>= 1` field required), `created_at_ms: i64`, `updated_at_ms: i64`.

**`StoredTemplateValues` (all fields optional, omitted when unset, `deny_unknown_fields`):**

| Field                               | Type                            | Default | Limits and notes                                                          |
| ----------------------------------- | ------------------------------- | ------- | ------------------------------------------------------------------------- |
| `username_secret_reference`         | Optional `String`               | Absent  | Complete secret reference only (no inline credentials).                   |
| `password_secret_reference`         | Optional `String`               | Absent  | Complete secret reference only (no inline credentials).                   |
| `onvif_port`, `http_port`           | Optional `u16`                  | Absent  | `1..=65535`.                                                              |
| `backend`                           | Optional `CameraBackend`        | Absent  | `"auto"`, `"retina"`, or `"reo-proto"`.                                   |
| `transport`                         | Optional `CameraTransport`      | Absent  | `"tcp"` or `"udp"`.                                                       |
| `record_generic_motion_events`      | Optional `bool`                 | Absent  | Generic motion-event opt-in.                                              |
| `recording_mode`                    | Optional `CameraRecordingMode`  | Absent  | `"off"`, `"sub"`, `"main"`, `"both"`, `"event-boost"`, or `"event-only"`. |
| `event_recording_duration_secs`     | Optional `u32`                  | Absent  | `1..=3600` seconds.                                                       |
| `event_pre_recording_duration_secs` | Optional `u32`                  | Absent  | `0..=30` seconds.                                                         |
| `event_recording_stream`            | Optional `EventRecordingStream` | Absent  | `"main"` or `"sub"`.                                                      |

## Notification rules

**Type:** `NotificationConfiguration` (`deny_unknown_fields`) | **Section:** `[notifications]` | **Ownership:** Server-managed | **Apply:** Live (UI/API) | **Sources:** [src/notifications/store.rs](https://github.com/xnorpx/keeppeek/blob/main/src/notifications/store.rs), [src/notifications/model.rs](https://github.com/xnorpx/keeppeek/blob/main/src/notifications/model.rs), [src/notifications.rs](https://github.com/xnorpx/keeppeek/blob/main/src/notifications.rs)

Top-level field: `rules: Vec<PersistedRuleRecord>` (`[[notifications.rules]]`, default `[]`, max `128` rules with unique IDs). Each `PersistedRuleRecord` (`deny_unknown_fields`) stores `id`, `owner_id`, `active: Option<Rule>` (absent before first activation), `active_revision: u64`, `draft: Rule` (max `64 KiB` JSON), `draft_revision: u64`, `created_at_ms: i64`, and `updated_at_ms: i64`. Rule IDs, owners, and revisions must match their containing record; runtime `last_match_at_ms` and `last_delivery_at_ms` are not persisted.

**`Rule` (under `draft` and optional `active`):**

| Field             | Type                      | Default  | Limits and notes                                                                                                                                                     |
| ----------------- | ------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`, `owner_id`  | `String`                  | Required | `1..=128` ASCII `[A-Za-z0-9._-]`.                                                                                                                                    |
| `name`            | `String`                  | Required | Nonblank `<= 128` chars.                                                                                                                                             |
| `enabled`         | `bool`                    | Required | `false` disables matching.                                                                                                                                           |
| `revision`        | `u64`                     | Required | Matches containing `draft_revision` or `active_revision`.                                                                                                            |
| `triggers`        | `Vec<Trigger>`            | Required | Nonempty unique list of `"event_created"`, `"event_updated"`, `"event_ended"`, `"outage_started"`, `"recovery"`, `"storage_health"`, `"recording_health"`, `"test"`. |
| `filter`          | `Filter`                  | Empty    | Nested filter predicates below.                                                                                                                                      |
| `schedule`        | `Schedule`                | Required | Timezone, active windows, and optional quiet hours below.                                                                                                            |
| `cooldowns`       | `Vec<Cooldown>`           | `[]`     | Unique scopes below.                                                                                                                                                 |
| `rate_limits`     | `Vec<RateLimit>`          | `[]`     | Unique scopes below.                                                                                                                                                 |
| `critical_bypass` | Optional `CriticalBypass` | Absent   | Bounded critical-severity quiet-hours bypass.                                                                                                                        |
| `enrichment`      | `EnrichmentPolicy`        | Required | Staged attachment enrichment limits below.                                                                                                                           |
| `actions`         | `Vec<Action>`             | Required | `1..=8` delivery actions below.                                                                                                                                      |
| `failure`         | `FailurePolicy`           | Required | Retry and outbox expiry policy below.                                                                                                                                |

### Filters and schedules

**`Filter`:**

| Field                                             | Type            | Default | Limits and notes                                          |
| ------------------------------------------------- | --------------- | ------- | --------------------------------------------------------- |
| `source_ids`, `group_ids`, `event_kinds`, `zones` | `Vec<String>`   | `[]`    | Max `128` nonblank entries each; empty does not restrict. |
| `minimum_confidence`                              | Optional `f64`  | Absent  | Finite value in `0.0..=1.0`.                              |
| `attachment_required`                             | Optional `bool` | Absent  | Require or exclude an available non-private attachment.   |
| `minimum_duration_ms`                             | Optional `u64`  | Absent  | Minimum matching event duration in ms.                    |
| `severities`                                      | `Vec<Severity>` | `[]`    | `"info"`, `"warning"`, `"critical"`.                      |
| `reviewed`, `bookmarked`                          | Optional `bool` | Absent  | Match review or bookmark state.                           |

Filter fields describe matching inputs and do not imply every camera/producer supplies them.

**`Schedule`, `QuietHours`, and `WeeklyWindow`:**

| Field                                     | Type                  | Default  | Limits and notes                                                                                                              |
| ----------------------------------------- | --------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `schedule.timezone`                       | `String`              | Required | Valid IANA timezone (e.g., `"UTC"`).                                                                                          |
| `schedule.active_windows`                 | `Vec<WeeklyWindow>`   | `[]`     | Empty = always active.                                                                                                        |
| `schedule.quiet_hours`                    | Optional `QuietHours` | Absent   | Single field `windows: Vec<WeeklyWindow>`.                                                                                    |
| `WeeklyWindow.weekdays`                   | `Vec<Weekday>`        | Required | Nonempty unique list of `"monday"`..`"sunday"`.                                                                               |
| `WeeklyWindow.start_minute`, `end_minute` | `u16`                 | Required | `0..=1439` local minutes after midnight (`end_minute` exclusive, must differ from `start_minute`; `end < start` = overnight). |

### Cooldowns, limits, and delivery

All policy durations below are milliseconds in `1..=2592000000` (30 days) unless tighter.

| Type               | Fields                                                                                                                              | Validation                                                                                             |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `Cooldown`         | `scope: CooldownScope`, `duration_ms: u64`                                                                                          | Scopes: `"event"`, `"camera_event_kind"`, `"group"`, `"rule"`, `"outage"`.                             |
| `RateLimit`        | `scope: RateLimitScope`, `maximum: u32`, `window_ms: u64`                                                                           | `maximum` `1..=10000`; scopes: `"rule"`, `"channel"`, `"principal"`, `"global"`.                       |
| `CriticalBypass`   | `maximum: u32`, `window_ms: u64`                                                                                                    | `maximum` `1..=10`.                                                                                    |
| `EnrichmentPolicy` | `deadline_ms: u64`, `maximum_revisions: u32`, `maximum_attempts: u32`, `maximum_attachment_bytes: u64`, `wake_after_deadline: bool` | All required. Revisions `1..=32`, attempts `1..=8`, attachment bytes `1..=4194304` (4 MiB).            |
| `FailurePolicy`    | `maximum_attempts: u32`, `maximum_retry_interval_ms: u64`, `expiry_ms: u64`                                                         | All required. Attempts `1..=10`. Enabled `"push"` actions require `maximum_retry_interval_ms >= 5000`. |

**`Action` (`actions[]`):**

| Field                   | Type               | Default  | Limits and notes                                                                                                                                                                                                                                                                                                      |
| ----------------------- | ------------------ | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`               | `bool`             | `true`   | Disabled actions skip destination validation and delivery.                                                                                                                                                                                                                                                            |
| `channel`               | `Channel`          | Required | `"browser"`, `"push"`, `"webhook"`, `"forwarder"`, or `"keeppeek_service"` (use channels advertised by the server).                                                                                                                                                                                                   |
| `destination`           | `String`           | Required | Max `2,048` B after secret resolution. `"browser"` and `"keeppeek_service"` require `""`; `"push"` uses Pushover format; `"webhook"` requires HTTP(S) URL without credentials; `"forwarder"` requires nonblank text.                                                                                                  |
| `template`              | `Template`         | Required | `title` (`<= 256` chars) and `body` (`<= 4,096` chars; providers may enforce tighter limits). Allowed `{{field}}` tokens (not secret refs): `source.id`, `source.name`, `event.id`, `event.kind`, `event.zone`, `event.confidence`, `event.duration`, `health.state`, `notification.stage`, `notification.deep_link`. |
| `attachment`            | `AttachmentPolicy` | Required | `"never"`, `"when_available"`, or `"required"`. `"keeppeek_service"` rejects `"required"` (never transmits media attachments).                                                                                                                                                                                        |
| `allow_second_delivery` | `bool`             | Required | Enables staged enrichment follow-up delivery.                                                                                                                                                                                                                                                                         |

Non-browser destinations saved through the rule store are stored in `secrets.toml` under managed `KEEPPEEK_NOTIFICATION_...` keys with separate active/draft secret references. Legacy `notifications.db` is a one-time migration input; delivery jobs, retry state, history, and counters are runtime state. See [Notifications and integrations](./notifications-and-integrations.md).

## Runtime profiles

**Type:** `ProfileRegistry` (`deny_unknown_fields`) | **Section:** `[runtime_profiles]` | **Ownership:** Server-managed (do not edit manually) | **Apply:** Live (asynchronous camera convergence) | **Sources:** [src/profiles.rs](https://github.com/xnorpx/keeppeek/blob/main/src/profiles.rs), [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

| Field               | Type                               | Default              | Limits and notes                                                                                                                                           |
| ------------------- | ---------------------------------- | -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `identity_schema`   | `i64` (persistence marker)         | `1`                  | Server-managed rollback guard stripped before struct deserialization and re-stamped on save; unknown versions fail closed without modifying `config.toml`. |
| `revision`          | `u64`                              | `0`                  | Monotonic CAS revision (`0..=i64::MAX`).                                                                                                                   |
| `active_profile_id` | Optional `String`                  | Absent (Base active) | Must exist in `profiles`; active profile cannot be deleted.                                                                                                |
| `profiles`          | `BTreeMap<String, RuntimeProfile>` | `{}`                 | Max `32` profiles; keys match `profile.id` (`1..=128` ASCII `[A-Za-z0-9._-]`, not `.` or `..`).                                                            |

Each `RuntimeProfile` (`deny_unknown_fields`) has `id`, `name` (nonblank `1..=80` UTF-8 B, no control chars), `revision` (`1..=i64::MAX`), and `cameras` (max `127` entries keyed by camera `profile_identity` UUID, each setting at least one field of `CameraProfileOverride`):

| Override field             | Type                           | Default       | Limits and notes                                                                                                                                                                                      |
| -------------------------- | ------------------------------ | ------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `availability`             | Optional `bool`                | Absent (Base) | `false` stops runtime ingress without editing Base; cannot enable disabled Base.                                                                                                                      |
| `recording_mode`           | Optional `CameraRecordingMode` | Absent (Base) | `"off"`, `"sub"`, `"main"`, `"both"`, `"event-boost"`, or `"event-only"`. Cannot enable recording when Base is `"off"`; resolves to `"off"` when `availability = false` or `privacy_enforced = true`. |
| `privacy_enforced`         | Optional `bool`                | Absent (Base) | Feeds `PrivacyRegistry`; `true` enforces privacy; `false` cannot weaken an active privacy schedule.                                                                                                   |
| `notifications_suppressed` | Optional `bool`                | Absent (Base) | Suppresses matching notification candidates and queued outbox delivery without altering saved rules.                                                                                                  |

Credentials, secret refs, addresses, ports, stream URLs, codecs, transport, storage paths, retention, detectors, zones, masks, access control, PTZ, and notification rules are rejected by `CameraProfileOverride`.

- **Control surface:** Managed only via **Settings → Runtime profiles** or authorized WebRTC `StateStoreCommand` on `service/runtime-profiles/` (no HTTP, REST, MQTT, Home Assistant, webhook, CLI, scheduler, presence, or geofence trigger exists). Users can observe active profile and camera-scoped status, not definitions or mutations.
- **Failure, retry, and recovery:** Desired selection commits atomically before camera workers converge. Visible cameras report `pending`, `applied`, or `degraded` with provenance (`base`/`profile`) across eight bounded status documents. Failed/timed-out targets stay `degraded` (restrictive privacy remains closed; no silent fallback to Base). Missing targets report `degraded` (`configured camera is missing`) until removed or Base is activated. Recover via **Reapply active policy** (or WebRTC CAS re-put). Full backups preserve `[runtime_profiles]`, `identity_schema = 1`, and camera `profile_identity` bindings; rolling back to a pre-migration binary requires restoring a pre-migration backup.

## Settings-backed state store

**Type:** `server::state_store_settings` | **Section:** `[state_store]` | **Ownership:** Server-managed (do not edit manually) | **Apply:** Live (WebRTC `StateStoreCommand` CAS) | **Sources:** [src/server/state_store_settings.rs](https://github.com/xnorpx/keeppeek/blob/main/src/server/state_store_settings.rs), [docs/state-store.md](https://github.com/xnorpx/keeppeek/blob/main/docs/state-store.md)

Stores settings-backed `StateStore` adapter namespaces directly in `config.toml` under the shared `config_update` lock so they travel with configuration ZIP backups. Generic runtime/lease `StateStore` namespaces are stored separately in `state-store.db` under `storage.long_term_path`, not here.

| Path / Field                                  | Type  | Default          | Limits and notes                                                                                                                                                                                                                                                                                                                                                                        |
| --------------------------------------------- | ----- | ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[state_store."<namespace>"].revision`        | `u64` | `0` (when empty) | Nonnegative monotonic namespace revision (`0..=i64::MAX`); required when `entries` is nonempty.                                                                                                                                                                                                                                                                                         |
| `[state_store."<namespace>"].entries."<key>"` | Table | `{}`             | Max `1,024` entries per namespace; TTLs are rejected (configuration entries never expire). Each entry records `revision` (`1..=namespace.revision`), `schema`, `owner_id`, `updated_ms`, and adapter-validated schema fields (currently registered adapter: `service/state-store-test/`, schema `keeppeek.settings-test.v1`, fields `display_name` `<= 128` chars and `enabled: bool`). |

## Pending storage migration

**Type:** `StorageMigration` | **Section:** `[storage_migration]` | **Ownership:** Server-managed (do not edit manually) | **Apply:** Startup migration (consumed and removed on completion) | **Source:** [src/config.rs](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs)

| Field                          | Type                            | Default | Limits and notes                                                                   |
| ------------------------------ | ------------------------------- | ------- | ---------------------------------------------------------------------------------- |
| `medium_term`                  | Optional `StoragePathMigration` | Absent  | Medium-term directory move (`{ from, to }`).                                       |
| `long_term`                    | Optional `StoragePathMigration` | Absent  | Long-term directory move (`{ from, to }`).                                         |
| `recording_catalog`            | Optional `StoragePathMigration` | Absent  | Explicit catalog move when not covered by a recording-root move.                   |
| `event_thumbnails`             | Optional `StoragePathMigration` | Absent  | Explicit thumbnail move when not covered by a recording-root move.                 |
| `recording_catalog_after_move` | Optional `PathBuf`              | Absent  | Nonempty catalog path whose stored recording paths are rewritten after root moves. |

Each `StoragePathMigration` requires nonempty `from: PathBuf` and `to: PathBuf` that do not contain one another, do not overlap other routes, and do not split one source into two destinations. `StorageMigrationPaths` is a runtime helper, not a TOML section. Incompatible with `[storage.metadata_pending]`.

## Compatibility and completeness

- Always preserve server-managed sections (`[access_credentials]`, `[external_identities]`, `[peek_layouts]`, `[saved_event_searches]`, `[configuration_templates]`, `[notifications]`, `[runtime_profiles]`, `[state_store]`, `[storage.metadata]`, `[storage.metadata_pending]`, `[storage_migration]`) when editing `config.toml`; serializing `Config` alone omits sections with dedicated loaders.
- Legacy files (`access.toml`, `peek-layouts.json`, `configuration-templates.json`, `notifications.db`) are startup migration inputs only; never create new separate settings files.
- There is no `[users]`, `[permissions]`, or `[groups]` section; never invent them or treat a grid's `audience` as camera authorization.
- Keep this reference synchronized whenever any serialized struct, section name, default, limit, enum, or migration changes, and verify load/write round-trips with synthetic fixtures.

For legacy store upgrades, see [What startup migrates](./upgrades-and-migrations.md#what-startup-migrates). For restart durability of runtime vs config data, see [What survives a restart](./backup-and-restore.md#what-survives-a-restart).
