# KeepPeek Release QA Bugs

Tested 2026-08-23 against the local `v0.1.0` release build on macOS 26.6.1 with
nine real cameras. Browser checks used Chromium at the authored Paper viewports of
1440 x 900 and 390 x 844. Visual expectations came from `KeepPeek - NVR Design
System & Spec`, especially Boards 06, 10, 11, 22, 30, 31, and 34.

Severity:

- **P0**: Release blocker. The core workflow is effectively unusable or exhausts the client.
- **P1**: Major. A primary workflow fails or reports dangerously incorrect state.
- **P2**: Moderate. The workflow works with a material UX, accessibility, or reliability defect.
- **P3**: Minor. Incorrect or confusing presentation with a viable workaround.

## KP-UI-001 [P0] Events requests an entire day of attachments and freezes the browser

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

Primary replay is now deferred ahead of timeline and filmstrip work, prefetch is clamped to the
selected day, and default overscan is one hour instead of twelve. The final real-camera run kept
continuous replay at source rate, but the six-hour view still requested 405 minutes and produced
119-157 ms long tasks. This issue remains open for narrower viewport queries and incremental
thumbnail work.

## KP-UI-013 [P2] Keep revokes blob URLs while previews are still loading

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
