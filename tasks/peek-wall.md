# Issue #114: Configurable Peek Wall

Scope: the live wall and display settings in the existing server-owned dashboard registry.
Issue #133, recording management, and the protected API contracts remain unchanged.

## Pull Request Verification

The user authorized a pull request on 2026-09-08. The feature branch is based on
`eefbed4ddfec28d72e8c71d93074d1602c4c34cb`, including the upstream focused digital-zoom feature.
The shared video component retains its zoom wrapper and unscaled controls while rendering the
wall's stable media frame and freshness overlay. All 18 focused component tests and 47 combined
wall, dashboard, layout, and digital-zoom E2E tests passed after integration.

The final integrated tree passed
`CI=1 KEEPPEEK_RUN_SLOW_TESTS=1 KEEPPEEK_E2E_BACKEND_PORT=4347 KEEPPEEK_E2E_FRONTEND_PORT=4197 ./check.sh`:
2,352 Rust tests, 358 Bun tests across 62 files, 181 browser/visual tests, 57 compatibility tests,
and 241 end-to-end tests. The opt-in main/sub, checked-in media, and storage-pipeline tests ran
and passed. The 21 existing Rust skips and two existing browser codec skips remain unchanged;
there were no failures or flaky results. The complete log and success marker are in
`target/issue-114-pr-check.log`. Only evidence documentation changed after that gate.

The rebuilt nine-camera fixture also passed on the integrated tree. The baseline is
`58edafeb1565a1460ba6c9598f0b6db3785dab3d`; the environment, twelve 500 ms samples per state,
four-stream capacity, and metric definitions match the original measurements below. The final
tree additionally includes the upstream zoom changes. Different media offsets and machine load
mean these measurements demonstrate resource bounds, not statistically significant speedup.

| Metric                                    |      Baseline | PR Candidate |      Delta |    Budget |
| ----------------------------------------- | ------------: | -----------: | ---------: | --------: |
| Smart renderer CPU p95                    |       17.812% |      14.465% |  -3.347 pp |      <50% |
| Continuous renderer CPU p95               | Not available |      15.717% |   New mode |      <50% |
| Hidden renderer CPU p95                   |        7.234% |       2.016% |  -5.218 pp |      <50% |
| Smart JS heap p95                         |    25.363 MiB |   27.364 MiB | +2.001 MiB |  <128 MiB |
| Continuous JS heap p95                    | Not available |   27.347 MiB |   New mode |  <128 MiB |
| Hidden JS heap p95                        |    24.529 MiB |   27.360 MiB | +2.831 MiB |  <128 MiB |
| Maximum visible streams/attached decoders |         4 / 4 |        4 / 4 |      0 / 0 | <=4 / <=4 |
| Hidden streams/attached decoders          |         0 / 4 |        0 / 0 |     0 / -4 |     0 / 0 |
| Hidden decoded-frame progress             |             0 |            0 |          0 |         0 |

The current `target/peek-performance/wall/final.json` and
`target/issue-114-pr-performance.log` contain this PR-candidate run. Earlier tables below retain
the history of prior builds. No generated artifacts, private configuration, or camera imagery
are included in the commits.

## Dashboard-Owned Follow-up

The user explicitly superseded #114's device-local preference requirement on 2026-09-08:
all wall settings belong to their dashboard on the server. Different dashboards serve different
screens. The original device-local evidence below is historical, not the current storage contract.

- [x] Persist every display field under each dashboard in `config.toml`.
- [x] Preserve display values across restart, duplication, exchange, old-client writes, and default-grid synchronization.
- [x] Enforce Administrator edits, whole-number bounds, object/string shapes, and stale revisions.
- [x] Add 0-24 px gap/radius controls with Current (10/10), Hairline (2/0), Flush (0/0), and custom values.
- [x] Add live preview, explicit Save/Discard, retained error state, and read-only User controls.
- [x] Match the settings button's visible 32 px frame to the dropdown while retaining a 44 px touch target.
- [x] Remove browser-local wall preference reads/writes and test that legacy values are ignored.
- [x] Update the book chapter and the serialized configuration-field reference.
- [x] Verify the rebuilt real-server UI, resource measurements, and canonical gate on the final tree.

Focused evidence: 19 Rust registry tests, 24 display-format tests, 17 layout/client/exchange tests,
six settings component tests, and eleven wall E2E tests pass. No protected protobuf/schema file
was changed. The existing versioned StateStore dashboard document carries the optional display object.

Real-server verification saved the Hairline preset in 57 ms, reloaded it with gap 2/radius 0, and
confirmed those fields in the synthetic server's TOML. The Current preset was restored afterward.
The toolbar's visual frames both measure 32 px high, share the same background and 4 px radius,
and retain a 44 px settings-button hit target. Save/Discard stay visible while options scroll.

The nine-camera resource harness passed after the server-backed follow-up. It uses the same
four-stream budget, twelve 500 ms samples per state, Chromium version, viewport, and metric
definitions as the original measurements below. Fixture start offsets and machine load vary;
the table demonstrates budget compliance, not statistically significant speedup.

| Metric                             | Prior wall implementation | Dashboard-owned settings |      Delta |    Budget |
| ---------------------------------- | ------------------------: | -----------------------: | ---------: | --------: |
| Smart renderer CPU p95             |                   21.332% |                  11.427% |  -9.905 pp |      <50% |
| Continuous renderer CPU p95        |                   28.661% |                  18.380% | -10.281 pp |      <50% |
| Hidden renderer CPU p95            |                    3.319% |                   2.538% |  -0.781 pp |      <50% |
| Smart JS heap p95                  |                25.831 MiB |               27.053 MiB | +1.222 MiB |  <128 MiB |
| Continuous JS heap p95             |                27.029 MiB |               27.296 MiB | +0.267 MiB |  <128 MiB |
| Hidden JS heap p95                 |                26.878 MiB |               27.472 MiB | +0.594 MiB |  <128 MiB |
| Admitted streams/attached decoders |                     4 / 4 |                    4 / 4 |      0 / 0 | <=4 / <=4 |
| Hidden streams/attached decoders   |                     0 / 0 |                    0 / 0 |      0 / 0 |     0 / 0 |
| Hidden decoded-frame progress      |                         0 |                        0 |          0 |         0 |

The follow-up report is `target/peek-performance/wall/final.json`. Canonical follow-up verification
uses `CI=1 KEEPPEEK_E2E_BACKEND_PORT=4347 KEEPPEEK_E2E_FRONTEND_PORT=4197 ./check.sh` and saves
its final successful output to `target/dashboard-display-check-final.log`.

The final canonical gate passed: 2,278 Rust tests, 339 Bun tests across 61 files, 159 browser unit
tests, 57 compatibility tests, and 232 end-to-end tests. The 21 existing Rust skips and two existing
browser codec skips remain unchanged; no test was weakened or newly skipped. Strict Clippy,
dependency checks, formatting, Markdown, Svelte/TypeScript diagnostics, visual-harness checks,
and the production build passed. The book build also passed.

The first follow-up gate exposed Bun treating an empty-array parameterized case as a row without
arguments. Wrapping each document in an object preserves all five malformed-input assertions and
passes both runners. The failed run remains in `target/dashboard-display-check.log`.

Full-page mobile validation then caught the application header reducing the space below the
settings trigger. The popover now respects Bits UI's available-height constraint. At 390 by 844,
the panel ends at 832 px and both action buttons end at 815 px. The wall E2E suite asserts full
panel and action-button visibility at 320, 390, 768, and 1440 px. The canonical gate was rerun
after this fix; only this evidence document changed after that final successful run.

## Original Prototype

## Implementation

- [x] Add bounded Smart/Continuous admission with explicit capacity exclusions.
- [x] Add versioned, validated device-local preferences and reset.
- [x] Add stable 16:9, 4:3, and native tile frames with contain/cover fit.
- [x] Surface stream demand, admission reasons, and actual frame freshness.
- [x] Add user-activated, visibility-aware wake lock with complete cleanup.
- [x] Add the live-wall book chapter and configuration/viewer ownership references.
- [x] Verify desktop, tablet, mobile, accessibility, resource bounds, and canonical checks.
- [x] Record every issue acceptance criterion and measured performance evidence.

## Decisions and Bounds

- Smart remains the default. Both modes use the smaller of the device estimate and the
  configured 1-12 stream ceiling; the server still authorizes every subscription.
- Continuous admits visible and focused cameras only. Smart additionally permits bounded
  prefetch and a one-second visibility grace period. Both admit batches of up to three new streams
  with a 40 ms delay for queued batches and suspend subscriptions when the document is hidden.
- Defaults are 16:9, contain, Smart, the automatic device ceiling, and wake lock off.
- Device choices are not layout definitions. They remain in browser storage and never enter
  configuration/layout exchange or affect recording/detection.
- Native ratios use valid metadata within 1:16 through 16:1 and latch per camera. Stable
  layout slots prevent metadata and quality changes from repacking neighboring tiles.
- Wake lock is independent of media scheduling, has at most one outstanding request, and
  requires a gesture before its first acquisition. Denial does not trigger a retry loop.
- Existing tests and the canonical `./check.sh` gate remain mandatory. Do not weaken limits,
  remove tests, alter API schemas, commit, branch, or close issues during this work.

## Verification Plan

1. Scheduler and preference unit tests prove valid, invalid, zero, boundary, and hidden states.
2. Browser component tests prove native geometry, fit, freshness, controls, and wake-lock races.
3. Playwright proves responsive interaction and stable sessions, and measures bounded live
   subscriptions/decoders, browser CPU, and memory using the existing Peek fixture.
4. Run `./check.sh` and the documentation gate; leave unverified issue criteria open.

## Acceptance Criteria Verification

| Criterion                                                                  | Observable outcome                                                                                                                     | Verification and evidence                                                                                                      |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| 16:9, 4:3, Native, Contain, and Cover without distortion                   | Media frames retain their ratio; fit changes preserve the video element; focused video remains uncropped                               | `PeekWallTile.svelte.spec.ts`, `FirstKeyframeState.svelte.spec.ts`, `peek-wall.e2e.ts`, existing real `peek-live.e2e.ts`       |
| Metadata and quality changes do not repack the wall or obscure controls    | Portrait/panoramic native dimensions latch; stable outer slots and a separate toolbar prevent control overlap                          | Native-metadata component tests, 320/390/768/1440 px Playwright geometry, saved-layout overflow tests, real-video screenshots  |
| Smart and Continuous expose bounded resource behavior                      | Device ceilings apply to every active plan, old subscriptions release first, codecs are checked, and server refusals remain tile-local | Scheduler tests, live-peer eviction/refusal/handoff tests, quality-rank and unsupported-codec E2E tests, nine-camera benchmark |
| Suspended and non-admitted tiles show truthful evidence                    | Capacity, queue, visibility, codec, and refusal reasons remain distinct from camera health; frames show observed age or unknown time   | Tile component tests, clipping/scroll visibility tests, hidden-page E2E and real-media measurements                            |
| Wake lock is opt-in, visibility-aware, and honest                          | Gesture-gated acquisition, release/reacquisition, denial, unsupported state, late results, and rapid toggle/release races are tested   | 11 wake-lock controller tests and the full navigation/visibility browser workflow                                              |
| Settings are device-local and reset safely                                 | Version/field/size validation; isolated storage; explicit unavailable-storage state; reset never writes server configuration           | 10 preference tests, settings component tests, reload/reset E2E assertions with zero layout writes                             |
| Responsive, accessibility, scheduling, wake-lock, and resource checks pass | Keyboard/named controls, 44 px touch targets, no toolbar/camera overlap or horizontal overflow, bounded real media                     | Focused suites pass; final canonical gate and resource results are recorded below                                              |

## Scope and Review

- The implementation does not modify `api/`, recording deletion, catalog reconciliation, or server
  configuration. No branch, commit, pull request, or issue closure is performed.
- Fresh-context review found delayed wake-lock release and failed media-eviction cases. Both now
  have regressions that failed before their fixes and pass afterward. Cross-model CLI review was
  not run because the user was unavailable to authorize it.
- Existing geometry assertions now verify the deliberate 64 px toolbar strip and 160 px narrow
  tile minimum. Overflow, camera-region overlap, media ratio, session identity, and focus checks
  remain enforced; no test is skipped or removed.
- A temporary intersection-coordinate probe was removed after identifying Chromium's subpixel
  iframe coordinates. Browser intersection tests use five-decimal-place precision; zero visibility
  and pure geometry checks remain exact.

## Evidence Status

The initial canonical run passed all 2,274 Rust tests (21 existing skips), strict Clippy, dependency,
formatting, Markdown, Svelte, and UI unit gates. Its browser run had 220 passes, two existing codec
skips, and eight failures. Stable-tree reruns confirmed five failures were caused by live reloads;
the three geometry failures were repaired and individually verified. Final gate results follow
after the unchanged-tree run. The issue remains open pending PR/hosted CI and owner review.

The next canonical run passed: 2,274 Rust tests, 155 browser/visual unit tests, 57 Vitest
compatibility tests, all 61 Bun-compatible files, and 228 end-to-end tests (two existing codec
skips). Final screenshot review then separated crop/freshness text from camera health in one
stacked overlay, removed redundant queue labels, and added a 320 px capacity-state regression.
All eight wall E2E tests, eleven affected component tests, and Svelte diagnostics pass after
that correction. The final canonical rerun passed with `CI=1` and the repository's existing CI
retry policy: 2,274 Rust tests, all 61 Bun-compatible files, 155 browser/visual unit tests,
57 Vitest compatibility tests, and 229 end-to-end tests. There were 21 existing Rust skips and
two existing browser codec skips, no final failures, and no flaky test results. Strict Clippy,
dependency checks, formatting, Svelte/TypeScript diagnostics, Paper/visual-harness checks, and
the production build all passed. The last executable changes were the stacked status overlay
and preservation of the canonical health-label text; only this evidence document changed afterward.

## Performance Evidence

Environment: macOS arm64, Apple M5 Max, Bun 1.4.0, Chromium 151.0.7922.34, Vite development
frontend at 1440 by 900. The existing nine-camera fixture streams real H.264/H.265 media with
640 by 360 wall variants. Reported hardware concurrency is fixed to eight, giving a four-stream
budget. One baseline process run and one final process run each collect twelve 500 ms samples
per state. Fixture start offsets vary, so these are bounded-work measurements, not a claim of a
statistically significant speedup or an unattended hardware soak.

| Metric                               |      Baseline |           Final |      Delta |        Budget |
| ------------------------------------ | ------------: | --------------: | ---------: | ------------: |
| Smart renderer CPU p50               |       10.832% |         12.221% |  +1.389 pp | Informational |
| Smart renderer CPU p95               |       17.812% |         21.332% |  +3.520 pp |          <50% |
| Smart JS heap p95                    |    25.363 MiB |      25.831 MiB | +0.468 MiB |      <128 MiB |
| Continuous renderer CPU p50          | Not available |         10.379% |   New mode | Informational |
| Continuous renderer CPU p95          | Not available |         28.661% |   New mode |          <50% |
| Continuous JS heap p95               | Not available |      27.029 MiB |   New mode |      <128 MiB |
| Hidden renderer CPU p50              |        6.306% |          0.904% |  -5.402 pp | Informational |
| Hidden renderer CPU p95              |        7.234% |          3.319% |  -3.915 pp |          <50% |
| Hidden JS heap p95                   |    24.529 MiB |      26.878 MiB | +2.349 MiB |      <128 MiB |
| Maximum admitted wall streams        |             4 | 4 in both modes |          0 |           <=4 |
| Maximum attached wall video decoders |             4 | 4 in both modes |          0 |           <=4 |
| Hidden admitted streams              |             0 |               0 |          0 |             0 |
| Hidden attached wall videos          |             4 |               0 |         -4 |           <=4 |
| Hidden decoded-frame progress        |      0 frames |        0 frames |   0 frames |      0 frames |

CPU is the Chromium `Performance.getMetrics` renderer `TaskDuration` delta divided by elapsed
time, not total system or GPU CPU. Heap is `JSHeapUsedSize`, not process RSS. Decoder count is
the number of wall video elements with an attached media stream, supported by measured decoded
frame progress. The final visible samples advance by 398 Smart frames and 396 Continuous frames.
Hidden visibility is injected through the browser event boundary; native wake-lock denial and
release use controlled test handles. Real-camera device policy remains browser-dependent.

Reproduction from the repository root:

```sh
bun run --cwd ui demo:fixtures:prepare
bun run --cwd ui test:e2e:prepare
KEEPPEEK_PEEK_PERF_PORT=4175 KEEPPEEK_WALL_PERF_PHASE=final \
  bun run --cwd ui test:e2e:run --config playwright.peek-wall-performance.config.ts
CI=1 KEEPPEEK_E2E_BACKEND_PORT=4347 KEEPPEEK_E2E_FRONTEND_PORT=4197 ./check.sh
mdbook build book
bun run --cwd ui format:markdown:check
```

The baseline used main `58edafeb1565a1460ba6c9598f0b6db3785dab3d` with the benchmark added and
the compatible Smart scheduler increment; wall UI, subscriptions, and frame attachment remained
unchanged then. Run the same harness with `KEEPPEEK_WALL_PERF_PHASE=baseline` against that baseline
implementation. Raw reports are in `target/peek-performance/wall/baseline.json` and `final.json`.
The successful final gate log is `target/issue-114-check-verified.log`; the earlier logs retain
the failed attempts and their diagnostic evidence. The CI-mode machine-readable Playwright result
is `ui/test-results/playwright.json`, and its final state reports no failed tests.
