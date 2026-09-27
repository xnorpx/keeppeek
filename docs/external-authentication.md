# External authentication proposal

Status: implementation in progress; the owner approved the initial HTTP and
protobuf contract scope on 2026-09-25 and restricted further expansion to protobuf
on 2026-09-26. This is the contract checkpoint for
[#123](https://github.com/xnorpx/keeppeek/issues/123), the next ranked item after
#172 in [the release roadmap](https://github.com/xnorpx/keeppeek/issues/147).
Source baseline: `7ab761bb5190a741253b14cb487c2b969659a73b`.
The issue remains the implementation and acceptance tracker.

For deployment examples, migration steps, and failure recovery, see the
[operator guide](external-authentication-operations.md).

## Existing behavior to preserve

- `src/server.rs` owns HTTP authentication, principals, WebRTC session authorization,
  and session closure. External authentication must extend that owner, not introduce
  a parallel authorization path.
- `src/server/camera_access.rs` resolves camera grants and checks media delivery.
  Contrary to the issue's older introductory text, per-camera grants already exist.
  External identities must use them. Existing credential permissions do not change.
- Trusted-local administration and the forwarding-chain rules in
  [access control](access-control.md) remain unchanged. External authentication is
  opt-in and must not reinterpret arbitrary identity headers as local access.
- Settings and administration stay on protobuf/WebRTC. Browser sign-in happens
  before a WebRTC session exists, so OIDC needs a narrowly scoped HTTP surface.

## OIDC library and validation boundary

Authorization requests, PKCE generation, and code exchange use
[`oauth2` 5.0.0](https://docs.rs/oauth2/5.0.0/oauth2/). JWS verification uses
[`jsonwebtoken` 11.1.0](https://docs.rs/jsonwebtoken/11.1.0/jsonwebtoken/) with its
AWS-LC backend, without PEM support or a custom cryptographic provider. The
initial `openidconnect` dependency was replaced because its mandatory `rsa`
dependency fails [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).
The dependency audit has no exception for this advisory.

The application applies [OIDC ID-token validation](https://openid.net/specs/openid-connect-core-1_0.html#IDTokenValidation)
after library signature verification. It accepts only RS256, PS256, ES256, or
Ed25519 EdDSA, advertised by the configured provider. Keys must match the key ID,
algorithm, curve, and any declared signature/verification use; ambiguous matches
fail closed. Token-supplied key URLs or keys never establish trust. Unsupported
critical, encrypted, compressed, and unencoded-payload headers are rejected.

Issuer and audience must match the configured strings. Extra audiences are not
trusted, and an authorized party, when present, must be the configured client.
Required claims have typed schemas. Nonce and optional access-token hash are
checked in constant time. Time validation uses one injected clock read after the
token and any refreshed keys arrive: expiry is strict, issue time permits at most
60 seconds ahead or five minutes behind, and optional not-before permits at most
60 seconds ahead. Numeric dates must be nonnegative whole Unix seconds that fit
the millisecond clock. This explicit time check replaces the JWT library's
wall-clock expiry check; signature verification is never disabled.

[Discovery](https://openid.net/specs/openid-connect-discovery-1_0.html#ProviderConfigurationResponse)
requires the exact issuer and validated HTTPS endpoints through the pinned,
bounded transport. Confidential clients use `client_secret_basic`; an advertised
incompatible authentication method or PKCE method fails before code exchange.
The existing shared issuer cache and refresh budgets remain in force.

## Requested protected-contract scope

The current-task approval covers `api/openapi.yaml`,
`api/webrtc.proto`, `api/webrtc.md`, and corresponding authentication descriptions
in `api/README.md` and `api/backup.md`. It does not change `api/backup.proto`,
SDP, media payloads, or unrelated commands.

Further administration and verification additions must use protobuf. Do not add
HTTP routes, query parameters, request headers, or response modes beyond the
previously approved browser sign-in contract. In particular, `GET /auth/session`
does not accept a candidate-plan ID, and restore does not accept proof headers.

### Browser bootstrap HTTP

All responses below use `Cache-Control: no-store`. No endpoint returns provider
tokens, cookie values, raw claims, or secrets. Only explicitly configured HTTPS
origins are eligible; cookies are host-only, with `Secure`, `HttpOnly`,
`SameSite=Lax`, and `Path=/`. There is no cross-origin credentialed CORS support.

| Route                | Proposed contract                                                                                                                                                                                                |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /auth/session`  | Return enabled method IDs/display names, sanitized current identity when authenticated, and a session-bound CSRF token. Anonymous bootstrap establishes a bounded pre-login session.                             |
| `POST /auth/login`   | Accept provider ID and an optional validated same-origin relative return path. Require exact Origin and pre-login CSRF binding. Return a 303 redirect to the configured provider.                                |
| `GET /auth/callback` | Accept the authorization code or provider error and one-use state. Verify browser, provider, origin, nonce, PKCE, and token validation before issuing a new session cookie; return 303 to the saved return path. |
| `POST /auth/logout`  | Require exact Origin and session CSRF binding. Revoke the browser session and dependent WebRTC sessions, clear the cookie, and return 204.                                                                       |

Callback query credentials are the OIDC protocol exception, not bearer credentials
for application routes. Redact callback query strings from access logs and errors;
send `Referrer-Policy: no-referrer` and immediately redirect to a clean URL.
Return-path validation rejects absolute URLs, network-path references, backslashes,
and encoded bypasses. Missing or malformed credentials return 401; denied mapping
and Origin/CSRF failures return 403; invalid input returns 400; bounded-capacity
exhaustion returns 429; unavailable providers return 503. Provider failures use
stable public categories, not upstream response bodies.

Anonymous bootstrap uses a separate transaction cookie, not the authenticated
session cookie. It does not compete with bearer authentication. An authenticated
cookie plus bearer remains an error even if the cookie has expired: the UI must
explicitly clear the cookie through logout before switching methods. Logout clears
stale cookies with exact Origin and the bootstrap CSRF binding when no active
authenticated session exists. Method discovery must not create an authenticated
identity or prevent bearer fallback.

For replacement-method verification, `/auth/login` also accepts an optional
server-issued candidate-plan ID. An Administrator prepares that bounded,
five-minute plan through protobuf; it binds the initiating principal/session,
current revision, and SHA-256 fingerprint of the exact proposed configuration.
The callback for this transaction returns only a verification receipt tied to
that plan, never an authenticated application session. Receipt retrieval and
confirmed apply use protobuf, not a URL or provider-visible parameter. Any edit,
expiry, revocation of the initiating session, or revision change invalidates the
receipt. A trusted-proxy replacement must similarly prove its identity from its
configured immediate peer against the candidate; forwarding a claim via protobuf
does not constitute that proof.

The prepared plan returns a one-use start challenge through protobuf. Submit it
as the existing login form's CSRF token, together with the approved candidate-plan
ID. The server binds it to the exact candidate, provider, origin, and live initiating
session before establishing the OIDC transaction cookie. No candidate-aware
`GET /auth/session` call is part of this flow.

Candidate verification opens a separate browser window. The initiating page keeps
its WebRTC connection and in-memory bearer credential; it polls the receipt through
that connection. The callback window displays only a completion message and closes.
It never receives the receipt or changes the application's authenticated cookie.
Closing or revoking the initiating session invalidates the plan, including an
in-flight callback. No session rebinding or navigation-based recovery is supported.

For a different candidate origin, open the existing UI at that origin and pass the
start challenge with origin-checked `postMessage` to that exact window. The window
must skip normal authentication bootstrap and submit the form to its own origin.
Do not weaken the Origin or fetch-site checks to allow a cross-origin POST. Neither
the bearer credential nor the verification receipt is transferred to the window.

For a proxy candidate, `POST /auth/login` with the candidate-plan ID checks the
candidate's assertions on that immediate HTTP peer and returns 204 after recording
the proof. This branch does not redirect or create a browser identity. It requires
the same exact Origin and prepared challenge binding as OIDC candidate verification.
The initiating Administrator authorizes the plan through protobuf; no bearer token
is sent on this proof-only HTTP request. Candidate assertions are evidence, not a
second application authentication method.

Optional provider logout is disabled by default. When configured, logout can return
a 303 to an explicitly allowed provider logout endpoint, without an ID-token hint
or other provider token. Local revocation completes first and does not depend on
the provider. The 204 behavior remains the default.

### Protobuf administration

- Extend the existing configuration get/plan/apply lifecycle with typed external
  authentication settings and redacted validation results. Preserve atomic apply,
  revision checks, and secret references. Add typed confirmation for a concrete
  last-remote-administrator transition, not a generic bypass flag.
- Extend `ServerCommand` with bounded, paginated external identity/browser-session
  listing and explicit revocation. Keep existing WebRTC session list/revoke commands.
  Add optional authentication-method/provider/identity metadata to `AccessSession`.
  External identities are not fabricated bearer credentials.
- Return only a stable server identity ID, provider label, subject fingerprint,
  display name, role, applicable permission policy, session times, and revocation
  state. Keep raw provider subjects and claims out of administrative diagnostics.
  Users can inspect their own session; directory/list/revoke administration requires
  Administrator. Self-logout uses the HTTP contract above.
- Preserve unknown-field compatibility, existing tags and enum meanings. Allocate
  additive tags against current main after approval and regenerate bindings.
  Coordinate shared schema changes with #172 before integration.

## Identity and permission decisions

OIDC identity is keyed by exact issuer plus subject. Proxy identity is keyed by a
configured provider namespace plus subject. Email and display name are mutable
metadata, never identity keys. Persist only the bounded JIT identity record and
policy binding in `config.toml`; keep reusable private strings in `secrets.toml`.

Rules map explicitly to Administrator or User. No matching rule denies access;
ambiguous rules deny rather than choosing the most privileged match. Administrators
retain the existing unrestricted camera policy. Every external User rule must
explicitly specify the existing camera-access model: `all_cameras`, camera IDs,
or group IDs. Omitted policy is invalid; an explicitly empty policy grants no
cameras. Do not copy grants from a similarly named credential or infer unrestricted
access. Group resolution and removal use the current camera-access owner.

Dashboard audiences remain independent from camera grants. Extend existing
audience-ID validation and Administrator identity selection to include stable JIT
identity IDs alongside credential IDs. Preserve existing serialized audience
fields and credential IDs. A selected dashboard grants no additional camera access;
new external Users see only dashboards whose existing audience policy allows them.

Authentication-method ambiguity is rejected: a remote request presenting a bearer
credential and a browser cookie cannot silently select a different principal.
Trusted-proxy identity and other remote methods likewise cannot compete. Invalid
assertions from a configured identity proxy fail closed; identity headers from
untrusted transport peers are ignored and confer no privileges.

A proxy-authenticated browser cookie is valid only with matching assertions from
that proxy on every HTTP request. These corroborate one identity; they are not
competing methods. Missing, changed, or mismatched assertions deny the cookie.
Proxy cookies use the same Origin and CSRF rules as OIDC cookies.

Proxy trust uses the immediate socket peer, separately from forwarded client
classification. Require exact configured header names, one value per assertion,
explicit role rules, and optional shared-secret evidence. Reject malformed,
duplicate, oversized, or unmapped assertions. Do not offer mTLS configuration
unless the actual TLS peer certificate is available to the server; a header is
not certificate evidence. Proxy examples must strip caller assertions and emit a
sanitized forwarding chain. Existing trusted-local classification remains a local
Administrator path, so deployments needing external authentication for LAN users
must explicitly narrow `local_networks`; enabling OIDC does not do that silently.

## Sessions, enforcement, and revocation

Use a maintained OIDC implementation after reviewing its official documentation;
do not implement JWS verification. Follow
[OIDC ID-token validation](https://openid.net/specs/openid-connect-core-1_0.html#IDTokenValidation)
and [OAuth Security BCP](https://www.rfc-editor.org/rfc/rfc9700.html).
Discovery and token exchange use verified HTTPS, bounded responses and deadlines,
and configured endpoint/network policy. Private issuers require explicit operator
configuration, not an exception discovered from untrusted metadata.

Store only hashes of opaque browser handles. Keep login state, nonce, PKCE verifier,
and CSRF bindings server-side; consume login transactions once. Rotate handles on
login and privilege change. Do not persist provider access/refresh tokens.
Sessions expire at the configured idle/absolute limits, survive provider outage
only until those limits, and are invalidated on server restart or config restore.
Persist JIT identities, not active sessions. Changes to identity mappings, provider
configuration, or camera permissions invalidate affected session revisions.

Session discovery expires an invalid, expired, or revoked session cookie before
returning anonymous sign-in state. It retains the separate login cookie and its
CSRF binding so the browser can start a fresh provider login. The stale cookie
never authorizes a request or falls back to proxy identity on that same request.

| Surface                                                                            | Required enforcement                                                                                                                     |
| ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `POST /create`, `POST /delete`                                                     | Same principal, ownership and current revision; cookie authentication additionally requires exact Origin and CSRF binding.               |
| `POST /config/apply`                                                               | Administrator, current revision, exact Origin and CSRF for cookie authentication; restore cannot bypass lockout validation.              |
| `GET /config/export`, `/logs`, `/logs/snapshot`, `/metrics`, `/recording-coverage` | Preserve existing role/camera restrictions with the same external principal; revalidate long-lived delivery and cancel on revocation.    |
| WebRTC control, settings, health, state namespaces                                 | Revalidate identity/session revision and role on every command, including after signaling succeeds. Preserve namespace ownership checks. |
| Live media, PTZ/talk, playback, event search, export/download                      | Resolve existing camera grants at admission and delivery. Revocation cancels queued work, streams, searches, and watches.                |

Invalidation occurs before acknowledgment of logout/revoke/config apply. No new
authorization succeeds with the old revision afterward. Preserve the existing
100 ms expiry scan and at-most-one-second graceful WebRTC close bound; transport
closure does not substitute for per-command/delivery authorization. Audit bounded
failure categories, mapping changes, login/logout, revoke, and sensitive access,
without retaining raw claims, cookies, codes, provider tokens, or secret values.

## Migration and lockout prevention

Default behavior remains local administration plus existing bearer credentials.
Enabling external authentication requires explicit selection of enabled remote
methods. A mixed-mode migration records an explicit transition deadline; deadline
expiry must never silently remove the last usable remote Administrator method.
Require a fresh successful remote Administrator authentication against the proposed
replacement configuration, an expiring proof bound to that configuration revision,
and explicit confirmation before removing the last existing method. Candidate
authentication proves the replacement but does not grant access before activation.
Outage, stale proof, validation failure, or persistence failure leaves the working
configuration and its sessions unchanged. Local recovery remains available.

Use one shared last-remote-administrator check for configuration apply/restore,
mapping removal, identity disable/revoke, and existing credential disable/revoke
commands. Count usable Administrator paths, not merely enabled method names.
Add optional typed candidate-proof/confirmation inputs to affected protobuf
commands. For ZIP restore, bind the plan to the exact archive digest; preparation,
receipt retrieval, and confirmation must use authenticated protobuf. The existing
`POST /config/apply` route must not gain proof headers or a dry-run response mode.
For a lockout-sensitive restore, the server must match the uploaded archive to its
confirmed authorization before staging any changes. The protobuf flow uploads bounded chunks for inspection,
reuses replacement-Administrator proof, and confirms the exact ZIP on the original
control connection. See [the protocol contract](../api/webrtc.md#configuration-archive-verification).
Stale or missing proof
rejects a lockout transition before persistence. No confirmation is required for
ordinary changes that preserve a usable remote Administrator path.

The unchanged-path shortcut requires a non-expiring Administrator credential with
bearer authentication remaining enabled without a transition deadline, or an
enabled Administrator identity whose provider, mappings, allowed origins, and
relevant transport/network trust settings remain unchanged. A credential expiring
later does not protect against a scheduled lockout. Candidate proof must pass JIT
admission against the post-operation directory, including disabled-identity and
capacity checks; a valid provider token and Administrator rule alone are insufficient.
Revalidate this admission at commit, without retaining the raw subject or claims.

Rollback restores prior validated settings and invalidates external sessions; it
does not delete identity or audit records. Backup/restore preserves supported
secret references and identity policies while excluding cookies and provider tokens.

## Bounds and completion evidence

Initial proposed limits: 4 providers, 128 mapping rules, 128 camera/group IDs per
policy, 1,024 persisted external identities, 256 pending logins, five-minute login
transactions, 64 KiB metadata/token responses, 16 KiB selected claims, 64-byte
header names, 256-byte subjects, 64-byte display names, and 128 group assertions
of at most 256 bytes each. Reject over-capacity writes instead of evicting durable
identities. Browser sessions retain existing per-principal/address limits and have
a 4,096-session global cap. Outbound requests have a 30-second deadline; normal
key refresh has a five-minute floor, with at most one emergency unknown-key refresh
per issuer per minute across active and candidate configurations. Cache entries and
concurrent requests remain bounded.

Implement the issue's eight slices with tests before behavior changes. Required
evidence includes a real local TLS OIDC fixture, adversarial token/redirect/CSRF
tests, proxy trust matrices, role/camera matrices, active-session revocation,
lockout/failed-write tests, and secret scans. UI changes require the canonical
root `check.bat` gate and real-browser verification. Do not mark the issue complete
or open its implementing PR until all acceptance rows have final-build evidence.

Performance measurement uses 30 login runs and matching release-build bearer and
validated-cookie workloads. Proposed gate: bearer p95 regression no greater than
the larger of 5% or 1 ms; validated-cookie local authorization p95 within 1 ms of
bearer; revocation closure within one second. Record p50/p95/max, rejected-login
flood CPU, memory/cache high-water, hardware, workload, and commit. Identity-provider
network latency is measured separately, not hidden inside authorization overhead.

This document alone is not implementation, a completed security review, or evidence
that any acceptance criterion passes.
