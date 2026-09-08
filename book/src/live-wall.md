# Live wall and kiosk display

The Dashboard is KeepPeek's multi-camera live wall. Open **Wall display settings** with the
sliders icon in the upper-right corner to change its presentation and resource policy. These
choices belong to the selected dashboard on the server, not to camera recording or detection.

## Defaults and ownership

| Setting            | Default                                     | Supported choices               |
| ------------------ | ------------------------------------------- | ------------------------------- |
| Tile shape         | 16:9                                        | 16:9, 4:3, Native               |
| Media fit          | Contain                                     | Contain, Cover (cropped)        |
| Gap                | 10 px                                       | Whole pixels from 0 through 24  |
| Corner radius      | 10 px                                       | Whole pixels from 0 through 24  |
| Streaming mode     | Smart                                       | Smart, Continuous               |
| Stream limit       | 12, further limited by the browser estimate | Whole numbers from 1 through 12 |
| Keep display awake | Off                                         | Opt-in Screen Wake Lock         |

Administrators can preview changes in the current wall and select **Save** to store them in
the selected dashboard. **Discard changes** restores the saved values. A rejected or conflicting
save leaves the preview intact; discarding after an error reloads the server's latest values.
Users without dashboard-edit permission see read-only controls.

**Reset wall settings** previews the defaults and releases any requested wake lock. Select Save
to make the reset permanent. Shape, fit, and radius changes do not recreate the WebRTC session,
restart recording, or alter exported media. Gap changes can naturally change which tiles are visible.

The server stores each dashboard's versioned `display` object under `[peek_layouts]` in the existing
`config.toml`. Settings follow the dashboard through duplication, layout export/import, configuration
export/import, and restart. A kiosk and a phone can use different dashboards with different settings;
loading the same dashboard uses the same saved choices, subject to each browser's runtime limits.

Older dashboards without a display object use defaults. Writes from older clients that omit the
object preserve the stored settings. Invalid versions, fields, types, and values are rejected before
the configuration changes. Browser-local wall preferences are no longer read or written and are not
automatically uploaded. See the [layout registry reference](./configuration-reference.md#peek-layout-registry).

## Gap, corners, and presets

The appearance controls combine live sliders with exact pixel inputs:

| Preset   |   Gap | Corner radius |
| -------- | ----: | ------------: |
| Current  | 10 px |         10 px |
| Hairline |  2 px |          0 px |
| Flush    |  0 px |          0 px |

Adjust either value to create a custom appearance. The preset indicator follows the actual values;
presets change only gap and radius, leaving shape, fit, streaming, and wake-lock settings intact.
Zero radius gives square corners and removes the normal video-tile border. Health warnings retain
an inset outline. Zero gap joins neighboring tile slots; Contain can still add letterboxing when
source and tile ratios differ. No appearance setting stretches pixels or forces Cover fit.

The automatic All cameras dashboard retains its generated camera grid, but its display settings
are editable. Other dashboards retain their own independent display settings.

## Tile shape and media fit

**16:9** and **4:3** select the wall's presentation ratio. **Native** uses the first valid decoded
dimensions for each camera, with 16:9 reserved until dimensions arrive. Native ratios from 1:16
through 16:1 are accepted. Invalid or missing dimensions retain the reserved frame.

Media frames fit inside stable grid slots. Native metadata can change the frame inside its slot,
but it does not repack neighboring tiles. A later main/sub quality switch retains the camera's
latched presentation ratio for that mounted wall. Labels, health evidence, and diagnostics remain
outside the resized media frame, including for portrait and panoramic cameras.

The wall reserves a compact toolbar strip above the camera grid. On narrow screens, tile slots
retain at least 160 pixels of height for labels and status, while media keeps the selected ratio.
The wall scrolls inside its viewport without extending under the surrounding navigation.

- **Contain** preserves the complete source image. Black bars are expected when the source and
  presentation ratios differ.
- **Cover (cropped)** fills the presentation frame without stretching. A **Cropped** label
  identifies the omitted edges. Select Contain to reverse the crop.
- Selecting a tile opens the focused Viewer, which always offers uncropped video independently
  of the wall's fit choice.

These settings do not add per-camera crop positions or alter the saved grid's tile coordinates.

## Smart and Continuous streaming

**Smart** prioritizes the focused camera, visible tiles, nearby offscreen tiles, and the existing
audio/visibility signals. It permits bounded prefetch and a one-second release grace period for
a tile that leaves the viewport.

**Continuous** attempts to keep every visible tile subscribed, independently of motion and
events. It does not prefetch offscreen tiles. Both modes prefer the lowest browser-compatible
quality rank for wall previews and preserve the focused Viewer's separate quality selection.
When ranks are unavailable, the existing resolution, bitrate, frame-rate, and stream fallback
ordering applies. A codec rejected by the browser is not requested.

The settings panel shows requested streams and decoders, the effective budget, the active count,
and any excess demand before Continuous is selected. The initial browser estimate is half of
reported hardware concurrency, rounded down and bounded to 4-12 decoders. Missing hardware
information uses four. The effective ceiling is the smaller of that estimate and **Stream limit**.
The estimate is not a guarantee that every codec or camera will decode smoothly; lower the limit
when this device cannot sustain the workload.

Admission uses batches of up to three cameras, with a 40 ms delay while more admissions are queued.
Focused and warm background
streams use the same ceiling. Existing subscriptions are released before their replacements are
requested. The server still authorizes every subscription and enforces its negotiated transport
and resource limits; the current capability snapshot does not advertise a numeric server quota.

A server-refused stream shows **Server did not admit this stream**, while other admitted streams
continue. A failed eviction stops the local live session and reports an unavailable state instead
of retaining unconfirmed media work. Lower the stream limit, inspect camera diagnostics, and retry
the live view when the server or network is available.

Both modes suspend media subscriptions when the document becomes hidden. Wake lock does not
override suspension. Returning to the visible wall resumes admission within the same budget.
None of these actions stops camera ingest, recording, detection, or retention on the server.

## Frame freshness and health

A camera's server-reported health and a browser tile's live admission are separate facts. A
healthy camera can have a paused tile when the browser has exhausted its budget.

Paused tiles identify whether they are waiting for admission, over the device budget, offscreen,
hidden, unsupported by the browser, or unavailable. A retained frame shows its age when the current
viewer observed it. A cached frame without a capture timestamp says **Frame time unknown**.
**No frame received** means there is no observed frame to present. A paused frame is not evidence
that the camera itself stopped recording.

Use the camera-information control for transport, decoded video, and recording evidence. See
[camera and stream health](./camera-health.md) for the authoritative health meanings.

## Keep a display awake

Enable **Keep display awake** after interacting with the Dashboard, then save it for that dashboard.
The setting uses the browser's
[Screen Wake Lock API](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Wake_Lock_API).
Support requires a secure context, normally HTTPS or localhost, and permission from the browser,
embedding policy, and device.

A saved enabled preference waits for a user interaction before its first request. Only one request
is outstanding at a time. The status reports waiting, requesting, active, released, unsupported,
or denied. A request that receives no response within ten seconds is also reported as denied.

Only the intent is stored on the server. The active wake-lock handle, browser permission, current
visibility, and measured decoder capacity are runtime facts, not persisted settings. Switching to
a dashboard with wake lock disabled releases the previous dashboard's lock.

The lock is released when the setting is turned off, the document is hidden, the user leaves the
Dashboard, or the view is destroyed. A late request result is released too. After a visibility
return, KeepPeek may reacquire the lock if it is still enabled and the browser permits it.
Rapid toggles and visibility changes reconcile to the latest choice after release completes.

The browser or operating system can release or deny a lock, including in battery-saving mode.
KeepPeek does not repeatedly prompt or immediately reacquire a system-released lock. To try again
after denial, turn the setting off and on. If **Release failed** appears, close the Dashboard tab
to end its use of the browser resource. Wake lock is not native operating-system kiosk management
and does not guarantee that a device can never sleep.
