# Authentication and access control

This chapter explains KeepPeek's local-first trust model, trusted-local vs remote classification, Administrator and User roles, per-camera/group grants, HTTP authentication endpoints, bearer and external sign-in (OIDC and identity-aware proxy), session revocation, reverse-proxy HTTPS rules, and audit/metrics boundaries. Configuring access requires **Administrator** privileges; external authentication is opt-in and never disables trusted-local recovery.

## Access model

KeepPeek evaluates every request in two steps:

1. **Network classification:** Is the client's effective IP address inside `access.local_networks`?
2. **Remote identity:** If not local, does the client present a valid named bearer credential or an active opt-in external authentication session (OIDC or identity-aware proxy)?

| Classification      | Source address rule                    | Resulting access                                                |
| ------------------- | -------------------------------------- | --------------------------------------------------------------- |
| Local               | Inside `access.local_networks`         | **Administrator** automatically, no sign-in prompt              |
| Remote              | Outside `access.local_networks`        | Must authenticate with bearer key or configured external method |
| Unknown / malformed | Missing or invalid forwarded IP / peer | Treated as remote; denied without a valid credential            |

> **Security boundary:** Treat `access.local_networks` as an authentication boundary. Every device in a configured local range receives full **Administrator** access without signing in.

## First run

On first startup, KeepPeek generates a cryptographically random **Initial Administrator** key (UUID).

1. From a trusted-local browser, open `/setup` (or **Settings → Access**).
2. Select **Retrieve initial key**. The key is shown only for this operation and is never logged.
3. Save it immediately using **Copy** or **Download** (`keeppeek-initial-administrator-key.txt`), then select **Hide permanently**.

KeepPeek reports remote access as unready until the initial key is retrieved, while local camera setup remains available. If the key is lost, a local Administrator can rotate the **Initial Administrator** credential in **Settings → Access**.

## Local networks and reverse-proxy classification

Default `access.local_networks` covers IPv4/IPv6 loopback, RFC 1918 private ranges, IPv4/IPv6 link-local ranges, and IPv6 unique-local ranges. IPv4-mapped IPv6 addresses are normalized before matching.

- **Docker caution:** Container bridge ranges commonly fall inside `172.16.0.0/12` (local by default). Narrow `local_networks` if sibling containers must not get Administrator access.
- **Replacement semantics:** Setting `local_networks` in `config.toml` replaces all defaults; include `127.0.0.0/8` and `::1/128` explicitly when customizing.
- **VPN policy:** Include the VPN client CIDR in `local_networks` to grant all VPN clients Administrator access, or omit it to require individual credentials and roles.
- **Apply change:** Restart KeepPeek after editing `[access]` network settings.

See [Access policy](./configuration-reference.md#access-policy) for every `[access]` key, default, and bound.

## Roles

KeepPeek enforces two fixed roles across every HTTP endpoint and WebRTC control command. Hidden UI controls improve clarity for Users but are never the security boundary. Custom roles are not supported.

| Capability                                                        | Administrator |      User       |
| ----------------------------------------------------------------- | :-----------: | :-------------: |
| View live media, recordings (**Keep**), and events                |      Yes      | Granted cameras |
| Operate PTZ, recall presets, play supported quick replies/chimes  |      Yes      | Granted cameras |
| Use personal notification and event-review state                  |      Yes      | Granted cameras |
| Use browser **Talk** and **Broadcast talkback**                   |      Yes      |       No        |
| Add/configure cameras, storage, recording, runtime profiles, MQTT |      Yes      |       No        |
| Export evidence, delete recordings, change retention or backups   |      Yes      |       No        |
| View logs, health diagnostics, `GET /metrics`, and security audit |      Yes      |       No        |
| Manage credentials, camera grants, external auth, and sessions    |      Yes      |       No        |

## Camera and dashboard access

1. Open **Settings → Access** (`/settings#access`) as an Administrator.
2. In **Access credentials**, select the person icon (**User access**) beside a **User** credential.
3. Choose **Everything** (default; includes current and future groups and cameras) or **Selected groups and cameras**.
4. When restricted, select camera groups, individual cameras, or both. Matching **either** an allowed group or an explicit camera ID grants access; selecting neither grants zero cameras.
5. Save. Saving camera grants immediately closes that User's active sessions so they sign in again with updated scope. Stale concurrent saves return a revision conflict without discarding your draft.

Key boundaries:

- **Camera groups** come from configuration namespaces (for example, `[outdoor.front_door]` belongs to `outdoor`). Each credential supports up to 128 `group_ids` and 128 `camera_ids`, and cannot combine `all_cameras = true` with explicit IDs.
- **Server-wide enforcement** covers live and recorded streams, event thumbnails/previews, notification inbox/history, recording coverage, and PTZ/motion-read APIs. Grants never unlock Administrator-only operations such as export or camera configuration.
- **Dashboard audiences are separate.** Sharing a dashboard layout (`peek_layouts`) controls who can see that layout; it never grants access to denied cameras. A User sees only permitted camera tiles and cannot modify or share server-owned layouts, while the Administrator's full layout stays intact.
- **Trusted-local precedence.** To restrict someone on your LAN or VPN, exclude their address from `access.local_networks` so they authenticate as a remote **User**.

Camera grants persist in `access_credentials.credentials[].camera_access` and dashboards in `peek_layouts`; see [Credential records](./configuration-reference.md#credential-records) for exact fields and limits.

## HTTP authentication endpoints

All `/auth/*` responses include `Cache-Control: no-store` and `Referrer-Policy: no-referrer`. Browser auth endpoints enforce HTTPS (or a configured trusted proxy) and same-origin `Sec-Fetch-Site` checks. Credentials are never accepted in URL query parameters.

| Method & path        | Purpose                                                                                                                                                   |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /auth/session`  | Reports `local`, `bearer_enabled`, configured external `methods`, active `identity`, and CSRF token; bootstraps proxy sessions and clears invalid cookies |
| `POST /auth/login`   | Starts an OIDC authorization-code + PKCE S256 login flow for a configured provider                                                                        |
| `GET /auth/callback` | Completes the OIDC callback, validates tokens/claims, and issues the host-only session cookie                                                             |
| `POST /auth/logout`  | Revokes the active browser session cookie and dependent WebRTC/control connections                                                                        |
| `POST /create`       | Creates a WebRTC session (`Authorization: Bearer <UUID>` or valid external session cookie)                                                                |
| `POST /delete`       | Closes a WebRTC session (a client may delete only its own session; idempotent)                                                                            |

## Bearer credentials and remote sign-in

### Browser and API sign-in

- **Browser:** Open KeepPeek over HTTPS and enter the UUID in **Remote sign-in**. The browser keeps the key in memory only—never in the URL, logs, `localStorage`, or `sessionStorage`. Reloading or closing the tab discards the key.
- **API clients:** Send `Authorization: Bearer <UUID>` on HTTP requests. Never pass keys in query parameters, copied links, SDP payloads, or shell history.

### Credential lifecycle (**Settings → Access**)

Create a dedicated named credential for every person, browser, service, or integration (up to 128 credentials). Metadata, camera grants, and SHA-256 verifiers persist atomically under `[access_credentials]` in `config.toml`; raw keys are never stored there (the compatibility Initial Administrator key lives in owner-only `secrets.toml`).

| Action                | Effect                                                                                                 |
| --------------------- | ------------------------------------------------------------------------------------------------------ |
| **New credential**    | Sets name (<=64 B), optional description (<=256 B), role, optional expiry (1–3,650 d). Key shown once. |
| **Rotate credential** | Issues a replacement key once, increments revision, and closes sessions on the old key.                |
| **Disable / Enable**  | Suspends without deleting history; disabling closes sessions; enabling bumps revision.                 |
| **Revoke credential** | Permanent (requires **Confirm revoke**). Closes sessions; cannot be re-enabled/rotated.                |

## External sign-in (OIDC and identity-aware proxy)

Configure opt-in OIDC providers or identity-aware proxies under **Settings → Access** using exact origins and explicit role/camera mappings:

- **Cookies and tokens:** Browser cookies are `Secure`, `HttpOnly`, host-only, and bound to a single allowed origin. Provider tokens never reach the web UI. Restarting KeepPeek invalidates all external browser sessions.
- **Secrets:** Provider secrets remain `{secret:KEY}` references into `secrets.toml`.
- **Anti-lockout guard:** Any change or configuration ZIP restore that alters or removes the last usable remote Administrator path requires fresh verification against the exact proposed candidate and explicit confirmation on the original control connection. Unavailable providers, stale proofs, or failed writes leave the active config untouched.
- **Desktop client note:** Cross-origin OIDC login and popup-based admin proof require a standard browser; see [Desktop application](./desktop-app.md).

See [External authentication configuration](./configuration-reference.md#external-authentication-configuration) for TOML fields and [Operate external authentication](https://github.com/xnorpx/keeppeek/blob/main/docs/external-authentication-operations.md) for reverse-proxy header sanitation, mixed-mode migration deadlines, and troubleshooting.

## Sessions, limits, and audit

Each WebRTC session uses a random opaque ID bound to its principal, role, effective address, classification, credential revision, and expiry; a session ID never authorizes a request alone.

| Control / limit                    | Default / bound                                                             |
| ---------------------------------- | --------------------------------------------------------------------------- |
| Session idle / absolute timeout    | 30 minutes (`1800` s) / 24 hours (`86400` s); credential expiry can shorten |
| Concurrent session caps            | 64 per principal, 128 per effective address                                 |
| Failed bearer auth rate limit      | 5 failures per 60 s per effective address (up to 1,024 tracked addresses)   |
| External login/callback rate limit | 30 attempts per 60 s per peer IP; 10 per 60 s per browser handle            |
| Expiration sweep & graceful close  | Swept every 100 ms; 1 s graceful WebRTC close before warning                |
| Bounded security audit buffer      | 1,024 in-memory events (resets on restart)                                  |

Rotating, disabling, revoking, or expiring a credential (or revoking a session in **Settings → Access → Active sessions**) terminates matching WebRTC sessions and log streams and releases owned PTZ movement, talkback, stored-media cursors, searches, discovery, and queued transfers.

**Audit and metrics:** **Security audit** records principal, role, action, target, result, client classification, and UTC timestamp—never raw keys, verifiers, `Authorization` headers, cookies, SDP, log text, or media. Audit history, active sessions, and last-use timestamps live in memory and reset on restart. For durable compliance evidence, collect audit records externally before eviction/restart and retain protected service logs off-host. `GET /metrics` exposes label-free auth/session counters and gauges and requires **Administrator**.

## Video walkthrough

This walkthrough demonstrates creating named credentials, assigning roles, scoping per-user camera and group grants in **User access**, and revoking an active session in **Settings → Access**:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.mp4">Download the access and roles demo</a>.
</video>

## Reverse proxies and HTTPS

Prefer a private VPN for remote access. If KeepPeek is exposed behind a reverse proxy, terminate HTTPS at the proxy, firewall port `8081` against direct remote bypass, and list only the proxy's exact CIDR in `access.trusted_proxies`. With `require_secure_remote = true` (default), direct remote HTTP returns `HTTP 426`.

KeepPeek accepts one strict forwarding contract: a single `X-Forwarded-For` header with comma-separated IPs (no ports or quotes), walked right-to-left to the first non-trusted IP. Strip `Forwarded`, `X-Real-IP`, `X-Forwarded-Host`, and `X-Forwarded-Proto` at the proxy:

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

```toml
[access]
trusted_proxies = ["127.0.0.1/32"]
require_secure_remote = true
```

## Troubleshooting

| Symptom                                        | Cause and resolution                                                                                                                                                          |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Local browser shows **Remote sign-in**         | Source IP is outside `local_networks`, or an untrusted peer sent a forwarding header. Fix `trusted_proxies` and strip non-`X-Forwarded-For` headers.                          |
| Remote request returns `HTTP 426`              | Plain remote HTTP hit KeepPeek with `require_secure_remote = true`. Use HTTPS via a trusted proxy (a VPN alone does not satisfy TLS checks if kept outside `local_networks`). |
| Remote sign-in rejected / rate limited         | Exceeded 5 failed attempts in 60 s (or external login budget). Wait out the window and confirm credential is active and unexpired.                                            |
| Credential works for viewing, not **Settings** | Credential has the **User** role. Issue an **Administrator** credential only for operators who manage config/security.                                                        |
| Shared dashboard is empty or missing tiles     | Dashboard audience does not grant cameras. Update **User access** for that credential, save, and sign in again.                                                               |
| Restricted person still has **Administrator**  | Their effective IP matched `access.local_networks`. Remove their subnet from `local_networks` while keeping local loopback recovery.                                          |
| Key was lost or exposed                        | From a local Administrator session, rotate that named credential, store the new key, and update only that client.                                                             |

### Session expired after a permission change

Saving **User access** camera grants deliberately closes that credential's active sessions, as do rotation, disable, revoke, and expiry. If a browser tab opened during that transition stays on an expired-session error (such as **Your session expired or was revoked** or **API session expired or was revoked**), reload the page and sign in again with the current key.

For rotated credentials, enter the replacement key. Disabled, expired, or revoked credentials will not reconnect after reload; ask an Administrator to verify the credential's status in **Settings → Access**.

## Verify access before deployment

1. Create a disposable **User** credential and test from a genuinely remote-classified IP (local browser checks do not test remote boundaries).
2. Verify allowed vs denied cameras across live view, **Keep**, event previews, PTZ, and a shared dashboard containing one denied camera, and confirm **Settings**, exports, and diagnostics are denied.
3. Modify the User's camera grants, confirm the old session closes, reconnect to verify the new scope, then revoke the disposable credential.
4. Record your proxy/VPN topology in your [release qualification evidence](./release-readiness.md#representative-deployment-matrix).

Configuration ZIPs include `[access_credentials]` and grants (but not active sessions, last-use times, or audit history). Ensure you retain a valid local recovery path before restoring a ZIP from another host; see [what survives a restart](./backup-and-restore.md#what-survives-a-restart).
