# Access control

KeepPeek is local-first. It does not expose the recorder automatically, provide a cloud relay, or
make CORS an authentication mechanism. Operators choose which networks are local and how remote
traffic reaches the HTTP listener.

## Trust model

A protected request resolves from the immediate TCP peer:

- A direct address in `access.local_networks` is a local Administrator without sign-in.
- Every other direct address is remote and must use a named Bearer credential.
- A forwarding header from an untrusted peer never changes the effective address and forces remote
  classification.
- A peer in `access.trusted_proxies` must provide the one supported forwarded-client contract.
- Unknown, malformed, duplicate, or ambiguous forwarding information fails closed as remote.

The defaults trust loopback, RFC 1918, IPv4 link-local, IPv6 unique-local, and IPv6 link-local
CIDRs. IPv4-mapped IPv6 addresses are normalized. Container bridges commonly use `172.16.0.0/12`,
which is local by default; a co-resident container can therefore act as Administrator unless the
operator narrows `local_networks`. Unix-domain HTTP listeners are not implemented.

```toml
[access]
local_networks = ["127.0.0.0/8", "192.168.50.0/24", "::1/128"]
trusted_proxies = []
require_secure_remote = true
```

Changing `local_networks` replaces the defaults. Keep required loopback ranges explicitly. Use
exact host prefixes such as `/32` and `/128` for proxies whenever possible.

A VPN address range can be handled in either of two ways. Include it in `local_networks` when every
VPN client should be Administrator without sign-in. Leave it out when VPN clients should still use
individual Administrator or User credentials.

## Remote deployment

Prefer a private VPN. When Internet-reachable access is required, terminate HTTPS at a maintained
reverse proxy, restrict the proxy-to-KeepPeek link, and firewall the KeepPeek listener so clients
cannot bypass the proxy.

KeepPeek accepts exactly one `X-Forwarded-For` field from a configured trusted proxy. The value is a
comma-separated IP chain without ports, quotes, `unknown`, or RFC 7239 parameters. KeepPeek walks
from the right and selects the first untrusted hop. The proxy must remove client-supplied forwarding
headers before setting or appending its sanitized client address.

Common proxies also add `Forwarded`, `X-Real-IP`, `X-Forwarded-Host`, or `X-Forwarded-Proto` by
default. Remove them. Their presence makes the contract ambiguous and KeepPeek rejects local proxy
classification.

Example Nginx location:

```nginx
location / {
    proxy_pass http://127.0.0.1:8081;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header Forwarded "";
    proxy_set_header X-Real-IP "";
    proxy_set_header X-Forwarded-Host "";
    proxy_set_header X-Forwarded-Proto "";
    proxy_set_header Host $host;
}
```

Pair it with an exact proxy address:

```toml
[access]
trusted_proxies = ["127.0.0.1/32"]
require_secure_remote = true
```

A configured trusted proxy is the declared TLS boundary. KeepPeek cannot inspect the client-side
TLS connection after termination. Do not add a proxy to `trusted_proxies` unless its listener,
header sanitation, network path, and bypass firewall are under the same administrative control.

Direct remote HTTP receives `426`; use HTTPS or the trusted proxy. Credential values are accepted
only as `Authorization: Bearer <UUID>`. They are never read from URLs, copied recording links, SDP,
or cookies.

## Roles

Administrator can perform User operations and configure cameras, recording, storage, privacy,
integrations, notifications, logging, identities, deletion, health, and server settings.

User can view authorized live and stored media/events, operate PTZ and presets, use implemented
group/publication operations, and maintain personal notification/review state. User cannot mutate
camera or server configuration, identities, retention, archives, security, logs, or diagnostics.

KeepPeek checks the role in one server command policy before dispatch. Hidden UI controls are only a
usability measure and never replace server authorization. Custom roles and per-camera permissions
are not implemented.

## Credentials

First start creates an Initial Administrator credential. Local setup must retrieve its value once
before reporting remote access ready. The compatibility value is protected in owner-only
`secrets.toml`; `[access_credentials]` in `config.toml` stores its verifier and metadata. New named credentials store only a
SHA-256 verifier. A successful create or rotate operation returns the raw key once.

Credential metadata includes stable identity, name, optional bounded description, role,
created/rotated/last-used/expiry/revocation times, enabled state, and revision. Rotation, disable,
revoke, or expiry invalidates active work for that credential. A revoked credential cannot be
re-enabled.

The browser keeps a remote key only in the live `ControlClient` instance. It is not written to the
URL, rendered as hidden text, logged, or saved in `localStorage` or `sessionStorage`. A reload
requires remote sign-in again. Deliberate create/rotate/first-run panels show a returned key until
the Administrator copies, downloads, or hides it.

## Sessions and revocation

WebRTC session IDs are random non-zero 64-bit values. They are bound to principal, effective
address, role, client classification, credential revision, creation time, activity time, and
expiry. The defaults are 30 minutes idle, 24 hours absolute, 64 sessions per principal, and 128 per
address.

`POST /delete` is owner-bound and idempotent. Administrator session revocation is a separate
control operation. Credential lifecycle changes close matching WebRTC sessions and authenticated
log streams. The server checks expiration every 100 ms; the SSE reader checks cancellation at most
every 100 ms. WebRTC teardown allows up to one second for graceful close and reports a warning if
that bound is exceeded. Normal session cleanup releases PTZ movement, searches, discovery, stored
cursors, and queued transfers.

## Audit and metrics

The bounded access audit records authentication outcomes, credential changes, session lifecycle,
denied commands, and classification failures. Records include non-secret principal, role, action,
target, result, classification reason, and UTC timestamp. They exclude raw keys, verifiers,
Authorization fields, cookies, SDP, logs, and media. New records are visible immediately and flush
atomically to `config.toml` within one second and during graceful shutdown. On upgrade, a legacy
`access.toml` is imported once and removed after the consolidated file is written successfully.

`/metrics` exposes fixed label-free access counters and gauges for authentication, authorization,
session creation/revocation, active sessions, and active credentials. `/logs`, `/logs/snapshot`,
`/metrics`, and access administration require Administrator.
