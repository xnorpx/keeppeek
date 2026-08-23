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

## KP-UI-002 [P1] Keep indexed playback never reaches a playable frame

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
