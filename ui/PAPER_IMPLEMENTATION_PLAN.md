# KeepPeek Paper-to-Svelte Implementation Tracker

> Last updated: 2026-08-20
> Overall status: In progress
> Paper source: `KeepPeek — NVR Design System & Spec` (`01M0B0VBH78TMTX40GCYYQ37SG`)
> Canonical design revision: 34 boards
> Canonical token hash: `cf3b1cd7`

Implement the complete current 34-board Paper system as a phased Svelte 5 migration. The anti-drift path is **Paper → Storybook → Svelte → Loki → GitHub Actions**: Paper defines each approved scenario, Storybook renders that scenario with deterministic Svelte fixtures, Loki compares the rendered story to its approved reference, and GitHub Actions blocks drift. The same stories may carry structured demo direction that Playwright records as captioned videos. Playwright separately proves route integration and behavior. The checked-in `api/` contract owns transport: HTTP is limited to `/create`, `/delete`, `/logs`, and `/metrics`; application commands use binary protobuf over the negotiated WebRTC control channel and fail closed when not implemented.

## How To Track Progress

- Use `[ ]` for not started, `[~]` for in progress, `[x]` for complete, and `[!]` for blocked.
- Update **Current focus** before beginning a slice and **Last completed** after its acceptance gate passes.
- A slice is complete only after its Svelte implementation, Storybook scenarios, Playwright behavior tests, Paper overlay review, Loki approval, and the applicable repository checks all pass.
- Every Paper implementation frame must map to a stable Storybook story ID. Stories are the deterministic visual scenario boundary; production routes must reuse the same Svelte components and view models.
- Every new page or route must ship with at least one Playwright behavior test in the same slice.
- Every authored scenario, state, viewport, and interaction in the Paper manifest must map to a named Playwright test or parameterized test case. A broad smoke test or screenshot alone does not satisfy this requirement.
- Each applicable page spec must cover its happy path plus loading, empty, error, unavailable-capability, mobile, and keyboard behavior when those states exist in Paper.
- Record concrete evidence in the progress log: date, item, tests or commands, screenshot or Paper revision, and PR or commit when one exists.
- Never update Loki references without confirming the corresponding Paper reference and manifest revision.
- Never accept a Loki reference produced only from the implementation. Its manifest entry must identify the reviewed Paper frame and reference-image hash from which it was approved.

- **Current focus:** Close the remaining cross-platform PR checks with lockfile-free public-registry installs.
- **Last completed:** Linux Storybook/Loki passed all 50 deterministic scenarios against the 14 hash-locked Paper-approved references.
- **Current blockers:** The remaining 36 visual scenarios are deliberately capability-gated with named product/API evidence gaps in `design/paper/keeppeek-nvr-v34/COVERAGE.md`; they remain candidate artifacts rather than approved baselines.

## Progress Overview

| Phase | Status          | Scope                                     | Acceptance gate                              |
| ----- | --------------- | ----------------------------------------- | -------------------------------------------- |
| 0     | [x] Complete    | Freeze design and capability contracts    | 34-board manifest complete and reviewable    |
| 1     | [~] In progress | Tokens, shell, Storybook, Loki, visual CI | Foundation stories, tests, and Loki pass     |
| 2A    | [~] In progress | Peek, layouts, fleet, rewind              | Boards 06/08/11/22/31 accepted               |
| 2B    | [~] In progress | Keep, Events, export states               | Boards 04/09/10/22/29/33 accepted            |
| 2C    | [~] In progress | Camera, PTZ, onboarding, defaults         | Boards 07/12/21/23–25/33 accepted            |
| 2D    | [~] In progress | Health and diagnosis                      | Boards 15/20/26/30 accepted                  |
| 2E    | [~] In progress | Administration                            | Boards 13–20/23/27/28/33–34 accepted         |
| 3     | [~] In progress | Cross-cutting parity and final sign-off   | All 34 boards accounted for; full gates pass |

## Milestone Checklist

- [x] **0.1** Export all canonical Paper implementation frames, tokens, dimensions, IDs, and metadata. The complete 34-board source/token bundle and all 50 authored application references are checked in and hash-locked.
- [x] **0.2** Commit the board-to-route/state/fixture/theme/capability manifest and update `ui/PRODUCT.md` to 34 boards.
- [x] **0.3** Define the fail-closed versioned capability boundary and backend follow-up contract.
- [x] **1.1** Install Paper tokens and bundled Archivo/IBM Plex Mono typography. All 80 runtime tokens are generated and checked; exact installed Fontsource 5.3.0 Latin weights 300–700 are verified from package manifests, emitted as local production assets, and awaited before visual capture.
- [x] **1.2** Rebuild the responsive desktop rail and mobile tab shell without changing live-peer ownership.
- [x] **1.3** Add the exact capability provider and command-state tests.
- [x] **1.4** Add deterministic fixtures, Storybook scenarios, Loki references, Playwright ownership, and pinned Linux visual CI. All 50 rendered scenarios have unique hash-locked Paper references, production-component Storybook sources, named Playwright owners, stable scenario-based Loki paths, desktop/mobile routing, and CI candidate artifacts. The 14 Paper-accepted Linux baselines are committed, and Linux Storybook/Loki passes all 50 scenarios using exact direct dependency pins and lockfile-free public-registry installs.
- [x] **1.5** Add typed Storybook descriptions and Playwright demo-video renderers with captions, metadata, and CI artifacts. The registry validates ordered actions and completion signals; static stories trim from explicit `demo-start`, while production stories start a real server and wait for decoded media. Default artifacts are single-stream H.264 MP4, sidecar WebVTT, and fixture/commit metadata; raw WebM is deleted.
- [~] **1.6** Publish generated demo videos automatically. `Generate Demo Videos` records the registry and prepares the exact `keeppeek-demo-videos` artifact consumed by the Azure Blob publisher. Stable scenario links, overwrite-only uploads, stale-asset deletion, manifest-last publication, and no Git writes are implemented; environment configuration and the first main-branch publication remain.
- [~] **2A.1** Implement and accept Peek states, layout editing, fleet virtualization, mobile Peek, and rewind-to-Keep. The full Svelte and Playwright implementation is complete; Boards 06 and 31 are Paper-overlay accepted, Boards 08 and 11 have measured blocked candidates, and remaining Paper/Loki approval is open.
- [~] **2B.1** Implement and accept the Paper timeline, Keep modes, Events browser/detail, and gated export lifecycle. Boards 04, 09, 10, and 29 are implemented; Board 09 Calendar and Board 29 are Paper-overlay accepted. Export create/list/get/cancel/retry/download stays on the canonical WebRTC control and reliable-data channels, and missing capability still preserves the draft. Remaining Paper references and canonical Linux Loki approval remain blocked.
- [~] **2C.1** Implement and accept Camera/PTZ, add-camera/first-run wizard, camera defaults, and responsive states. Boards 07, 12, 21, 23, 24, and 25 include session-owned Reolink PTZ, presets, responsive controls, and fail-closed unsupported actions; Board 07 now has a measured candidate, while candidate probes, setup completion, editable defaults/inheritance, and Paper/Loki approval remain.
- [~] **2D.1** Implement and accept Health overview, logs integration, and the shared camera-diagnosis destination. Boards 15, 20, 26, and 30 are implemented; missing history/retry/probe contracts and Paper/Loki approval remain.
- [~] **2E.1** Implement and accept storage, defaults, event sources, integrations, notifications, groups, access, appearance, system, and responsive administration. Boards 13, 14, 16–20, 23, 27, and the Board 28 control contract are implemented fail-closed. Canonical media, typed camera/configuration/health evidence, metrics, Bearer access, and exact-origin CORS are implemented; async/light-theme work and Paper/Loki approval remain.
- [x] **3.1** Apply and test the keyboard action model across all implemented workflows. Board 32 is covered by pure resolver tests and nine named Playwright scenarios across global, Peek, Keep, Events, Cameras, and Settings owners.
- [x] **3.2** Apply every authored waiting, loading, empty, no-results, applying, and capability-loss state. Board 33's six states are implemented in their owning workflows with focused unit and Playwright coverage.
- [~] **3.3** Complete frame-by-frame Paper overlay and canonical screenshot acceptance. All 50 authored references are classified: 14 accepted with hash-locked Linux baselines, and 36 deliberately capability-gated with named blockers. Boards 01–03, 05, 28, and 32 are accounted contracts with no authored application raster.
- [~] **3.4** Pass focused tests, real-media E2E, accessibility checks, performance budgets, Linux visual CI, and `./check.sh`. The canonical repository gate and Linux visual CI pass, including 85 Playwright and 50 Loki scenarios; the final cross-platform CI rerun remains.
- [x] **3.5** Publish the final 34-board coverage report and close or link every deliberate capability gate. `design/paper/keeppeek-nvr-v34/COVERAGE.md` is generated from canonical manifests and freshness-checked by `paper:check`.

## Detailed Plan

### Phase 0 — Freeze the Design Contract

1. **Export and version the canonical Paper revision.** Through the configured Paper MCP, enumerate all 34 in-scope board IDs and implementation frames, then export native-scale lossless references, the complete token table, and board metadata under `ui/design/paper/keeppeek-nvr-v34/`. Create a manifest that records design file and revision, board and frame IDs, route, UI state, fixture key, theme, authored viewport, required capability IDs, and reference image. Export screen frames separately from explanatory board chrome so tests compare the application, not annotations. Keep `ui/PRODUCT.md` synchronized with the in-scope board set. This blocks component work: no frame is implemented until it has a manifest row and dimensions.
2. **Turn the boards into a coverage matrix.** Classify 01–05 as design-system and handoff constraints; 06–12 and 31 as Peek, Camera, layout, fleet, and review workflows; 13–21, 23, and 28–30 as administration, capability, export, and diagnosis; 22 and 24–27 as responsive frames; and 32–34 as keyboard, state, and theme contracts. Map every implementation frame to an existing route, a deliberate new route, or an in-page mode, and identify whether its production data is live, optional, absent, or capability-gated. Preserve existing deep links unless Paper explicitly requires a new destination.
3. **Lock the API and capability boundary.** Extract Board 28's exact capability identifiers into a typed frontend catalog. The current `ServerCapabilities` protobuf has structural camera and media capabilities but no generic versioned UI-capability list, so the Svelte provider must default unknown capabilities to unsupported and must never hardcode future support as true. Existing typed REST functions may remain live through an explicit compatibility adapter; opening new gates requires a separate server advertisement contract. Rust feature implementation is out of scope, and production UI must never display fixture-only values.

### Phase 1 — Build the Shared Foundation

4. **Install the Paper visual system.** Replace the legacy six-color and generic shadcn theme with the registered Paper color, typography, spacing, radius, container, breakpoint, signal, video, and light-theme tokens. Map those primitives into Tailwind v4 and shadcn semantic variables without duplicating values. Bundle exact Archivo and IBM Plex Mono weights from pinned public npm packages so KeepPeek remains local-first and snapshots do not depend on system fonts or a CDN. Preserve the existing `keeppeek-theme` preference while making the initial paint deterministic.
5. **Rebuild the application shell and repeated primitives.** Keep `setLivePeer()` and browser logging ownership in the root layout, but implement the Paper desktop rail, fixed workspace geometry, and the 390×844 mobile tab bar (`Peek`, `Keep`, `Events`, `Health`, `More`). Add only repeated, independently meaningful primitives: page and section headings, status signals, exact capability gates, fixed action bars, desktop anchor rail, mobile tab bar, timeline markers, and deterministic loading or empty shells. Reuse shadcn-svelte and Lucide; do not create another component vocabulary or custom SVG icons.
6. **Add the capability provider and command-state rules.** Model exact IDs, structural camera capabilities, pending, succeeded, and failed commands, and mid-edit capability loss in a request-isolated Svelte 5 context or factory. A missing capability leaves observed data visible, replaces the command with `Server update required · <id>`, and preserves drafts. Applying states are pessimistic rather than optimistic. Add focused component tests for supported, unsupported, failure, and capability-loss cases before routes consume the provider.
7. **Create deterministic Storybook, Loki, and Playwright infrastructure.** Loki 0.35 supports Storybook through version 8, while KeepPeek's production Vite 8 requires a newer Storybook line. Isolate the visual harness as a Bun workspace pinned to Storybook 8 and Vite 6 instead of downgrading or adding unsupported peers to the production app. Every Paper scenario names a Storybook story, and every route names its owning Playwright spec and test case in the design manifest. Stories render production Svelte components using canonical fleet, event, recording, health, capability, and config fixtures. Loki runs those stories in pinned Linux Chromium at native Paper viewports with fixed time, bundled fonts, disabled motion and caret, stable scrollbars, and static video posters. Commit Paper references and Loki references; update Loki only with a matching Paper revision and overlay review. Capture current, reference, difference, story metadata, and Paper reference artifacts on failure. Playwright continues to run behavior E2E across Ubuntu, macOS, and Windows. Visual assertions supplement rather than replace behavior assertions.
8. **Generate deterministic demo timelines from stories.** Give demo-capable stories human-readable documentation plus typed demo metadata for title, purpose, ordered narration cues, captions, viewport, source timing, and actions. Use Playwright against either the static story preview or an isolated real-server application fixture. Treat Playwright WebM as disposable input, retain the trimmed silent H.264/yuv420p source for auditability, then publish one audio-paced H.264/AAC MP4, metadata JSON, and narration-timed sidecar WebVTT file per story. Loki uses a stable visual story or stable checkpoint, never a frame captured while the demo is moving. GitHub Actions validates demo metadata, measured WAV manifests, and final media streams.

### Phase 2 — Migrate User Workflows

9. **Peek, layouts, fleet, and rewind (Boards 06, 08, 11, 22, 31).** Rebuild `/` around the Paper live wall while preserving the shared `LivePeer.configure/attach` lifecycle and native video playback. Implement healthy, degraded, reconnecting, and offline tile states, quiet recording versus red failure signals, quality and focus behavior, the in-place 12-column layout editor with undo, discard, and done, Activity Focus and pinned overrides, and the two-minute drag-to-rewind transition into Keep without renegotiating or disturbing other live tiles. Rework `/cameras` into the fixed 56px fleet rows and a bounded fixed-row virtualizer for 127 sources. Persisted layout commands remain capability-gated when absent. Validate desktop, mobile, empty, degraded, keyboard, and light or dark frames before moving on.
10. **Keep, Events, and export (Boards 04, 09, 10, 22, 29, 33).** Preserve the existing recording player and timeline math, but render the exact newest-at-top single right-edge column: ruler, availability, explicit gaps, event thumbnails and cards, leader lines, selected playhead, fixed zoom levels, live-follow and Back-to-live behavior, and no scrollbar-position indicator. Add Paper's stories, calendar availability, and up-to-eight shared-clock swimlane modes without inventing production data. Add `/events` with URL-backed structured filters, no-image, story, and low-confidence cards, source provenance, detail drawer, payload, zone, bounding box, and revisions when returned. Implement the export range UI and all four job states, and gate create, cancel, retry, and download on the exact advertised media-export capability. Test URL restoration, seek and follow, gaps, filters, detail, gated export, and draft preservation.
11. **Camera, PTZ, onboarding, and defaults (Boards 07, 12, 21, 23–25, 33).** Refactor the existing camera page into the Paper live-preview plus sticky-section design for connection, events, audio, advanced settings, capability evidence, and per-camera overrides. Render PTZ only when the camera reports support; route all commands through one typed control adapter and fail closed if no command transport is advertised. Replace the inline add-camera form with the five-step wizard shared by `/cameras/new` and first-run: discovery progress is visible, step 3 ends in stream evidence, and no config write occurs before step 5. Add shared camera defaults with explicit inheritance and override semantics and test storage-not-writable, discovery-empty, command-failure, and mobile states.
12. **Health and diagnosis (Boards 15, 20, 26, 30).** Restyle `/system-health` around the critical verdict, cost-ranked issues, server, storage, and camera evidence, and browser WebRTC diagnostics already available. Add one responsive camera-diagnosis destination at `/system-health/camera/[cameraId]` and point every Diagnose affordance to it. Populate the fact block, ordered remediation, evidence chart, and outage cost only from current health data or known issue mappings; do not infer camera facts. Keep `/settings/logs` functional and visually integrate it with the Paper shell. This step can run in parallel with Step 11 after Steps 4–8.
13. **Administration surfaces (Boards 13–20, 23, 27, 28, 33–34).** Split the current settings route into cohesive sections behind one Paper anchor rail: storage and retention, camera defaults, event sources, integrations, notifications, groups, access, appearance and system, and logs. Reuse current config, discovery, and logging APIs where they exist. Render missing backend-owned values as honest empty or read-only states and every unavailable command with its exact capability ID; never fabricate tokens, users, integrations, destinations, audit rows, or delivery status. Keep local-access-as-Administrator and remote-auth-required copy aligned with `PRODUCT.md`. Responsive administration uses the same components, not separate mobile routes. This step can proceed section by section in parallel with Steps 9–12 once the shell and gate primitives are stable.

### Phase 3 — Cross-Cutting Completion and Sign-Off

14. **Apply keyboard, waiting, empty, and responsive contracts continuously.** Implement Board 32 as a scoped action registry: typing wins over single-key shortcuts, no destructive single key, gated actions stay gated, focus is visible, and `?` exposes the current bindings. Implement Board 33 in each owning workflow rather than as vague generic copy: first keyframe, cold seek, five-second discovery, no search result with the constraining clause, applying without optimism, and lane-preserving non-shimmer skeleton. At each slice, validate authored desktop and 390×844 frames plus intermediate widths for overflow, focus order, fixed media and timeline dimensions, and touch targets.
15. **Perform frame-by-frame parity acceptance.** For each manifest row, render the named Storybook story, assert its semantic state, then compare the Loki candidate with the Paper frame using side-by-side and 50% overlay review. Record acceptance in the manifest; do not approve an implementation-generated Loki reference that has not been compared to Paper. Loki protects the approved story from later visual drift. Playwright protects the real route and interaction flow from behavioral drift. Keep real-media E2E separate so deterministic Storybook posters never weaken `peek-live.e2e.ts` coverage of the native video element and advancing decoded frames.
16. **Stabilize the full application.** Update rather than discard existing route E2E tests, add focused unit and browser tests for capability state, timeline math, virtualized rows, URL serialization, keyboard scoping, and draft preservation, then run all behavior and visual suites. Fail the coverage review when any route, Paper manifest scenario, or designed viewport lacks a named Playwright owner. Profile the Paper timeline targets with representative dense data, verify zero layout shift while scrubbing, bounded DOM rows at 127 sources, and no live-peer renegotiation during Peek-to-Keep rewind. Finish with the repository gate and a board-coverage report showing every one of the 34 boards as constraint, implemented frame, or intentionally capability-gated state.

## Relevant Files

- `ui/design/paper/keeppeek-nvr-v34/` — canonical Paper exports, token data, manifest, and board or frame coverage.
- `ui/PRODUCT.md` — keep the evidence record synchronized with all 34 in-scope boards and their product surfaces.
- `ui/src/app.css` and `ui/src/styles/style.css` — Paper primitives, semantic mappings, fonts, dark and light themes, and stable layout foundations.
- `ui/src/routes/+layout.svelte` — retain live-peer and logging ownership while replacing desktop and mobile shell and theme bootstrap.
- `ui/src/lib/stream-peer.svelte.ts`, `ui/src/lib/stream-peer-context.ts`, and `ui/src/lib/components/LiveVideo.svelte` — preserve one peer connection and native video while adapting Paper states.
- `ui/src/lib/capabilities.ts` and `ui/src/lib/capability-context.svelte.ts` — exact-ID catalog, fail-closed provider, structural capability adapter, and command state.
- `ui/src/lib/components/shell/` and `ui/src/lib/components/design/` — repeated Paper shell, status, gate, and action primitives only.
- `ui/src/routes/+page.svelte` and `ui/src/routes/cameras/+page.svelte` — Peek wall, layout editor and rewind, and fleet virtualization.
- `ui/src/routes/keep/+page.svelte`, `ui/src/lib/components/VerticalTimeline.svelte`, and `ui/src/lib/components/RecordingFilmstrip.svelte` — Keep modes, exact timeline, stories, calendar, swimlanes, and export surface.
- `ui/src/routes/events/+page.svelte` — Events browser and detail surface.
- `ui/src/routes/camera/+page.svelte`, `ui/src/routes/cameras/new/+page.svelte`, and `ui/src/routes/setup/+page.svelte` — camera and PTZ, reusable five-step wizard, and first-run composition.
- `ui/src/routes/system-health/+page.svelte` and `ui/src/routes/system-health/camera/[cameraId]/+page.svelte` — health overview and single diagnosis destination.
- `ui/src/routes/settings/+page.svelte`, `ui/src/routes/settings/logs/+page.svelte`, and `ui/src/lib/components/settings/` — Paper anchor rail and cohesive live or gated administration sections.
- `ui/src/lib/api.ts` and `ui/src/lib/types.ts` — retain API ownership and add only response or view-model adapters supported by real wire data.
- `ui/.storybook/`, `ui/src/**/*.stories.ts`, and `ui/src/lib/storybook/` — deterministic Storybook configuration, Svelte scenarios, decorators, and shared fixtures.
- `ui/loki.config.cjs` and `ui/.loki/` — pinned Linux Chromium configurations, approved references, current captures, and differences.
- `ui/src/lib/storybook/demo.ts`, `ui/src/lib/server/storybook/azure-openai-tts.ts`, `ui/src/lib/server/storybook/demo-video.ts`, and `ui/scripts/render-story-demos.ts` — typed demo metadata, Azure OpenAI TTS narration, Playwright recording, ffmpeg muxing, captions, and artifact metadata.
- `ui/src/lib/server/storybook/video-publish.ts`, `ui/scripts/prepare-demo-publish.ts`, and `.github/workflows/publish-demo-videos.yml` — stable hosted-video manifests and automatic Azure OIDC publishing.
- `ui/e2e/`, `ui/playwright.config.ts`, and `ui/package.json` — route behavior specs, story and visual scripts, and pinned tooling dependencies.
- `.github/workflows/ci.yml` — retain cross-platform Playwright behavior jobs and add Storybook build plus pinned Linux Loki jobs with diff artifacts.

## Verification

1. Validate the Paper manifest: all 34 boards accounted for, every implementation frame has an existing Paper reference, Storybook story ID, viewport, route and state, fixture, theme, capability set, owning Playwright file, and named test case, with no duplicate ownership.
2. Build Storybook and run Loki for every mapped Paper story at its authored viewport and theme.
3. Validate every demo-tagged story's description, narration, captions, duration, actions, and stable completion signal; render at least one smoke video in CI.
4. For each page, run every mapped happy-path and applicable loading, empty, error, capability-gated, keyboard, mobile, and responsive Playwright scenario.
5. After each slice, run the narrow unit or component test, Storybook build, Loki story subset, demo metadata validation, and matching Playwright file first, then `bun run check`, `bun run test:e2e:typecheck`, and the affected behavior E2E specs from `ui/`.
6. Run pinned Linux Loki against every authored story; require exact static chrome and explicit local treatment for truly dynamic media pixels. Upload Paper reference, Loki reference, current, and difference images on failure.
7. Run accessibility checks on representative dark and light desktop and mobile stories and routes, keyboard-only paths, focus order, names, contrast, reduced motion, and touch sizing; keep `svelte-check --fail-on-warnings` clean.
8. Run existing real-media coverage, especially `ui/e2e/peek-live.e2e.ts`, to verify native `<video>`, expected dimensions, advancing frames, and no canvas fallback after the visual refactor.
9. Run `./check.sh` from the repository root, then Storybook build and Loki, and complete the Paper overlay and scenario-coverage review before declaring parity.

## Decisions

- Scope is Boards 01–34, delivered in independently verifiable phases. Native iOS and Android viewer boards 35–43 are intentionally ignored.
- This is a Svelte-first implementation. Current APIs work; absent backend features remain visible and read-only and use exact capability gates. Identity and roles, rules, notification delivery, integration registries, group administration, offsite archive, and persisted layout backend work are excluded.
- Paper exports and Loki references are committed. Pinned Linux Chromium is the canonical raster renderer; Playwright behavior E2E remains cross-platform.
- Pixel parity is exact at authored frame sizes after deterministic setup. Intermediate widths are responsive-correctness targets, not invented pixel-reference designs.
- Boards 01–05, 28, and parts of 32–34 are contracts and test matrices, not routes.
- Production never uses design fixtures, and missing server data is not synthesized.
- Existing URL and deep-link behavior, one shared peer connection, native video rendering, Svelte 5 runes and context, shadcn-svelte, Lucide, Bun, and `./check.sh` remain repository requirements.
- Detection, face or license recognition, zone or mask editors, and any competitor naming remain out of scope.

## Known Prerequisite

Board 28 capability IDs are delivered only by `ServerCapabilities.capability_ids` over the negotiated control channel. The server advertises `keeppeek.media-export.v1` because that complete owning contract has shipped; all other IDs remain absent until their contracts ship. The UI must not infer support from version numbers, partial command families, or optimistic endpoint assumptions.

## Board Coverage Ledger

| Boards                  | Surface or contract                                 | Implementation | Paper parity | Notes                                                                           |
| ----------------------- | --------------------------------------------------- | -------------- | ------------ | ------------------------------------------------------------------------------- |
| 01–05                   | Positioning, tokens, IA, timeline contract, handoff | [x]            | [~]          | Boards 01/02/03/05 accounted; Board 04 is capability-gated                      |
| 06, 08, 11, 22, 31      | Peek, layouts, fleet, mobile, rewind                | [x]            | [~]          | Boards 06/31 accepted; Board 08/11 candidates; other references remain          |
| 04, 09, 10, 22, 29, 33  | Keep, Events, export and states                     | [~]            | [~]          | Board 09 Calendar accepted; other Board 09/10 candidates and Loki remain        |
| 07, 12, 21, 23–25, 33   | Camera, PTZ, onboarding, defaults                   | [~]            | [~]          | Board 07 candidate exists; remaining backend contracts, references, Loki open   |
| 15, 20, 26, 30          | Health, logs and diagnosis                          | [~]            | [~]          | Board 30 shared story aligned; history/retry/probe evidence remains unavailable |
| 13–20, 23, 27–28, 33–34 | Administration, gates and themes                    | [~]            | [ ]          | Boards 13/14/16–20/23/27 implemented; capability/async/theme references open    |
| 32–34                   | Keyboard, async and empty states, light theme       | [x]            | [~]          | Board 32 contract accounted; Boards 33/34 Paper overlays accepted; Loki remains |

## Playwright Page and Scenario Ledger

Every row must have behavior coverage and every Paper manifest frame must have canonical visual coverage before its implementation milestone can be marked complete. Existing files should be extended when they already own the workflow; new files should stay focused on one route or coherent cross-route contract.

| Page or surface                   | Playwright owner                                                                                     | Required scenarios                                                                                                                                                                               | Status |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------ |
| Application shell and all routes  | Extend `ui/e2e/theme.e2e.ts`; add `ui/e2e/shell.e2e.ts`                                              | Desktop rail, mobile tab bar, active destination, light and dark persistence, keyboard help, no console or page errors                                                                           | [ ]    |
| Peek `/`                          | Extend `ui/e2e/dashboard.e2e.ts` and `ui/e2e/peek-live.e2e.ts`; add focused layout and rewind specs  | Empty, loading, live, degraded, reconnecting, offline, quality, focus, layout edit, Activity Focus, pinned override, rewind-to-Keep, mobile                                                      | [~]    |
| Cameras `/cameras`                | Add `ui/e2e/cameras.e2e.ts`                                                                          | Mixed source rows, 127-source virtualization, selection and navigation, empty and no-results, responsive layout                                                                                  | [ ]    |
| Camera `/camera`                  | Extend `ui/e2e/camera.e2e.ts`                                                                        | Details, inherited and overridden values, PTZ absent, PTZ supported, presets, command failure, capability loss, mobile                                                                           | [~]    |
| Add camera `/cameras/new`         | Add `ui/e2e/add-camera.e2e.ts`                                                                       | Discovery progress, discovered and manual camera, evidence step, validation failure, storage not writable, cancel, no write before step 5, mobile                                                | [ ]    |
| First run `/setup`                | Add `ui/e2e/setup.e2e.ts`                                                                            | New installation, storage check, optional remote sign-in, first camera, completion, resume after failure, empty state                                                                            | [ ]    |
| Keep `/keep`                      | Extend `ui/e2e/keep-events.e2e.ts` and `ui/e2e/keep-filmstrip.e2e.ts`; add timeline and export specs | Availability, explicit gap, seek, zoom, drag, live follow, Back to live, story, calendar, swimlanes, cold seek, empty day, mobile                                                                | [~]    |
| Events `/events`                  | Add `ui/e2e/events.e2e.ts`                                                                           | URL-backed filters, no results, no-image event, story, low confidence, detail drawer, source, payload, zone, bounding box, revisions, mobile                                                     | [~]    |
| Export lifecycle                  | Add `ui/e2e/keep-export.e2e.ts`                                                                      | Unsupported gate, range preserved, running, cancel, ready, download, partial gap, trim, failed, retry                                                                                            | [x]    |
| Health `/system-health`           | Extend `ui/e2e/health.e2e.ts`                                                                        | Healthy, degraded, critical verdict, issue ordering, storage evidence, WebRTC evidence, empty metrics, mobile                                                                                    | [ ]    |
| Camera diagnosis                  | Add `ui/e2e/camera-diagnosis.e2e.ts`                                                                 | Entry from each Diagnose action, fact block, ordered remedy, recommended action, evidence chart, outage cost, missing evidence, mobile                                                           | [ ]    |
| Settings `/settings`              | Extend `ui/e2e/settings.e2e.ts` and add focused section specs as needed                              | Storage and retention, camera defaults, event sources, integrations, notifications, groups, access, appearance and system, exact capability gates, applying, failure, draft preservation, mobile | [ ]    |
| Logs `/settings/logs`             | Extend `ui/e2e/logging.e2e.ts` and preserve `ui/e2e/logging-fullstack.e2e.ts`                        | Loading, streaming, filtering, persistence, connection failure, export or download when supported, mobile                                                                                        | [ ]    |
| Cross-cutting visual parity       | Add manifest-driven visual specs under `ui/e2e/visual/`                                              | Every Paper implementation frame at its authored theme and viewport, geometry-critical assertions, approved reference and baseline revision                                                      | [ ]    |
| Cross-cutting states and keyboard | Keep scenarios in each owning page spec; add a focused keyboard contract spec                        | First keyframe, cold seek, discovery, no results, applying, skeleton, capability loss, shortcut suppression while typing, focus visibility, destructive-action safeguards                        | [x]    |

## Paper-Locked Storybook, Loki, and Playwright Protocol

Paper is the design source, Storybook is the deterministic Svelte scenario catalog, Loki protects approved story pixels, Playwright protects route behavior, and GitHub Actions enforces the chain. None replaces another.

### Scenario Contract

Every visual scenario manifest entry must contain:

- A stable scenario ID, such as `peek.desktop.degraded`.
- The Paper file ID, token hash, board ID, and inner implementation-frame ID.
- The committed Paper reference path and its SHA-256 hash.
- Route, query string, fixture ID, viewport, color scheme, locale, time zone, and frozen clock.
- Required and missing capability IDs.
- The stable Storybook story ID and story source path.
- The owning Playwright file and exact test title.
- The approved Loki reference path and SHA-256 hash.
- Any dynamic-region mask or raster tolerance, with a reason. No global masks or global tolerance increases.

CI must fail when a Paper reference hash changes without a new review, a Loki reference changes without its Paper metadata, a Paper frame has no Storybook story, a route has no owning Playwright test, or a story or test points to an unknown scenario.

### Story Description and Demo Contract

Each Storybook scenario may expose two complementary text layers:

- `parameters.docs.description.story` is the readable explanation shown in Storybook Docs.
- `parameters.demo` is typed production data for video generation. It contains `title`, `purpose`, optional `narration`, viewport, total duration, ordered captions, and action timing.

The story's typed `parameters.demo.actions` own executable interaction through accessible selectors and keys. The Playwright renderer executes that ordered timeline; it never parses prose or narration to guess what to click. Storybook `play` functions may reuse the same actions for in-browser documentation, but they are not a second timing authority.

```ts
export const RewindToKeep = {
	parameters: {
		docs: {
			description: {
				story: 'Drag a live tile back 38 seconds, then continue reviewing in Keep.'
			}
		},
		paper: {
			boardId: '5GD-0',
			scenarioId: 'peek.desktop.rewind-to-keep'
		},
		demo: {
			title: 'Review what just happened',
			purpose: 'Show the live-to-recorded transition without disturbing other cameras.',
			narration: {
				voice: 'coral',
				instructions: 'Speak clearly in a calm product-demo tone.',
				cues: [
					{ atMs: 0, text: 'First, choose one live camera.', pauseAfterMs: 250 },
					{ atMs: 2500, text: 'Then drag backward to review the last two minutes.' }
				]
			},
			durationMs: 9000,
			viewport: { width: 1440, height: 860 },
			actions: [
				{
					kind: 'pointer-drag',
					atMs: 2500,
					selector: '[aria-label="Rewind Front Door"]',
					deltaX: 0,
					deltaY: 160,
					durationMs: 1000,
					holdAfterMs: 2000
				}
			],
			completionSignal: { selector: '[data-demo-landed-in-keep]', state: 'visible' },
			captions: [
				{ atMs: 0, text: 'Every camera remains live.' },
				{ atMs: 2500, text: 'Drag one tile back 38 seconds.' },
				{ atMs: 6500, text: 'Continue at that moment in Keep.' }
			]
		}
	}
};
```

Visual stories and demo stories may share a fixture, but Loki captures a stable state. If a demo moves through several states, create explicit static checkpoint stories for Loki rather than snapshotting arbitrary video frames.

Narration uses the GA Azure OpenAI `gpt-4o-mini-tts` model. Each story pins a voice, optional delivery instructions and speed, plus ordered text cues on the silent source timeline. `AZURE_OPENAI_ENDPOINT` and `AZURE_OPENAI_TTS_DEPLOYMENT` select the deployment. Local and CI generation authenticate with short-lived Entra tokens; local API-key authentication is disabled on the Azure account. Credentials are never copied into story metadata or artifacts.

Playwright internally records a silent WebM and does not expose an H.264 recorder. After the browser context closes, ffmpeg trims and transcodes it to a one-stream H.264/yuv420p audit source, then deletes the WebM. Azure OpenAI produces one normalized PCM WAV per cue. The canonical published MP4 contains one H.264/yuv420p video stream and one AAC narration stream; WebVTT remains a narration-timed sidecar.

All source media uses one scenario clock in milliseconds. Playwright records before navigation, then the renderer records the monotonic offset at the explicit `demo-start` signal as `recordingPreRollMs`. ffmpeg trims that pre-roll and resets source timestamps to zero. Azure narration is generated per cue and measured with `ffprobe`; manifests bind each WAV's source time, duration, byte count, and SHA-256 to immutable story text.

Each cue owns the visual phase from its source timestamp to the next cue. The phase plays at normal speed. If narration plus its authored pause lasts longer, ffmpeg clones the phase's final frame until speech catches up; only then can the next phase and action begin. Short narration never accelerates video. The generated sidecar captions use the expanded output timeline, and metadata records every output start and freeze duration.

The generated artifact set is:

- `assets/<scenario-id>.mp4` containing one H.264/yuv420p video stream and one AAC narration stream.
- `assets/<scenario-id>.vtt` containing narration cues on the expanded output timeline.
- `assets/<scenario-id>.json` containing Paper IDs, story ID, fixture hash, source and output durations, commit SHA, codec evidence, WAV measurements, and freeze durations.
- `silent/<scenario-id>.mp4`, `.vtt`, and `.json` preserving the authored source timeline for auditability.
- `narration/<scenario-id>/*.wav` plus `manifest.json`, retained in the Actions artifact and excluded from public Blob publication.

GitHub Actions validates all demo metadata on pull requests. Video rendering is a separate manual or release job so normal pull requests stay fast; failures upload the partial recording, console log, and metadata.

### Story and Route Test Shape

Each Storybook and Loki scenario follows this sequence:

1. Load the named deterministic fixture before navigation.
2. Set viewport, theme, locale, time zone, reduced motion, and frozen time from the manifest.
3. Render the stable Storybook story and wait for fonts, fixtures, media posters, and an explicit visual-ready condition.
4. Assert the story's semantic state before capture.
5. Assert critical geometry such as the 64px rail, 52px context bar, fixed video ratio, timeline lanes, and absence of horizontal overflow when the Paper frame specifies them.
6. Capture only the implementation frame, excluding browser and Paper annotation chrome.
7. Compare against the reviewed Loki reference at zero drift for static chrome. Permit a local threshold only for documented raster differences or masked media pixels.
8. Attach actual, expected, diff, scenario metadata, and the Paper reference to a failed CI run.

The matching Playwright case navigates through the real application route, performs the intended interaction, and asserts state, URL, accessibility, capability behavior, errors, and critical geometry. It does not own the visual reference.

### Baseline Approval

`loki update` creates candidates, never approvals. To approve a candidate:

1. Confirm the manifest still resolves to the intended Paper frame and reference hash.
2. Compare candidate and Paper reference side by side and with a 50% overlay.
3. Resolve unexplained spacing, typography, color, alignment, clipping, and responsive differences in code.
4. Record the reviewer, date, Paper token hash, Storybook story ID, and accepted Loki hash in the manifest.
5. Commit the Paper reference, manifest change, story, and Loki reference together.

CI never runs `loki update` or `loki approve`. A changed Loki reference without its linked manifest approval is a failure, not a routine regeneration.

### Determinism Rules

- Canonical Loki tests run only in pinned Linux Chromium; cross-platform Playwright remains responsible for behavior.
- Fonts are bundled and `document.fonts.ready` must resolve before capture.
- Animations, transitions, caret, blinking status indicators, and random IDs are disabled or fixed in visual mode.
- Times, dates, locale, time zone, network responses, capability sets, and media posters come from versioned fixtures.
- Live WebRTC behavior remains covered separately by real-media E2E; Storybook uses deterministic posters and never replaces production rendering with canvas.
- One scenario represents one meaningful state. Large screenshots that hide several unasserted states are not accepted as coverage.

## Progress Log

| Date       | Item                      | Status change                 | Evidence                                                                                                                                                | Notes                                                                                                              |
| ---------- | ------------------------- | ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| 2026-08-18 | Planning and discovery    | Not started → planned         | Repository, UI, API, Playwright, and design-history audit                                                                                               | Scope, backend boundary, and baseline policy aligned                                                               |
| 2026-08-18 | 0.1 Paper inventory       | Planned → in progress         | Live Paper file: 34 artboards, 6,755 nodes, token hash `cf3b1cd7`                                                                                       | Inner implementation frames still need manifest rows and image exports                                             |
| 2026-08-19 | 1.4 anti-drift protocol   | Planned → specified           | Paper-linked scenario contract, baseline approval, and deterministic capture rules                                                                      | Implementation waits on the versioned frame manifest and references                                                |
| 2026-08-19 | 1.5 demo story contract   | Planned → in progress         | Typed metadata, WebVTT, Azure OpenAI TTS, duration guards, and tested ffmpeg mux                                                                        | Storybook discovery, Playwright recording, and CI artifacts remain                                                 |
| 2026-08-19 | 0.1 Paper source export   | In progress → source complete | 34 board JSX snapshots, 80 tokens, manifest SHA-256 validation                                                                                          | Lossless inner-frame PNG references remain                                                                         |
| 2026-08-19 | 0.2 scenario manifest     | Planned → complete            | 43 route, state, fixture, theme, and viewport cases cover all 34 boards                                                                                 | Storybook and Playwright owners reserved for every planned screen                                                  |
| 2026-08-19 | 0.3 capability contract   | Planned → complete            | Seven exact Board 28 IDs; unknown and mismatched versions fail closed                                                                                   | Backend advertisement transport remains a prerequisite                                                             |
| 2026-08-19 | 1.1 Paper runtime theme   | Planned → in progress         | 80 generated and CI-checked Tailwind/runtime tokens; dark/light build and E2E pass                                                                      | Bundled font package download remains blocked                                                                      |
| 2026-08-19 | 1.2 responsive shell      | Planned → complete            | Desktop 64/52/32px and mobile 78px geometry; 2 Playwright scenarios; screenshot review                                                                  | Legacy route contents migrate in their owning workflow milestones                                                  |
| 2026-08-19 | 1.3 capability provider   | Planned → complete            | Request-isolated context, command states, exact fallback gate, and 6 focused tests                                                                      | Server advertisement transport remains fail-closed                                                                 |
| 2026-08-19 | 2A.1 Peek live wall       | Not started → in progress     | 5 view-model tests; 7 focused route, shell, and real-media Playwright scenarios; desktop and 390×844 trace review                                       | Paper image/Loki approval, layout editor, fleet, and rewind remain                                                 |
| 2026-08-19 | 2A.1 layout editor        | In progress → checkpoint      | 5 layout-model tests; named Board 08 Playwright scenario; 1440×840 trace review; `./check.sh`                                                           | Persistence remains gated; fleet, rewind, and Paper/Loki approval remain                                           |
| 2026-08-19 | 2A.1 127-source fleet     | In progress → checkpoint      | 7 fleet/virtualizer tests; 2 named Playwright scenarios; desktop/mobile trace review; 23-test `./check.sh`                                              | Fleet commands remain gated; rewind and Paper/Loki approval remain                                                 |
| 2026-08-19 | 2A.1 Peek-to-Keep         | In progress → implemented     | 5 timing tests; named real-media Playwright scenario; drag/Keep trace review; 24-test `./check.sh`                                                      | Svelte/behavior complete; Paper image and Loki approval remain blocked                                             |
| 2026-08-19 | 2B.1 Keep timeline        | Not started → checkpoint      | 11 focused timeline tests; 2 named Board 04 Playwright scenarios; desktop/mobile trace review; 26-test `./check.sh`                                     | Stories, swimlanes, Events, export, and Paper/Loki approval remain                                                 |
| 2026-08-19 | 2B.1 Keep modes           | In progress → checkpoint      | 5 mode-model tests; 4 Board 09 Playwright scenarios; desktop/mobile review; 30-test `./check.sh`                                                        | Enabled export jobs, Events, and Paper/Loki approval remain                                                        |
| 2026-08-19 | 2B.1 Events               | In progress → checkpoint      | 4 event-browser tests; 2 named Board 10 Playwright scenarios; desktop/mobile review; 32-test `./check.sh`                                               | Enabled export jobs and Paper/Loki approval remain                                                                 |
| 2026-08-19 | 2B.1 export lifecycle     | Planned → blocked             | Repository search: no capability advertisement, job types, REST routes, or server implementation                                                        | Unsupported range draft is covered; enabled Board 29 states need backend                                           |
| 2026-08-19 | 2C.1 Camera/PTZ           | Not started → checkpoint      | 3 control-model tests; 5 Camera Playwright scenarios; desktop/mobile review; 34-test `./check.sh`                                                       | Browser PTZ transport, onboarding/defaults, and Paper/Loki approval remain                                         |
| 2026-08-19 | 2C.1 add-camera wizard    | In progress → checkpoint      | 4 draft-model tests; 3 named Board 12/25 Playwright scenarios; desktop/mobile review; 37-test `./check.sh`                                              | Candidate auth/stream probe API is absent; first run/defaults remain                                               |
| 2026-08-19 | 2C.1 first run            | In progress → checkpoint      | 3 evidence-model tests; named Board 21 Playwright scenario; 1440×900 trace review; 38-test `./check.sh`                                                 | Storage write probe, server timezone/setup completion, and identity APIs are absent                                |
| 2026-08-19 | 2C.1 camera defaults      | In progress → checkpoint      | 2 evidence-model tests; named Board 23 Playwright scenario; 1310px trace review; 39-test `./check.sh`                                                   | Defaults schema/API, inheritance markers, propagation, and override reset are absent                               |
| 2026-08-19 | 2D.1 health + diagnosis   | Not started → checkpoint      | 4 presentation-model tests; 3 named Board 15/26/30 Playwright scenarios; desktop/mobile trace review; 41-test `./check.sh`                              | Recording-gap history, retry state/command, credential probe, and approved refs are absent                         |
| 2026-08-19 | 2E.1 storage retention    | Not started → checkpoint      | 3 evidence-model tests; named Board 13 Playwright scenario; 1310px trace review; 42-test `./check.sh`                                                   | Oldest-footage history, fill-policy choice, per-camera retention, and offsite APIs are absent                      |
| 2026-08-19 | 2E.1 event sources        | In progress → checkpoint      | 3 evidence-model tests; named Board 14 Playwright scenario; 1310px trace review; 43-test `./check.sh`                                                   | Source registry, token/scope/mapping APIs, heartbeat, and publication runtime are absent                           |
| 2026-08-19 | 2E.1 Groups               | In progress → checkpoint      | 3 group-model tests; named Board 19 Playwright scenario; 1310px trace review; 44-test `./check.sh`                                                      | List/join/leave runtime, directory state, participants, and admin commands are absent                              |
| 2026-08-19 | 2E.1 Access & Roles       | In progress → checkpoint      | 3 access-model tests; named Board 16 Playwright scenario; 1310px trace review; 45-test `./check.sh`                                                     | Identity enforcement, people/roles/sessions/tokens, remote sign-in, and audit are absent                           |
| 2026-08-19 | 2E.1 Integrations         | In progress → checkpoint      | 3 integration-model tests; named Board 17 Playwright scenario; 1310px trace review; canonical `/metrics` evidence                                       | Exact-origin CORS and Bearer runtime exist; card package, scrape UI/health, event forwarder, and webhooks remain   |
| 2026-08-19 | 2E.1 Notifications        | In progress → checkpoint      | 3 notification-model tests; named Board 18 Playwright scenario; 1310px trace review; 47-test `./check.sh`                                               | Channel/rule runtime, quiet hours, tests, retries, permissions, and delivery history are absent                    |
| 2026-08-19 | 2D/2E Board 20 system     | In progress → checkpoint      | 4 appearance/system-model tests; named Board 20 Playwright scenario; dark/system trace review; 48-test `./check.sh`                                     | Server time/preferences, update/backup/erase, and full diagnostics bundle contracts are absent                     |
| 2026-08-19 | 2E.1 mobile admin         | In progress → checkpoint      | 3 mobile-index tests; named Board 27 Playwright scenario; index/focused 390×844 trace review; 49-test `./check.sh`                                      | Backend-gated sections remain honest; mobile Paper/Loki reference approval remains                                 |
| 2026-08-19 | 2E.1 Board 28 control     | In progress → checkpoint      | Typed camera/configuration/health/PTZ commands; binary client and server tests; 49 Playwright scenarios                                                 | Exact IDs remain empty until complete contracts ship                                                               |
| 2026-08-19 | 2A/2B media transport     | Compatibility → canonical     | Real browser-to-Rust RTP; indexed MP4 windows; refill/ENDED unit and server tests; Keep/Events scenarios pass                                           | Canonical media transport complete; enabled export lifecycle remains                                               |
| 2026-08-19 | Canonical HTTP contract   | Partial → canonical           | Router/source search limited to `/create`, `/delete`, `/logs`, `/metrics`; typed snapshots and full browser matrix                                      | Per-key scopes do not exist by design                                                                              |
| 2026-08-19 | 2C.1 PTZ runtime          | Fail-closed → implemented     | Fake-Reolink ownership/preset/disconnect test; client units; pointer/keyboard/mobile Camera Playwright scenario                                         | Relative move and preset save/delete remain explicitly unsupported                                                 |
| 2026-08-19 | 2B.1 export lifecycle     | Blocked → implemented         | Gap-preserving MP4/server tests; binary client test; 8 named Board 29 Playwright scenarios including verified download                                  | Behavior is complete; Paper image and Loki overlay approval remain blocked                                         |
| 2026-08-19 | 0.1 Board 29 reference    | PNG blocked → exported        | Paper `States` group exported at 1440×369 and hash-locked in the v34 storyboard; `bun run paper:check`                                                  | Use this Paper-owned image for Storybook/Loki overlay acceptance                                                   |
| 2026-08-19 | 1.4 Board 29 visual       | Planned → candidate           | Production-component story; Chromium geometry test; native candidate/overlay/diff; SSIM 0.689, PSNR 16.35 dB                                            | Two evidence-safe copy exceptions recorded; Storybook build/Loki await public-registry package transfer            |
| 2026-08-20 | 3.1 Board 32 keyboard     | Not started → complete        | Pure resolver tests; 9 named Playwright scenarios; global/Peek/Keep/list/settings focus and command contracts                                           | Behavior complete; Paper/Loki visual approval remains part of Phase 3                                              |
| 2026-08-20 | 3.2 Board 33 states       | Not started → complete        | 6 authored states; focused model tests; named Peek/Keep/Add Camera/Events/Settings/Cameras Playwright scenarios                                         | Behavior complete; Paper/Loki visual approval remains part of Phase 3                                              |
| 2026-08-20 | 3.2 Board 34 light        | Not started → complete        | Pre-hydration and persisted light roles; system preference; dark video; light offline panel; semantic color assertions                                  | Behavior complete; Paper/Loki visual approval remains part of Phase 3                                              |
| 2026-08-20 | 1.1 bundled typography    | In progress → complete        | Exact Fontsource 5.3.0 Archivo/IBM Plex Mono weights 300–700; verified SHA-1/SHA-512 lock; local WOFF/WOFF2 production assets                           | Isolated Storybook/Loki package transfer remains a separate blocker                                                |
| 2026-08-20 | 0.1 Board 34 reference    | PNG blocked → exported        | Paper `Light Peek` frame exported at 1440×362; SHA-256 `55828067…c73819f`; storyboard now verifies 2 references                                         | Paper-owned frame excludes annotation chrome                                                                       |
| 2026-08-20 | 3.3 Board 34 visual       | Planned → Paper accepted      | Production tile story; exact geometry browser test; 50% overlay; 0.360% at Δ16 and 0.092% at Δ64; 8 owning Playwright tests                             | Canonical Linux Loki baseline remains pending; next reference slice is Board 33                                    |
| 2026-08-20 | 0.1 Board 33 references   | PNG blocked → exported        | Six Paper-owned inner frames at 462×172/238; SHA-256 locked; storyboard now verifies 8 references                                                       | First keyframe, cold seek, discovery, no results, applying, and fleet skeleton                                     |
| 2026-08-20 | 3.3 Board 33 visual       | Planned → Paper accepted      | Six shared production stories; 7 Chromium contracts; named route suites; per-frame overlays and variance reasons                                        | Canonical Linux Loki baselines remain pending; next reference slice is Board 32                                    |
| 2026-08-20 | 3.3 Board 32 contract     | Reference triage → accounted  | Paper hierarchy review; exact 2px/2px focus CSS assertion; complete help discoverability; 9 keyboard Playwright scenarios                               | No authored dialog frame exists; do not seed a visual baseline from explanatory board chrome                       |
| 2026-08-20 | 3.3 Board 31 visual       | Planned → Paper accepted      | Two shared production stories; 2 Chromium contracts; real-media one-peer transition; ready 0.48 and scrub 0.65 mean Δ                                   | Landed mini-Keep frame is explanatory; full route landing remains behavior-owned                                   |
| 2026-08-20 | 3.3 Board 30 diagnosis    | Planned → blocked candidate   | Shared route/story owner; 2 Chromium contracts; 4 diagnosis E2E; WebRTC TCP update; strict Δ16 reduced 25.6% → 9.77%                                    | Packet-loss history, gap start, retry countdown, and credential probe remain                                       |
| 2026-08-20 | 3.3 Board 29 visual       | Candidate → Paper accepted    | Exact four-card/lane/action geometry; 8 export E2E; 3.84% at Δ16, 1.46% at Δ64, mean Δ 2.98                                                             | Canonical Linux Loki baseline remains pending; causal and storage-remedy copy remains evidence-safe                |
| 2026-08-20 | 3.3 Board 28 contract     | Reference triage → accounted  | Paper hierarchy review; seven exact-ID catalog/action tests; fail-closed state tests; WebRTC server advertises export only                              | No authored application frame exists; do not seed a visual baseline from the explanatory capability table          |
| 2026-08-20 | 3.3 Board 27 More         | Planned → Paper accepted      | Shared production mobile shell/index; exact 390×844 and 52/46px row contracts; 0.78% at Δ16 and 0.42% at Δ64                                            | Focused Camera Defaults and Access frames remain evidence-blocked; canonical Linux Loki baseline remains pending   |
| 2026-08-20 | 3.3 Board 27 focused      | Planned → blocked candidates  | Shared 62/52/660/68px lanes; 3 Chromium contracts; 2 named mobile Playwright scenarios; native Paper overlays                                           | Shared-login/recording and identity/token evidence are absent; evidence-safe compact states remain                 |
| 2026-08-20 | 3.3 Board 26 overview     | Planned → Paper accepted      | Shared production mobile Health owner; exact 390×844 lanes; named route scenario; 1.24% at Δ16 and 0.67% at Δ64                                         | Issue mute command is absent and its stable action lane remains empty; canonical Linux Loki baseline is pending    |
| 2026-08-20 | 3.3 Board 26 diagnosis    | Planned → blocked candidates  | Shared compact diagnosis owner; 2 Chromium contracts; 2 named mobile Playwright scenarios; WebRTC TCP update                                            | Gap/retry/probe and loss-history/causal-confidence fields are absent from health                                   |
| 2026-08-20 | 3.3 Board 25 add camera   | Planned → blocked candidates  | Shared three-stage mobile owner; 3 Chromium contracts; 3 named mobile Playwright scenarios; final-write invariant                                       | Candidate probe, shared-login inheritance, pre-save source ID, and retention projection APIs are absent            |
| 2026-08-20 | 3.3 Board 24 camera       | Planned → blocked candidates  | Single-peer responsive owner; shared PTZ controls; 3 Chromium contracts; 3 named mobile Playwright scenarios                                            | Deterministic video, recent events/audio, PTZ speed, inheritance/retention, and broad settings write are absent    |
| 2026-08-20 | 3.3 Board 23 defaults     | Planned → blocked candidate   | Exact 84/302/312px production lanes; Chromium contract; desktop/mobile evidence Playwright; strict Δ16 27.5% → 10.6%                                    | Shared-login, inheritance/override, recording-mode, and shared-default write contracts are absent                  |
| 2026-08-20 | 3.3 Board 22 mobile       | Planned → blocked candidates  | Exact 390×844 shell lanes; shared Peek tile, timeline, and Event card owners; 3 Chromium contracts; named route scenarios                               | Approved media imagery and event narrative/revision fields are absent                                              |
| 2026-08-20 | 3.3 Board 21 first run    | Planned → blocked candidate   | Exact 1440×785 row and 189/515/79px setup lanes; 2 Chromium contracts; named zero-write route scenario; native Paper overlay                            | Write probe/completion, server timezone, identity, and event-source registry contracts are absent                  |
| 2026-08-20 | 3.3 Board 20 system       | Planned → blocked candidate   | Exact 1440×581 and 466/466/468px panels; 2 Chromium contracts; named route scenario; native Paper overlay                                               | Server preferences, update/config/erase commands, and full diagnostics bundle are absent                           |
| 2026-08-20 | 3.3 Board 19 groups       | Planned → blocked candidates  | Exact 1440×416 admin and 1440×420 participant frames; 2 Chromium contracts; 2 named route scenarios; native Paper overlays                              | Group runtime, GroupState, participant, and administration evidence are absent                                     |
| 2026-08-20 | 3.3 Board 18 notices      | Planned → blocked candidate   | Exact 1440×1075 shell and 195/288/430px bands; 2 Chromium contracts; named route scenario; native Paper overlay                                         | Channel/rule registries, delivery history, quiet hours, tests, and retries are absent                              |
| 2026-08-20 | 3.3 Board 17 integrations | Planned → blocked candidate   | Exact 1440×869 shell and 205/236/278px bands; 2 Chromium contracts; named route scenario; native Paper overlay                                          | Card/token registry, MQTT/webhook runtimes, scrape configuration, and external health are absent                   |
| 2026-08-20 | 3.3 Board 16 access       | Planned → blocked candidate   | Exact 1440×1249 shell and 102/416/395/142px bands; 2 Chromium contracts; named route scenario; native Paper overlay                                     | Identity enforcement, directories, token lifecycle/scopes, and audit rows are absent                               |
| 2026-08-20 | 3.3 Board 15 health       | Planned → blocked candidate   | Exact 1440×1302 shell and 130/246/130/248/326px bands; shared verdict; 2 Chromium contracts; named route scenario                                       | Historical loss/writer/completeness, gap start, mute, and external-service evidence are absent                     |
| 2026-08-20 | 3.3 Board 14 sources      | Planned → blocked candidate   | Exact 1440×1048 shell, 240/1134px body, and 84/337/461px bands; 2 Chromium contracts; named route scenario                                              | Registry/heartbeat, today counts, token/scopes, mapping, and publication runtime are absent                        |
| 2026-08-20 | 3.3 Board 13 storage      | Planned → blocked candidate   | Exact 1440×1163 shell, 240/1134px body, and 84/128/235/216/278px bands; 2 Chromium contracts; named route scenario                                      | Oldest history, selectable fill policy, per-camera retention/pins, and offsite locations are absent                |
| 2026-08-20 | 3.3 Board 12 add camera   | Planned → blocked candidate   | Exact 1440×685 and 300/1140px columns; shared stream owner; 2 Chromium contracts; named five-step route scenario                                        | Candidate auth/probe, decoded/audio evidence, recording/retention/group fields, and source ID are absent           |
| 2026-08-20 | 3.3 Board 11 fleet        | Planned → blocked candidate   | Exact 1440×624 shell and shared 1334×56px rows; 2 Chromium contracts; named 127-source virtualizer scenario                                             | Last-event provenance, groups, service variants, and bulk-operation commands are absent                            |
| 2026-08-20 | 3.3 Board 10 Events       | Planned → blocked candidates  | Exact 1440×820 browse and 1440×669 detail lanes; 2 Chromium contracts; 3 named route scenarios; Δ16 2.34%/12.41%                                        | Narrative/publisher identity, attachment history, payload, revisions, approved media, and enabled actions absent   |
| 2026-08-20 | 3.3 Board 09 Keep modes   | Planned → mixed acceptance    | Three exact 467×413 panels and 1440×363 swimlanes; 4 Chromium contracts; 4 named route scenarios; Calendar Δ16 3.56%                                    | Calendar accepted; story evidence, keyframe proof, and cross-camera causal conclusions keep three candidates       |
| 2026-08-20 | 3.3 Board 08 layout       | Planned → blocked candidates  | Exact 1440×840 editor and 1440×396 registry frames; 3 Chromium contracts; 2 named route scenarios; editor Δ16 3.54%                                     | Layout persistence/directory/CRUD and runtime Activity Focus promotion contracts are absent                        |
| 2026-08-20 | 3.3 Board 07 Camera       | Planned → blocked candidate   | Exact 1440×2059 shell and 394/154/217/263/199/292/131/111px bands; 2 Chromium contracts; named WebRTC PTZ route; Δ16 6.36%                              | Retention/inheritance, publisher registry, audio/preset writes, speed, recording status, and approved media absent |
| 2026-08-20 | 3.3 Board 06 Peek         | Planned → Paper accepted      | Exact 1440×860 shell, 52/774/32px lanes, and six 439×340 shared tiles; 2 Chromium contracts; named route scenario; Δ16 2.44%                            | Honest 6-of-6 fixture count and runtime state copy replace unreturned 127-source/page claims                       |
| 2026-08-20 | 3.3 Board 05 handoff      | Planned → accounted contract  | 14 ordered requirements mapped to canonical scenarios; implemented/partial/blocked state and blockers enforced by paper:check                           | Explanatory board chrome is not used as an application raster                                                      |
| 2026-08-20 | 3.3 Board 04 timeline     | Planned → blocked candidate   | Exact 1280×720 shell, 64/818/396px columns, 52/46/620px timeline lanes; 2 Chromium contracts; named route scenario; Δ16 5.43%                           | Narrative/publisher/revision/story-frame evidence and approved deterministic media are absent                      |
| 2026-08-20 | 3.3 Board 03 IA           | Planned → accounted contract  | 6 ordered desktop destinations, 5 responsive tabs, 10 Settings sections, route files, scenario IDs, and path variance enforced                          | Paper's /health label is superseded by canonical /system-health and its diagnosis deep links                       |
| 2026-08-20 | 3.3 Board 02 tokens       | Planned → accounted contract  | 80-token hash/CSS parity; exact type counts and semantic roles; 2 Fontsource 5.3.0 families × 5 local weights verified from installed package manifests | Explanatory token board is not used as an application raster                                                       |
| 2026-08-20 | 3.3 Board 01 positioning  | Planned → accounted contract  | Local-first/no-relay evidence; exact Event/HTTP boundaries; five scenario-backed principles; prohibited-route checks                                    | Current persistence remains partial; POC and non-production-ready wording is mandatory                             |
| 2026-08-20 | 3.5 coverage report       | Not started → complete        | Generated 34/34-board, 56-scenario, 50-reference matrix; all story, Playwright, contract, and blocker owners freshness-checked                          | 14 references accepted; 36 candidates stay capability-gated; zero candidates are unclassified                      |
| 2026-08-20 | 3.4 canonical checkpoint  | Not started → in progress     | `./check.sh`; Rust tests/doctests, Clippy, cargo-machete, UI quality/build, and 85 Playwright scenarios pass                                            | Storybook build/Loki remains blocked by public-registry `ConnectionClosed` before package resolution               |
| 2026-08-20 | 1.5 demo recording        | In progress → complete        | Board 31; 9.000s source → 15.480s narrated; 3 WAV cues; H.264/AAC; VTT/JSON; publish manifest                                                           | Speech duration measured; silent source retained in artifact                                                       |
| 2026-08-20 | 1.5 camera lifecycle      | Static fixture → real server  | Real server/camera; WebRTC remove/re-add; 22.000s source → 31.920s narrated; 5 WAV cues; H.264/AAC                                                      | Apply remains pending; speech duration is measured                                                                 |

## Next Update Template

Add one row to the progress log using this shape:

| YYYY-MM-DD | Milestone ID and short name | Previous → new status | Tests, visual diff, PR or commit | Blocker, decision, or follow-up |
