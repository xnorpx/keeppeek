# External authentication verification

This report covers issue [#123](https://github.com/xnorpx/keeppeek/issues/123).
The implementation preserves the existing two roles, camera grants, trusted-local
classification, and configuration/secret files. Administrative changes, replacement
Administrator proof, and restore preparation use additive protobuf commands.
HTTP is limited to the four previously approved authentication routes; restore
does not acquire proof headers or a dry-run HTTP mode.

## Acceptance criteria verification

| Criterion                               | Observable outcome                                                                                                                                                                 | Verification                                                                                                                                                                                                    |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| OIDC Authorization Code with PKCE       | A configured provider authenticates a remote browser, rotates the opaque cookie, and establishes CSRF-bound WebRTC.                                                                | `oidc_flow_tests`, `oidc_tests`, `oidc_optional_claim_tests`, `authentication_oidc_tests`; desktop/mobile TLS browser login.                                                                                    |
| Trusted proxy boundary                  | Only the configured immediate peer and exact validated assertions establish identity; forged forwarding and caller assertions cannot elevate the user.                             | `proxy_identity_tests`, proxy downgrade regression, desktop/mobile TLS stripping-proxy tests.                                                                                                                   |
| Explicit Administrator/User mapping     | Unmapped and ambiguous claims fail closed; identity keys remain stable across display-name changes and retain explicit camera grants.                                              | `external_tests`, `identities_tests`, `external_oidc_and_proxy_roles_enforce_rtc_matrix_and_revoke`.                                                                                                            |
| One principal across protected surfaces | HTTP logs/configuration and WebRTC health/settings/state/playback/PTZ/talk/export use the external role and camera grant; revocation denies subsequent work.                       | `external_oidc_and_proxy_roles_enforce_http_logs_and_revoke`, `external_oidc_and_proxy_roles_enforce_rtc_matrix_and_revoke`, existing camera-access/delivery/namespace tests and session-lifecycle regressions. |
| Secure session lifecycle                | Cookies are Secure, HttpOnly, host-only and SameSite; CSRF/Origin, rotation, expiry, logout, live-session revocation, and fresh login after revocation are enforced.               | `browser_sessions_tests`, `login_transactions` tests, `authentication_cookie_recovery_tests`, authentication CSRF/expiry tests, TLS logout/revocation/re-login test.                                            |
| Local administration and recovery       | Existing local classification remains available; spoofed forwarding cannot invoke it. Recovery uses the configured direct local path.                                              | Existing local/remote forwarding and secure-transport tests, proxy immediate-peer tests, local verification rejection; operator recovery procedure.                                                             |
| Last-remote-Administrator protection    | A lockout-sensitive change needs fresh replacement proof, exact candidate/archive binding, and explicit confirmation; failed persistence retains working state.                    | `credential_migration_tests`, `authentication_verification_tests`, candidate OIDC/browser tests, `authentication_verification_restore_tests`, Administrator revocation tests.                                   |
| Adversarial coverage and redaction      | Invalid signatures/claims/keys, replay, redirects, CSRF, malformed proxy assertions and capacity violations fail closed; private material stays out of browser and audit surfaces. | OIDC protocol/token/budget/transport tests, login limits, directory redaction, TLS browser DOM/storage/URL/referrer/response/audit scans, dependency audit and canonical repository validation.                 |

## Recorded checks

The final canonical Windows check completed successfully on 2026-09-27 with
`KEEPPEEK_RUN_SLOW_TESTS=1`. All performance budgets and capacity/cleanup assertions
also passed against the source-fingerprinted final release build.

- `cargo test --locked --lib oidc -- --test-threads=8`: 41 passed.
- `cargo test --lib server::authentication:: -- --nocapture`: 67 passed after the
  conditional stale-cookie fix, external-role matrix, and bearer transport regression.
- `cargo clippy --locked --all --all-targets -- -D warnings`: passed after those
  additions, with no lint exceptions.
- Canonical Rust nextest run with `KEEPPEEK_RUN_SLOW_TESTS=1`: 2,838 passed across
  118 binaries in 426.110 seconds;
  24 skipped tests include opt-in workloads exercised separately where relevant.
- `cargo test --locked --doc --all`: passed for the full workspace.
- UI validation: 404 Bun tests, 240 browser-backed component/visual tests, and
  57 server-compatibility tests passed. Svelte reported zero errors and warnings.
- Full Playwright suite: 280 passed in 2.3 minutes using six workers; two
  codec-dependent H.265 scenarios were skipped by their existing capability gates.
  All eight real-TLS authentication scenarios passed, including renewed sign-in
  and WebRTC after revocation. Camera-permission and state-store regressions passed.
- `cargo audit --deny warnings`: passed for 651 locked dependencies with no
  advisory exceptions.
- `git diff --check`: passed. `api/backup.proto` and its generated TypeScript
  binding are unchanged.
- CI shared-fixture artifact selection: ten positive/negative selector cases and
  exact Bash syntax passed. Browser shards use the already tested precompiled
  executable override instead of compiling Rust inside the browser deadline.

Eleven release benchmark processes passed: bearer median p95 stayed at 600 ns,
cookie p95 was 1,900 ns, and maximum measured owner/watch cleanup was 0.4043 ms.
See [the performance report](external-authentication-performance.md) and its
sanitized metric rows for workload boundaries, budgets, baseline provenance,
before/after measurements, and transient-versus-retained transport counts.

## Browser and Paper verification

The production UI runs against a real local TLS OIDC provider and TLS-terminating
identity proxy. Only the browser accepts the fixture's self-signed certificate;
the production outbound provider client verifies its private CA. No insecure TLS
option is added to the application.

The eight browser scenarios cover OIDC User at 1440 and 390 pixels, proxy User at
both widths, Administrator settings at both widths, logout/revocation/re-login,
and provider outage with existing-session survival and new-login rejection.
The observer checks DOM, local/session storage, URLs, referrers, authentication
response bodies, audit/log output, Secure-cookie attributes, and CSRF-bearing
signaling. The two deliberate invalidated-session cleanup requests must return
401; each corresponding browser resource error is counted exactly. Other console
errors fail the test.

Fourteen final PNGs were retained: eight Administrator settings/mapping/identity/
session views, two sign-in views, two OIDC shells, and two proxy shells. The
Administrator controls were checked with viewport, hit-testing, and trial-click
assertions. Visual inspection confirmed wrapped fingerprints, usable desktop and
mobile layouts, and revoke controls above the mobile footer.

Paper's access-role, shell, people, policy, ZIP-restore, and mobile-administration
boards were inspected read-only through its local MCP service. Their structure,
typography and existing design tokens guided verification. The local Paper image
export returned black frames, so this is **not** a pixel-perfect Paper comparison.
The repository storyboard/token/reference checks and real-browser captures are
the reproducible visual evidence.

## Review and regression history

- Independent review covered signature algorithms and keys, issuer/audience/
  authorized-party/nonce/time/access-token-hash validation, provider transport,
  bounded caches, secret redaction, and commit-time authorization. No remaining
  findings were reported after the fixes.
- The configuration commit race was demonstrated failing against the old binary
  and passing with the authentication guard held through persistence and activation.
- Native Windows TLS fixture keys collided between parallel test processes. Eight
  old-binary processes produced four failures; the in-memory rustls fixture passed
  all eight. Production native-TLS certificate validation is unchanged.
- The stale-cookie browser recovery regression failed with HTTP 400 before the
  fix and now completes a new provider login with HTTP 303. Cookie-free discovery
  does not expire a session cookie, avoiding a delayed anonymous response clearing
  another tab's fresh login.
- Bearer-only discovery returned HTTP 426 despite the explicit legacy
  `require_secure_remote = false` policy. The regression now returns anonymous
  bearer discovery, while the secure default and all configured external methods
  still reject insecure transport. Forged forwarding headers do not change this.
- Self-review was used as an explicitly disclosed fallback for the additional
  doubt-driven pass; it is not represented as an independent security audit.

## Reproduction and environment

The reference machine uses Windows 11 Pro, an AMD Ryzen 5 5600G, 12 logical CPUs,
Rust 1.98.1, Bun 1.4.0, Python 3.12.10 and installed FFmpeg. The base is main
`26ef5225c368c9c5440a6d1cd0187a90fbc10456`.

From the repository root run `check.bat` on Windows or `./check.sh` on Unix. The
canonical entry point builds Rust, runs nextest, Clippy, dependency-use and format
checks, and then the complete UI quality and browser suites. Set
`KEEPPEEK_RUN_SLOW_TESTS=1` to include the main-only slow camera tests. The final
run used isolated E2E ports 54327/54184 and run ID `issue123-canonical-final`.
The TLS scenarios
are also reproducible with `bun run test:e2e:run -- e2e/external-authentication.e2e.ts`
under `ui/` after the normal E2E preparation. The focused eight-test run uses
`bun run test:e2e:run -- --config e2e/fixtures/external-authentication.config.ts`.
Test fixtures are synthetic; do not
substitute production credentials or publish raw HAR/configuration archives.

The implementing PR must link the final-commit CI results. This report does not
authorize deployment or merge; both remain subject to review.
