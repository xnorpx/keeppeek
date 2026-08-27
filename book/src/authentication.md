# Authentication and access control

KeepPeek is local-first: cameras and recordings stay on the recorder, and KeepPeek does not expose
itself through a cloud relay. Access depends on two decisions:

1. Is the client on a trusted local network?
2. If not, which remote credential and role does the client have?

Local clients use KeepPeek as **Administrator** without signing in. Remote clients must sign in
with a named Bearer credential assigned either the **Administrator** or **User** role.

> Treat local network configuration as an authentication boundary. Any device in a configured
> local range receives Administrator access.

## First run

On first start, KeepPeek creates a cryptographically random **Initial Administrator** credential.
Open the setup page from a local client and select **Retrieve initial key**. The key is displayed
only for that operation.

Save it immediately using **Copy** or **Download**, then choose **Hide permanently**. KeepPeek does
not log the key and cannot display it again. If it is lost, a local Administrator can rotate the
Initial Administrator credential from **Settings > Access & roles**.

KeepPeek does not report remote access as ready until the initial key has been retrieved. Local
camera setup remains available while remote access is pending.

## Local and remote clients

KeepPeek classifies the effective client address before checking a credential.

- **Local:** the address is in `access.local_networks`. The client becomes Administrator without a
  sign-in screen.
- **Remote:** every other address. The client must provide an active, unexpired Bearer credential.
- **Unknown or malformed:** treated as remote and denied without a valid credential.

The default local networks include IPv4 and IPv6 loopback, RFC 1918 private networks, IPv4 and IPv6
link-local networks, and IPv6 unique-local networks. IPv4-mapped IPv6 addresses are normalized
before matching.

Container bridge networks commonly fall within `172.16.0.0/12`, which is local by default. Narrow
the list if other containers on the host must not receive Administrator access.

Configure the policy in `config.toml`:

```toml
[access]
local_networks = ["127.0.0.0/8", "192.168.50.0/24", "::1/128"]
trusted_proxies = []
require_secure_remote = true
failed_authentication_limit = 5
failed_authentication_window_secs = 60
session_idle_timeout_secs = 1800
session_absolute_timeout_secs = 86400
max_sessions_per_principal = 64
max_sessions_per_address = 128
```

Setting `local_networks` replaces the defaults. Include the loopback ranges explicitly when you
customize it.

A VPN can use either policy:

- Add its client range to `local_networks` when every VPN client should be Administrator.
- Leave it out when each VPN client should sign in with an individual credential and role.

Restart KeepPeek after changing the network policy.

## Roles

KeepPeek has two fixed roles.

| Operation                                               | Administrator | User |
| ------------------------------------------------------- | :-----------: | :--: |
| View live and recorded media and events                 |      Yes      | Yes  |
| Operate PTZ and presets                                 |      Yes      | Yes  |
| Use personal notification and review state              |      Yes      | Yes  |
| Configure cameras, storage, recording, and integrations |      Yes      |  No  |
| Export, delete, or change retention                     |      Yes      |  No  |
| View logs, health diagnostics, and security audit       |      Yes      |  No  |
| Manage credentials and sessions                         |      Yes      |  No  |

The server enforces this policy for HTTP and every WebRTC control operation. Hidden navigation and
controls make the User interface clearer, but they are not the security boundary.

Custom roles and per-camera permissions are not currently available.

## Remote sign-in

Open the recorder through HTTPS and enter the issued UUID in the **Remote sign-in** form. The
browser keeps the key only in memory for the current page session. It is not placed in the URL,
rendered as hidden text, logged, or written to `localStorage` or `sessionStorage`.

Reloading or closing the page discards the key and requires sign-in again. Expired or revoked
sessions return to the sign-in screen without displaying the requested resource.

API clients send the same credential in the HTTP header:

```http
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
```

KeepPeek never accepts credentials in query parameters. Do not put a key in a copied recording
link, guest link, SDP document, or command line that may be retained in shell history.

## Managing credentials

Open **Settings > Access & roles** as an Administrator. The credential table shows each identity's
name, description, role, status, last use, expiry, and revision without revealing its key.

### Create

Choose **New credential**, enter a descriptive name, select Administrator or User, and optionally
set an expiry. The new key is shown once. Store it before hiding the result.

Use a separate named credential for each person, browser installation, service, or integration.
This makes audit records useful and allows one client to be revoked without affecting the others.

### Rotate

Rotate a credential when its key may have been copied or exposed. Rotation returns a replacement
key once, invalidates the previous revision, and closes work authenticated with the old key.

### Disable and enable

Disable a credential to suspend it without deleting its identity or history. Disabling closes its
active sessions. Enabling it increments the revision; clients must establish new sessions.

### Revoke

Revocation is permanent. It closes active sessions and prevents reconnecting with that credential.
A revoked credential cannot be enabled or rotated; create a replacement identity instead.

Credential metadata and SHA-256 verifiers are stored in the owner-only `access.toml` beside
`config.toml`. Raw keys are not stored there. The compatibility Initial Administrator key remains
protected in owner-only `secrets.toml`.

## Sessions and revocation

Each WebRTC session has a random opaque ID and is bound to its principal, role, effective client
address, network classification, credential revision, and expiry. A session ID does not authorize a
request by itself.

By default, sessions expire after 30 minutes without control activity or after 24 hours absolute,
whichever comes first. Credential expiry can shorten that lifetime. KeepPeek allows up to 64 active
sessions per principal and 128 per effective address.

Administrators can list and revoke sessions from **Settings > Access & roles**. A client can delete
only its own session. Repeating deletion for an already closed session is safe.

Credential rotation, disable, revoke, or expiry closes matching WebRTC and authenticated log
streams. KeepPeek checks expiration every 100 ms. WebRTC gets up to one second for graceful close,
after which KeepPeek reports a warning. Normal cleanup releases PTZ movement, stored-media cursors,
searches, discovery, and queued transfers owned by the session.

## Reverse proxies and HTTPS

Prefer a private VPN for remote access. If KeepPeek is Internet-reachable, terminate HTTPS at a
maintained reverse proxy, firewall the KeepPeek listener so clients cannot bypass that proxy, and
trust only the proxy's exact address.

Direct remote HTTP is rejected when `require_secure_remote = true`. A configured trusted proxy is
the declared TLS boundary: KeepPeek cannot inspect the client-side TLS connection after the proxy
terminates it.

KeepPeek accepts one forwarded-client contract: exactly one `X-Forwarded-For` field containing a
comma-separated chain of IP addresses without ports or quoted values. The proxy must remove any
client-supplied forwarding headers before setting it. KeepPeek walks the chain from right to left
and selects the first address that is not a trusted proxy.

Other forwarding headers make the contract ambiguous. Remove `Forwarded`, `X-Real-IP`,
`X-Forwarded-Host`, and `X-Forwarded-Proto` before forwarding to KeepPeek.

Example Nginx configuration:

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

Do not trust a proxy unless its listener, forwarding-header sanitation, network path, and bypass
firewall are all under the same administrative control.

## Audit and metrics

KeepPeek keeps a bounded security audit for remote authentication, credential changes, session
lifecycle, denied commands, sensitive Administrator operations, and proxy-classification failures.
Each record includes the principal, role, action, target, result, client classification, and UTC
timestamp. It never contains a raw key, verifier, Authorization header, cookie, SDP, log payload, or
media.

New records appear in memory immediately and are written atomically to `access.toml` within one
second and during graceful shutdown.

`GET /metrics` exposes label-free counters and gauges for authentication successes and failures,
authorization denials, session creation and revocation, active sessions, and active credentials.
Logs, metrics, diagnostics, and audit access require Administrator.

## Troubleshooting

### A local browser shows Remote sign-in

Check the browser's source address and `access.local_networks`. A forwarding header from an
untrusted peer intentionally forces remote classification. If a proxy is involved, configure its
exact address in `trusted_proxies` and send only the supported `X-Forwarded-For` contract.

### Remote access returns HTTP 426

The request reached KeepPeek over unprotected direct HTTP while `require_secure_remote` is enabled.
Use HTTPS through a configured trusted proxy or connect through a VPN.

### Remote sign-in is temporarily limited

Five failed attempts from one effective address within 60 seconds are rejected by default. Wait for
the window to expire, verify the key, and confirm the credential is enabled, unexpired, and not
revoked.

### A credential works for viewing but not Settings

It has the User role. Issue an Administrator credential only when that client must change recorder
configuration or manage security.

### A key was lost or exposed

From a local Administrator session, rotate the affected credential. Store the replacement key
before hiding it, then update only the client or integration that owns that named credential.
