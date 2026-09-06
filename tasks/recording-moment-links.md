# Issue #119: Authenticated Recording-Moment Links

## Scope

Add a copy-link command in Keep and event detail using one canonical route contract. Links restore
the stable source, stream preference, Keep mode, UTC date, and absolute millisecond timestamp.
They grant no access and contain no credentials, sessions, host paths, or temporary media IDs.
Preserve the existing camera authorization and recorded-codec selection policies. Do not change
`api/`, camera adapters, motion ingestion, or ISAPI.

## Contract

- Preserve existing `camera`, `date`, `at`, `stream`, `mode`, `event`, and `returnTo` route names.
- The absolute `at` timestamp determines the UTC date, even when an input date conflicts.
- Accept finite integer milliseconds from Unix epoch through year 9999; reject malformed,
  duplicate, oversized, or control-character-bearing parameters with a safe visible state.
- Bound route input to 8192 characters and 16 parameters; source/event IDs to 256 characters;
  return context to 2048 characters and known Events filter keys only.
- Serialize from typed current state, never by copying the current query or fragment.
- Snapshot the current playback clock only on invocation. Copying does not seek, pause, reload,
  or write browser history. Clipboard failure exposes a keyboard-accessible manual-copy dialog.
- Never silently choose a different source or move an exact requested timestamp into a recording.
  Show unavailable/unauthorized source and missing-footage states with bounded neighboring actions.
- Preserve event/filter return context and respond to same-route back/forward navigation.

## Increments

- [x] Canonical route parsing/serialization and secret-safe return context, with unit tests.
- [x] Accessible clipboard command and fallback, with browser component tests.
- [x] Integrate Keep and event detail, including explicit unavailable states and navigation.
- [x] Browser regression/performance evidence, book documentation, and `./check.sh`.

## Verification

Use the existing Vitest server/client projects and Playwright fixtures. Cover special IDs, UTC
boundaries, inconsistent dates, malformed/excessive inputs, clipboard success/denial/unavailable,
gap/retention/source/authorization outcomes, compatible fallback, and event/back navigation.
Fresh authorized contexts must restore the target within one second or one source frame duration,
whichever is larger. Compare copy to a no-copy playback baseline: no extra session/subscription,
no pause/seek, no URL/history or scroll mutation. Measure copy latency over repeated runs and
report p50/p95 with a 250 ms p95 budget in the deterministic clipboard fixture.

The book describes the final route and authorization boundary. All added behavior requires
focused tests and the canonical platform gate before completion. The follow-up request authorizes
a feature branch, focused commits, a push, and a pull request. Issue closure remains outside this
publication step.

## Evidence

- Controlled Chromium, macOS ARM64, 20 iterations per workload: no-copy frame baseline
  p50/p95 8.4/8.4 ms; copy-to-next-frame p50/p95 8.3/8.7 ms. This measures UI completion with
  a deterministic clipboard, not operating-system permission-prompt latency. Sessions and stored
  opens remained 1; closes, seeks, and live subscriptions remained 0. URL, history, and scroll
  were unchanged. The regression budget is 250 ms p95.
- Real H.264 media through KeepPeek's stored-media transport reopens in a fresh local authorized
  session within 1000 ms, with nonblank decoded-pixel and uninterrupted-copy assertions. A separate
  remote-user fixture proves sign-in is required before opening a copied moment in another timezone.
- Clipboard success uses the native browser API in the Events round trip. Component tests cover
  denial, absence, timeout, repeat invocation, and completion after destruction. Mobile Escape
  preserves the underlying selected event and restores focus.
- Desktop (1440 px), tablet (768 px), and phone (390 px) screenshots and command edge hit tests
  cover copy/fallback fit. Existing 1024/1188 px compact-header assertions remain intact.
- The real fixture had seeded `main` while default `event-boost` resolves playback to physical
  `sub`. Its seed now matches that policy. No server, API schema, or camera adapter change was
  necessary. The server's opaque source ID is preserved even when it resembles an IP address.
- The canonical `./check.sh` gate passed: 1706 Rust tests, 292 Bun-compatible tests, 133 browser
  component/visual tests, 57 Vitest compatibility tests, and 219 Playwright tests. The existing
  20 Rust ignored tests and two unsupported-codec browser skips remain unchanged. All 15 new
  recording-link end-to-end tests passed. `mdbook build book` also passed.
- The pre-PR opt-in slow checks passed with `KEEPPEEK_RUN_SLOW_TESTS=1`: both named
  test-camera regressions (one test each) and all four storage-pipeline tests. The storage run
  used the canonical macOS `macos-test-aws-crypto` feature.
- The link icon is included in the existing Vite prebundle list so browser test imports are not
  invalidated by a dependency-optimizer reload during a cold run.

Playwright writes screenshots and structured timing evidence under `ui/test-results/playwright/`.
The complete canonical validation output is retained in `target/recording-link-check.log`.
