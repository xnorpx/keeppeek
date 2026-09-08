# Digital zoom and pan

Digital zoom enlarges the media already decoded by your browser. It is available in the focused
Peek Viewer, Keep recorded playback, and event detail images. It does not move the camera, request
a different stream, or increase the source resolution.

Compact Dashboard tiles and the camera filmstrip do not capture inspection gestures. Open a camera
in Viewer before using digital zoom.

## Inspect a frame

The **Digital** controls show the current magnification. The minimum is **1.0x**, which fits the
complete image, and the maximum is **8.0x**. The image keeps its original aspect ratio. Panning is
bounded to the fitted image area, so a drag cannot move the image out of view.

| Input                             | Result                                                                        |
| --------------------------------- | ----------------------------------------------------------------------------- |
| Zoom-in icon                      | Double the magnification, up to 8.0x.                                         |
| Zoom-out icon                     | Halve the magnification, down to 1.0x.                                        |
| Reset icon                        | Return to the complete, centered frame.                                       |
| Two-finger pinch inside the image | Zoom around the midpoint between the fingers; moving that midpoint also pans. |
| Mouse, pen, or one-finger drag    | Pan while magnification is above 1.0x.                                        |
| Alt + wheel or trackpad scroll    | Zoom around the pointer. Use Option on macOS keyboards.                       |
| Double-click or double-tap        | Switch between 1.0x and 2.0x around the pointer or tap.                       |
| `+` or `=`                        | Zoom in when the inspection surface has focus.                                |
| `-`                               | Zoom out when the inspection surface has focus.                               |
| `0`                               | Reset when the inspection surface has focus.                                  |
| Arrow keys                        | Pan while zoomed and focused.                                                 |
| Escape                            | Reset digital zoom before the normal Viewer or event-detail exit action.      |

Tab reaches the named zoom buttons. Activating a zoom button focuses the inspection surface, where
the keyboard commands work. The buttons have at least 44-pixel targets, visible focus indicators,
and disabled states at the limits. The zoom value is also available to assistive technology.

Ordinary wheel scrolling does not change magnification. Control/Command zoom shortcuts still
belong to the browser. Touch gestures are captured only inside the focused image; scrolling and
browser gestures remain available elsewhere on the page. Zoom and reset do not require animated
transitions.

## Keep playback controls

Keep's play/pause, skip, position, volume, mute, speed, and fullscreen controls remain outside the
transformed video. Timeline scrubbing still selects recording time, not an image crop. Arrow keys
pan when the zoomed image has focus; Keep's normal frame-stepping shortcuts remain available at
full-frame view or outside the inspection surface.

The fullscreen button expands the whole recorded player, including its controls. Zoom bounds
update when entering or leaving fullscreen. Browser-native Escape handling can leave fullscreen
before the page receives the key. A disabled fullscreen button indicates that the browser does not
offer the Fullscreen API in that context; digital zoom remains available without fullscreen.

## Reset and evidence

Changing the camera, recording, or canonical event image resets digital zoom. Play/pause and
ordinary progress within the same selected media preserve the inspection position. Resizing the
window, rotating the device, or changing decoded dimensions recomputes the bounds. Reset always
returns to the complete frame at the current size.

Event bounding boxes use the canonical image's coordinate space and move with that image. Player
controls, camera information, and diagnostics do not scale. Live camera information reports digital
magnification separately from decoded resolution.

The inspection crop exists only in the current browser view. It is not saved into configuration,
recording metadata, bookmarks, copied moment links, or exported evidence. Exports still contain the
original recorded frame. See [Recording and evidence](./recording-and-evidence.md).

## Digital zoom is not PTZ

Digital zoom is labelled **Digital** and works for any media you can already view, including
recordings and offline-camera event images. It does not require PTZ permission or a PTZ-capable
camera. Physical pan, tilt, and optical zoom remain separate camera commands with their existing
capability and permission checks.

Magnifying a low-resolution stream does not reveal detail that the camera did not send. Choose a
higher available stream quality separately when you need more decoded pixels.
