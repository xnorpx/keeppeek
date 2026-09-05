# Configuration reference

KeepPeek stores settings in one application configuration file, named `config.toml` by default.
The existing companion `secrets.toml` holds reusable private strings. Layouts, notification rules,
credential metadata, and camera templates belong in the application configuration, not in separate
settings files. Media files and recording catalogs are data, not additional settings stores.

This chapter describes the supported on-disk sections and their serialized Rust types. It is not
a list of every API response or runtime struct. Use [visual configuration](./configuration-management.md)
for normal administration and [configuration exchange](./configuration-export-import.md) for
validated export and import.

## Location and editing

| Environment | Default application configuration                        |
| ----------- | -------------------------------------------------------- |
| macOS       | `~/Library/Application Support/keeppeek/config.toml`     |
| Linux       | `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/config.toml` |
| Windows     | `%APPDATA%\keeppeek\config.toml`                         |
| Docker      | `/config/keeppeek/config.toml`                           |

`--config <path>`, `--config=<path>`, or `-c <path>` selects another file. Its companion secrets
file is always named `secrets.toml` in the same directory. These flags do not create a second
settings store.

Use the UI and typed configuration operations while KeepPeek runs. Stop the server before manual
edits so a concurrent settings update cannot overwrite them. Server writers validate candidates,
preserve unrelated sections and secret references, and replace the file atomically under the shared
configuration-update lock. Some changes apply live; listener or storage changes can require the
restart or migration reported by the configuration plan.

On Unix, KeepPeek creates its default private directory with mode `0700` and private files with
mode `0600`. Restrict the directory to the service account on every platform. Secret values are
plaintext at rest, not encrypted by TOML.

In the tables below, `optional` means omit the field when unset: TOML has no `null`. Integer types
refer to the Rust model; values must also fit TOML's integer representation. `Vec<T>` is an array,
and a map is a table keyed by the indicated identity. Byte limits and character limits are different.
Root fields must appear before a table header or they belong to that table.

## Section index

| TOML section                                       | Owning type                                         | Ownership                                     |
| -------------------------------------------------- | --------------------------------------------------- | --------------------------------------------- |
| Root fields                                        | `Config`                                            | Operator settings                             |
| `[access]`                                         | `AccessConfig`                                      | Administrator security policy                 |
| `[direct_card]`                                    | `DirectCardConfig`                                  | Exact browser-origin allowlist                |
| `[storage]`                                        | `StorageToml`                                       | Recording paths, retention, and safety        |
| `[battery_wake]`                                   | `BatteryWakeConfig`                                 | Reolink battery-camera wake service           |
| `[logging]`                                        | `LoggingConfig`                                     | Service log destination                       |
| `[operational_events]`                             | `OperationalEventsConfig`                           | Health-event timing                           |
| `[operational_events.cameras."<camera-id-or-ip>"]` | `OperationalEventOverride`                          | Per-camera timing overrides                   |
| `[event_forwarder.mqtt]`                           | `MqttForwarderConfig` inside `EventForwarderConfig` | MQTT configuration; server-owned revision     |
| `[camera_defaults]`                                | `CameraCredentialDefaults`                          | Shared camera defaults                        |
| `[<namespace>.<camera-key>]`                       | `CameraConfig`                                      | Camera settings                               |
| `[access_credentials]`                             | `PersistedAccessCatalog`                            | Server-managed credential records             |
| `[peek_layouts]`                                   | `StoredRegistry`                                    | Server-managed layouts and per-user selection |
| `[configuration_templates]`                        | `StoredTemplateDocument`                            | Server-managed camera templates               |
| `[notifications]`                                  | `NotificationConfiguration`                         | Server-managed rule drafts and active rules   |
| `[storage_migration]`                              | `StorageMigration`                                  | Server-managed pending storage move           |

`homekit` is a reserved root section, not an implemented HomeKit configuration schema. Do not use
it as a camera namespace. Every other non-reserved root table is interpreted as a camera namespace;
`cameras` is the conventional name, not the only allowed one. A typo in a section name can therefore
be interpreted as camera configuration. Some structs ignore unknown fields, so successful TOML
parsing alone does not prove a field has an effect.

Sources: [configuration](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs),
[cameras](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/mod.rs), and the subsystem sources
linked below. `Config.source` is an in-memory copy of the original TOML table, marked `serde(skip)`;
there is no writable `source` field in the file.

## Passwords and secret references

`Secrets` is a flat `BTreeMap<String, String>`. Its keys must match `[A-Z_][A-Z0-9_]*`.
Nested tables, arrays, numbers, and booleans are not secret values. Enter real values directly on
the server; do not put them in source control, screenshots, logs, or shell arguments.

Example companion secrets file, with placeholders that must be replaced locally:

```toml
CAMERA_USERNAME = "replace-with-camera-user"
CAMERA_PASSWORD = "replace-with-camera-password"
FRONT_CAMERA_PASSWORD = "replace-with-front-camera-password"
MQTT_PASSWORD = "replace-with-broker-password"
```

The application configuration refers to those keys:

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

- `{secret:KEY}` substitutes the string directly. It can occupy the entire value or part of it.
- `{secret:KEY|url}` percent-encodes a URL component. Use it for a password, username, or query
  value embedded in a URL, not for an entire URL. Bytes other than RFC 3986 unreserved characters
  are encoded, including `@`, `:`, `/`, spaces, and non-ASCII UTF-8 bytes.
- `KEEPPEEK_SECRET_<KEY>` in the server process environment overrides the same key in the file.
  A literal value does not consult either source.
- A missing referenced key, malformed reference, unsupported modifier, or invalid secrets file
  fails loading or the corresponding settings operation. Diagnostics identify the key without
  including its value.
- Resolution substitutes strings, not TOML syntax. It does not convert a quoted numeric secret
  into an integer or expand table keys. Inserted secret values are not recursively expanded.
- Reusing a key gives several cameras the same credential. A per-camera key overrides the shared
  default. Rotate an ordinary secret in the companion file and restart KeepPeek to reconnect with
  the new value.

### Reference boundaries

| Setting                                                                                        | Supported resolution                                                                                                                                                           |
| ---------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Root settings, camera defaults, camera entries, and ordinary settings sections                 | String values and strings inside arrays/tables are resolved before typed deserialization. The resolved string must still satisfy the field's type and validation.              |
| `[event_forwarder.mqtt].password`                                                              | Direct or referenced string. The MQTT settings writer stores a supplied password in the companion file under `MQTT_PASSWORD`.                                                  |
| Notification `active.actions[].destination` and `draft.actions[].destination`                  | The notification loader explicitly resolves destination references. Non-browser destinations saved through the rule store use managed `KEEPPEEK_NOTIFICATION_...` secret keys. |
| Camera-template `username_secret_reference` and `password_secret_reference`                    | Complete reference tokens only, not inline credentials or interpolated surrounding text. Values are resolved when applying camera configuration.                               |
| `[access_credentials]`, `[peek_layouts]`, other notification fields, and other template fields | Not blanket-expanded as secret templates. These sections have dedicated loaders.                                                                                               |
| `[storage_migration]`                                                                          | Concrete paths written by the server, not a secret-template interface.                                                                                                         |

Configuration responses and unrelated settings updates preserve existing reference tokens instead
of returning or persisting resolved credentials. Inline camera credentials remain supported but
are not automatically moved to the secret file. Intentionally replacing a camera credential in
the editor can replace its reference with an inline value. Ordinary layout exchange must not
include credentials, verifiers, or unrelated user state. Full recovery archives have a different
security contract; see [backup and restore](./backup-and-restore.md).

The first-run access key is generated as `KEEPPEEK_ACCESS_KEY` in the companion file and referenced
from the root `access_key` field. Named credentials created later retain only verifiers in the
application configuration and return their raw keys once. See
[authentication](./authentication.md) and the
[secret lifecycle guide](https://github.com/xnorpx/keeppeek/blob/main/docs/secrets.md).

## Root settings

Type: `Config`. Nested section fields are listed in the section index.

| Field        | Type        | Default                                  | Meaning                                                                                                                                                                        |
| ------------ | ----------- | ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `host`       | `String`    | `"0.0.0.0"`                              | HTTP listener bind address.                                                                                                                                                    |
| `port`       | `u16`       | `8081`                                   | HTTP listener port.                                                                                                                                                            |
| `access_key` | `AccessKey` | Unset in the model; generated on startup | Compatibility initial Administrator key: a UUID string, a reference resolving to a UUID, or integer `0` as the unset marker. `0` is not a switch that disables authentication. |

## Access policy

Type: `AccessConfig`, section `[access]`.

| Field                               | Type         | Default        | Validation and meaning                                                                                                                  |
| ----------------------------------- | ------------ | -------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `local_networks`                    | `Vec<IpNet>` | Networks below | At most 64 CIDRs. Matches become trusted-local Administrators without sign-in. Setting the array replaces the defaults.                 |
| `trusted_proxies`                   | `Vec<IpNet>` | `[]`           | At most 64 CIDRs, checked separately from `local_networks`. Only these peers can supply the supported forwarded-client header contract. |
| `require_secure_remote`             | `bool`       | `true`         | Require the declared secure boundary for remote traffic.                                                                                |
| `failed_authentication_limit`       | `u32`        | `5`            | Nonzero failed-attempt limit per address/window.                                                                                        |
| `failed_authentication_window_secs` | `u64`        | `60`           | Nonzero authentication-rate window, in seconds.                                                                                         |
| `session_idle_timeout_secs`         | `u64`        | `1800`         | Nonzero; cannot exceed the absolute timeout.                                                                                            |
| `session_absolute_timeout_secs`     | `u64`        | `86400`        | Nonzero maximum session lifetime.                                                                                                       |
| `max_sessions_per_principal`        | `u32`        | `64`           | Nonzero concurrent-session limit per identity.                                                                                          |
| `max_sessions_per_address`          | `u32`        | `128`          | Nonzero concurrent-session limit per address.                                                                                           |

Default local networks are `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
`169.254.0.0/16`, `::1/128`, `fc00::/7`, and `fe80::/10`. A container bridge or VPN within those
ranges is trusted unless the operator narrows the list. Forwarding headers and CORS do not replace
authentication. Review [authentication and access control](./authentication.md) before exposing the
listener.

### Direct card origins

Type: `DirectCardConfig`, section `[direct_card]`.

| Field             | Type          | Default | Validation and meaning                                                                                                                      |
| ----------------- | ------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `allowed_origins` | `Vec<String>` | `[]`    | Unique canonical exact HTTP(S) origins, such as `"https://ha.example"`. No credentials, path, query, fragment, wildcard, or trailing slash. |

## Camera defaults and camera entries

Source: [camera types](https://github.com/xnorpx/keeppeek/blob/main/src/cameras/mod.rs) and
the camera loader in [configuration](https://github.com/xnorpx/keeppeek/blob/main/src/config.rs).

Type: `CameraCredentialDefaults`, section `[camera_defaults]`.

| Field                           | Type                           | Default or inheritance                                        |
| ------------------------------- | ------------------------------ | ------------------------------------------------------------- |
| `username`                      | `String`                       | Empty string; accepts secret references.                      |
| `password`                      | `String`                       | Empty string; accepts secret references.                      |
| `backend`                       | Optional `CameraBackend`       | No shared override; camera model defaults to `"auto"`.        |
| `transport`                     | Optional `CameraTransport`     | No shared override; camera model defaults to `"tcp"`.         |
| `record_generic_motion_events`  | Optional `bool`                | No shared override; camera model defaults to `false`.         |
| `recording_mode`                | Optional `CameraRecordingMode` | No shared override; camera model defaults to `"event-boost"`. |
| `event_recording_duration_secs` | Optional `u64`                 | No shared override; camera model defaults to `60` seconds.    |

Username and password fall back to their shared defaults when the per-camera resolved value is
empty, including when the field is omitted. The other shared defaults apply only when the
per-camera field is absent. Ports and RTSP URLs are not fields of `CameraCredentialDefaults`.

Type: `CameraConfig`, section `[<namespace>.<camera-key>]`, usually `[cameras.front_door]`.

| Field                           | Type                  | Default                           | Meaning                                                                                                                         |
| ------------------------------- | --------------------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `ip`                            | `IpAddr`              | Required                          | Camera IP address, not a DNS hostname.                                                                                          |
| `name`                          | Optional `String`     | Camera table key                  | The loader replaces this value with the table key. Use `display_name` to change the UI label without changing storage identity. |
| `display_name`                  | Optional `String`     | Falls back to `name`              | Human-readable camera label.                                                                                                    |
| `manufacturer`                  | Optional `String`     | No override                       | Nonempty trimmed value takes precedence over discovery.                                                                         |
| `username`                      | `String`              | Shared default or empty           | Camera username; literal or secret reference.                                                                                   |
| `password`                      | `String`              | Shared default or empty           | Camera password; literal or secret reference.                                                                                   |
| `onvif_port`                    | Optional `u16`        | `8000` when unset                 | ONVIF service port. Typed configuration operations require `1..65535`.                                                          |
| `http_port`                     | Optional `u16`        | `80` when unset                   | Direct camera HTTP control port; typed operations require `1..65535`.                                                           |
| `main_rtsp_url`                 | Optional `String`     | Discover stream                   | Explicit main-stream RTSP URL takes precedence over ONVIF discovery. Blank values are not explicit URLs.                        |
| `sub_rtsp_url`                  | Optional `String`     | Discover stream                   | Explicit sub-stream RTSP URL. Use URL-escaped references in credential components.                                              |
| `uid`                           | Optional `String`     | None                              | Reolink P2P UID for direct BCUDP discovery.                                                                                     |
| `backend`                       | `CameraBackend`       | Shared default or `"auto"`        | `"auto"`, `"retina"`, or `"reo-proto"`.                                                                                         |
| `transport`                     | `CameraTransport`     | Shared default or `"tcp"`         | `"tcp"` or `"udp"`.                                                                                                             |
| `record_generic_motion_events`  | `bool`                | Shared default or `false`         | Opt in to supported generic motion events.                                                                                      |
| `recording_mode`                | `CameraRecordingMode` | Shared default or `"event-boost"` | `"off"`, `"sub"`, `"main"`, `"both"`, or `"event-boost"`.                                                                       |
| `event_recording_duration_secs` | `u64`                 | Shared default or `60`            | Event-triggered recording duration; typed configuration operations use `1..3600` seconds.                                       |

These are stored configuration types, not promises that every camera supports every backend,
transport, or recording capability. Discovery results such as `CameraCapabilities`, `CameraPorts`,
`MediaProfile`, `VideoConfig`, and `AudioConfig` are not additional editable camera tables.

## Recording storage

Type: `StorageToml`, section `[storage]`. Optional paths are resolved by the runtime when absent;
inspect the effective settings instead of assuming an omitted path is the current directory.

Both recording directories default to `recordings/` under the default OS KeepPeek configuration
directory, even when `--config` selects a configuration file elsewhere. An omitted catalog path
becomes `recordings.db` under the effective long-term directory; the thumbnail directory defaults
to `.event-thumbnails` under that same directory. See
[StorageConfig::from_toml](https://github.com/xnorpx/keeppeek/blob/main/src/storage/engine.rs).

| Field                    | Type              | Default                 | Meaning                                                  |
| ------------------------ | ----------------- | ----------------------- | -------------------------------------------------------- |
| `medium_term_path`       | Optional `String` | Runtime-selected        | Medium-term recording directory.                         |
| `long_term_path`         | Optional `String` | Runtime-selected        | Long-term recording directory.                           |
| `recording_catalog_path` | Optional `String` | Runtime-selected        | Recording catalog path.                                  |
| `event_thumbnail_path`   | Optional `String` | Runtime-selected        | Event-thumbnail directory.                               |
| `event_thumbnail_max_mb` | `u64`             | `1024`                  | Thumbnail storage budget, in MiB.                        |
| `short_term_secs`        | `u64`             | `120`                   | Short-term retention, in seconds.                        |
| `medium_term_secs`       | `u64`             | `1800`                  | Medium-term retention, in seconds.                       |
| `flush_interval_secs`    | `u64`             | `60`                    | Recording flush interval, in seconds.                    |
| `write_buffer_bytes`     | `usize`           | `8192`                  | Recording write-buffer size, in bytes.                   |
| `long_term_max_gb`       | `u64`             | `1024`                  | Long-term storage budget, in GiB.                        |
| `minimum_free_gb`        | `u64`             | `10`                    | Minimum-free-space safety threshold, in GiB.             |
| `maximum_used_percent`   | Optional `u8`     | No percentage threshold | When set, `1..99`; filesystem-used percentage threshold. |
| `warning_free_gb`        | `u64`             | `20`                    | Warning-free-space threshold, in GiB.                    |
| `critical_free_gb`       | `u64`             | `10`                    | Critical-free-space threshold, in GiB.                   |
| `cleanup_hysteresis_gb`  | `u64`             | `5`                     | Cleanup recovery margin, in GiB.                         |

The effective critical threshold is the greater of `critical_free_gb` and `minimum_free_gb`.
If `warning_free_gb` is zero while the effective critical threshold is nonzero, the warning
threshold is derived as critical plus hysteresis. Otherwise, warning must be at least critical.
Use the storage editor's validated path-migration workflow for moves; do not repoint a live
recording catalog by editing a path alone.

## Battery wake

Type: `BatteryWakeConfig`, section `[battery_wake]`. This is the Reolink wake middleman service,
not a general Wake-on-LAN or scheduled privacy configuration.

| Field              | Type                | Default   | Validation and meaning                                          |
| ------------------ | ------------------- | --------- | --------------------------------------------------------------- |
| `enabled`          | `bool`              | `false`   | Enable the wake service.                                        |
| `bind`             | Optional `Ipv4Addr` | Automatic | Local IPv4 binding selection.                                   |
| `middleman_port`   | `u16`               | `9999`    | Nonzero middleman port.                                         |
| `register_port`    | `u16`               | `58200`   | Nonzero registration port; must differ from the middleman port. |
| `heartbeat_secs`   | `u64`               | `20`      | Nonzero heartbeat interval.                                     |
| `stale_after_secs` | `u64`               | `80`      | Must cover at least one heartbeat interval.                     |

## Logging

Type: `LoggingConfig`, section `[logging]`.

| Field     | Type                    | Default  | Meaning                                                                                                       |
| --------- | ----------------------- | -------- | ------------------------------------------------------------------------------------------------------------- |
| `service` | `ServiceLogDestination` | `"file"` | `"file"` or `"event_log"`; service logging destination, with Event Log relevant to Windows service operation. |

This section does not define arbitrary tracing filters, log payloads, or a diagnostics archive.

## Operational events

Type: `OperationalEventsConfig`, section `[operational_events]`.

| Field                    | Type                              | Default | Meaning                                                          |
| ------------------------ | --------------------------------- | ------- | ---------------------------------------------------------------- |
| `warning_hold_down_secs` | `u64`                             | `15`    | Delay before recording a warning transition.                     |
| `outage_hold_down_secs`  | `u64`                             | `60`    | Delay before recording an outage.                                |
| `recovery_debounce_secs` | `u64`                             | `10`    | Stable-recovery debounce period.                                 |
| `record_short_flaps`     | `bool`                            | `false` | Retain supported brief-flap transitions.                         |
| `cameras`                | Map of `OperationalEventOverride` | Empty   | Overrides keyed by stable camera ID, with camera IP as fallback. |

Type: `OperationalEventOverride`, section `[operational_events.cameras."<camera-id-or-ip>"]`.

| Field                    | Type            | When absent                       |
| ------------------------ | --------------- | --------------------------------- |
| `warning_hold_down_secs` | Optional `u64`  | Inherit global warning hold-down. |
| `outage_hold_down_secs`  | Optional `u64`  | Inherit global outage hold-down.  |
| `recovery_debounce_secs` | Optional `u64`  | Inherit global recovery debounce. |
| `record_short_flaps`     | Optional `bool` | Inherit the global flag.          |

Each effective policy requires warning hold-down no greater than outage hold-down. Outage and
recovery durations cannot exceed `86400` seconds. Override keys contain 1 to 256 bytes after the
nonempty check. The stable camera ID takes precedence over an IP-keyed override.

## MQTT event forwarding

Types: `EventForwarderConfig` has one field, `mqtt: MqttForwarderConfig`. Section:
`[event_forwarder.mqtt]`. Source:
[MQTT configuration](https://github.com/xnorpx/keeppeek/blob/main/src/event_forwarder/config.rs).

| Field           | Type               | Default                   | Validation and meaning                                                                     |
| --------------- | ------------------ | ------------------------- | ------------------------------------------------------------------------------------------ |
| `revision`      | `u64`              | `1`                       | Server-managed settings revision.                                                          |
| `enabled`       | `bool`             | `false`                   | Enable forwarding.                                                                         |
| `broker_url`    | `String`           | `"mqtt://127.0.0.1:1883"` | `mqtt` or `mqtts` URL with host; no user-info, non-root path, query, or fragment.          |
| `client_id`     | `String`           | `"keeppeek"`              | Broker client ID.                                                                          |
| `instance_id`   | `String`           | `"home-nvr"`              | KeepPeek instance identity.                                                                |
| `forwarder_id`  | `String`           | `"mqtt"`                  | Forwarder identity.                                                                        |
| `topic_prefix`  | `String`           | `"keeppeek"`              | 1 to 512 bytes; no leading/trailing slash, NUL, `+`, or `#`. Interior slashes are allowed. |
| `username`      | Optional `String`  | None                      | Broker username, including supported references.                                           |
| `password`      | Optional `String`  | None                      | Requires `username`; prefer a reference to the companion secret file.                      |
| `tls_ca_path`   | Optional `PathBuf` | System trust              | Custom CA path; only valid with `mqtts`.                                                   |
| `qos`           | `u8`               | `1`                       | `0`, `1`, or `2`.                                                                          |
| `retain_events` | `bool`             | `false`                   | Retain event publications.                                                                 |
| `retain_health` | `bool`             | `true`                    | Retain health publications.                                                                |
| `outbox_max_mb` | `u64`              | `64`                      | 1 to 65536 MiB of bounded outbox storage.                                                  |
| `retry_min_ms`  | `u64`              | `250`                     | Nonzero retry minimum, no greater than maximum.                                            |
| `retry_max_ms`  | `u64`              | `30000`                   | Retry maximum, at most `3600000` ms.                                                       |

Client, instance, and forwarder IDs each contain 1 to 128 bytes without NUL, `/`, `+`, or `#`.
Do not embed credentials in `broker_url`; use the dedicated fields. MQTT's durable outbox is
runtime delivery data, not another settings file.

## Credential records

Source: [access persistence](https://github.com/xnorpx/keeppeek/blob/main/src/access.rs).
Use the Administrator credential controls, not hand-written verifiers or revision changes.

Type: `PersistedAccessCatalog`, section `[access_credentials]`.

| Field         | Type                    | Rule                                                 |
| ------------- | ----------------------- | ---------------------------------------------------- |
| `version`     | `u32`                   | Required; supported value is `1`.                    |
| `credentials` | `Vec<StoredCredential>` | Required array, at most 128 records with unique IDs. |

Type: `StoredCredential`, array `[[access_credentials.credentials]]`.

| Field                    | Type                    | Rule                                                                                                                                      |
| ------------------------ | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `id`                     | `Uuid`                  | Stable named-credential identity.                                                                                                         |
| `name`                   | `String`                | 1 to 64 printable bytes after trimming.                                                                                                   |
| `description`            | Optional `String`       | At most 256 printable bytes; empty descriptions normalize to absent.                                                                      |
| `role`                   | `AccessRole`            | `"administrator"` or `"user"`.                                                                                                            |
| `camera_access`          | Optional `CameraAccess` | Per-user group and camera policy. Absent is unrestricted; new credentials explicitly default to everything.                               |
| `verifier`               | `AccessKeyFingerprint`  | SHA-256 fingerprint serialized as 32 byte-valued integers, not the raw access key. Treat it as sensitive authentication material.         |
| `created_at_ms`          | `i64`                   | Creation time, Unix milliseconds.                                                                                                         |
| `rotated_at_ms`          | Optional `i64`          | Last rotation time; absent if never rotated.                                                                                              |
| `last_used_at_ms`        | Optional `i64`          | Accepted by the stored record type for compatibility, but cleared on load and omitted by the canonical writer. Activity is runtime state. |
| `expires_at_ms`          | Optional `i64`          | Expiry time; expired credentials are denied.                                                                                              |
| `disabled`               | `bool`                  | Disabled credentials cannot authenticate.                                                                                                 |
| `revoked_at_ms`          | Optional `i64`          | Permanent revocation time.                                                                                                                |
| `revision`               | `u64`                   | Nonzero credential revision used to invalidate stale sessions.                                                                            |
| `legacy`                 | `bool`                  | Compatibility initial-credential marker.                                                                                                  |
| `initial_secret_pending` | `bool`                  | Initial-key retrieval is still pending.                                                                                                   |

The runtime `AccessCatalog.audit` array is not part of `PersistedAccessCatalog`. Audit history,
session state, and credential last-use activity reset on restart; credential identities, verifiers,
roles, revisions, and lifecycle metadata persist. The legacy `access.toml` is a migration input,
not an additional active settings file.

Type: `CameraAccess`, nested table belonging to the credential record, not to a camera. The
`all_cameras` and `camera_ids` fields are required when the policy is present; `group_ids` defaults
to an empty list for compatibility. Unknown fields are rejected.

| Field         | Type          | Rule                                                                                                                                                                                        |
| ------------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `all_cameras` | `bool`        | Explicitly grant all current and future cameras. Must not be combined with a nonempty ID list.                                                                                              |
| `group_ids`   | `Vec<String>` | Camera configuration namespaces, such as `outdoor` for `[outdoor.front_door]`. Defaults to empty. At most 128 unique, nonblank names of at most 256 UTF-8 bytes without control characters. |
| `camera_ids`  | `Vec<String>` | At most 128 unique, nonblank IDs of at most 256 UTF-8 bytes without control characters. Use server-advertised camera IDs, not names.                                                        |

With `all_cameras = false`, access is the union of cameras in `group_ids` and explicit `camera_ids`;
both lists empty grants nothing. `all_cameras = true` requires both lists empty and includes future
groups and cameras. New credentials and absent policies default to everything. Existing explicit
restrictions are not broadened on upgrade. Administrators always have full access and cannot be
restricted through this policy. There is no secret substitution inside grant records.

The Administrator's user-access editor validates camera and group IDs against the current fleet and
saves under the shared configuration lock. Each save increments the credential revision, rejects
stale writes, and invalidates existing sessions. Grants survive configuration reload and export/import.
Group grants are saved as group names, not expanded camera lists; effective access is resolved from
the server's camera group membership. `available_group_ids` in management responses is bounded
discovery metadata for the editor, not a persisted field. The editor supports up to 128 available
groups using the same ID limits; an invalid or oversized inventory rejects management requests
before any policy mutation. Runtime group-membership changes close affected group-scoped sessions
and cancel their queued work so playback and event subscriptions cannot retain an obsolete grant.
Grid audiences do not grant camera permissions. Custom roles remain unsupported, and trusted-local
clients still follow the Administrator policy described above.

## Peek layout registry

Source: [layout persistence](https://github.com/xnorpx/keeppeek/blob/main/src/server/peek_layouts.rs).
Layouts use the existing main configuration file and the `keeppeek.peek-layouts.v1` capability.
Use the layout controls and revision-checked operations rather than manually constructing this
server-managed section.

Type: `StoredRegistry`, section `[peek_layouts]`.

| Field            | Type                        | Rule                                                                               |
| ---------------- | --------------------------- | ---------------------------------------------------------------------------------- |
| `schema_version` | `u32`                       | Required; supported value is `1`.                                                  |
| `revision`       | `u64`                       | Nonzero registry revision. Stale writes conflict.                                  |
| `shared_layouts` | `Vec<Layout>`               | Nonempty server-owned layout array.                                                |
| `users`          | Map of `StoredUserRegistry` | Keyed by principal ID; holds per-user active selection and legacy private layouts. |

Type: `StoredUserRegistry`, section `[peek_layouts.users."<principal-id>"]`.

| Field              | Type          | Rule                                                                                                          |
| ------------------ | ------------- | ------------------------------------------------------------------------------------------------------------- |
| `active_layout_id` | `String`      | Selected layout identity. Missing selections are repaired by the registry.                                    |
| `layouts`          | `Vec<Layout>` | Legacy private layout records; migrated to server-owned shared layouts with restricted audiences when opened. |

Type: `Layout`, under `shared_layouts[]` or legacy `users.<principal-id>.layouts[]`.

| Field            | Type              | Rule                                                                                |
| ---------------- | ----------------- | ----------------------------------------------------------------------------------- |
| `id`             | `String`          | Unique within the registry view; nonblank, at most 128 characters.                  |
| `name`           | `String`          | Nonblank, at most 80 characters.                                                    |
| `scope`          | `LayoutScope`     | `"shared"` for canonical layouts; `"private"` is retained for migration.            |
| `owner_id`       | `String`          | `"server"` for shared layouts; private records must match their principal.          |
| `audience`       | `LayoutAudience`  | Defaults to everyone with an empty credential list when the entire field is absent. |
| `activity_focus` | `bool`            | Whether activity can affect layout focus.                                           |
| `tiles`          | `Vec<LayoutTile>` | Ordered tiles, at most 64 per layout; no duplicate camera IDs or overlaps.          |

Type: `LayoutAudience`, nested `audience` table.

| Field            | Type          | Rule                                                                                                                                                                                                 |
| ---------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `everyone`       | `bool`        | When true, `credential_ids` must be empty.                                                                                                                                                           |
| `credential_ids` | `Vec<String>` | At most 128 unique canonical UUID strings. Save operations check that viewer identities exist. Empty with `everyone = false` gives no ordinary user access; Administrators retain management access. |

Type: `LayoutTile`, nested `tiles[]` array.

| Field         | Type     | Rule                                                        |
| ------------- | -------- | ----------------------------------------------------------- |
| `camera_id`   | `String` | Stable camera identity, not display name.                   |
| `column`      | `u32`    | One-based grid column.                                      |
| `row`         | `u32`    | One-based grid row.                                         |
| `column_span` | `u32`    | Positive horizontal span; tile must fit the 12-column grid. |
| `row_span`    | `u32`    | Positive vertical span; tile must fit the 12-row grid.      |
| `pinned`      | `bool`   | Tile pinning state.                                         |

The registry has a 256 KiB serialized bound and at most 32 layouts per validated view. Stored
camera references are retained when a camera disappears; additions are validated against known
cameras. The initial registry has revision `1` and an `All cameras` shared layout with ID
`default`, activity focus enabled, and up to 64 configured cameras. Opening the store also migrates
the legacy `peek-layouts.json`; that file is not an active persistence target.

The client-facing `LayoutRegistry` contains `schema_version`, `active_layout_id`, and `layouts`.
It is not the on-disk `StoredRegistry`; do not paste a client exchange document over the main
configuration. Grid audiences control grid visibility only. They must not be treated as permission
to view, export, or control an otherwise restricted camera.

The server intersects a User's returned tiles with their camera grants. Selecting a dashboard from
that filtered view updates only the principal's active selection; it does not remove other cameras
from the canonical layout. Unknown or no-longer-visible dashboard selections are rejected.

## Camera templates

Source: [configuration templates](https://github.com/xnorpx/keeppeek/blob/main/src/server/configuration.rs).
Templates are versioned, server-managed configuration records, not another settings file.

Type: `StoredTemplateDocument`, section `[configuration_templates]`.

| Field              | Type                  | Rule                                                                           |
| ------------------ | --------------------- | ------------------------------------------------------------------------------ |
| `document_version` | `u32`                 | Required; supported value is `1`.                                              |
| `templates`        | `Vec<StoredTemplate>` | Required array, at most 64 templates. Missing section means an empty document. |

Type: `StoredTemplate`, array `[[configuration_templates.templates]]`.

| Field           | Type                   | Rule                                                                                    |
| --------------- | ---------------------- | --------------------------------------------------------------------------------------- |
| `template_id`   | `String`               | Unique ID, 1 to 64 ASCII letters, digits, hyphens, or underscores.                      |
| `version`       | `u64`                  | Server-managed template version.                                                        |
| `name`          | `String`               | Nonblank, at most 128 bytes, no control characters; names are unique ignoring case.     |
| `description`   | `String`               | Defaults to empty and is omitted when empty; at most 1024 bytes, no control characters. |
| `values`        | `StoredTemplateValues` | At least one supported setting must be present.                                         |
| `created_at_ms` | `i64`                  | Creation time, Unix milliseconds.                                                       |
| `updated_at_ms` | `i64`                  | Last update time, Unix milliseconds.                                                    |

Type: `StoredTemplateValues`, nested `values` table. Every field is optional and omitted when unset.

| Field                           | Type                  | Rule                                                       |
| ------------------------------- | --------------------- | ---------------------------------------------------------- |
| `username_secret_reference`     | `String`              | A complete valid secret reference, not an inline username. |
| `password_secret_reference`     | `String`              | A complete valid secret reference, not an inline password. |
| `onvif_port`                    | `u16`                 | `1..65535`.                                                |
| `http_port`                     | `u16`                 | `1..65535`.                                                |
| `backend`                       | `CameraBackend`       | `"auto"`, `"retina"`, or `"reo-proto"`.                    |
| `transport`                     | `CameraTransport`     | `"tcp"` or `"udp"`.                                        |
| `record_generic_motion_events`  | `bool`                | Generic motion-event preference.                           |
| `recording_mode`                | `CameraRecordingMode` | `"off"`, `"sub"`, `"main"`, `"both"`, or `"event-boost"`.  |
| `event_recording_duration_secs` | `u32`                 | `1..3600` seconds.                                         |

The template writer enforces a 16 KiB serialized document limit. The legacy
`configuration-templates.json` is imported into this section and removed. Template references stay
as tokens in the stored template; applying a template must validate the resulting camera settings.

## Notification rules

Sources: [rule persistence](https://github.com/xnorpx/keeppeek/blob/main/src/notifications/store.rs),
[rule model](https://github.com/xnorpx/keeppeek/blob/main/src/notifications/model.rs), and
[cooldowns](https://github.com/xnorpx/keeppeek/blob/main/src/notifications.rs).

Type: `NotificationConfiguration`, section `[notifications]`.

| Field   | Type                       | Default and bound                              |
| ------- | -------------------------- | ---------------------------------------------- |
| `rules` | `Vec<PersistedRuleRecord>` | `[]`; at most 128 rules, with unique rule IDs. |

Type: `PersistedRuleRecord`, array `[[notifications.rules]]`.

| Field             | Type            | Meaning                                                 |
| ----------------- | --------------- | ------------------------------------------------------- |
| `id`              | `String`        | Stable rule ID.                                         |
| `owner_id`        | `String`        | Owning principal ID.                                    |
| `active`          | Optional `Rule` | Published rule; absent before first activation.         |
| `active_revision` | `u64`           | Active revision used for conflict checks.               |
| `draft`           | `Rule`          | Saved draft, which may be incomplete before activation. |
| `draft_revision`  | `u64`           | Draft revision used for conflict checks.                |
| `created_at_ms`   | `i64`           | Creation time, Unix milliseconds.                       |
| `updated_at_ms`   | `i64`           | Last update time, Unix milliseconds.                    |

Rule identity, owner, and revisions must agree with the containing record. A draft is bounded
to 64 KiB in its JSON representation. Activating it requires complete rule validation. Runtime
`RuleRecord.last_match_at_ms` and `last_delivery_at_ms` are not persisted fields.

Type: `Rule`, under each record's `draft` and optional `active` tables.

| Field             | Type                      | Validation or default                             |
| ----------------- | ------------------------- | ------------------------------------------------- |
| `id`              | `String`                  | 1 to 128 ASCII letters, digits, `.`, `-`, or `_`. |
| `name`            | `String`                  | Nonblank, at most 128 characters.                 |
| `enabled`         | `bool`                    | Required; disabled rules do not match events.     |
| `revision`        | `u64`                     | Corresponding draft or active revision.           |
| `owner_id`        | `String`                  | Same ID character and byte limits as `id`.        |
| `triggers`        | `Vec<Trigger>`            | Nonempty and unique; values below.                |
| `filter`          | `Filter`                  | Defaults to an empty filter.                      |
| `schedule`        | `Schedule`                | Required.                                         |
| `cooldowns`       | `Vec<Cooldown>`           | Defaults to `[]`; scopes must be unique.          |
| `rate_limits`     | `Vec<RateLimit>`          | Defaults to `[]`; scopes must be unique.          |
| `critical_bypass` | Optional `CriticalBypass` | Bounded critical-event quiet-hours bypass.        |
| `enrichment`      | `EnrichmentPolicy`        | Required.                                         |
| `actions`         | `Vec<Action>`             | 1 to 8 actions.                                   |
| `failure`         | `FailurePolicy`           | Required.                                         |

`Trigger` values are `"event_created"`, `"event_updated"`, `"event_ended"`, `"outage_started"`,
`"recovery"`, `"storage_health"`, `"recording_health"`, and `"test"`.

### Filters and schedules

Type: `Filter`, nested `filter` table.

| Field                 | Type            | Default and rule                                        |
| --------------------- | --------------- | ------------------------------------------------------- |
| `source_ids`          | `Vec<String>`   | `[]`; at most 128 nonblank values.                      |
| `group_ids`           | `Vec<String>`   | `[]`; at most 128 nonblank values.                      |
| `event_kinds`         | `Vec<String>`   | `[]`; at most 128 nonblank values.                      |
| `zones`               | `Vec<String>`   | `[]`; at most 128 nonblank values.                      |
| `minimum_confidence`  | Optional `f64`  | Finite value in `0..1`, inclusive.                      |
| `attachment_required` | Optional `bool` | Require or exclude an available non-private attachment. |
| `minimum_duration_ms` | Optional `u64`  | Minimum matching event duration.                        |
| `severities`          | `Vec<Severity>` | `[]`; values `"info"`, `"warning"`, `"critical"`.       |
| `reviewed`            | Optional `bool` | Match the supplied review state.                        |
| `bookmarked`          | Optional `bool` | Match the supplied bookmark state.                      |

Empty filter arrays do not restrict that dimension. Filter fields describe matching inputs;
their presence does not mean every camera or event producer supplies them.

Type: `Schedule`, nested `schedule` table.

| Field            | Type                  | Default and rule                                    |
| ---------------- | --------------------- | --------------------------------------------------- |
| `timezone`       | `String`              | Required recognized IANA timezone, such as `"UTC"`. |
| `active_windows` | `Vec<WeeklyWindow>`   | `[]`, meaning no active-window restriction.         |
| `quiet_hours`    | Optional `QuietHours` | No quiet-hours suppression when absent.             |

`QuietHours` has one required field, `windows: Vec<WeeklyWindow>`. `WeeklyWindow` fields are:

| Field          | Type           | Rule                                                |
| -------------- | -------------- | --------------------------------------------------- |
| `weekdays`     | `Vec<Weekday>` | Nonempty and unique; `"monday"` through `"sunday"`. |
| `start_minute` | `u16`          | `0..1439`, local minutes after midnight.            |
| `end_minute`   | `u16`          | `0..1439`, exclusive end; must differ from start.   |

An end earlier than the start describes an overnight window.

### Cooldowns, limits, and delivery

All policy durations below are in milliseconds and must be between `1` and `2592000000`
(30 days), inclusive, unless a tighter rule is stated.

| Type               | Fields                                                                                                                              | Validation                                                                                                |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `Cooldown`         | `scope: CooldownScope`, `duration_ms: u64`                                                                                          | Scopes: `"event"`, `"camera_event_kind"`, `"group"`, `"rule"`, `"outage"`.                                |
| `RateLimit`        | `scope: RateLimitScope`, `maximum: u32`, `window_ms: u64`                                                                           | Maximum `1..10000`; scopes `"rule"`, `"channel"`, `"principal"`, `"global"`.                              |
| `CriticalBypass`   | `maximum: u32`, `window_ms: u64`                                                                                                    | Maximum `1..10`.                                                                                          |
| `EnrichmentPolicy` | `deadline_ms: u64`, `maximum_revisions: u32`, `maximum_attempts: u32`, `maximum_attachment_bytes: u64`, `wake_after_deadline: bool` | Revisions `1..32`, attempts `1..8`, attachment bytes `1..4194304` (4 MiB). All fields required.           |
| `FailurePolicy`    | `maximum_attempts: u32`, `maximum_retry_interval_ms: u64`, `expiry_ms: u64`                                                         | Attempts `1..10`. An enabled push action requires retry interval at least `5000` ms. All fields required. |

Type: `Action`, nested `actions[]` array.

| Field                   | Type               | Default and rule                                                                                                                                                                                                                              |
| ----------------------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`               | `bool`             | `true`.                                                                                                                                                                                                                                       |
| `channel`               | `Channel`          | `"browser"`, `"push"`, `"webhook"`, or `"forwarder"`. Use channels advertised as available by the server.                                                                                                                                     |
| `destination`           | `String`           | At most 2048 bytes after resolution. Browser actions use the owner and require an empty destination. Push uses the Pushover destination format; webhook requires an HTTP(S) URL without user-info; forwarder requires a nonblank destination. |
| `template`              | `Template`         | Required `title: String` and `body: String`.                                                                                                                                                                                                  |
| `attachment`            | `AttachmentPolicy` | `"never"`, `"when_available"`, or `"required"`.                                                                                                                                                                                               |
| `allow_second_delivery` | `bool`             | Required flag for staged delivery.                                                                                                                                                                                                            |

Template title and body limits are 256 and 4096 characters respectively; provider-specific limits
can be tighter. Allowed `{{field}}` substitutions are `source.id`, `source.name`, `event.id`,
`event.kind`, `event.zone`, `event.confidence`, `event.duration`, `health.state`,
`notification.stage`, and `notification.deep_link`. These are notification template substitutions,
not secret references.

Provider destinations can contain credentials, so the rule writer moves non-browser destination
strings into the companion secret file and stores references. It keeps active and draft secret
versions separate. Do not inline tokens into a book example or export them with a layout.

The legacy `notifications.db` is a rule-migration input. Delivery jobs, retry state, evaluation
history, and counters are runtime state, not extra `[notifications]` settings. See
[notifications and integrations](./notifications-and-integrations.md) for provider setup.

## Pending storage migration

Type: `StorageMigration`, section `[storage_migration]`. The server creates this journal for a
validated move and consumes it during migration. Do not use it as an alternative storage editor.

| Field                          | Type                            | Meaning                                                                    |
| ------------------------------ | ------------------------------- | -------------------------------------------------------------------------- |
| `medium_term`                  | Optional `StoragePathMigration` | Medium-term recording move.                                                |
| `long_term`                    | Optional `StoragePathMigration` | Long-term recording move.                                                  |
| `recording_catalog`            | Optional `StoragePathMigration` | Explicit catalog move when not covered by a recording-root move.           |
| `event_thumbnails`             | Optional `StoragePathMigration` | Explicit thumbnail move when not covered by a recording-root move.         |
| `recording_catalog_after_move` | Optional `PathBuf`              | Catalog whose stored recording paths must be rewritten after moving roots. |

Each `StoragePathMigration` has two required `PathBuf` fields: `from` and `to`. Paths must be
nonempty, must not contain one another, and must satisfy the migration overlap checks. A single
source cannot be split into conflicting destinations. `StorageMigrationPaths` is a runtime helper
of borrowed paths, not another TOML section.

## Compatibility and completeness

- Preserve server-managed sections when editing ordinary settings. Serializing only `Config`
  would omit the separate credential, layout, template, and notification records.
- The legacy access, layout, template, and notification files are migration inputs only. Do not
  create new instances of them for new settings.
- There is no generic `[users]`, `[permissions]`, `[groups]`, or persistent runtime-profile schema
  in this configuration. Do not invent those sections or assume a grid's audience restricts camera
  access elsewhere.
- Future layout and permission settings must extend the existing application configuration and
  its validation, atomic writes, and recovery coverage. Secret values remain in the existing
  companion secret store.
- When a serialized struct, section name, default, limit, enum, or migration changes, update this
  reference in the same change and verify both load and write paths with synthetic fixtures.
