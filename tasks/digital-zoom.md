# Issue #118: Accessible digital zoom and pan

Tracking issue: <https://github.com/xnorpx/keeppeek/issues/118>. Roadmap:
<https://github.com/xnorpx/keeppeek/issues/147>. Issues #133 and #114 are outside this work.

## Contract and decisions

- One reusable Svelte viewport owns local inspection state for focused live video, Keep playback,
  and canonical event images. Compact tiles and filmstrips opt out.
- Scale is 1x through 8x. Double-click/tap uses 2x. Alt/Option-wheel avoids taking over ordinary
  scrolling or browser zoom. Keyboard pan uses 40 CSS pixels per keypress.
- Fit the actual media aspect ratio before applying compositor transforms. Media-coordinate
  overlays share the layer; player controls and diagnostics do not.
- Reset on camera, recording, canonical image, or explicit reset. Resize preserves normalized pan
  position and revalidates bounds. Pause and playback progress do not replace the media element.
- Gesture state retains at most two pointers, one previous tap, and one pending animation frame.
  Reset, resize, cancellation, lost capture, blur, and teardown release gesture ownership.
- Keep's capture-phase shortcuts use a synchronous pan-ownership marker. Native video focus stays
  available for existing keyboard callers. Unscaled transport controls reuse Keep's playback
  actions and expand the whole player for native fullscreen.
- Recording position uses the selected catalog interval and commits through Keep's bounded seek
  action. MediaSource's infinite native duration is not a usable recording range. Slider changes
  commit on `change`, not every pointer move.
- The E2E camera uses its existing paced-loop mode rather than disconnecting at fixture EOF.
  Continuous-frame tests retain their session/decode assertions, and camera health must settle to
  healthy with sustained input.
- No protected API, configuration, export, subscription, or PTZ contract changes.
- The public npm registry was unreachable during implementation. The implementation uses native
  browser APIs and adds no dependency or alternate registry.

## Resource sketch and verification budgets

Each gesture performs constant-size coordinate math over at most two pointers. It schedules at
most one compositor update per animation frame. It adds no network or disk work and does not
allocate decoded media buffers. Scale notifications occur only when scale changes; panning emits
no telemetry. There is no persistent crop or gesture queue.

The browser benchmark compares the same bounded input workload before and after the implementation.
Report gesture-dispatch duration and animation-frame interval in milliseconds, p50 and p95, sample
count, environment, and baseline/result delta. The gesture CPU budget is 8 ms p95. Required media
invariants are zero extra session creation, zero media load/empty events, zero media-node replacement,
and zero gesture-induced layout shift while decoded frames continue to advance.

## Task checklist

- [x] Add failing fit, clamp, centroid, resize, and invalid-input tests; implement pure geometry.
- [x] Add failing pointer, pinch, wheel, double-tap, keyboard, cancellation, and bounded-render tests.
- [x] Implement the reusable accessible viewport and test stable media nodes and aligned overlays.
- [x] Integrate event images and focused Peek without enabling compact tile gestures.
- [x] Preserve unscaled recorded playback controls, fullscreen, and capture-phase keyboard routing.
- [x] Add desktop and real multi-touch Playwright coverage at 320, 390, and 768 pixels.
- [x] Add the book chapter, navigation entry, and implementation reference.
- [x] Measure baseline and result with actual decoded media and record reproducible performance evidence.
- [x] Review screenshots, accessibility, and adversarial edge cases.
- [x] Run the complete canonical `./check.sh` gate and production UI build.
- [x] Complete the acceptance evidence table from the final executable tree.

## Acceptance criteria verification

| Criterion                                                               | Observable outcome                                                                                  | Verification                                                           | Evidence                                                       |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | -------------------------------------------------------------- |
| Focused live, recorded, and event images support bounded local zoom/pan | Scale stays within 1x-8x; media remains visible; compact tiles stay unchanged                       | Geometry, gesture, component, and `digital-zoom.e2e.ts` tests          | 43 focused tests, 9 workflow tests, and full gate pass         |
| Input modes coexist                                                     | Alt-wheel, pointer, pinch, double-tap, keyboard, and buttons work without conflicting with playback | Gesture tests, real multi-touch tests, existing Keep keyboard test     | Focused tests pass                                             |
| Reset returns the full frame after interaction and resize               | Identity/reset clears stale pan; resize fits the complete frame                                     | Geometry, viewport, canonical image, and E2E tests                     | Focused tests pass                                             |
| Overlays align; controls and diagnostics remain unscaled                | Bounding-box corners share the media transform; control sizes remain fixed                          | Viewport and EventPreview component tests                              | Focused tests pass                                             |
| Media changes reset; pause preserves context                            | Camera/canonical identity resets; play/pause retains zoom and the media node                        | Component and E2E identity/transport tests                             | Focused tests pass                                             |
| Digital and physical zoom remain distinct                               | Digital controls do not require or send PTZ commands                                                | Component PTZ independence test, capability-limited E2E fixtures, book | Focused tests pass                                             |
| Accessibility, geometry, mobile, overlay, and performance checks pass   | Named controls, 44-pixel touch targets, keyboard access, stable decode, bounded gesture cost        | Focused suites, browser benchmark, full gate                           | Full gate passes; gesture input p95 0.8 ms against 8 ms budget |

## Performance results

Baseline UI: `58edafeb1565a1460ba6c9598f0b6db3785dab3d`. Result: the compact single-row toolbar and
focused viewport implementation in this PR.
Environment: macOS (Darwin 25.6.0), Apple M5 Max, Chromium 151.0.7922.34, Vite 8.2.1 development UI,
release recorder/test-camera binaries, 1440x900. The source is the repository's 640x360 H.264 clip
at 15 fps, paced and looped with `--start-at-seconds 0`.

Each variant runs three times with 120 animation frames per run and 20 pointer moves plus one
Alt-wheel event per frame. There are 360 timing samples per variant. The original UI receives
identical input without applying a digital transform.

| Metric                       | Baseline p50 | Result p50 | Baseline p95 | Result p95 | p95 delta | Budget                |
| ---------------------------- | -----------: | ---------: | -----------: | ---------: | --------: | --------------------- |
| Input dispatch, ms           |          0.2 |        0.3 |          0.3 |        0.8 |      +0.5 | <8 ms p95             |
| Animation-frame interval, ms |         66.7 |       66.6 |         67.1 |       67.5 |      +0.4 | Diagnostic comparison |

Every run has zero added sessions, zero media load/empty events, zero media-node replacement, and
zero layout shift. Decoded frame counters advance in every run. The observed frame cadence is
specific to this browser and 15 fps fixture, not a 60 fps or physical-camera claim.

Run the current implementation from `ui/`:

```sh
KEEPPEEK_E2E_BACKEND_PORT=4517 KEEPPEEK_E2E_FRONTEND_PORT=4518 \
  bun run test:e2e:run --config playwright.digital-zoom-performance.config.ts
```

For the original baseline, extract `ui/` and `assets/` from the baseline commit into a temporary
directory, make the same installed UI dependencies available in its `ui/node_modules`, and run
`bun run build` there. Start that original UI with
`KEEPPEEK_API_TARGET=http://127.0.0.1:4517 bun run dev -- --host 127.0.0.1 --port 4519 --strictPort`.
Then run the same harness from this PR's `ui/`:

```sh
KEEPPEEK_ZOOM_BASELINE=1 KEEPPEEK_E2E_BACKEND_PORT=4517 KEEPPEEK_E2E_FRONTEND_PORT=4519 \
  bun run test:e2e:run --config playwright.digital-zoom-performance.config.ts
```

The baseline mode reuses only the explicitly supplied original frontend. Both variants start the
same recorder fixture with the matching allowed origin and retain the same stability assertions.
`DIGITAL_ZOOM_PERFORMANCE` in stdout records p50/p95 results. The test also attaches full sample
data and produces a real-video screenshot.

## Validation results

- Focused geometry, gesture, viewport, event-image, and transport suites: 43 tests pass.
- Complete E2E suite: 230 pass, two existing unsupported-codec skips. This includes all nine
  digital-zoom workflows, existing keyboard/swimlane flows, and real WebRTC and recording tests.
- Real recorded pixels and controls are verified at 320, 768, 1024, and 1440 pixels. Mobile
  multi-touch uses Chromium emulation at 320, 390, and 768 pixels; physical iOS/Safari qualification
  is not claimed.
- The production UI build, strict Svelte check, E2E typecheck, and book build pass. The installed
  mdbook-mermaid preprocessor reports a 0.5.0/0.5.4 version warning while producing the complete book.
- The original implementation passed `KEEPPEEK_RUN_SLOW_TESTS=1 ./check.sh`: 2,274 Rust tests
  (21 existing skipped tests), 316 server UI tests, 160 browser/visual tests, 57 compatibility
  tests, and 228 E2Es (two existing unsupported-codec skips). The command emitted
  `ISSUE_118_PUBLISH_GATE_PASSED`.
- The compact-toolbar update passes `./check.sh` and `bun run build`: 2,348 Rust tests passed
  (one reported leaky, 21 skipped), 316 server UI tests, 163 browser/visual tests, 57 compatibility
  tests, and 230 E2Es (two codec skips). The command emitted
  `ISSUE_118_COMPACT_TOOLBAR_FINAL_GATE_PASSED`. No timeouts, retries, or quality gates were relaxed.

## Review decisions

- Zoom sits in the top-left toolbar above the picture. Focused Peek composes zoom, Live/History,
  quality, and camera information into a single non-wrapping row. Desktop groups are all 32 px
  high; coarse-pointer groups are 48 px high with 44 px zoom targets. Narrow rows scroll rather
  than introducing a second row or obscuring the media. Placement and height checks cover
  1440, 1024, 900, 768, 390, and 320 px viewports.
- The compact-toolbar real-video benchmark uses the same baseline and workload described above.
  It reports input p50/p95 of 0.3/0.8 ms and frame-interval p50/p95 of 66.6/67.5 ms. Input p95 is
  0.5 ms above the original baseline and remains below the 8 ms budget. Media load/empty events,
  additional sessions, node replacements, and layout shifts remain zero in every run.
- ResizeObserver cancels active pointers before refitting. A browser regression confirms that
  moves from a pre-resize pinch cannot apply stale transforms; no layout read is added to each
  pointer movement.
- Live quality changes preserve inspection of the same camera. Decoded dimension changes refit
  and clamp the media; automatic stream adaptation must not unexpectedly reset local zoom.
- Compact previews keep their existing cover fit. Focused event detail requests contain fit and
  transforms the image and bounding boxes together.

## Sources

- <https://svelte.dev/docs/svelte/@attach>
- <https://svelte.dev/docs/svelte/bind#Audio-and-video>
- <https://svelte.dev/docs/svelte/$props#$props.id()>
- <https://developer.mozilla.org/en-US/docs/Web/API/Element/setPointerCapture>
- <https://developer.mozilla.org/en-US/docs/Web/API/ResizeObserver>
- <https://developer.mozilla.org/en-US/docs/Web/CSS/touch-action>

## Publication

The user requested an implementing PR after verification. The PR carries the acceptance table,
measured performance, and final validation evidence. The issue remains open until merge; this task
does not merge the PR or publish a release.
