# Operate external authentication

External authentication is opt-in. It preserves the two roles and the existing
trusted-local recovery policy. Configure it in **Settings → Access**, through the
protobuf configuration plan/verify/apply workflow. Do not expose the HTTP listener
until its TLS boundary, proxy trust, and firewall are configured.

## OIDC setup

Register a confidential web client at your provider with the exact callback
`https://nvr.example.net/auth/callback`. Enable authorization-code flow and PKCE
S256. The issuer must use verified HTTPS. A private issuer additionally needs
explicit `private_networks`; never disable certificate verification.

This is an example target configuration, not a replacement for your entire file:

```toml
[external_auth]
allowed_origins = ["https://nvr.example.net"]
bearer_enabled = false

[[external_auth.providers]]
id = "company"
name = "Company sign-in"

[external_auth.providers.method]
kind = "oidc"
issuer = "https://identity.example.net"
client_id = "keeppeek"
client_secret = "{secret:KEEPPEEK_OIDC_CLIENT_SECRET}"
redirect_uri = "https://nvr.example.net/auth/callback"
scopes = ["openid", "profile", "groups"]
display_name_claim = "name"

[[external_auth.providers.mappings]]
claim = "groups"
value = "keeppeek-administrators"
role = "administrator"

[[external_auth.providers.mappings]]
claim = "groups"
value = "keeppeek-viewers"
role = "user"
camera_access = { all_cameras = false, group_ids = ["outdoor"], camera_ids = [] }
```

Add `KEEPPEEK_OIDC_CLIENT_SECRET` to the existing companion `secrets.toml`, then
enter only its reference in the editor. Public clients may omit `client_secret`.
Use claims and scopes actually emitted by your provider. A person matching both
example groups is denied: mappings must select exactly one role. External User
rules must explicitly declare camera access; they do not inherit another user's
permissions. Provider issuer plus subject identifies the person, not their email.

Discovery and token endpoints must remain on the issuer origin or explicitly
listed `endpoint_origins`. Provider HTTP redirects are not followed. Configure
each hostname exactly; wildcard origins, callback aliases, and subpath deployments
are not accepted. Cookies are host-only and origin-bound: signing in on one
hostname does not sign in on another. A provider's callback origin must match the
origin used to start its normal login. Use separate provider entries when needed.

## Identity-aware proxy setup

Use a dedicated trusted proxy peer and firewall KeepPeek against direct remote
connections. Forwarded-address trust and identity-assertion trust are separate:
configure both deliberately. This example keeps direct loopback recovery while
requiring other clients, including LAN clients, to authenticate:

```toml
[access]
local_networks = ["127.0.0.0/8", "::1/128"]
trusted_proxies = ["127.0.0.1/32"]
require_secure_remote = true

[external_auth]
allowed_origins = ["https://nvr.example.net"]
bearer_enabled = false

[[external_auth.providers]]
id = "gateway"
name = "Company gateway"

[external_auth.providers.method]
kind = "proxy"
trusted_peers = ["127.0.0.1/32"]
subject_header = "X-KeepPeek-Subject"
role_header = "X-KeepPeek-Role"
name_header = "X-KeepPeek-Name"

[[external_auth.providers.mappings]]
claim = "role"
value = "nvr-admin"
role = "administrator"

[[external_auth.providers.mappings]]
claim = "role"
value = "nvr-user"
role = "user"
camera_access = { all_cameras = true, camera_ids = [], group_ids = [] }
```

The proxy must replace caller-supplied identity assertions. Here is an Nginx
location template inside an HTTPS server. `identity_gateway` denotes an existing
authentication service that returns trusted subject, role, and name response
headers after validating its own login session. Its own login flow and upstream
TLS configuration are deployment-specific.

```nginx
location = /_identity_check {
    internal;
    proxy_pass http://identity_gateway/verify;
    proxy_pass_request_body off;
    proxy_set_header Content-Length "";
    proxy_set_header X-KeepPeek-Subject "";
    proxy_set_header X-KeepPeek-Role "";
    proxy_set_header X-KeepPeek-Name "";
}

location / {
    auth_request /_identity_check;
    auth_request_set $identity_subject $upstream_http_x_keeppeek_subject;
    auth_request_set $identity_role $upstream_http_x_keeppeek_role;
    auth_request_set $identity_name $upstream_http_x_keeppeek_name;
    proxy_set_header X-KeepPeek-Subject $identity_subject;
    proxy_set_header X-KeepPeek-Role $identity_role;
    proxy_set_header X-KeepPeek-Name $identity_name;
    proxy_set_header Authorization "";
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header Forwarded "";
    proxy_set_header X-Real-IP "";
    proxy_set_header X-Forwarded-Host "";
    proxy_set_header X-Forwarded-Proto "";
    proxy_set_header Host $host;
    proxy_pass http://127.0.0.1:8081;
}
```

Nginx's [auth-request module](https://nginx.org/en/docs/http/ngx_http_auth_request_module.html)
must be enabled. The asserted headers come from its validated subrequest, never
`$http_x_keeppeek_*` caller values; [proxy header replacement](https://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_set_header)
removes client copies. The example intentionally removes bearer authorization
because proxy identity and a bearer credential cannot compete on one request.
Repeat sanitation in every location that proxies to KeepPeek. Use a separate
explicitly configured path/peer for a bearer migration; do not exempt application
routes from authentication.

Optional `secret_header` and `shared_secret = "{secret:PROXY_SHARED_SECRET}"`
require a separately injected proxy-side secret on every request. Never derive it
from a caller header. No mTLS checkbox is offered: the HTTP owner cannot verify a
terminated client's certificate. Identity-provider peer networks cannot overlap.
IPv4-mapped IPv6 peers are normalized; configure the corresponding IPv4 CIDR.

KeepPeek requires matching proxy assertions on every request using a proxy cookie.
An identity mismatch invalidates its binding. Logging out revokes KeepPeek's cookie
and dependent work, but does not end the identity gateway's session; its next
validated assertion may sign the browser in again. End the upstream session too
when a shared workstation must remain signed out.

## Migrate, confirm, and recover

1. Keep a known working remote Administrator session and direct trusted-local
   recovery access. Back up the existing configuration privately.
2. Review active methods. For mixed mode, explicitly enable bearer authentication
   and choose a short transition deadline in Unix milliseconds. Do not assume an
   enabled method is a usable Administrator path.
3. Plan the candidate. Authenticate a replacement Administrator against that exact
   candidate in the separate verification window, or prove a retained permanent
   bearer credential when returning to bearer-only mode. The new method has no
   application privileges before activation.
4. Confirm and apply on the original control connection. A draft edit, expiry,
   origin/configuration/secret change, or loss of the initiating session requires
   new proof. Failure leaves the previous working configuration in place.
5. Verify a fresh remote login and User restrictions before ending the transition.
   Bearer access, including existing bearer sessions, stops at the deadline.
   Planning already treats that future removal as a lockout transition: an
   expiring bearer method cannot stand in for a verified replacement Administrator.

Configuration ZIP restore uses the same guard. Remote Administrators upload the
exact ZIP through bounded protobuf preparation, inspect its candidate, prove the
replacement when required, and explicitly confirm it before the existing HTTP
upload. The archive digest and initiating session bind the one-use authorization.
The archive's own secrets are validated. A different ZIP cannot reuse the proof.

Browser sessions, pending logins, and confirmation proofs are memory-only. Restart
or restore invalidates them. Provider outage does not invalidate an already
validated session before its normal idle/absolute deadline. Mapping/provider or
identity changes invalidate affected revisions; revoke closes dependent control,
media, downloads, searches, and watches before acknowledgment.

After expiry or session revocation, sign in again. Session discovery clears the
unusable cookie without requiring a manual browser-cookie reset. Revoking an
identity still denies that identity on a fresh provider login.

If remote authentication becomes unusable, connect directly from a configured
trusted-local address; do not spoof forwarding headers or widen trusted networks.
If the guarded UI cannot prove a replacement, restore the prior validated
authentication settings in the existing `config.toml` on the host while retaining
identity/audit records, then restart to invalidate old sessions. Protect configuration archives:
they contain `secrets.toml`, even though settings editors show only references.

## Troubleshooting and safe diagnostics

| Symptom          | Check                                                                                                      |
| ---------------- | ---------------------------------------------------------------------------------------------------------- |
| 400 at login     | Exact provider ID, callback/return path, and bounded form fields.                                          |
| 401              | Expired/revoked cookie, stale login state, or missing authentication. Start a fresh login.                 |
| 403              | Exact Origin/CSRF, one matching role rule, proxy peer and sanitized assertions.                            |
| 409 during apply | Draft/configuration revision, live initiating session, proof expiry, or last-Administrator guard. Re-plan. |
| 426              | HTTPS or the explicitly trusted TLS proxy boundary.                                                        |
| 429              | Login/session/proof capacity. Wait for the bounded window; do not disable protections.                     |
| 503 at login     | Issuer reachability, certificate trust, discovery/JWKS policy, and provider availability.                  |

Login start and callback share a 60-second limit of 30 attempts per immediate
transport IP and 10 per browser handle. New anonymous/proxy bootstrap admissions
also count; authenticated session polling does not. Reverse proxies share the
peer-IP budget, so also enforce appropriate upstream per-client limits. Registry
capacities are 1,024 peer addresses and 4,096 hashed browser handles; full registries
reject new keys instead of evicting live limits. Configuration changes do not reset
these budgets. Issuer discovery and emergency key-refresh budgets are shared across
active and candidate configurations.

Inspect bounded audit outcome categories and counts, not raw callback URLs. At the
edge and provider, disable query-string logging for `/auth/callback` and redact
Authorization, Cookie, Set-Cookie, codes, tokens, client secrets, and assertions.
Do not attach a raw browser HAR, configuration ZIP, or `secrets.toml` to a bug report.
Optional provider logout uses the configured allowed endpoint without an ID-token
hint; local revocation completes even if that provider is unavailable.

See the [complete field reference](../book/src/configuration-reference.md#external-authentication-configuration),
[security design](external-authentication.md), and
[protobuf restore workflow](../api/webrtc.md#configuration-archive-verification).
