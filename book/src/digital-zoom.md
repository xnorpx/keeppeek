# Digital zoom and pan

This page covers browser-side digital zoom and pan (`1.0x`–`8.0x`) on already-decoded media in Viewer (`/viewer`), Keep recorded playback (`/keep`), and Events detail images (`/events`). Digital zoom enlarges local pixels only—it never moves a camera, switches streams, changes source resolution, or modifies recordings and exports.

## Video walkthrough

Watch `1.0x`–`8.0x` browser-side **Digital** zoom, bounded drag and keyboard panning, and instant reset—independent of physical PTZ:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.mp4">Download the digital zoom and pan demo</a>.
</video>

## Supported surfaces

| Surface                | Route     | Digital zoom supported | Notes                                                                   |
| ---------------------- | --------- | ---------------------- | ----------------------------------------------------------------------- |
| **Viewer**             | `/viewer` | Yes                    | Shares one top control row with Live/History, quality, and camera info. |
| **Keep player**        | `/keep`   | Yes                    | Playback controls, timeline, and fullscreen chrome stay outside zoom.   |
| **Event detail image** | `/events` | Yes                    | Event bounding boxes scale and pan with the canonical image.            |
| **Dashboard tiles**    | `/`       | No                     | Compact wall tiles do not capture inspection gestures; open `/viewer`.  |
| **Camera filmstrip**   | `/viewer` | No                     | Select a camera into the focused viewport first.                        |

## Controls, gestures, and shortcuts

The **Digital** toolbar sits at the top-left above the media frame without obstructing the image (scrolling horizontally in Viewer rather than wrapping). Magnification spans `1.0x` (full fitted frame) through `8.0x`, preserves aspect ratio, and clamps panning to the fitted image rectangle so the media cannot be dragged out of view.

| Input                                 | Behavior                                                                        |
| ------------------------------------- | ------------------------------------------------------------------------------- |
| **Digital zoom in** button            | Doubles magnification around center, up to `8.0x`.                              |
| **Digital zoom out** button           | Halves magnification around center, down to `1.0x`.                             |
| **Reset digital zoom** button         | Returns immediately to `1.0x`, centered.                                        |
| Two-finger pinch inside image         | Zooms around finger midpoint; moving the midpoint pans simultaneously.          |
| Mouse, pen, or one-finger drag        | Pans while magnification is above `1.0x`.                                       |
| `Alt` + wheel/trackpad (`Option` Mac) | Zooms around pointer position.                                                  |
| Double-click or double-tap            | Toggles between `1.0x` and `2.0x` around pointer or tap point.                  |
| `+` or `=`                            | Zooms in when the inspection viewport has focus.                                |
| `-`                                   | Zooms out when the inspection viewport has focus.                               |
| `0`                                   | Resets to `1.0x` when focused.                                                  |
| Arrow keys                            | Pans by `40 px` per press while zoomed (`> 1.0x`) and focused.                  |
| `Escape`                              | Resets digital zoom first, before normal Viewer or event-detail close behavior. |

**Browser and accessibility behavior:**

- Ordinary wheel scrolling and `Ctrl`/`Cmd` zoom shortcuts remain browser-owned; touch gestures are captured only inside the focused image viewport.
- `Tab` reaches the named **Digital** buttons; activating any button focuses the inspection surface for keyboard controls.
- Peek desktop control groups are `32 px` high; touch targets are at least `44 px`. Buttons expose visible focus rings and disabled states at `1.0x` / `8.0x`, and magnification is announced via `role="meter"`. Zoom and reset do not depend on animated transitions.

## Keep playback and fullscreen interaction

- **Unscaled transport chrome:** Keep's play/pause, skip, scrubber, volume, mute, speed, and fullscreen controls stay outside the transformed video layer. Scrubbing the timeline always changes recording time, never the crop.
- **Arrow-key precedence:** When the zoomed image (`> 1.0x`) has focus, arrow keys pan. At `1.0x`, or when focus is outside the viewport, pause the recording and use `Left`/`Right` to step recorded frames (or approximate frame intervals on older recorders/unsupported browsers).
- **Portrait phone layout:** The **Digital** bar stays above the image while **Playback options** opens volume, speed, and stream quality in a separate sheet without replacing the recording or moving the playhead. Changing stream quality requests different media; **Digital** enlarges only already-decoded pixels.
- **Fullscreen:** Expands the full recorded player including controls and recomputes pan/zoom bounds on entry and exit. Browser-native `Escape` can exit fullscreen before the page sees the key. A disabled fullscreen button means the Fullscreen API is unavailable in that context; digital zoom still works.

## Reset rules and evidence boundaries

| Trigger                                              | Effect on digital zoom                                          |
| ---------------------------------------------------- | --------------------------------------------------------------- |
| Switch camera, recording segment, or canonical image | Resets to `1.0x`, centered.                                     |
| Play, pause, or normal playback progress             | Preserves current magnification and pan offset.                 |
| Window resize, device rotation, decoded size change  | Recomputes pan bounds for the current viewport size.            |
| Copy moment link, create bookmark, export MP4        | Inspection crop is never saved; exports contain original media. |

Event bounding boxes use the canonical image's coordinate space and move with it. Controls, camera info, and diagnostics never scale, and live camera information reports digital magnification separately from decoded resolution.

## Digital zoom vs. physical PTZ

Digital zoom is always labelled **Digital**, requires no PTZ permission or PTZ hardware, and works on recordings and offline-camera event snapshots. Physical pan, tilt, and optical zoom remain separate camera commands subject to device capability and access grants. See [Camera controls](./camera-controls.md).

Magnifying a low-resolution stream cannot recover detail absent from the encoded video—switch to a higher stream quality when you need more decoded pixels.
