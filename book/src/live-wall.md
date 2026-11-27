# Live wall and kiosk display

This page covers multi-camera live viewing on Dashboard (`/`), single-camera viewing on Viewer (`/viewer`), saved dashboard management and layout editing, wall display/resource controls, Screen Wake Lock kiosk behavior, and how runtime profiles affect live viewing. Display and streaming settings govern browser presentation only; they never alter camera recording, detection, or exported media.

## Dashboard (`/`) and Viewer (`/viewer`)

KeepPeek separates multi-camera monitoring from focused single-camera inspection:

| Surface       | Route      | Purpose                                                                                                                                    |
| ------------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| **Dashboard** | `/`        | Multi-camera live wall with dashboard switcher, **Wall display settings**, and bounded browser admission.                                  |
| **Viewer**    | `/viewer`  | Full-shell single-camera view with a `PEEK / camera` overlay, camera selector, filmstrip, stream quality controls, and local digital zoom. |
| **Keep**      | `/keep`    | Recorded-day timeline, event context, and evidence exports. See [Recording and evidence](./recording-and-evidence.md).                     |
| **Cameras**   | `/cameras` | Device settings, physical PTZ, detector configuration, and browser Talk. See [Camera controls](./camera-controls.md).                      |

Legacy links to `/?camera=<id>` redirect to `/viewer?camera=<id>`. Viewer remembers the last selected camera on that device and falls back to the first authorized camera. Live and History are viewing modes; pausing or closing a browser tab never stops server recording.

When no cameras are available, Administrators see **No cameras yet** with **Add camera** (opens the camera wizard), while Users see **No cameras available** and a prompt to ask an administrator for access.

Select the information control on any tile or Viewer bar to inspect transport, decoded video, and recording evidence without mistaking a paused browser tile for a server outage.

### Live wall walkthrough

Watch nine real-time RTSP cameras populate Dashboard (`/`) and inspect live WebRTC feed evidence:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4">Download the nine-camera live-wall demo</a>.
</video>

The virtual-camera source is _Big Buck Bunny_, © 2008 Blender Foundation,
[licensed under CC BY 3.0](https://creativecommons.org/licenses/by/3.0/).

## Saved dashboards, layout editor, and audience sharing

Use the dashboard selector in the upper-left of `/` to switch layouts. **All cameras** is the automatic fleet layout whose grid updates whenever cameras change; its tile placement cannot be rearranged, but Administrators can edit its display settings.

Administrators manage custom dashboards in **Settings → Dashboards** using **New dashboard**, **Rename dashboard**, **Manage access**, **Duplicate dashboard**, **Delete dashboard**, **Export**, and **Import**:

1. Create or duplicate a custom dashboard, then open the layout editor.
2. Add available cameras to the 12×12 grid, select a tile, and move or resize it with pointer controls or arrow keys. Toggle **Activity focus** if desired.
3. Select **Save** to publish the layout, or **Discard** / **Cancel** to keep the saved revision intact.

**Audience vs. camera grants:** A dashboard's audience controls who can see that layout (everyone or selected named Users; Administrators always retain access), not which cameras they may view. The server filters every viewer's tiles to the cameras granted to that identity, so a shared dashboard can contain cameras for one User and appear empty for another. Never broaden camera grants merely to match another person's wall layout. See [Authentication and access control](./authentication.md).

- **Offline and removed cameras:** An offline configured camera keeps its normal tile; a removed camera shows a labelled placeholder until removed or remapped.
- **Optimistic revisions:** Every save sends the current registry revision. Stale saves are rejected instead of overwriting a newer revision while keeping your unsaved draft; reload the saved layout before retrying.
- **Layout JSON export/import:** Carries dashboard definitions, display settings, and explicit audience credential IDs—never access keys, camera credentials, or media. Import validates the full document before mutation, requires mapping or omitting missing camera IDs, and assigns imported dashboards Administrator-only access until explicitly shared. See [Export and import configuration](./configuration-export-import.md).

### Layout editor and wall display walkthrough

Watch arranging a custom dashboard layout, adjusting **Wall display settings**, and assigning a dashboard audience:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.mp4">Download the layout editor and wall display demo</a>.
</video>

## Wall display settings and persistence

Open **Wall display settings** (sliders icon, upper-right of `/`) to adjust presentation and browser streaming policy for the active dashboard. Changes are persisted per dashboard in `[peek_layouts]` in `config.toml` under its versioned `display` object (`version = 1`), following duplication, layout export/import, configuration export/import, and restart. See [Peek layout registry](./configuration-reference.md#peek-layout-registry).

| Setting                | Default                                   | Supported values             | Runtime effect                                                             |
| ---------------------- | ----------------------------------------- | ---------------------------- | -------------------------------------------------------------------------- |
| **Tile shape**         | `16:9`                                    | `16:9`, `4:3`, `Native`      | Sets tile presentation ratio; `Native` latches first valid decoded ratio.  |
| **Media fit**          | `Contain`                                 | `Contain`, `Cover (cropped)` | `Contain` preserves full frame; `Cover` fills slot with **Cropped** badge. |
| **Gap**                | `10 px`                                   | Integer `0`–`24` px          | Spacing between grid tiles; changing gap can alter visible tile count.     |
| **Corner radius**      | `10 px`                                   | Integer `0`–`24` px          | `0 px` removes normal tile borders; health warnings keep an inset outline. |
| **Streaming mode**     | `Smart`                                   | `Smart`, `Continuous`        | Chooses priority/prefetch admission vs. all visible tiles.                 |
| **Stream limit**       | `12` (capped by browser decoder estimate) | Integer `1`–`12`             | Upper bound on concurrent browser-admitted live streams.                   |
| **Keep display awake** | Off                                       | Opt-in switch                | Requests browser Screen Wake Lock while `/` is active and visible.         |

Administrators preview edits live, then select **Save** (or **Discard changes** to reload server values). A failed or conflicting save keeps the preview intact; discarding after an error reloads the latest server revision. **Reset wall settings** previews defaults and releases any active wake lock until saved. Users without edit permission see read-only controls.

Browser-local wall preferences are neither read nor written. Older clients that omit `display` preserve existing server values; invalid versions, fields, types, or ranges are rejected before configuration mutates.

### Appearance presets, tile shape, and media fit

| Preset       |   Gap | Corner radius |
| ------------ | ----: | ------------: |
| **Current**  | 10 px |         10 px |
| **Hairline** |  2 px |          0 px |
| **Flush**    |  0 px |          0 px |

Presets update only gap and corner radius without changing shape, fit, streaming mode, or wake lock.

- **Native tile shape:** Accepts decoded aspect ratios from `1:16` through `16:1`, reserving `16:9` until valid dimensions arrive (invalid or missing metadata keeps `16:9`). The latched ratio stays stable across later Main/Sub switches during that wall mount and never repacks neighboring grid slots. Labels, health indicators, and diagnostics stay outside the resized media frame, including for portrait and panoramic cameras. On narrow screens, tile slots keep at least `160 px` height while scrolling inside the wall viewport.
- **Contain vs. Cover (cropped):** Shape, fit, and radius changes never recreate WebRTC sessions, restart recording, or crop Viewer/exports. **Contain** may show letterboxing; **Cover (cropped)** fills the frame without stretching pixels and displays a **Cropped** label. Selecting any tile opens `/viewer`, which always shows uncropped video.

## Streaming modes and browser resource limits

All wall tiles and warm background streams share one browser admission scheduler and one WebRTC peer connection.

| Policy dimension   | **Smart** (default)                                                               | **Continuous**                                                           |
| ------------------ | --------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| **Target tiles**   | Focused tile, visible tiles, nearby offscreen prefetch, audio/visibility priority | Every visible tile independently of motion/events; no offscreen prefetch |
| **Release grace**  | `1 s` grace after leaving viewport                                                | Immediate release when offscreen                                         |
| **Preview stream** | Lowest browser-compatible quality rank (Viewer keeps its own quality selection)   | Lowest browser-compatible quality rank                                   |
| **Hidden tab**     | Suspends all media subscriptions (wake lock does not override suspension)         | Suspends all media subscriptions                                         |

**Effective decoder budget:** KeepPeek estimates browser decoder capacity as `floor(hardwareConcurrency / 2)` clamped to `4`–`12` decoders (`4` if hardware concurrency is unavailable). The effective ceiling is `min(decoderEstimate, streamLimit)`. **Wall display settings** displays requested streams/decoders, effective budget, active count, and any over-budget count before you enable **Continuous**. Lower **Stream limit** if the viewing device cannot decode smoothly; a codec unsupported by the browser is never requested.

- **Admission pacing:** Admits up to `3` cameras per batch with a `40 ms` delay while additional cameras are queued, releasing replaced subscriptions before requesting replacements.
- **Server authorization and eviction errors:** The server authorizes every subscription under its negotiated transport and resource limits (the capability snapshot does not expose a numeric server quota). A server-refused tile shows **Server did not admit this stream** while other admitted streams continue. If a subscription eviction fails, KeepPeek stops the local live session and reports an unavailable state rather than retaining unconfirmed media work—lower **Stream limit**, inspect diagnostics, and retry.

## Viewer stream switching and live video quality

In `/viewer`, select **Auto**, **High**, **Low**, **Main**, or **Sub** for the focused camera. When a requested stream uses a codec the browser cannot decode, or live playback fails to start, Viewer falls back to a compatible stream and shows an explicit notice rather than silently hiding the mismatch.

- **Auto quality:** Uses native RTP bandwidth feedback on the shared WebRTC connection, starting with Sub when available. KeepPeek reserves bandwidth for audio and manually selected video streams, then splits the remaining estimate across Auto video subscriptions—adding cameras can lower each Auto camera's budget.
- **Keyframe-gated transitions:** Auto waits for sustained headroom before upgrading and sustained pressure before downgrading. The current picture stays visible until the replacement supplies a compatible keyframe. An alternative missing codec configuration needed to verify compatibility (such as an unverified H.264 profile) is reported as unavailable rather than replacing a working stream.
- **Manual selections:** **High** and **Low** keep the requested quality when compatible and available (Auto never overrides manual selections), even if the choice exceeds current network capacity. Fixed reliable-data delivery does not use RTP Auto adaptation.
- **Recorded playback independence:** Stored playback does not use this live controller, and a matching recorded time range is not guaranteed to carry identical captured frames.

Open the live video information panel to compare runtime metrics:

| Diagnostic field             | Meaning                                                                                        |
| ---------------------------- | ---------------------------------------------------------------------------------------------- |
| **Requested quality**        | Operator's chosen quality preference (`Auto`, `High`, `Low`, `Main`, `Sub`).                   |
| **Active stream**            | Server's currently active stream source (does not prove browser decoded a fresh frame).        |
| **Pending stream**           | Replacement stream waiting for a compatible keyframe gate before handoff.                      |
| **Auto status**              | Controller state: holds, missing network evidence, single variant, or unavailable alternative. |
| **Session network estimate** | Shared WebRTC connection bandwidth estimate (unknown when unavailable).                        |
| **Camera budget**            | Adaptive Auto camera's allocation (`0` differs from unknown or unsupported older server).      |
| **Receive bitrate**          | Media bitrate arriving in this browser.                                                        |

Reconnecting or removing a subscription clears prior diagnostic values.

## Frame freshness vs. server health

Server-reported camera health and browser tile admission are independent facts: a healthy, recording camera can still display a paused tile when the browser hits its decoder budget.

| Tile indicator / state | Meaning                                                                                                          |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Admission state badges | Identifies `queued` (waiting for admission), over device budget, offscreen, hidden, unsupported, or unavailable. |
| Frame age              | Elapsed time since the current viewer observed the retained frame.                                               |
| **Frame time unknown** | Cached frame lacks an observed capture timestamp.                                                                |
| **No frame received**  | No frame has been observed to display yet.                                                                       |

None of these browser states proves the camera stopped recording. Use camera diagnostics or [Camera and stream health](./camera-health.md) for authoritative server evidence.

## Runtime profiles and live privacy

Administrators can switch between **Base** and named profiles in **Settings → Runtime profiles** to apply per-camera privacy, availability, recording, and notification overrides. See [Runtime profiles](./configuration-reference.md#runtime-profiles).

When an active profile enables camera privacy, the server's privacy gate immediately blocks live media for that camera. Viewers carrying an affected stream may briefly disconnect; KeepPeek automatically attempts up to `3` reconnects to restore remaining authorized streams (an access denial ends retries). Any interrupted browser talkback, chime, or quick reply is not automatically resumed—restart it only if intended. Changing a runtime profile never alters saved dashboard definitions.

## Kiosk display and Screen Wake Lock

Enable **Keep display awake** in **Wall display settings** and save it on the dashboard used by a wall monitor or kiosk. KeepPeek uses the browser [Screen Wake Lock API](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Wake_Lock_API), which requires a secure context (`HTTPS` or `localhost`) plus browser, embedding-policy, and OS permission.

- **Persisted intent vs. runtime state:** Only the enabled preference is saved on the server; lock handles, permissions, visibility, and decoder estimates are local runtime facts.
- **Activation and lifecycle:** A saved enabled preference waits for a trusted user interaction before its first request. Only one request can be in flight at a time, and requests with no response within `10 s` are reported as denied. Switching to a dashboard with wake lock off, hiding the tab, leaving `/`, or closing the view releases the lock (including any late-resolving request). Returning to a visible tab may reacquire it if permitted.

| Wake lock status                              | Action / behavior                                                                        |
| --------------------------------------------- | ---------------------------------------------------------------------------------------- |
| **Off**                                       | Wake lock disabled for this dashboard.                                                   |
| **Waiting for interaction**                   | Saved on; click, tap, or press a key on `/` to issue the browser request.                |
| **Requesting** / **Active**                   | Lock request in progress or currently holding the display awake.                         |
| **Released**                                  | Released on tab hide, navigation, or OS/battery-saver policy (not aggressively retried). |
| **Unsupported in this browser or connection** | Non-HTTPS origin or browser lacks Wake Lock API.                                         |
| **Denied by browser or device**               | Permission denied or `10 s` timeout elapsed; toggle off and on to retry.                 |
| **Release failed**                            | Close the Dashboard tab to release the browser wake-lock resource.                       |

Screen Wake Lock is not native OS kiosk management and cannot guarantee that OS power policies or battery-saving modes will never sleep the display.
