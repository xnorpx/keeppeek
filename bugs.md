# KeepPeek Release QA Bugs

The original findings below remain as historical regression evidence. The current audit is in
[Alpha audit, 2026-09-12](#alpha-audit-2026-09-12).

Tested 2026-08-23 against the local `v0.1.0` release build on macOS 26.6.1 with
nine real cameras. Browser checks used Chromium at the authored Paper viewports of
1440 x 900 and 390 x 844. Visual expectations came from `KeepPeek - NVR Design
System & Spec`, especially Boards 06, 10, 11, 22, 30, 31, and 34.

## Final remediation status

All 13 findings are resolved in the 2026-08-26 remediation candidate. The original reproduction
and failure evidence remains below as regression context.

| Finding   | Status       | Final verification                                                                                       |
| --------- | ------------ | -------------------------------------------------------------------------------------------------------- |
| KP-UI-001 | **Resolved** | Metadata-first 18-card pages render in 223-243 ms; previews are exact, lazy, bounded, and cancellable.   |
| KP-UI-002 | **Resolved** | H.264 recordings decode in 0.64-1.68 s; HEVC uses exact RFC 6381 signaling or a truthful fallback.       |
| KP-UI-003 | **Resolved** | One canonical stale fixture agrees across Peek, Cameras, Health, and diagnosis.                          |
| KP-UI-004 | **Resolved** | Mobile fleet geometry stays within 390 px with identity, health, selection, and navigation visible.      |
| KP-UI-005 | **Resolved** | Runtime storage opens a validated editor with cancel, apply, conflict, and restart-required states.      |
| KP-UI-006 | **Resolved** | Session deletion is idempotent; route teardown returns 200 and preserves the shared control channel.     |
| KP-UI-007 | **Resolved** | Mobile selection uses a 44 x 44 px target around the compact checkbox.                                   |
| KP-UI-008 | **Resolved** | Sub-second storage durations use millisecond precision with boundary coverage.                           |
| KP-UI-009 | **Resolved** | The date index selects the newest recorded day first and emits one budgeted `KeepFirstSegment`.          |
| KP-UI-010 | **Resolved** | Stories and Swimlanes use bounded pages/windows; real first content measured 325 ms and 303 ms.          |
| KP-UI-011 | **Resolved** | Generic motion retention is per-camera, defaults off, and leaves classified events enabled.              |
| KP-UI-012 | **Resolved** | Availability-first metadata measured 48.2 ms p50 and 75 ms p95/max; thumbnail refreshes are incremental. |
| KP-UI-013 | **Resolved** | Real cold-open and camera switching produce zero failed Blob requests or console errors.                 |

Severity:

- **P0**: Release blocker. The core workflow is effectively unusable or exhausts the client.
- **P1**: Major. A primary workflow fails or reports dangerously incorrect state.
- **P2**: Moderate. The workflow works with a material UX, accessibility, or reliability defect.
- **P3**: Minor. Incorrect or confusing presentation with a viable workaround.

## KP-UI-001 [P0] Events requests an entire day of attachments and freezes the browser

**Status:** **Resolved 2026-08-26**

**Area:** Events, performance, memory, responsiveness

**Paper reference:** Board 10, Events browse/search/detail

### Reproduction

1. Start KeepPeek with the nine-camera production configuration.
2. Open Peek at 1440 x 900.
3. Select **Events**.
4. Leave the default date and camera filters unchanged.

### Actual

- The Events shell appears in 125 ms, but remains at `0 events` and `Loading events...`
  for more than 15 seconds.
- The load did not complete during the following 30-second observation window. Simple
  browser probes stopped returning while the document was overloaded.
- Chromium recorded a 577 ms main-thread task followed by repeated 50-95 ms long tasks,
  including stalls at roughly one-second intervals.
- The affected VS Code renderer reached approximately 2.15 GB RSS while the KeepPeek
  process was approximately 443 MB RSS and 3.1% CPU.
- The page only adds `?date=2026-08-23` to the URL after the complete data set finishes,
  so the visible loading state gives no progressive results.

### Cause indicated by the implementation

- `ui/src/routes/events/+page.svelte` runs one full-day `getRecordingEvents` request for
  every camera in a `Promise.all`, waits for all cameras, flattens every result, and mounts
  every matching card.
- `ui/src/lib/control-client.ts` sets `includeAttachments: true` for each request, buffers
  all attachment chunks until that query ends, and creates all object URLs before resolving.
- There is no server pagination, viewport window, list virtualization, visible-only preview
  fetch, or request cancellation on route/filter changes.
- `VideoDecoder` pooling cannot fix the initial loader: Events does not mount preview
  decoders before these timeline and attachment queries complete.

### Expected

Events should work from a small moving window around what the user can see. It must not
load or retain a whole day across all cameras before showing the first result.

### Acceptance criteria

- Fetch metadata first in bounded, keyset-paged time windows, starting with the most recent
  seconds/minutes relevant to the visible viewport.
- Render only visible cards plus approximately one viewport of overscan.
- Fetch attachments/previews only as their cards approach the viewport, with bounded
  concurrency and cancellation.
- Show the first metadata page within 1 second on the tested local nine-camera data set.
- Keep filter, scroll, navigation, and back actions responsive during loading; produce no
  task longer than 50 ms in the representative release fixture.
- Memory must scale with the visible/overscan window, not total events or attachments for
  the selected day.
- Changing date/filter or leaving Events must cancel in-flight server and client work.

## KP-UI-002 [P1] Keep replay is codec-blind and misses its first-frame and smoothness budgets

**Status:** **Resolved 2026-08-26**

**Area:** Keep, stored playback, latency

**Paper reference:** Boards 04, 22, and 31

### Reproduction

1. At 390 x 844, open `/keep?camera=192.168.137.121&stream=main&date=2026-08-23`.
2. Observe the Deck player.
3. Switch from **Main** (H.265) to **Sub** (H.264).
4. Press play and wait.
5. Repeat from a fresh document after closing the overloaded Events page.

### Actual

- The H.265 main player remains at `readyState=0`, `currentTime=0`, with no media error.
- The H.264 substream reached metadata in one run but never reached current data or advanced.
- The UI displayed `Opening indexed recording - 73.2s`; the `play()` promise was still
  pending and navigation was required to cancel it.
- In the isolated rerun, the player took 6.2 seconds to mount, then remained at
  `readyState=0`, `currentTime=0`, and an empty buffered range for at least another 12 seconds.
- The player shows an indefinite spinner instead of a bounded error/retry state.
- A fresh real-camera run reproduced the H.265 failure after four fragments and 40.8 seconds
  with zero decoded frames.
- The same camera's H.264 substream produced its first fragment in 307 ms and first frame in
  322 ms after an in-page stream switch, proving that the control and indexed-read path can be
  fast when the selected codec is browser-compatible.
- A clean fresh-page benchmark at a stable historical timestamp measured **4.14-5.10 seconds**
  to first frame across all seven H.264 substreams.
- During the following five-second sample, those streams advanced only 2.97-4.15 media seconds
  and presented 5.4-12.8 fps against 10 or 15 fps sources. Every run produced a 114-135 ms
  main-thread task.
- The two cameras whose substreams are also H.265 received fragments but produced no decoded
  frame within ten seconds.
- Camera switches logged `net::ERR_FILE_NOT_FOUND` for revoked `blob:` URLs. After one failed
  switch, later recordings decoded a frame but remained paused because playback intent was
  inherited from the failed predecessor.
- One H.264 run reached `readyState=4` and started playback, then advanced only 7.5 media
  seconds over 12 wall-clock seconds before becoming stuck at the end of its buffered range.

### Expected

Opening a local indexed H.264 recording should produce the first frame within 2 seconds.
Unsupported H.265 should fail over to a playable variant or explain the incompatibility.

### Acceptance criteria

- Instrument and bound query, open, initialization, first-fragment, append, and first-frame
  phases separately.
- Produce a playable H.264 frame within 2 seconds for the tested local recording.
- If playback cannot start, show a specific actionable error and retry within 5 seconds.
- Never leave `HTMLMediaElement.play()` pending indefinitely.
- Cancel open/refill work immediately when the camera, stream, date, route, or play attempt
  changes.
- Prefer a browser-decodable H.264 profile when the selected/default H.265 profile is unsupported.
- Keep playback intent explicit across camera, stream, filmstrip, date, and live-edge transitions.
- Do not revoke an MSE object URL until its media element has detached from it.
- Continue across newly finalized live-edge fragments without pausing or requiring another click.

The optimized real-camera rerun reduced average first-frame latency across seven H.264 substreams
from 4.66 seconds to 1.02 seconds. Six continuous recordings reached their source rate and advanced
the full 15-second sample: five at 14.9-15.1 fps and Right Side at 10.1 fps. Front Gate advanced
4.32 seconds because the sampled MP4 ends at 23:35:04 and the next starts around 23:35:40; the UI
remained playing without a browser error and correctly exposed the recording gap. The final run
produced no request failures or console errors.

## KP-UI-003 [P1] Camera health state contradicts itself across Peek, Cameras, and diagnosis

**Status:** **Resolved 2026-08-26**

**Area:** Health correctness, status semantics

**Paper reference:** Boards 06, 15, and 30

### Reproduction

1. Open **Health** while Front Gate is experiencing a stale stream.
2. Note the top warning and Front Gate findings.
3. Select **Diagnose Front Gate**.
4. Open **Cameras** and inspect the Front Gate row and `Not healthy` count.
5. Open mobile Peek and inspect the Front Gate tile.

### Actual

- Health reports `warning - CAMERA RECORDING` and `One or more stream reports are stale`.
- Diagnosis labels the same camera `ONLINE` while its primary evidence reports a 38,114 ms
  maximum frame gap, 42,348 drops, and 16 errors.
- Diagnosis says `9 cameras currently report online`.
- Cameras reports `Not healthy 0` and gives Front Gate a green status dot.
- Peek shows `NO KEYFRAME AFTER 11.6S` with an amber degraded panel, but the title chip on
  the same tile remains green and the page summary says `9 / 9 cameras reporting`.

### Expected

Connected transport, fresh frames, decodable video, and recording health must be distinct,
consistent states. A stale/degraded stream must not be represented as healthy green.

### Acceptance criteria

- Use one shared, server-evidence-based status projection across all four surfaces.
- Distinguish `connected`, `reporting fresh frames`, `decodable`, and `recording` in labels.
- A degraded tile must use the degraded signal in both its chip and detail panel.
- Fleet and page counts must state exactly what they count, such as `connected` or `healthy`.
- Add an end-to-end case asserting the same stale fixture on Peek, Cameras, Health, and
  diagnosis.

## KP-UI-004 [P1] Mobile Cameras clips most columns and the trailing row action

**Status:** **Resolved 2026-08-26**

**Area:** Mobile responsive layout, Cameras

**Paper reference:** Board 11 plus the 390 px shell contract in Board 22

### Reproduction

1. Set the viewport to 390 x 844.
2. Open `/cameras`.
3. Inspect any camera row.

### Actual

- The desktop-width row is clipped inside the 390 px page.
- Transport values are cut off at the right edge.
- Streams, recording, throughput, GB/day, last event, and the trailing open action are not
  visible.
- The trailing actions are laid out around `x=1294` while the document reports a 390 px
  scroll width, so the user cannot scroll horizontally to reach them.

### Expected

The mobile fleet should prioritize camera identity and health, then expose secondary facts
and actions through a responsive row/detail pattern without clipping.

### Acceptance criteria

- No interactive or informational element may be placed outside the 390 px content width.
- Camera identity, truthful health, and a clear row navigation action remain visible.
- Secondary desktop columns collapse into a detail view or intentional stacked content.
- Validate 390 x 844 and intermediate widths with geometry assertions.

## KP-UI-005 [P1] Edit runtime storage is an enabled no-op

**Status:** **Resolved 2026-08-26**

**Area:** Settings, storage configuration

**Paper reference:** Boards 13 and 27

### Reproduction

1. At 390 x 844, open **More**.
2. Select **Storage & retention**.
3. Select **Edit runtime storage**.

### Actual

The enabled button receives focus but causes no dialog, form, navigation, state change, or
feedback. The URL and page content remain unchanged.

### Expected

The command should open an editable storage form, or be visibly capability-gated with an
exact explanation when editing is unavailable.

### Acceptance criteria

- Wire the button to the runtime storage editor and cover open, cancel, validation, apply,
  failure, and restart-required states; or replace it with an honest disabled capability gate.
- Never render an enabled command that performs no observable action.

## KP-UI-006 [P2] Leaving Peek sends a failing `/delete` request

**Status:** **Resolved 2026-08-26**

**Area:** WebRTC lifecycle, console cleanliness, route transitions

### Reproduction

1. Open Peek and wait for live video.
2. Focus Deck and select **History**, or select **More** from Peek.
3. Observe browser responses and console output.

### Actual

- The client navigation itself is fast (28 ms in the History run).
- `/delete` returns HTTP 404 on both Peek to Keep and Peek to Settings transitions.
- Chromium logs `Failed to load resource: the server responded with a status of 404`.

### Expected

Session teardown should be idempotent and produce no failed request or console error during
normal navigation.

### Acceptance criteria

- Ensure one owner releases each session exactly once.
- Treat deletion of an already-closed session as a successful idempotent outcome.
- Add route-transition coverage that asserts no failed requests, console errors, or leaked
  sessions.

## KP-UI-007 [P2] Camera selection targets are 13 x 13 px on mobile

**Status:** **Resolved 2026-08-26**

**Area:** Mobile accessibility, touch input

**Paper reference:** 390 px mobile interaction contract

### Reproduction

1. Open `/cameras` at 390 x 844.
2. Inspect or attempt to tap a row selection checkbox.

### Actual

Each checkbox has a measured interactive rectangle of 13 x 13 CSS pixels.

### Expected

Selection controls need a reliable touch target without requiring pixel-precise taps.

### Acceptance criteria

- Provide at least a 44 x 44 px hit target while preserving the compact visual checkbox.
- Keep row navigation and row selection as separate, keyboard-accessible actions.

## KP-UI-008 [P3] Storage timings are rounded to misleading zero-second values

**Status:** **Resolved 2026-08-26**

**Area:** Settings, value formatting

**Paper reference:** Board 13

### Reproduction

1. Open **More > Storage & retention**.
2. Inspect Tier 1 and Tier 2 descriptions.

### Actual

- Short-term buffer shows `TIME WINDOW 0 seconds`.
- Active writer says `Flushes every 0 seconds with a 8.19 kB write buffer`.

### Expected

Sub-second configured values should retain useful precision, for example milliseconds or a
fractional second, rather than appearing disabled.

### Acceptance criteria

- Format durations below one second in milliseconds.
- Add boundary tests around 0 ms, sub-second, one second, and minute values.

## KP-UI-009 [P2] Keep scans an empty current day before discovering the newest recorded day

**Status:** **Resolved 2026-08-26**

**Area:** Keep, recording discovery, startup latency

**Paper reference:** Boards 04, 09, and 31

### Reproduction

1. Open `/keep` for a camera that has no recordings today but does have recordings on an earlier day.
2. Leave the date unspecified so Keep selects the current day.
3. Measure the time from route navigation to the first selected recorded day and playable segment.

### Actual

Keep calls `loadRecordings()` for the current day first. For an empty day,
`loadInitialRecordingWindow()` serially expands recording-range requests from five minutes to
the whole day before `initialize()` calls `discoverRecordingDates()`. The date index that could
select the newest recorded day is deliberately requested only after the empty-day scan finishes.

On a high-latency or busy recorder this produces up to eight dependent control round trips before
the UI can even learn that the selected day is empty. The page shows a recording-loading state
instead of navigating promptly to the latest available footage.

### Expected

Keep should determine the newest available recording date before performing an exhaustive scan of
an implicit current-day selection. Opening historical footage must prioritize time to first
playable frame over proving that today is empty.

### Acceptance criteria

- For an unspecified date, request the recording-date index concurrently with camera and health
  data, then select its newest day before loading recording ranges when today has no footage.
- Keep the five-minute-first expanding window only for an explicitly requested date or timestamp.
- Display the newest available segment or an honest empty state within one control round trip after
  the date index resolves.
- Add a controlled E2E case with an empty current day and an older recorded day. Assert no more
  than one empty-day range query occurs before the older day is selected.
- Emit a `KeepFirstSegment` timing metric and enforce a representative local budget of under one
  second from navigation to first selected segment.

## KP-UI-010 [P2] Stories and swimlanes request full-day event metadata on mode entry

**Status:** **Resolved 2026-08-26**

**Area:** Keep, events, timeline query volume, responsiveness

**Paper reference:** Boards 04 and 09

### Reproduction

1. Open a camera/day with a dense event history in Keep.
2. Select **Stories** or **Swimlanes**.
3. Observe stored-timeline request ranges, first content timing, main-thread work, and retained
   event metadata while changing modes or dates.

### Actual

When `mode !== 'timeline'`, the Keep route unconditionally calls `timelineRepository.loadWindow()`
with the selected day's complete 24-hour range and `includeEvents` enabled. The repository caps
its retained event collection at 10,000 entries, but it still asks the server for the full day
before rendering the mode. The request is not shaped by the visible story list, selected swimlanes,
or an initial viewport, so dense days can transfer, merge, sort, and retain far more event metadata
than the user can inspect.

Against the real nine-camera catalog, **Swimlanes took 7.37 seconds** to replace its loading state
and caused a 121 ms main-thread task. It queried a full day for eight cameras to render only one
shared hour. Stories reused partial timeline cache coverage and became ready in 249 ms, but it
still issued an additional 8.17-hour metadata query despite reporting zero story events.

### Expected

Mode changes should show the first relevant stories or lane evidence quickly and expand only as the
user scrolls, changes the time window, or asks for more cameras. Timeline metadata should remain
bounded by visible demand across every Keep mode.

### Acceptance criteria

- Use a newest-first, keyset-paged event query for Stories, with a small initial page and
  cancellation when the date, camera, or mode changes.
- Request swimlane availability at the selected shared-clock window first; fetch event markers only
  for visible lanes and the viewport plus bounded overscan.
- Do not issue a full-day event-metadata request solely because a non-timeline mode was selected.
- Keep decoded event metadata and thumbnail work bounded to visible items plus overscan.
- Add dense-fixture performance coverage asserting first content under one second, no main-thread
  task over 50 ms, and no more than the page/viewport budget of event records retained.

## KP-UI-011 [P1] Generic motion floods the event catalog and thumbnail store

**Status:** **Resolved 2026-08-26**

**Area:** Event ingestion, storage, Keep timeline performance

**Paper reference:** Boards 04, 09, 10, and 14

### Reproduction

1. Run the real nine-camera recorder with camera alarm subscriptions enabled.
2. Let the recorder ingest motion and AI alarm events over several days.
3. Compare `recording_events.kind` counts and thumbnail files.

### Actual

- The real catalog contains 13,418 events from August 14-24: 13,084 generic `motion`, 151
  `person`, 98 `animal`, and 85 `vehicle`.
- Generic motion is 97.5% of all event rows and 13,060 of the 13,392 thumbnail files. The
  thumbnail directory occupies 338 MB.
- In the latest 24 hours, KeepPeek stored 2,372 generic motion events versus 47 classified
  person or animal events.
- One camera contributed 10,387 generic motion events.
- Only 77 generic motion rows had a person, animal, or vehicle event from the same camera
  within one second; 13,007 were motion-only.
- Each accepted alarm kind creates a separate event and queues the same camera snapshot path,
  so generic motion consumes catalog, disk, query, decode, and UI work even when an AI event is
  the only evidence the user wants.

### Expected

Generic motion retention should be an explicit per-camera setting that defaults off. Camera motion
detection may remain enabled, but KeepPeek should store event rows and snapshots only for person,
animal, and vehicle classifications unless the user opts that camera into generic motion history.

### Acceptance criteria

- Add a per-camera **Store generic motion events** setting, defaulting to off for existing and new
  cameras.
- When off, a generic motion-only alarm creates neither a `recording_events` row nor a snapshot.
- Person, animal, and vehicle alarms continue to create their normalized events and snapshots.
- When enabled, generic motion behavior is restored for that camera without affecting others.
- Preserve the setting across unrelated camera edits and expose its current value without exposing
  camera credentials.
- Add unit coverage for motion-only, mixed motion plus AI, disabled, enabled, and alias
  normalization behavior.

The local fix adds this per-camera setting with a default of off. Two real-camera verification
runs, including the all-camera replay benchmark, left both the 13,084 motion-row count and 13,392
thumbnail-file count unchanged. Historical generic-motion data is intentionally not deleted.

## KP-UI-012 [P1] Keep overfetches timeline history and churns thumbnail object URLs

**Status:** **Resolved 2026-08-26**

**Area:** Keep, timeline, CPU, query volume

**Paper reference:** Boards 04 and 22

### Reproduction

1. Open Keep's live day for a camera with dense motion history.
2. Select a playable H.264 substream and leave the page untouched for 12 seconds.
3. Capture `keeppeek:timeline-performance` events and browser long tasks.

### Actual

- A clean live-page run issued an initial **1,470-minute** timeline query for the default six-hour
  view. Its first page took 2.17 seconds and the query completed in 6.43 seconds.
- During the same navigation and a following 15-second playback sample, Keep emitted 214 thumbnail
  cache/fetch signals and made two additional ten-minute live-edge queries.
- The initial query is expanded by the 12-hour prefetch on both sides of the visible range and is
  not clamped to the selected day. Dense generic-motion history amplifies the transfer, merge,
  sort, thumbnail lookup, and object-URL churn.
- The work competes directly with the primary MSE open: the real H.264 first frame arrived at
  3.40 seconds in this run while the timeline query was still running.

### Expected

Initial and live refresh work should stay near the visible time range and process only newly visible
or changed thumbnails. Timeline metadata must not delay the primary replay.

### Acceptance criteria

- Clamp prefetch to the selected day and reduce default six-hour overscan to a bounded fraction of
  the visible window.
- Give primary recording discovery/open priority over timeline metadata and filmstrip previews.
- Refresh only the advancing live-edge delta.
- Queue or touch only newly visible thumbnail identities; do not emit one cache-hit event per
  cached thumbnail on every refresh.
- In the real dense-camera fixture, first timeline metadata must render within 250 ms, the initial
  query must not exceed the selected day, and no task may exceed 50 ms.
- Replay remains at source frame rate while live timeline refreshes run.

Primary replay is deferred ahead of timeline and preview work. Availability is aggregated into
bounded buckets before a capped metadata page, prefetch is clamped to the selected day, and
date-priority indexes keep the visible query ahead of broad discovery. The final real-catalog run
measured 48.2 ms p50 and 75 ms p95/max for first metadata. Refreshes touch only thumbnail
identities newly entering the viewport instead of re-emitting one cache hit per cached thumbnail.

## KP-UI-013 [P2] Keep revokes blob URLs while previews are still loading

**Status:** **Resolved 2026-08-26**

**Area:** Keep, media lifecycle, console cleanliness

**Paper reference:** Boards 04, 22, and 31

### Reproduction

1. Open a dense recorded day directly in Keep.
2. Switch among cameras or let timeline thumbnails and the Other cameras filmstrip populate.
3. Observe failed browser requests and console errors.

### Actual

- Every H.264 camera in the fresh-page benchmark produced failed `blob:` requests.
- Each page attempted to load 5-12 distinct object URLs after they had already been revoked.
- Chromium reported 47-144 combined request-failure and console-error notifications per camera
  during the cold-open plus five-second playback sample.
- Failures occur while timeline thumbnail eviction and multiple filmstrip replay cursors compete
  with the primary player lifecycle.

### Expected

Object URLs should remain valid until every consuming media element has detached, and background
previews must not interfere with the primary replay.

### Acceptance criteria

- Revoke thumbnail and MSE object URLs only after their owning DOM consumers detach or switch source.
- Prioritize the primary player through its first decoded frame before opening filmstrip cursors.
- Bound simultaneous filmstrip cursors independently of hardware-concurrency overestimates.
- A real-camera cold-open and camera-switch benchmark produces no failed `blob:` requests or
  console errors.

The final seven-camera cold-open benchmark produced zero failed requests and zero console errors
over 15-second samples. Filmstrip cards remain synchronized and clickable while primary playback
owns the decoder budget; visible previews are admitted when the primary player is paused. Mocked
camera-switch coverage passes. The remaining real format error was caused by stored H.265 media
being advertised as bare `hvc1`, which Chromium rejects for MSE. Stored playback now derives the
full RFC 6381 codec from the MP4 decoder configuration. A real North Frontyard Sub to Main switch
decoded H.264 in 1.10 seconds and HEVC in 319 ms without a media error or stale cold-seek overlay.

## Performance checks that passed

- Peek uses native `<video>` elements and no canvas renderer.
- At 1440 x 900, DOM content loaded in 56 ms, the first live video was ready in 1.76 seconds,
  and all nine videos were ready in 2.32 seconds.
- During a steady five-second desktop sample, all nine streams advanced approximately five
  seconds. No compositor interval exceeded 50 ms.
- At 390 x 844, all nine live elements mounted in 516 ms and were ready in 2.21 seconds,
  except the independently degraded Front Gate stream, which correctly exposed a no-keyframe
  panel.
- Focus quality changes were responsive: Deck switched to 640 x 360 low in 688 ms and back
  to 3840 x 2160 high in 1.01 seconds.
- Dark/light theme switching and the core mobile bottom navigation remained functional.

## Alpha audit, 2026-09-12

**Decision: do not close #145 or declare a feature freeze from this audit.** The original audit
found a security defect, and the release gate promises workflows outside the documented shipped
scope. Physical-camera, clean-host recovery, cross-browser/device, and soak evidence is incomplete.
The subsequent fixes and their verification are recorded below.

Audited main `4bc3ab18f3240bb0777c84c45a4c48dcb0829dae`. Runtime checkout
`4df9a94e2a09fbc02346af363ea79f6b8027d8a2` has the identical Git tree. Environment: Windows 11
build 26200 x64, Ryzen 5 5600G, 12 logical CPUs, 15.34 GiB RAM, Bun 1.4.0, Chromium
151.0.7922.34. Runtime fixtures lived on NTFS. Changes include audit tests, this ledger, empty book
chapters and their contents entries, and the subsequently authorized Paper design/reference update.
The original audit did not change application implementation, protected contracts, or approved
Loki baselines. The subsequent authorized remediation changes implementation and adds regressions;
protected contracts and approved Loki baselines remain unchanged.

The baseline is the complete successful [PR CI run](https://github.com/xnorpx/keeppeek/actions/runs/34722005032)
and [nine-job extended Main run](https://github.com/xnorpx/keeppeek/actions/runs/34722240086)
on that equivalent production tree. The previous canonical Windows check passed 2,398 Rust tests,
611 UI unit tests, and 242 browser tests. Its 21 Rust and two browser skips were pre-existing.
New checks below distinguish a current reproducer from inherited baseline evidence.

The initial `check.bat` attempts failed in existing Rust deadline tests (KP-QA-010 and
KP-QA-011). A subsequent serial `cargo nextest run --all --no-fail-fast` completed with
2,398 passed and 21 pre-existing skips in 453.144 seconds. Both intermittent cases passed
in that run; this does not resolve their earlier failures or make the canonical check green.
Clippy with warnings denied, unused-dependency checks, Rust/TOML/Python formatting, and the
book build passed separately. The book used CI's mdBook 0.5.4 and Mermaid preprocessor 0.17.1.
The fresh end-to-end run passed 242 tests with the two pre-existing codec skips in 4.5 minutes,
using two workers and the unchanged release binaries. UI static checks passed. The normal UI
quality command failed as recorded in KP-QA-012; Bun and compatibility phases independently
passed 361 and 57 tests respectively. Serial browser-unit diagnostics passed 125 client and
68 visual tests, completing all 611 UI unit cases across the separate runs. This preserves
coverage but does not make the default quality command reliable. Logs are retained under
`target/alpha-audit/`. The final canonical rerun stopped at the native-event shutdown bound
(KP-QA-014). Fresh UI static validation then passed separately, including Svelte's zero-error,
zero-warning check and E2E type checks. The complete Bun unit phase passed 366 tests across
64 files, including all five new reference tests. Logs: `ui-reference-static.log` and
`ui-reference-bun.log`. That audit candidate did not pass the complete canonical gate.

### Remediation verification

The user subsequently authorized implementation fixes and regression tests. All 14 original
findings have fixes; two additional camera-navigation defects were reproduced and fixed during
this work. The final Windows `check.bat` passes: **2,417 Rust tests**, **617 UI unit tests**
(367 Bun, 193 browser, 57 compatibility), and **255 E2E tests**. The 21 existing Rust skips and
two existing HEVC browser skips remain unchanged. Clippy, dependency, Rust/TOML/Python formatting,
UI lint/types/formatting, and Paper checks also pass.

Validation used the hash-matched NTFS checkout described above, two Cargo build jobs, and serial
Rust tests. E2E used its default six workers with no retries; all 13 new mobile cases pass,
including exact-midnight selection. Evidence: `target/alpha-fixes/canonical.log` and the source
SHA-256 manifest `candidate-files.json`. The Rust suite passed in 313.596 s and E2E in 1.8 min.

Earlier integrated attempts caught a stale Paper positioning evidence string and an inherited
Bun worker marker in the new Playwright collection regression. Both are corrected: the active
evidence matches README's Alpha status, and the standalone child CLI excludes `JEST_WORKER_ID`
while retaining all four collection assertions. The parallel Bun reproduction and passing rerun
are recorded in `target/alpha-fixes/bun-collector-red.log` and `bun-collector-green.log`.
The rows below describe focused verification.

| Finding       | Fix and regression evidence                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| KP-QA-001     | Protected Windows file creation and handle-based ACL verification before writes. Twelve Windows tests pass, including inherited readers, incomplete transfer cleanup, existing files, long paths, alternate streams, malformed ACLs, and SYSTEM ownership. Restoring the old implementation makes the before-write test fail. Unix `0600` behavior has an added regression; Unix and SMB runtime were not exercised locally.          |
| KP-QA-002     | Cancel has a 44×44 px hit area. The default E2E test measures the button and taps it to verify cancellation.                                                                                                                                                                                                                                                                                                                          |
| KP-QA-003     | The timeline config selects only `timeline.performance.ts`. A normal Bun regression runs the actual Playwright collector. Unqualified `bun run perf:timeline` passes its 150 ms p95 budget and 1,600-node guard.                                                                                                                                                                                                                      |
| KP-QA-004–009 | Corrected audit/session and MQTT restart contracts, camera grants, all seven native event fields and limits, Alpha status, and maintenance qualification evidence. Independently checked against source and existing operator guides; mdBook 0.5.4 with Mermaid 0.17.1 builds. Roadmap #147 prose now records all 12 core Alpha issues closed while #145 stays open; no gate checkboxes changed.                                      |
| KP-QA-010–011 | Controlled transport tests exercise actual authentication and ureq timeout configuration, asserting exact 1,000→800→600 ms snapshot budgets, 150 ms SOAP budgets, and exact-expiry rejection. Real three-request authentication and stalled header/body cases retain completion watchdogs. ONVIF focused validation passes 56 tests with one pre-existing ignored test; removing the new late-body guard makes its regression fail.   |
| KP-QA-012     | Each browser project has one worker, bounding the two pools to two workers total. The unchanged standard command passes all 193 browser-unit tests in 25.35 s; the original default run had 12 failures in 48.87 s. Tests and deadlines are unchanged. These single runs demonstrate the observed result, not a statistical flake-rate claim.                                                                                         |
| KP-QA-013     | Compact mobile Keep follows Board 46. Default E2E tests cover initial event intersection at 320/390 px, healthy decoded footage, all rates and volume, camera/day sheets, Back/Forward, focus, copy failure, quality/refresh, rotation, and real fullscreen. Media identity and open/close/seek counts verify player and session continuity during sheets and resizing; those mocked transitions do not prove uninterrupted decoding. |
| KP-QA-014     | A private clock/wait seam verifies the exact five-second drain and a 4.99-second acknowledgement. The real worker still must finish in five to six seconds, with acknowledgement lifetime, stopped state, and dropped counts intact. All five consumer pressure tests pass in 5.33 s.                                                                                                                                                 |
| KP-QA-015–016 | Same-clock camera changes retain timeline viewport information and issue new-camera event/day queries. Camera/day selection uses the existing exact-moment policy, showing gaps instead of silently jumping. Request-level and timestamp regressions failed before their fixes and pass afterward; explicit next-recording recovery remains tested.                                                                                   |

The standalone timeline run used the recorded Windows/Bun/Chromium environment with a concurrent
Rust compile: desktop initial-render p95 94 ms, maximum interaction p95 48.9 ms, and 1,430 peak
nodes; mobile initial-render p95 34.1 ms, maximum interaction p95 49.2 ms, and 205 peak nodes.
It passed in 14.7 s. This verifies the repaired command against its budgets; it is not an isolated
before/after comparison of application performance. Evidence: `target/alpha-fixes/timeline-performance.log`
and `browser-bounded.log`. Focused Rust and mobile evidence remains under `target/alpha-audit/`.

The final release CLI export check also passes: a complete 266-byte synthetic archive, exit zero,
and no broad inherited read rules. Evidence: `target/alpha-fixes/windows-export-acl.json`,
binary SHA-256 `6215b0fba41079cbd86c38480b47f65856cae00944dfd56efcf8a0e62ebded7f`.
The final opt-in render audit passes all 39 cases in 44.7 s; evidence:
`target/alpha-fixes/render-audit.log`.

### Findings and fixes

The following descriptions retain the original failure evidence. Resolution status refers to the
working-tree fixes above. Owning areas are triage destinations, not assigned people.

#### KP-QA-001 [P1] Windows configuration export leaves plaintext secrets readable through inherited ACLs

**Status:** Resolved in the working tree. **Owner area:** Backup/security. **Target:** Alpha blocker.

1. On Windows, create an NTFS directory with an inheritable `BUILTIN\Users` read permission.
2. Run `keeppeek config --server http://127.0.0.1:<fixture-port> export --output <directory>/configuration.zip`.
3. Inspect the exported file's ACL. Use the synthetic reproducer below; real secrets are unnecessary.

The command exits zero and writes the complete archive, but the file retains inherited
`S-1-5-32-545` `ReadAndExecute, Synchronize`. The format contains plaintext `secrets.toml`.
`src/backup/http_client.rs:222` applies private permissions only under `cfg(unix)`;
`docs/backup-and-restore.md` promises an owner-only CLI export. A readable destination therefore
exposes secrets to other local users despite a successful export.

**Expected:** Establish restrictive Windows permissions at file creation, or reject an unsafe
destination without leaving readable secret content. Verify both successful private export and
failure cleanup. Until fixed, export only into an independently verified private directory.

**Reproducer:** `powershell -NoProfile -ExecutionPolicy Bypass -File tests/qa/windows-export-acl.ps1`.
It creates synthetic ZIP data, uses loopback only, checks the successful CLI result before the
security assertion, and cleans its files and processes. It failed on the original audit candidate.
Evidence: `target/alpha-audit/windows-export-acl.json`, including the tested binary SHA256.

#### KP-QA-002 [P2] Mobile Add Camera cancellation has an 18 px touch target

**Status:** Resolved in the working tree. **Owner area:** Onboarding/accessibility. **Target:** Alpha usability.

Open `/cameras/new` at 390 x 844 and inspect **Cancel add camera**. The button's actual bounding
box is 18 x 18 CSS px; `MobileAddCameraWizard.svelte:175` explicitly uses `size-[18px]` without
a larger hit-area wrapper. It is unnecessarily difficult to cancel the wizard with a finger.
This is below the design audit's 44 x 44 touch-target guideline; this finding does not claim
a WCAG failure without evaluating that standard's spacing exceptions.

**Expected:** Keep the icon compact while giving its button at least a 44 x 44 hit area.
**Regression:** From `ui/`, run `bunx playwright test mobile-keep.e2e.ts --grep 44px`.
The assertion reports `Expected >=44, Received 18`. Screenshot and geometry are in
`target/alpha-audit/ui-surfaces-verified/` and `target/alpha-audit/ui-surfaces.json`.

#### KP-QA-003 [P2] The documented timeline benchmark selects incompatible live-wall tests

**Status:** Resolved in the working tree. **Owner area:** Performance QA. **Target:** Before Beta measurements.

From `ui/`, run `bunx playwright test --config playwright.timeline-performance.config.ts --list`.
It selects the timeline, Peek transitions, and Peek wall tests. The config's
`testMatch: '**/*.performance.ts'` is broader than its setup: it starts only the static timeline
preview, while Peek needs a nine-camera backend and the production UI. Consequently the documented
`bun run perf:timeline` is not an isolated, reproducible timeline gate.

**Expected:** The command selects exactly the workload its servers and fixtures support.
**Workaround used for measurement:** Append `timeline.performance.ts` explicitly.

#### KP-QA-004 [P2] The book promises access audit persistence that the server deliberately does not provide

**Status:** Resolved in the working tree. **Owner area:** Access documentation. **Target:** Alpha.

`book/src/authentication.md:237` promises atomic audit writes to `config.toml` within one second
and on shutdown. `docs/access-control.md:187` states that audit history, sessions, and last-use
activity reset on restart. The implementation agrees with the latter: `src/access.rs` clears
audit on open, has a no-op `flush_audit`, and tests `credential_lifecycle_persists_in_config_and_audit_resets_on_reopen`.
Operators could rely on evidence that will not survive restart.

**Expected:** The book distinguishes persistent credentials/grants from transient audit/activity
and explains the external log retention needed for durable evidence. No audit persistence claim
should remain without a restart test proving it.

#### KP-QA-005 [P2] Release guidance incorrectly promises MQTT outbox recovery after restart

**Status:** Resolved in the working tree. **Owner area:** Integrations documentation. **Target:** Alpha.

`book/src/release-readiness.md:68` groups MQTT outbox state with durable recovery checks.
`book/src/notifications-and-integrations.md:102` and `docs/event-forwarder.md` explicitly discard
pending publications and deduplication history on restart. `src/event_forwarder/outbox.rs` tests
the new empty outbox. This creates an incorrect delivery/recovery expectation.

**Expected:** State the actual loss/replay boundary and a reproducible restart procedure without
claiming durable pending delivery. Keep durable operational event records distinct from transient
notification and MQTT runtime state.

#### KP-QA-006 [P2] Release limitations say per-camera permissions are unavailable

**Status:** Resolved in the working tree. **Owner area:** Access documentation. **Target:** Alpha.

`book/src/release-readiness.md:101` recommends separate instances because per-camera permissions
are supposedly unavailable. `book/src/authentication.md:89` documents shipped server-enforced
camera grants and dashboard audiences. Server tests cover camera filtering, guarded direct
subscriptions, revocation, and revision-bound persistent grants. The limitation directs operators
away from a supported security workflow.

**Expected:** Describe the tested camera-grant model and its actual limits consistently.

#### KP-QA-007 [P2] The configuration reference omits native camera event settings

**Status:** Resolved in the working tree. **Owner area:** Camera-event documentation. **Target:** Alpha.

The `CameraConfig` field table in `book/src/configuration-reference.md:215` omits the `events`
table and its mode, metadata-stream, source/topic/endpoint, and snapshot fields. These fields
are documented in `docs/onvif-events.md` and `docs/configuration-management.md:99`, and some are
file-only. An operator using the advertised complete reference cannot configure this feature.

**Expected:** Synchronize the existing reference with supported fields, defaults, limits, and
secret-reference behavior. The empty `native-camera-events.md` placeholder is for the missing
operator workflow; it does not repair this existing reference.

#### KP-QA-008 [P3] Release phase guidance and roadmap prose are stale

**Status:** Resolved in the working tree. **Owner area:** Release documentation. **Target:** Alpha.

The book introduction, get-started, and release-readiness chapters still discuss MVP/#144;
README still says POC, while #147 says Alpha. #147 also checks #133/#114 complete but calls them
remaining implementation work in its prose. Readers cannot identify the current gate reliably.

**Expected:** Use one evidence-backed phase and link #145 as the active gate. Do not imply that
Alpha qualification is complete merely because implementation issues are closed.

#### KP-QA-009 [P3] Recording-maintenance qualification notes retain obsolete status

**Status:** Documentation resolved; remaining hardware limits still need qualification.
**Owner area:** Maintenance documentation. **Target:** Alpha.

`book/src/recording-maintenance.md:5` labels #133 incomplete, although it closed September 8.
Its closing verification section also says Windows compilation/runtime remain unverified.
The linked current Main run and local canonical run now exercise Windows implementation and
synthetic deletion. The book should link that evidence while retaining any untested real-filesystem
or physical deployment limits; synthetic tests alone do not establish every storage claim.

#### KP-QA-010 [P2] Snapshot deadline regression assumes authentication progress under scheduling contention

**Status:** Resolved in the working tree. **Owner area:** ONVIF test reliability.
**Target:** Before release qualification can rely on repeatable full-suite results.

During canonical `check.bat` with two concurrent Rust tests, `onvif::event_snapshot_deadline`
failed `snapshot_uses_one_deadline_across_challenges_and_the_body` at line 56: expected three
requests, observed one. The error and one-second deadline assertions passed. An immediate isolated
run of the same compiled test passed in 1.03 seconds. This is scheduling-sensitive test evidence,
not a demonstrated snapshot timeout violation.

The fixture sleeps 200 ms before each of two challenges and assumes both finish within the
overall one-second request budget. Contention can consume that budget before the second request.
The full run stopped after 1,388 passes, leaving 1,009 tests unrun. Raw evidence is in
`target/alpha-audit/alpha-audit-canonical.log`.

**Expected:** Preserve proof of one deadline across authentication and body reads while making
protocol-stage progress independent of scheduler slack. Keep the deadline assertion; do not fix
this by simply deleting the three-request coverage or broadly extending timeouts. A full serial
rerun is recorded separately and does not erase the failure.

#### KP-QA-011 [P2] SOAP deadline test exceeds its wall-clock bound in the serial suite

**Status:** Resolved in the working tree. **Owner area:** ONVIF deadlines/test reliability.
**Target:** Before release qualification relies on repeatable timing evidence.

`onvif::event_client::soap_deadline_covers_stalled_headers_and_body` failed at
`crates/onvif/onvif/tests/event_client.rs:58`: elapsed 470.2203 ms against a 150 ms request timeout
plus 150 ms allowed overhead. The serial canonical run stopped after 1,269 passes, leaving 1,128
tests unrun. The same compiled test immediately passed alone in 0.31 seconds.

This establishes unreliable wall-clock qualification on this Windows host. It does not establish
whether the overrun comes from application deadline accounting, Windows socket behavior, or
scheduler delay. Preserve both stalled-header and stalled-body cases and profile the deadline path
under load before choosing a fix. Do not simply raise its limit or remove the assertion.
Evidence: `target/alpha-audit/alpha-audit-canonical-final.log`.

#### KP-QA-012 [P2] Default Windows browser-unit validation times out across unrelated features

**Status:** Resolved in the working tree. **Owner area:** UI test infrastructure.
**Target:** Reliable Alpha validation.

Run `bun run quality:check` from `ui/` on this 12-thread Windows host. Static checks and all
361 Bun tests passed, then the Chromium client/visual phase reported 181 passed and 12 failed
across 57 files in 48.87 seconds. Every failure hit the existing 15,000 ms test deadline;
there were no assertion mismatches. The compatibility phase did not start because the command
stopped on the browser failures.

Affected files: `CameraAccessDialog`, `CopyMomentLink`, `EventPreview`, `FocusedMediaViewport`,
`HorizontalTimeline`, `NotificationsRuntime`, `PeekWallSettings`, `RecordedPlaybackControls`,
`VerticalTimeline`, Home Assistant `elements`, `Board06PeekLiveWall`, and `Board13StorageRetention`.
These are one observed validation problem, not twelve established production defects.

`ui/vite.config.ts` leaves browser workers at their default. Installed Vitest 4.1.11 creates
separate concurrent pools for the client and visual projects; on this host each can use 11
workers. Nearby passing interaction tests took 8–14 seconds while many static checks took
less than 200 ms. Excess concurrency is a supported hypothesis, not a proven root cause.
An attempted `bun run test:unit:browser --maxWorkers=2` diagnostic still reported 187 passed
and six timeouts. Installed Vitest does not forward that root option to these project browser
pools, so it did not establish the intended concurrency limit. A valid controlled diagnostic
runs `bun run test:unit run --project client --no-file-parallelism` and then the corresponding
`--project visual` command separately, retaining every test, assertion, and original deadline.
Those runs passed all 125 client tests in 25.79 seconds and all 68 visual tests in 18.06 seconds.
This supported browser-pool contention as the cause hypothesis. The subsequent project-level worker limits and standard-command pass are recorded in the remediation table.

**Expected:** The documented quality command completes reliably on supported development/CI
hosts with bounded browser concurrency. Preserve functional coverage and timing assertions
while diagnosing scheduling and browser interaction readiness.
Evidence: `target/alpha-audit/ui-quality.log` and `target/alpha-audit/ui-browser-bounded.log`.
The successful diagnostics are `target/alpha-audit/ui-client-serial.log` and
`target/alpha-audit/ui-visual-serial.log`. The independently completed compatibility phase
passed all 57 tests.

#### KP-QA-013 [P2] Mobile Keep pushes recording history below the initial viewport

**Status:** Resolved in the working tree. **Owner area:** Keep/mobile design.
**Target:** Alpha usability.

Open a populated camera/day in Keep at 390 x 844. The stacked mode, camera, date, quality,
refresh, and copy controls consume the space above the player. Playback controls then push
the timeline's event row below the fixed bottom navigation. The first screen provides almost
no recording history to browse. Scrolling reaches it; this is not a demonstrated playback or
seek failure.

**Evidence:** `target/alpha-audit/ui-surfaces-verified/alpha-audit-keep-fixture-r-f995e-low-or-page-errors-at-390px/surface.png`.
From `ui/`, capture it with
`bunx playwright test --config playwright.alpha-audit.config.ts --grep 'keep fixture.*390px'`.
That render check currently passes because it checks overflow/errors, not initial history visibility.
The existing mobile case at `ui/e2e/keep-timeline.e2e.ts:609` verifies event-card containment
and 44 px controls but also lacks an assertion that history appears above bottom navigation.

The added touch regression `mobile Keep shows recording history above navigation without scrolling`
checks the first event card against the navigation boundary without scrolling. The focused NTFS
run fails at the intended assertion: event bottom 902.47 px, navigation top 766 px, and page/main/
content scroll positions all zero. Its metadata-only fixture displays the existing media
initialization error, so this result qualifies initial history geometry in that state, not decoded
playback. The earlier render capture also shows the crowded layout. Evidence:
`target/alpha-audit/keep-mobile-initial-viewport-ntfs.log`, `keep-mobile-initial-geometry.json`,
and `keep-mobile-initial-viewport.png`. Preserve a healthy decoded-media case when fixing this.

The horizontal portrait timeline is deliberate: commit `e6137adf` introduced it, and
`ui/src/routes/keep/+page.svelte:365` selects it below 768 px in portrait. The older Board 22
Paper story still forces `VerticalTimeline` and uses a decorative player. Orientation alone
is not a defect, and a compact replacement Paper reference does not implement a production fix.

**Expected:** Keep the horizontal timeline and group the command area so the default 390 x 844
view shows footage, primary playback controls, and the timeline event row above bottom navigation.
Preserve camera/day navigation, all modes, quality, refresh, copy, transport, volume, speed,
fullscreen, and digital zoom; secondary controls can use an accessible menu or sheet. Retain
44 x 44 touch targets, keyboard access, focus/error states, and zoom controls outside the image.
Keep the added initial-viewport geometry assertion alongside interaction coverage.

#### KP-QA-014 [P2] Native-event shutdown deadline fails in the canonical Windows check

**Status:** Resolved in the working tree. **Owner area:** Native events/test
reliability. **Target:** Reliable Alpha qualification.

After a fresh Windows debug build, `check.bat` failed
`camera_events::consumer::pressure_tests::shutdown_finishes_within_six_seconds_when_native_ack_is_withheld`.
Nextest reported 11.214 seconds for the test process. The failure is the post-join
`started.elapsed() < Duration::from_secs(6)` assertion at
`src/camera_events/consumer/pressure_tests.rs:228`, not its channel-receive assertion.
The production consumer has a five-second pending-event drain deadline in
`src/camera_events/consumer.rs:139`. This evidence does not isolate runtime shutdown behavior
from Windows scheduling delays; investigate both before treating the failure as harmless.

The run used one nextest thread and stopped after 181 passes and this failure; 2,216 tests did
not run, and the canonical script did not reach its later quality stages. The earlier complete
2,398-test pass remains separate evidence. Preserve the shutdown bound, acknowledgement lifetime,
stopped state, and dropped-event assertions when diagnosing this failure.

**Evidence:** `target/alpha-audit/canonical-reference-update.log`.
Focused follow-up: `cargo nextest run --all -E 'test(shutdown_finishes_within_six_seconds_when_native_ack_is_withheld)' --no-fail-fast`.
That isolated follow-up passed in 5.595 seconds without assertion changes. Its log is
`target/alpha-audit/native-ack-shutdown-isolated.log`. The narrow remaining margin and earlier
failure warrant investigation; one passing rerun does not resolve the canonical failure.

#### KP-QA-015 [P2] Same-clock camera switches can omit timeline events and recorded days

**Status:** Resolved in the working tree. **Owner area:** Keep navigation.

Open a populated camera at 06:07, wait for its timeline metadata, and select another camera at
the same day and clock position. The camera's video changes, but event and recorded-day queries
for the new camera can remain absent. Previous-day navigation then stays disabled even when
the new camera has older recordings.

`selectCamera` cleared `latestTimelineViewport`, while the horizontal timeline reports its
viewport again only when its geometry or day changes. A same-clock switch changes neither.
Recording-day discovery also needs to consume pending work when secondary loading has already
been released. Retain the current viewport and let the existing bounded query scheduling use
the new camera identity.

The new default E2E regression observes outgoing event and day-bucket queries after a switch.
Removing the minimal fix makes both remain absent for the full ten-second assertion deadline;
it does not merely inspect a mocked button state. Evidence:
`target/alpha-audit/mobile-camera-metadata-red.log`.

#### KP-QA-016 [P2] Camera selection can silently jump away from an unavailable recording moment

**Status:** Resolved in the working tree. **Owner area:** Keep navigation.

Select 06:04 on one camera, then switch to a camera whose nearest recording starts at 06:06:40.
The original path silently changes the playhead from `1787033040000` to `1787033200000` instead
of keeping the investigation moment and showing the gap. The new regression failed on this
exact timestamp mismatch in `target/alpha-audit/mobile-keep-expanded-retry.log`.

Camera and recorded-day navigation must preserve the selected UTC clock position. When the
destination has no footage there, retain that moment, show the gap, and keep explicit previous/
next-recording actions available. Forward the existing exact-moment policy through these
selection paths; do not add a second seeking policy or fabricate footage.

The exact playback-end boundary also needs normalized UTC time-of-day. At midnight, subtracting
the old selected day's start produced a full-day offset and selected the day after the user's
chosen date. A regression drives the route's `timeupdate` and `ended` handlers, changes to an
older recorded day, and checks that day's midnight and explicit gap. It failed before the modulo
fix and passes afterward, alongside the existing date/navigation test (2/2 in 10.3 s).
Evidence: `target/alpha-audit/mobile-midnight-red.log` and `mobile-midnight-green.log`.

#### KP-QA-017 [P2] Revocation during request dispatch can leave an invalid API transport open

**Status:** Fix implemented; focused regressions and full canonical validation pass.
**Owner area:** Access/session lifecycle and CI reliability.

Changing camera grants or revoking a credential while its dashboard initializes can leave
`API session expired or was revoked` visible without returning to sign-in. The server still
denies the invalid session's requests; the defect is transport cleanup and client recovery,
not an authorization bypass.

A request can observe the new credential revision before the bulk session-close operation.
Authorization removes the stale owner entry and requests transport closure, but the control
preauthorization and data-message adapters discard that closure flag. Bulk cleanup then cannot
find the removed owner, leaving the WebRTC worker connected until another lifecycle event.

Preserve the terminal-close flag across both adapters. A control rejection queues closure through
the existing after-send action; a data rejection has no response and signals worker shutdown
immediately. Ordinary role or camera permission denials must leave a valid session connected.
The camera-access browser test also waits for the expected dashboard and exact visible camera
set before deliberately changing policy, keeping initialization and grant-change assertions distinct.

Three new tests use accepted API sessions with real workers and binary control/data dispatch.
They invalidate the credential in the exact interval before bulk cleanup, covering both revision
changes and revocation. Before the fix, the control test has no deferred close action and the data
worker fails the existing one-second completion bound. After the fix, all three pass in 0.48 s,
including ordinary role, camera, and data permission denials that preserve the live session.
Fourteen existing access and output-ordering tests also pass. These are focused regression
results, not a statistical flake-rate or transport-latency benchmark.

Evidence: `target/alpha-fixes/session-authorization-red-final.log`,
`session-authorization-green.log`, `session-authorization-existing.log`, and
`camera-access-isolated.log`. Main's separate ZIP-inspection timeout remains a diagnostic
uncertainty: explicit Python selection and redacted process diagnostics improve reproducibility,
while the ten-second timeout and archive-content assertions remain unchanged.

#### KP-QA-018 [P2] A cold recording seek can count the previous video offset twice

**Status:** Fix implemented; deterministic regressions and full canonical validation pass.
**Owner area:** Keep recording playback, keyboard navigation, and export marks.

When a seek needs a new recording fragment, the same-segment path applies the requested video
offset against the old fragment before awaiting the new fragment. It then replaces the absolute
anchor without applying the new fragment's relative offset. A queued `timeupdate` can therefore
combine the old offset with the new anchor and move the playhead beyond the requested frame.

The keyboard trace shows a first 40 ms step becoming 80 ms without another key press. A second
step reaches 120 ms and then 200 ms, and the export end mark captures the incorrect timestamp.
The fixture returns a fragment anchored at the requested timestamp, making the extra offset
observable. Real decoded-media impact still requires validation; the trace establishes the
route's incorrect timestamp transition with the controlled media fixture.

The same-segment seek now applies the new fragment's relative offset after accepting its response.
A version-owned guard suppresses obsolete media-clock updates only while the source and offset
are being committed; normal updates continue during the network wait. A controlled regression
holds the seek response and delivers `timeupdate` at source replacement. Its first version failed
with an 80 ms playhead after one 40 ms step. A second case with a preexisting 500 ms offset exposed
a transient extra 500 ms in the published DOM, requiring the commit guard as well as the offset fix.

The final regression checks the transient and settled timestamps, a paused player, the reset
relative video offset, and exactly one seek request. The existing keyboard/export test now requires
exact 40 ms frame steps and a 40 ms marked interval. The final source passes full canonical
validation, including all 258 runnable browser E2E cases. An earlier decoded-media case stalled
during `/create`, before any player or seek existed; that case passes in the final run, but the
earlier stall's cause remains unproven.

Evidence: `target/alpha-fixes/e2e-trace-red/` and `ci-followup-e2e-trace.log`, keyboard test
`controls Keep transport, exact frames, live follow, and export range from the keyboard`.
Deterministic evidence is in `cold-seek-red-artifacts`, `cold-seek-nonzero-red-artifacts`, and
`cold-seek-trace-green-artifacts` under the same `target/alpha-fixes` directory.

#### KP-QA-019 [P2] Browser timing assertions include unrelated test-runner delays

**Status:** Timing corrections implemented; focused and full canonical validation pass.
**Owner area:** Events performance measurement and Keep startup regression tests.

The dense Events test measures first-page time after Playwright's card-count assertion finishes.
In the failing trace the results are visible at navigation +1,699 ms, within the unchanged
2,000 ms budget. The count assertion samples zero at +1,569 ms and does not observe the populated
page until +2,202 ms; the runner then records 2,231 ms. This measures observer latency as part of
render time. A test helper now records the first completed card render with the browser's
navigation clock. A delayed-consumer regression independently observes the populated page, waits
longer than the budget, then checks the recorded render timestamp against that earlier observation.
Replacing the saved timestamp with readout time makes this regression fail.

A subsequent trace measured a real 2,574 ms render, dominated by Vite's initial module transforms:
229 successful requests, with session creation not starting until 2,365 ms. The same fixture and
assertions against the recorder's built UI rendered in 540 ms with 63 successful requests and no
long task. The two Events performance tests therefore target the already-running built UI. Their
1,000/2,000 ms render budgets, 50/150 ms long-task budgets, and card-count, transfer, and DOM checks
remain intact. Other browser tests still use Vite. These measurements compare test-serving modes;
they do not claim an application runtime speedup.

The Keep fallback test starts its five-second observation window immediately after navigation.
In the failing trace session creation completes about 2.077 seconds into that window, before the
stored open can establish the product's three-second startup timer. The assertion stops at least
73 ms before that timer could expire. Start the fallback observation at the playback operation it
measures, retaining both the three-second product deadline and five-second test bound.

Evidence: `target/alpha-fixes/e2e-trace-red/`, including `frame-1699ms.jpeg` in the Events failure
directory, and `ci-followup-e2e-trace.log`. These diagnoses do not explain the earlier isolated
frontend-startup timeout or the three initial-page failures whose subsequent trace run passed.

#### KP-QA-020 [P2] Create-lease regression infers a deadline from unrelated worker duration

**Status:** Test correction implemented; all 25 focused PullPoint tests and full canonical validation pass.
**Owner area:** Native ONVIF event subscriptions and CI reliability.

Main CI run `34736789426` on `1b4e5c0` failed on macOS in
`delayed_create_uses_request_start_for_its_delivery_deadline`. Its 900 ms watchdog began at worker
spawn and included discovery, Digest authentication, two capability exchanges, a scripted 400 ms
response delay, queue pressure, and unsubscribe. The failure did not identify which phase consumed
the margin and does not establish a production lease-calculation regression.

A private helper preserves the production request-start, HTTP execution, parsing, and lease-clock
ordering. The replacement regression keeps the real authenticated HTTP create and unsubscribe,
but advances a controlled application clock by exactly 400 ms when the response returns. It checks
expiry at the request start plus one second, renewal at plus 666,666,667 ns, and 600 ms remaining.
Capturing the timestamp after the response must fail these exact assertions.

Existing real queue-pressure, delivery-timeout, reset, recovery, and cleanup tests remain. The
full-pressure test retains its 1.6-second bound, one pull, error, unsubscribe, no active subscription,
stall, and drop assertions, and now explicitly checks that no renewal occurs. This separates exact
lease arithmetic from real delivery and cleanup behavior without increasing a watchdog.

The timestamp-after-response mutation failed by exactly 400 ms. Restoring request-start capture
passes all 25 PullPoint tests in 13.81 seconds, including the real pressure and cleanup cases.
This is focused regression evidence; canonical and hosted qualification are recorded in the PR.

Evidence: `target/alpha-fixes/main-1b4-ci-34736789426-macos-rust.log`, lines 2796–2808,
`lease-create-order-final-mutation-red.log`, and `pullpoint-final-green.log`.

Final Windows validation for KP-QA-017 through KP-QA-020 passed with slow tests enabled:
2,420 Rust tests, 617 UI tests, and 258 E2E tests with six workers and zero retries. The existing
21 ignored Rust tests and two codec skips remain. Formatting, lint, static analysis, type checks,
and builds passed. All 1,987 source files matched the validation checkout and remained unchanged
during the run. Evidence: `target/alpha-fixes/ci-regressions-canonical-20260913.log` and
`ci-regressions-post-validation-integrity.json`. Hosted qualification is recorded in the PR.

### Release-contract discrepancies requiring an owner decision

These are not mislabelled runtime failures. Current behavior is intentionally documented, but
it cannot satisfy #145 as written. Keep the gate open until implementation or an explicit,
reviewed gate change resolves each discrepancy.

| Gate requirement                                                     | Shipped/documented behavior                                                                                              | Required decision or evidence                                                                       |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| Dry-run recovery and clean-host playback/export of restored metadata | Format-3 export contains exactly configuration and plaintext secrets; no databases/media and no separate dry-run request | Define and rehearse a consistent recording archive recovery workflow, or explicitly revise the gate |
| Home Assistant live/event/recorded navigation                        | Direct card supports live only; event ribbon and timeline playback are not implemented                                   | Implement the promised navigation or explicitly narrow the Alpha claim                              |
| No normal Alpha feature requires manual configuration editing        | Native camera event policy and direct-card allowed origins still require file edits                                      | Supply the intended visual workflows or record deliberate scope exceptions with consequences        |

### Evidence and feature coverage

The new visual suite captures 13 surfaces at 1440 x 900, 390 x 844, and 320 x 844: Dashboard,
Viewer, Cameras, onboarding, Keep, Events, Health, Settings, storage, access, integrations, logs,
and maintenance. **39/39** fixture cases passed document-overflow and uncaught-error checks after
correcting the audit fixtures. Accessibility trees, screenshots, and control geometry are retained.
This uses desktop pointer behavior at each width and checks rendering and navigation structure,
not every mutation, decoded media, touch gesture, or WCAG compliance. The separate touch-target
test enables mobile and touch emulation; existing end-to-end tests cover coarse-pointer zoom
controls and horizontal toolbar scrolling.
The original separate mobile target assertion failed; its fix now has a default touch regression.
The mechanical design detector's one border-accent
warning was rejected as a false positive: it marks an intentional retention warning, not a defect.

The visual review found a coherent shell, consistent camera-health states, readable desktop/mobile
hierarchy, and no horizontal document overflow in the checked states. The mobile hit area is fixed;
complete contrast and screen-reader verification remain outstanding. Theming has
existing regression tests, but this added screenshot sweep is not a complete light-theme certification.

| Feature family                                    | Available verification                                                                                                  | Remaining Alpha qualification                                                    |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Installation and onboarding/catalog               | Existing wizard, authentication, probe, manual RTSP, catalog, real-video/keyframe tests; new three-width captures       | Physical mixed-fleet onboarding and packaged clean installation                  |
| Configuration, inheritance, templates, bulk edits | Existing revision/secret/conflict/draft/restart tests and full browser baseline                                         | Real device activation and every file-only policy                                |
| Dashboards, layouts, Viewer, kiosk                | Existing persistence/import/audience/admission/keyboard/browser tests and captures                                      | Measured nine-camera wall on representative devices and long-running kiosk       |
| Keep, codecs, zoom, moment links                  | Existing real H.264/mixed-codec/seek/pixel tests; measured zoom/copy; captures                                          | Current physical-camera cold seek, supported browser codecs and low-end devices  |
| Events, reviewed/dismissed/bookmarks, exports     | Existing paging, conflict, held evidence, canonical media and export lifecycle tests                                    | Real export throughput/memory and recovery of restored review metadata           |
| Access, roles, camera grants                      | Existing server filtering/revocation/origin tests and browser role tests; passing Windows export ACL regressions        | Actual remote proxy/VPN topology and other supported-platform security matrices  |
| Health, logging, diagnostics                      | Existing stale/fresh/recovery/redaction tests; captures; measured diagnostics                                           | External durable audit/log retention rehearsal                                   |
| Backup, restart, migration                        | Existing malformed archive, atomic two-file activation and rollback tests; passing ACL tests and rebuilt CLI check      | Recording archive recovery on a clean host and version upgrade/rollback matrix   |
| Deletion and reconciliation                       | Existing durable job, confinement, held evidence, race/restart tests and real-media browser deletion                    | Real deployment filesystem crash/recovery rehearsal                              |
| Native events and external analysis               | Existing renewal/dedupe/saturation/recorder-isolation tests; focused Python tests: 28 passed, one existing Windows skip | Physical generic/native sources and real model start/fail/restart                |
| Notifications, MQTT, integrations                 | Existing authorization, retry, resource-bound and secret-safety coverage; corrected restart documentation               | Provider/network failure and MQTT restart rehearsal                              |
| Home Assistant                                    | Existing direct-card pixels, sharing/reconnect/origin and real-container CI                                             | Published install/upgrade/rollback; gate's unsupported event/recorded navigation |

### Performance measurements

Sequential runs used existing budgets without production changes. Full workloads, commands,
environment, and JSON reports are in `target/alpha-audit/performance/report.md`.

| Workload                                                 | Samples                                | Measured p95                          | Existing budget |
| -------------------------------------------------------- | -------------------------------------- | ------------------------------------- | --------------- |
| Diagnostics, 10,000 server and 2,000 browser logs        | 3 warmups + 15 measured                | 283.33 ms                             | 1,500 ms        |
| Dense timeline initial render, 1,440 segments/600 events | 10 per viewport                        | Desktop 82.4 ms; mobile 26.6 ms       | 150 ms          |
| Timeline zoom/pan/seek/drag/filter                       | 20 per operation                       | Worst desktop 48.9 ms; mobile 49.3 ms | 150 ms          |
| Live digital zoom, H.264 640 x 360 at 15 fps             | 3 x 120 frames, 20 pointer moves/frame | Input 1.0 ms                          | 8 ms            |
| Copy recording link                                      | 20 baseline + 20 copy                  | Copy 17.8 ms; baseline 18.4 ms        | 250 ms          |

Timeline DOM remained bounded at 1,430 desktop/205 mobile nodes against 1,600. Copy added no
session/open/close/seek/subscription work. Desktop timeline startup recorded a descriptive 102 ms
long-task p95; interaction budgets passed, so profile startup separately before calling it an
interaction regression. No real-user Core Web Vitals claim is made.

Additional **hosted evidence**, not fresh local measurements, came from the successful PR CI on
the same `4df9a94e` production tree:

- External analysis, 20 samples: commit p95 60.472 ms against 2,000 ms; fanout p95 131.323 ms
  against 2,500 ms. Resident-memory delta p95 was 93,687,808 bytes over 61 observations against
  134,217,728 bytes. Queue high-water marks were one of 64 items and 439,108 of 8,454,144 bytes.
  Evidence used H.264/H.265 640 x 360 low streams and a 3840 x 2160 JPEG of 438,622 bytes.
- Home Assistant, ten paired samples: shared bootstrap p95 1,895.5 ms against 10,000 ms;
  isolated bootstrap was 1,079.2 ms. Sharing reduced sessions and subscriptions from three to one,
  with a measured startup-latency tradeoff. All five real-container tests passed.
- Backup/configuration benchmarks were explicitly ignored in the coverage job. The Slow Rust job
  ran six stream/storage tests and supplied no backup or 127-camera benchmark measurements.

Downloaded hosted artifacts and the extraction report are under `target/alpha-audit/performance/`.

Unmeasured in this audit: nine-camera wall/transition fixture (derivatives absent and FFmpeg not
available), real historical seek latency, large-fleet browser latency/memory, large export resource
use, physical cameras, hardware/browser matrix, cold disks, remote networks, and sustained soak.
Existing virtualization bounds and old measurements do not substitute for those current measurements.

### Paper reference comparison and update

The live [Paper reference](https://app.paper.design/file/01M0B0VBH78TMTX40GCYYQ37SG/1-0) was inspected
and updated after the user authorized reference-design changes. The current export is
[`ui/design/paper/keeppeek-nvr-alpha`](ui/design/paper/keeppeek-nvr-alpha/README.md): 36 NVR boards
(01–34, 45, 46), 56 lossless 1× PNGs, original board JSX, and all 82 live tokens at revision
`b35ec365`. The file's other products are excluded. All 80 shared token values still match the
application; no runtime theme change was needed.

Board 46 now specifies a compact mobile Keep default, playback-options sheet, and camera/date
sheet. It preserves horizontal history, every existing playback rate, direct seeking, quality,
camera/day navigation, moment links, refresh, mute/volume, fullscreen, and external zoom controls.
The compact implementation now follows that proposal, with the route and interaction regressions
listed above. Complete device/accessibility and pixel qualification remain separate. The current storage, access,
ZIP, and recording-integrity supplements are also captured. The
[design decisions](ui/design/paper/keeppeek-nvr-alpha/DESIGN-DECISIONS.md) distinguish deliberate
implementation choices, historical frames, proposed improvements, and required runtime acceptance.

Five new tests in the normal Bun unit suite pass with 3,366 assertions. They require all 36 boards
and 56 scenario IDs, verify export hashes and PNG dimensions, and compare shared token values.
They detect incomplete exports; they do not establish production pixel parity or live-file freshness.
The historical v34 generated exports and existing approved Loki pixels were preserved. Its active positioning-evidence string now checks the corrected Alpha README status. Synthetic story diffs
remain descriptive comparison artifacts and must not be treated as real-route acceptance.

### Missing UI and automation coverage

These are coverage gaps, not additional confirmed product defects. Existing unit, mocked-browser,
real-server, and performance evidence above remains valid within its tested boundary.

- **Opt-in render audit:** The 39 render cases remain under `ui/qa/`, selected only by
  `playwright.alpha-audit.config.ts`. The two touch/history regressions moved into default
  `e2e/mobile-keep.e2e.ts`, alongside eleven additional touch regressions. Windows export ACL
  coverage is now part of normal Rust tests; the separate CLI reproducer remains available.
- **Unreviewed Paper baselines:** The preserved v34 inventory lists 49 references, with 11 approved
  Linux Loki baselines and 38 capability-gated candidates (`ui/design/paper/keeppeek-nvr-v34/COVERAGE.md:8`).
  Loki uses `--requireReference=false`; missing references produce review artifacts rather than
  approved comparisons (`ui/visual-harness/README.md:21`). The new Alpha reference tests validate
  exported hashes, dimensions, scope, and shared tokens, not actual-route visual parity. Review
  candidate overlays and test real-route geometry and expanded/error states before claiming parity.
- **Missing live authorization transition:** Rotate, disable, and revoke an actual remote
  credential while live and recorded media are decoding; assert media stops and the old key cannot
  reconnect. `ui/e2e/access-auth.e2e.ts:27` simulates control-channel closure. The real-server grant
  test at `ui/e2e/camera-access.e2e.ts:31` verifies camera filtering and denied control, but revokes
  only after closing the user context, without a decoding-media assertion.
- **Missing role/URL combinations:** Exercise a real remote User opening protected nested URLs
  directly, plus a remote Admin signing in and completing a real mutation. Existing role tests
  check hidden navigation and camera grants, not every restricted-prefix redirect in
  `ui/src/routes/+layout.svelte:347`. Existing route tests cover Viewer, setup, diagnosis, and
  maintenance; this is a role/state matrix gap, not an absence of those routes from all tests.
- **Missing browser-to-restart persistence:** Save fleet settings/templates and dashboard imports
  through the UI, restart the real server, and read them from a fresh browser. Configuration tests
  (`ui/e2e/configuration.e2e.ts:112`) mock activation; dashboard CRUD/import tests
  (`ui/e2e/peek-layout.e2e.ts:362`) reload a mocked registry. Backend persistence tests already
  exist, but do not join the complete browser workflow to restart and reload.
- **Missing complete storage move:** Stage a storage change through the UI, execute the actual
  restart/move, then decode an existing recording, export it, and verify recording resumes.
  `ui/e2e/settings.e2e.ts:584` checks staging with a mocked server and intentionally expects no
  restart. `ui/e2e/backup.e2e.ts:170` performs a real configuration restart while preserving local
  storage paths; neither is an end-to-end storage migration rehearsal.
- **Missing dirty-state reconnect:** Disconnect the actual transport during a dirty editor or
  pending mutation, reconnect, and verify draft, revision/conflict, completion, and error states.
  `ui/e2e/configuration.e2e.ts:344` withdraws and restores capabilities on a living mock channel;
  separate conflict/navigation tests do not exercise authentication loss and layout teardown.
- **Browser and accessibility qualification:** The automated browser projects use Chromium;
  Firefox/WebKit projects and corresponding codec/fullscreen/touch workflows remain absent.
  Chromium on macOS does not qualify Safari. Named controls, keyboard, and touch-target tests
  exist, but complete contrast, focus/dialog traversal, zoom/reflow, and screen-reader workflows
  across themes and supported browsers remain unqualified. The 39 render captures do not supply
  those assertions.
- **Performance automation and workload gaps:** Dedicated timeline, diagnostics, digital-zoom,
  nine-camera wall, and transition benchmarks exist but their dedicated runners are not invoked
  by current workflows. Backup/configuration benchmarks are ignored, and the 127-camera catalog
  benchmark is separate. Current fleet interaction latency/memory and sustained export
  throughput/memory measurements remain missing despite lifecycle and virtualization coverage.
  Nine-camera demo generation already runs on main pushes; external-analysis isolation and real
  Home Assistant container checks are already enforced and passed in the inspected CI evidence.
- **Soak and clean-host recovery qualification:** No automated combined recording/export/cleanup/
  reconnect soak with memory-growth bounds was found. A sustained run and restore onto a clean
  host, followed by independent playback/export verification, remain open release-gate work.
  Existing atomic configuration restore/restart tests do not restore the media/catalog archive;
  resolve the release-contract discrepancy above before treating a configuration ZIP as that test.

### Empty book placeholders

These files are deliberately zero bytes and linked from `SUMMARY.md`. They mark missing work;
they are not finished documentation or evidence for closing the gate.

- `book/src/native-camera-events.md`
- `book/src/external-analysis.md`
- `book/src/upgrades-and-migrations.md`
- `book/src/recording-archive-recovery.md`
- `book/src/camera-controls.md`

### Reproducing the audit

Install UI dependencies with `bun install --no-save` from `ui/` and install Chromium with
`bunx playwright install chromium`. The Windows ACL reproducer requires a release executable at
`target/release/keeppeek.exe`, or pass its absolute path with `-KeepPeekBinary`.

Performance commands from `ui/`. The zoom and recording-link checks require the `keeppeek`
and `test_camera` release binaries; prepare them with `bun run test:e2e:prepare` first.

```powershell
bun scripts/benchmark-diagnostics-bundle.ts
bun run perf:timeline
bunx playwright test --config playwright.digital-zoom-performance.config.ts
bunx playwright test recording-links.e2e.ts -g 'copy has bounded latency' --workers=1
```

From `ui/`, `bunx playwright test --config playwright.alpha-audit.config.ts` runs the 39 opt-in
render cases. `bunx playwright test mobile-keep.e2e.ts` runs the default mobile regressions.
From the repository root,
`powershell -NoProfile -ExecutionPolicy Bypass -File tests/qa/windows-export-acl.ps1` checks CLI
export permissions. Rebuild first or pass a freshly built executable with `-KeepPeekBinary`;
the old audit binary retains the reproduced defect. These assertions are enforced through the
default E2E and Rust suites without skips or changed application limits.

Local ignored evidence is under `target/alpha-audit/`; commands and observed values above remain
in this tracked ledger so a clean checkout can reproduce findings without those local artifacts.
