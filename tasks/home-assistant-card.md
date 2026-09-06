# Issue #63: Home Assistant Live Card

## Scope and Ownership

Implement the live Lovelace card from [issue #63](https://github.com/xnorpx/keeppeek/issues/63)
in this repository. Keep its handwritten code under `ui/src/lib/home-assistant/` and use
the existing Bun, Svelte, protobuf, Vitest, and Playwright dependencies. A separate ES-module
build is the installation artifact; Home Assistant never proxies media.

The user delegated the repository-location decision while unavailable. Keeping the card here
allows real-server compatibility tests without duplicating the protected protocol. This
issue-specific plan preserves the existing task history and the concurrent agent's workspace.

Do not change `api/`, server authentication, camera ingestion, motion detection, or ISAPI.
The richer event/timeline design in `docs/home-assistant.md` is not part of issue #63's live-card
acceptance criteria. Do not expose controls for those unimplemented views.

## Design and Bounds

- Register `keeppeek-card`, `keeppeek-card-editor`, and card-picker metadata.
- Support an HTTPS endpoint (HTTP only on loopback), a resolved YAML token reference, stable
  source IDs, titles, quality, grid/single layouts, columns, and fixed video aspect ratios.
- Never render an existing token in the editor. Preserve it across unrelated edits and emit
  standard `config-changed` events. Never log or persist credentials or fingerprints.
- Key browser-local connections by canonical endpoint and SHA-256 token fingerprint. Keep
  credentials, peer connections, and mutable subscription state out of SSR module scope.
- Bound each connection to 16 live video subscriptions, 64 card consumers, and 32 pending RPCs.
  Establish all receive transceivers before the offer. Share identical source/quality requests.
- A consumer owns a release handle. Release hidden cards, obsolete configurations, and removed
  elements. Close the session after the final release, including creation/release races.
- Reconnect with capped backoff and a finite retry budget. Stop automatic retries on authentication
  failure. Bound HTTP, initial capabilities, and RPC waits. Rebuild MID/subscription maps.
- Use native WebRTC video playback, muted by default. CSS inherits Home Assistant theme variables;
  use stable responsive grids, semantic controls, visible focus, and Lucide icons.

## Ordered Increments

- [x] Configuration: validate input and prove secret-safe errors with 23 focused unit tests.
- [x] Shared connection: test direct gzip bootstrap, protobuf capabilities/subscription,
      identity sharing, cleanup races, reconnect, and source unavailability.
- [x] Card and editor: implement the custom elements and verify editor round trips, navigation,
      visibility, theme changes, and resizing in browser tests.
- [x] Package: produce a versioned single-module artifact, HACS metadata, a release-artifact
      workflow, and manual installation instructions without publishing a release.
- [x] Evidence: run a real KeepPeek camera fixture, desktop/mobile browser tests, comparative
      connection measurements, and the canonical `./check.sh` gate.
- [ ] Release-dependent verification: publish an approved release and verify HACS installation,
      upgrade, rollback, and dashboard behavior in a complete disposable Home Assistant instance.
      Branching, committing, pushing, and opening a PR were authorized during handoff. Publishing
      a release and closing this issue still require the release-dependent evidence.

## Verification Commands

From `ui/`, use `bun run test:unit -- run --project server src/lib/home-assistant/` for
configuration and connection tests. Browser components run through the `client` Vitest project.
Build the isolated card with `bun run build:home-assistant`. Run real-server tests with
`bun run test:e2e -- home-assistant.e2e.ts`. Run `./check.sh` from the repository root last.

## Acceptance Evidence

| Criterion                                                       | Required evidence                                                  | Result                                                                                                                        |
| --------------------------------------------------------------- | ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| One and multiple live cameras in a Home Assistant harness       | Real WebRTC decoded frames in desktop/mobile Playwright            | Passed at 1440 and 390 pixels with two real fixture sources                                                                   |
| Multiple cards share a connection until final release           | Session and subscription counters plus cleanup tests               | Three cards share one peer and one subscription; final release returns the server gauge to zero                               |
| Reconnect/navigation do not leak or duplicate subscriptions     | Lifecycle unit tests and browser navigation                        | Forced peer interruption, consumer removal, hidden-tab resume, late-create cleanup, and source updates pass                   |
| Visual editor round-trips supported YAML configuration          | Config-change event assertions and retained credential             | Real source discovery and config-changed checks preserve keys and Home Assistant grid metadata                                |
| Invalid endpoint/origin/credential/source states are actionable | Negative unit and browser tests, DOM/log secret scan               | Unsafe endpoints, CORS denial, invalid keys, unknown/offline sources, and secret-safe diagnostics pass                        |
| Desktop/mobile layouts and themes                               | Browser screenshots and layout assertions                          | Real video screenshots, 320-pixel overflow check, theme inheritance, and single-camera width pass                             |
| Installation, upgrade, CORS, and security documentation         | Built artifact, HACS metadata, reproducible procedures             | Local module, checksum, manifest, workflow structure, and operator instructions verified; published HACS installation pending |
| Performance measurements                                        | Same workload with isolated/shared connections, run count, p50/p95 | Ten final-gate paired runs: sessions/subscriptions 3 to 1; shared bootstrap p95 1896.8 ms versus 1959.1 ms baseline           |
| Repository checks                                               | Canonical platform gate on the final executable tree               | `./check.sh` passed; 204 Playwright tests passed with two existing codec skips. All eight card E2E tests passed.              |

The original browser harness implements Home Assistant's custom-element lifecycle. The follow-up
[real container verification](home-assistant-container.md) additionally tests actual Home Assistant
onboarding, YAML resources, Lovelace, and its visual editor. Do not close issue #63 until the
separate HACS release-download and upgrade checks are recorded.

## Local Review

An independent lifecycle review was checked against the code and executable tests. The reported
RPC-limit and listener-removal concerns were disproved by exact boundary and reentrant-removal
tests. Additional local checks found and fixed stalled offer creation, retry without an acquired
lease, stale cameras after invalid configuration, and the editor's initial column selection.
No cross-model CLI was invoked in this non-interactive session.

The pre-commit review also checked invalid-capabilities cleanup and manual retry during a pending
close. Invalid-capabilities cleanup already resolves the wait through `DirectPeer.close()`.
A new failing test reproduced a manual retry inheriting the old backoff while cleanup was in
flight. Recovery now coalesces these requests and reconnects immediately after cleanup, retaining
current source demand. The regression and existing reconciliation tests pass.

## Final Build Evidence

The initial card executable tree passed `./check.sh` on 2026-09-05 local time. Subsequent container
test work is recorded separately in the linked follow-up. The card includes 51 focused configuration/transport tests,
11 browser component tests, and eight real-server E2E tests. The canonical test routing executes
the pure configuration tests in the existing Bun lane and the Vitest-only transport mocks in
the existing compatibility lane; no card tests are skipped.

The distributable is `target/home-assistant-card/dist/keeppeek.js`, version `0.1.0`, 323181 bytes
(88751 gzip bytes, below the 512000-byte budget). Its SHA-256 is
`7f64762a58bd76840c4a11778a079a9742490e8a7c60baede6c1ca453266bc98`.

Performance command from `ui/`:

```sh
bun run test:e2e:run -- --config playwright.home-assistant.config.ts \
  --grep 'measured bootstrap'
```

The canonical gate runs the same test in the full browser suite. Environment: macOS Darwin
25.6.0 arm64, Apple M5 Max, Chromium 151.0.7922.34, Node 26.0.0, Bun 1.4.0. Workload: three
visible cards showing one paced 640x360 H.264 RTSP fixture, ten runs per mode in alternating
order. Baseline uses three distinct User credentials to force independent connections;
the shared mode uses one User credential. Timing includes card mounting and at least two decoded
frames in every tile. Percentiles use nearest-rank p50/p95. The final full-suite run has more
contention than the earlier isolated run; do not treat the small latency delta as an independent
optimization claim.

| Metric                       | Isolated Baseline | Shared Result | Delta            | Budget                 |
| ---------------------------- | ----------------- | ------------- | ---------------- | ---------------------- |
| Bootstrap p50                | 917.8 ms          | 861.3 ms      | -56.5 ms (-6.2%) | Informational          |
| Bootstrap p95                | 1959.1 ms         | 1896.8 ms     | -62.3 ms (-3.2%) | Less than 10000 ms     |
| Active sessions              | 3                 | 1             | -2 (-66.7%)      | One per identity       |
| Media subscriptions          | 3                 | 1             | -2 (-66.7%)      | One per source/quality |
| Sessions after final removal | 0                 | 0             | 0                | Zero                   |

Machine-readable evidence is in the Playwright output for `shared connections meet the measured
bootstrap and resource budgets`, including environment details. The release workflow parses and
its versioned artifact build passes locally; GitHub-hosted execution and published HACS
installation cannot be inferred from these initial local results. The implementing PR records
the final commit, rerun measurements, and hosted CI evidence separately.

## Resource Sketch

For three cards displaying the same source, sharing should reduce server sessions and media
subscriptions from three to one (66.7% fewer). Three distinct sources should use one session and
three subscriptions. The transport adds no application-level frame copies. Measure bootstrap
latency with the real fixture; require p95 below 10 seconds, zero sessions after cleanup, and
a compressed distribution below 500 KiB. Record measured results, not inferred completion.
