# Secrets configuration

KeepPeek stores reusable private strings in `secrets.toml` beside the selected `config.toml`. The
file is created with owner-only permissions on Unix. For the default configuration, its location
is:

- macOS: `~/Library/Application Support/keeppeek/secrets.toml`
- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/secrets.toml`
- Windows: `%APPDATA%\keeppeek\secrets.toml`
- Docker: `/config/keeppeek/secrets.toml`

Do not add this file to source control, logs, screenshots, support bundles, or command arguments.
Enter real values directly on the machine running KeepPeek.

## Flat namespace

The file is one flat TOML string-to-string table. Nested tables, arrays, numbers, and general
templating are rejected. Keys use uppercase ASCII letters, digits, and underscores:

```toml
CAMERA_USERNAME = "admin"
CAMERA_PASSWORD = "replace-on-the-local-machine"
FRONT_CAMERA_PASSWORD = "replace-on-the-local-machine"
CAMERA_HOST = "camera.internal.example"
HOME_ASSISTANT_TOKEN = "replace-on-the-local-machine"
KEEPPEEK_ACCESS_KEY = "replace-with-a-UUID"
```

The values can hold camera credentials, tokens, private hostnames, path fragments, or other strings
consumed by a KeepPeek configuration field.

## References

Use `{secret:KEY}` inside a quoted `config.toml` string. A reference may replace the complete value
or appear inside a larger string:

```toml
[camera_defaults]
username = "{secret:CAMERA_USERNAME}"
password = "{secret:CAMERA_PASSWORD}"

access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[cameras.front_door]
ip = "192.0.2.10"
password = "{secret:FRONT_CAMERA_PASSWORD}"
main_rtsp_url = "rtsp://{secret:CAMERA_HOST}/stream1"
```

Camera defaults apply only when the camera does not define that field. A camera can use an inline
literal or a different reference for either field.

Use `{secret:KEY|url}` when a value is inserted into a URL component that requires percent
encoding. It encodes every byte except RFC 3986 unreserved characters:

```toml
[cameras.front_door]
main_rtsp_url = "rtsp://camera.internal/stream?token={secret:CAMERA_TOKEN|url}"
```

KeepPeek resolves references before a camera or supported service consumes its configuration.
Missing keys, malformed references, invalid modifiers, and malformed `secrets.toml` fail startup or
the corresponding configuration write. Errors name the key but never include the resolved value.

## Precedence and round trips

An inline literal does not consult `secrets.toml`; the literal is the configured value. For a
reference, `KEEPPEEK_SECRET_<KEY>` from the process environment wins over the same key in
`secrets.toml`. A missing value in both sources is an error.

Configuration editors and API responses retain `{secret:...}` instead of returning resolved
values. Saving an unrelated camera or runtime setting preserves unchanged references. Entering a
new credential intentionally replaces that one reference with an inline literal; `secrets.toml` is
not generally editable through the UI. The compatibility first-run KeepPeek access key is the sole
exception: its dedicated local-only control can retrieve the generated value once or rotate it
without exposing other secrets.

On first start, KeepPeek generates `KEEPPEEK_ACCESS_KEY` in the owner-only secret file and writes
only `{secret:KEEPPEEK_ACCESS_KEY}` to `config.toml`. It never prints the generated value. Existing
inline access keys and non-zero `--access-key` values are migrated into `secrets.toml` on startup.
The `[access_credentials]` section of `config.toml` stores that credential's verifier and lifecycle
metadata. Named credentials created later store only verifiers there; their raw values are returned once
and are never added to `secrets.toml`.

Existing inline camera credentials remain supported and are not moved automatically. To migrate
one, create a key in `secrets.toml`, replace the inline value in `config.toml` with its reference,
then restart KeepPeek.

The `keeppeek-camera discover --credentials-from <config.toml>` and `keeppeek-camera test --config
<config.toml>` commands use resolved camera defaults and per-camera references through the normal
camera loader.

## Rotation

On first run, a local Administrator can retrieve, copy, or download the initial remote
Administrator key once. It is not rendered until that explicit action. **Settings > Access** lists
credential metadata and can create, rotate, disable, or revoke named Administrator and User
credentials. Create and rotate show the replacement only for that operation. Lifecycle changes
invalidate matching HTTP/WebRTC work and active sessions. Rotation of the compatibility initial
credential is unavailable while `KEEPPEEK_SECRET_KEEPPEEK_ACCESS_KEY` overrides the file.

To rotate any other secret without changing `config.toml`:

1. Replace the value for the existing key in `secrets.toml`.
2. Keep the file restricted to the service account (`0600` on Unix).
3. Restart KeepPeek so cameras and services reconnect with the new value.
4. Confirm authentication succeeds before revoking the old credential at the camera or service.

Resolved values are removed from captured KeepPeek logs and diagnostic exports. URL user-info and
fields named as passwords, tokens, credentials, or secrets retain the existing redaction rules.
