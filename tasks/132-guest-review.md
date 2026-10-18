# Guest review links (#132)

Issue: https://github.com/xnorpx/keeppeek/issues/132

Local branch: `feat/132-guest-review`. Selection began at `dc76cec`; the local checkpoint
is now rebased onto merged archive main `7f9e326`. Both archive and sharing contracts,
playback readers, export metadata and Paper designs are preserved. The two separately
validated archive browser regression fixes are also carried locally. The owner requested
a PR after local validation. Its scope is the completed-export increment; full #132
acceptance remains open, so publication uses a draft PR.

## Final validated local checkpoint

Frozen source and tests at `fb250675d3d60947304b13c619a0091c204dde5f` passed the full
Windows `check.bat` gate with Rust 1.99.0, incremental compilation disabled and slow
tests enabled. The run passed all 3602 Rust tests in 480.404 seconds (four slow tests,
27 existing skipped fixtures), strict workspace/all-target Clippy, unused-dependency,
Rust/Python formatting and every UI static check. Svelte reported zero errors and
warnings. UI suites passed 438 Bun tests, 317 Chromium component/visual tests and
89 compatibility tests. Playwright passed 299 E2E cases in 2.8 minutes, with two
existing skipped cases. The supported authentication-fixture override reused the
verified current executable from the preceding complete UI preflight.

Script: `target/agent-artifacts/132-check-windows.ps1`. Complete output:
`target/agent-artifacts/132-check-windows.log`. The subsequent evidence-only commit
changes this task note; runtime source, API, UI, vendor code and tests remain identical
to the validated checkpoint. The current book also builds successfully; its log is
`target/agent-artifacts/132-book-build.log`.

This completes a validated local start of the immutable-export workflow. It does not
complete #132: exact ranges, the remaining creator entry points, actual guest HTTPS
playback and full adversarial/resource qualification remain open. Existing real TLS
authentication tests and mocked guest isolation tests do not prove guest HTTPS delivery.
This evidence was recorded before PR publication. Hosted CI is left to the user.

## Product contract

An Administrator may share one completed immutable export, or one explicitly bounded
recording range, through a revocable, expiring guest link. Guests receive access to that
evidence only. They receive no KeepPeek role, credential, control channel, live view,
search, adjacent recording, snapshot, PTZ, talk, settings, health or logs access.

The existing issue is the acceptance contract. The first implementation increment is
completed-export sharing through actual guest playback, optional download and active
revocation. Recording-range creation and all four creator entry points remain required
for the full issue. No original acceptance item is closed by implementing a grant list.

## Boundaries and resource limits

| Boundary              | Required behavior                                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Creator to management | Administrator plus current evidence/source permission; no ordinary User creation by default                                                |
| Grant to evidence     | Immutable export artifact generation, digest, byte length, source and availability horizon; never mutable job ID alone                     |
| Link to guest session | At least 256 CSPRNG bits; token in fragment; bounded POST exchange; hash only in runtime storage; one-time creator disclosure              |
| Guest to ordinary API | Dedicated session type, no principal conversion or normal WebRTC connection; trusted-local policy is tested separately                     |
| Guest to media        | Recheck grant revision, expiry, policy, artifact identity, privacy and retention before delivering bounded chunks                          |
| Configuration         | Existing config.toml owns deployment/origin policy; no new settings file, public tunnel or cloud relay                                     |
| Restart/restore       | Sessions and active transfers are invalidated; delivered counters survive restart; authorization-state restore requires explicit treatment |

Issue ceilings: 256 active grants, 32 per creator, at most 24 hours and no later than
artifact expiry; inherited 2-minute/512 MiB evidence limits; 128 UTF-8 bytes for display
name and 512 for note; 8 guest sessions globally and 2 per grant. Expired/revoked audit
metadata has a 30-day retention ceiling. Current code hides older records from lists
and performs bounded batch cleanup; physical removal within this ceiling remains part
of full qualification. Total audit cardinality is finite: reject new work when retained
metadata fills the store instead of growing indefinitely.

Fixed-capacity request budgets must use the actual classified transport address, not
arbitrary forwarded headers. Token lookup, sessions, ranges, downloads, egress buffers,
workers and timeouts all need explicit limits. No attacker-keyed unbounded maps.

## Accounting and failure semantics

A playback session reserves one view atomically. Its first media delivery commits that
view durably; seeks and range requests in the same session do not count again. Commit
occurs immediately before returning first bytes, so a crash can conservatively consume
an undelivered view but cannot refund delivered access. Downloads reserve and commit
one bounded transfer separately. Unused reservations expire; restart drops sessions
and reservations without refunding durable committed counters.

Creation has a stable request identifier. A retried request never discloses the old
secret or silently creates a replacement. After an indeterminate creation result, the
creator can list and revoke the grant. Persistence failure must never grant access or
advance an in-memory authorization transition ahead of durable state.

Unknown token, expired token and revoked token have the same unauthenticated denial.
After successful handoff, only that bound grant's safe status can distinguish expired,
revoked, retained-away, limit reached or unavailable. Neither token nor hash appears in
list responses, errors, debug output, diagnostics, referrers or browser persistent state.

No implicit evidence hold is added. Grant expiry is capped by artifact expiry and the
preview states that retention/deletion can make footage unavailable sooner.

## Delivery design to verify

Use an isolated `/share` page, remove the fragment immediately, then exchange the token
in a POST body for a short-lived Secure/HttpOnly/SameSite cookie. The page must load only
bundled resources, with no normal shell, navigation, capability query or control client.

The user approved the following additive contracts for the current #132 task:

- Sharing policy/preview/create/list/revoke/evidence-revoke management in
  `api/webrtc.proto`: `ShareCommand` request tag 31 and `ShareResult` success tag 45,
  with regenerated TypeScript bindings.
- Dedicated `/guest/session` exchange/status/end-session, `/guest/media` playback and
  `/guest/download` optional transfer contracts in `api/openapi.yaml`.
- Matching `api/webrtc.md`, `api/README.md`, generated bindings and contract tests.

The approved schemas and protocol documentation are now present. Approval covers this
sharing scope; it does not establish runtime qualification or authorize unrelated API
changes. Guest status and media use a public session UUID bound to the guest cookie, so
a second guest tab cannot switch the first tab to another grant. An invalid fragment
must deny access instead of resuming a previous cookie.

Reuse catalog export leases and owned-file identity checks. Do not call the existing
normal download path: it reads the entire artifact into memory and prepares all chunks.
A dedicated reader must have constant bounded buffering and validate single HTTP byte
ranges with checked arithmetic. Multi-range, overflow and cross-artifact requests are
rejected. Changing Content-Disposition alone is not permission enforcement.

Revocation must invalidate authorization first and stop active delivery within the
issue's proposed one-second bound. Read-side checks alone do not prove this: the
blocking HTTP writer can be stuck on a slow peer after a read. Transport timeout/abort
behavior must be established before exposing guest streaming or claiming this bound.

For recording-range shares, use a bounded derivative containing no decodable frames
outside the exact granted interval, or reject unsupported exact-bound requests. Never
send earlier GOP frames and rely on the player to hide them. The first completed-export
increment grants the immutable export's full media and does not claim exact-range work.

All guest success/error/range responses need private/no-store caching, strict bundled
asset CSP, frame prohibition, nosniff and no-referrer headers. Origin/Host must match the
configured reachable HTTPS origin. No fallback to ordinary API authentication.

## Ordered implementation and verification

1. Implement durable domain grants, hashed secrets, isolated sessions, bounded accounting
   and negative/boundary/restart/persistence tests without a reachable guest route.
2. Bind authorized management to immutable ready exports and synchronize the approved
   contracts. Prove role, source, ownership, generation and failed-write behavior.
3. Establish bounded transport cancellation, then implement isolated handoff, budgets,
   range playback/download and adversarial HTTP tests.
4. Design creator and guest screens in the existing KeepPeek Paper file. Implement the
   shared creation dialog, one-time copy, grant management and isolated viewer.
5. Add exact-range sharing and event/bookmark/Keep-range entry points using the same
   dialog, preserving selected evidence and coverage.
6. Run actual HTTPS browser isolation, retention, restart, clock/origin rotation, load
   and revocation tests. Compare authenticated export and guest delivery over 30 runs;
   record p50/p95/max first byte, memory, throughput and revoke latency.
7. Synchronize book/operator documentation and finish the Windows canonical check.bat.

Original acceptance remains open until its full evidence exists: authorized bounded
range/export creation; denial of every unrelated surface; token secrecy; deterministic
expiry/revoke/limits/retention/restart; strict origin/browser headers; bounded creator
audit/list/revoke; and complete adversarial/browser/resource verification.

## Evidence history

- Live roadmap #147, issue #132, origin/main and open PRs were refreshed before selection.
  The initial base-source review found no guest grant store, route, viewer or management
  UI. The bounded immutable-export increment is now implemented in the isolated checkout.
- The first durable-domain checkpoint passed 39 tests in 5.55 seconds. The first export
  provenance checkpoint passed four tests in 2.25 seconds. Logs:
  `target/agent-artifacts/shares-domain-green.log` and
  `target/agent-artifacts/export-provenance-green.log`. Subsequent status, receipt,
  clock, configuration and HTTP additions are not qualified by those older results.
- Before creator integration, `bun run check` reported zero Svelte errors and warnings;
  focused server/unit/Paper checks passed 32 tests in 636 ms, including six Paper
  integrity tests; guest viewer/layout Chromium components passed seven tests in
  7.16 seconds. Logs: `target/agent-artifacts/guest-ui-svelte-check.log`,
  `guest-ui-unit.log` and `guest-ui-components.log` in the same directory.
- The existing KeepPeek Paper file now contains boards 51-56 with desktop, 390px and
  320px exports. See [the UI design and evidence boundary](132-guest-review-ui-design.md).
- The first integrated UI checkpoint added the isolated `/share` viewer, early fragment
  removal, disposable normal-shell logging, bound guest API requests, the ready-export
  creator dialog and
  Settings Access policy/grant management. Its Svelte check passed with zero
  errors and warnings. The initial check's 15 errors are preserved separately;
  fixes added the missing typed capability, numeric props and installed Vitest APIs.
- That UI checkpoint passed: 29 Bun server/helper/capability/Paper tests in 243 ms,
  22 Vitest compatibility tests in 1.02 seconds, and five canonical-selection guards
  in 11 ms. Every source unit test retains exactly one canonical owner. Guest API and
  logging global mocks use the existing compatibility project; other pure server
  tests use Bun. All 21 guest/creator Chromium component tests pass in 3.79 seconds.
- All five real-Kit isolation E2E cases pass in 16.0 seconds with mocked guest HTTP.
  They cover fresh guest documents, invalid fragments, normal-to-guest navigation,
  queued navigation and later hashes, with token-storage and normal-module/logging
  assertions. The queued case first failed because replacing Kit's already-updated
  URL did not create a new document; the fallback now explicitly reloads. This is
  HTTP development-fixture evidence, not actual HTTPS/backend qualification.
- The focused UI logs use `target/agent-artifacts/guest-ui-current-*.log`, distinct from
  the earlier viewer-only logs. Its E2E TypeScript check also passed after
  explicitly checking that the fixture storage-audit binding is installed. Initial
  component locator and E2E typing failures remain in their separate logs.
- Vendor transport qualification subsequently passed: four TCP cancellation tests,
  two timeout tests, six EqualReader tests and three native TLS tests. The full vendor
  packages passed 142 tests with two existing ignored tests, and strict Clippy passed.
  Logs include `132-response-abort-shared-socket-green.log`, `132-native-tls-green.log`,
  `132-vendor-all-green.log` and `132-vendor-clippy.log` under the artifact directory.
  The old `132-response-abort-green.log` remains a historical three-pass/one-fail
  artifact despite its filename. These vendor tests are separate from the subsequent
  combined KeepPeek guest HTTP/configuration and canonical checkpoints below.
- The separate #130 canonical gate passed before the final #132 run. It provides no
  validation of guest sharing.

Before archive integration, the combined backend checkpoint passed 147 tests in
13.756 seconds. It covered
grant storage, clock/accounting, guest HTTP/media, configuration, management,
approved API contracts, export ownership/provenance and measured export envelopes.
The real export tests exercise pinned metadata and durable history. Proof publication
requires a real history writer; a missing or failed writer leaves ordinary downloads
available and withholds guest sharing. The envelope tests independently parse emitted
MP4 samples, including audio, composition offsets and the writer's duration rewriting.
Log: `target/agent-artifacts/132-core-prearchive-green.log`.

The first combined run is preserved as `132-core-first-behavior.log`: 123 passed and
23 failed. It exposed an invalid private directory leaf, a cross-site public-document
navigation rejection and an undersized canonical Host budget. Those production causes
are fixed. Fixture prerequisites now initialize real metadata history and rewind the
independent MP4 reader; the assertions remain intact. The new positive and negative
boundary cases and the on-disk proof restore all pass in the combined checkpoint.
This checkpoint predates integration of the merged archive work at `7f9e326`.

All four creator entry points and exact bounded recording derivatives remain required.
Actual guest HTTPS isolation/headers, backend-to-browser creation and delivery, active
revoke latency, restart/retention behavior and resource/performance qualification remain
open beyond this local checkpoint. The final canonical gate and historical focused
results do not close these full-issue acceptance items.

The pre-archive-integration workspace/all-target Clippy check passes with warnings
rejected in 55.18 seconds. The normal production build now includes the grant store;
initial test-only module gating was removed. Unused session output was removed, its
assertions now use authenticated session lookup, and management reuses creator-scoped
store listing. No diagnostic suppression or test weakening was added. Logs:
`132-clippy-prearchive-green.log`, with initial failures retained separately.

The first integrated canonical Windows run reached UI static validation after passing
all 3602 Rust tests in 503.966 seconds (four slow tests, 27 existing skipped fixtures),
strict workspace/all-target Clippy, dependency, formatting and Python checks. Both new
archive integration regressions pass: archive-backed envelope/byte parity after primary
removal, and archived ownership/proof preservation with typed job-ID fencing. Paper
artifacts now preserve both features with 46 selected boards and 85 PNG references.

That run stopped at the existing Paper storyboard validator because its canonical
OpenAPI path list had not been synchronized with the approved guest HTTP contracts.
This is a failed canonical run; it does not establish the UI/full gate. The complete
output is preserved in `target/agent-artifacts/132-integrated-check-first.log`.

The second integrated run again passed all 3602 Rust tests in 469.412 seconds,
strict Clippy, dependency, Rust/Python formatting and every UI static check. The Bun
suite passed 437 cases and failed one existing exact action-ownership guard: the
implemented guest-link action was absent from the target capability action catalog.
Guest review remains a target capability while full issue acceptance is open; changing
that delivery status or excluding the assertion would not fix the missing inventory.
This run remains a failed canonical gate. Complete output:
`target/agent-artifacts/132-integrated-check-second.log`.

The next HTTPS increment can reuse `issue123_browser_fixture` and the existing
external-authentication TLS launcher/proxy. Add an opt-in real recording/catalog and
private history seed, preserving camera/sharing configuration when authentication
changes. Let the signed-in remote OIDC Administrator create the export through real
WebRTC so its typed owner is genuine; do not pre-create a local-Administrator export.
A separate guest browser context then exercises decoding, seeking, optional download
checksum, revocation, isolation and actual headers. No production test endpoint is
needed. This first lifecycle still does not replace exact-range, four-entrypoint,
slow-client cancellation and 30-run resource/performance qualification.

The missing action inventory is now fixed: creation and management explicitly own
`keeppeek.guest-review.v1`, and the UI reuses those catalog values. The original exact
target-capability assertion and Administrator gates remain intact; guest delivery is
still marked as a target while full issue acceptance is open. Independent review found
no blocker in that five-file change.

Before the final canonical run, full UI preflight passed: 438 Bun tests, 317 Chromium
component/visual tests and
89 compatibility tests. All UI static checks report success, including Svelte with
zero errors and warnings. Full Playwright E2E passes 299 cases with two existing
skipped cases in 3.0 minutes. These include existing real authentication TLS cases
and five guest-isolation cases with mocked guest HTTP; they do not establish actual
HTTPS guest playback. Logs: `132-ui-preflight-quality.log` and
`132-ui-preflight-e2e.log` under `target/agent-artifacts/`.
