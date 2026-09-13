# Camera controls

As an Administrator, open **Cameras**, select a camera, and use its overview and control panels to inspect capabilities
and operate supported devices. A live picture, a manufacturer name, or a capability badge alone
does not prove that a camera command will succeed. KeepPeek reports command failures separately
from [camera and stream health](./camera-health.md).

The Cameras navigation and configuration editors are Administrator workflows. The API also allows
authorized Users to read motion-detection status and issue supported PTZ commands for their granted
cameras. Changing detector configuration requires Administrator access. Camera grants and advertised
capabilities still apply to each operation. See
[Authentication and access](./authentication.md) for the trusted-local policy and camera grants.

## Move a PTZ camera

On a supported camera, the PTZ panel provides directional movement, optical zoom, **Stop PTZ**, and
existing presets.

1. Check the live image and select the intended camera.
2. Press and hold a direction or optical zoom control. Release it to request a stop. Keyboard users
   can focus a movement button and hold Space or Enter.
3. Use **Stop PTZ** to stop movement explicitly.
4. Select an existing preset to recall that device position. Preset creation and deletion are not
   available through the shared KeepPeek controls; manage them in the camera's own interface.

Continuous movement belongs to the controlling WebRTC connection. KeepPeek requests a stop on
disconnect and after an uncertain movement acknowledgement. If a stop cannot be confirmed, it
reports the failure and retains ownership instead of treating the camera as safely idle. Check
the camera directly before another operation. Keep the device's own movement timeout enabled.

Hikvision/Annke controls use ISAPI HTTP. The current Reolink control adapter uses Reolink HTTP even
when the recording backend is `reo-proto`. Their movement speed scales differ. Reolink HTTP controls
currently address channel 0; do not assume arbitrary NVR channel control is supported. A missing
PTZ axis or unavailable preset is not supplied by software emulation.

For magnifying a picture without moving the device, use [Digital zoom and pan](./digital-zoom.md).
Digital zoom also works on recordings and event images.

## Enable or disable motion detection

The camera detail page's **Motion detection > Enabled** switch changes the camera's detector
configuration. It is independent of whether motion is occurring at this instant.

KeepPeek reads the current alarm configuration, changes the enabled field, and reads it back.
Unrelated settings such as masks and sensitivity remain intact. Wait for the result before
making another change. An error or mismatched read-back is a failed operation, and KeepPeek does
not automatically reboot the camera to apply it.

Three settings have different purposes:

| Setting                     | Effect                                                                  |
| --------------------------- | ----------------------------------------------------------------------- |
| Motion detection enabled    | Turns the device's detector on or off where supported.                  |
| Store generic motion events | Controls whether KeepPeek stores unclassified motion notifications.     |
| Camera recording policy     | Chooses which stream KeepPeek records and whether events boost quality. |

Turning off generic motion retention does not turn off the detector or native subscriptions.
Smart person or vehicle classifications may still depend on those subscriptions. Configure
detector regions, schedules and vendor-specific target filters in the camera interface. See
[Native camera events](./native-camera-events.md) and
[Recording and evidence](./recording-and-evidence.md).

## Audio and unsupported controls

Listening to an available camera audio stream is separate from sending microphone audio back to
its speaker. **Talk is not implemented in the browser application**, including on devices that
advertise a speaker or two-way-audio capability. KeepPeek's reusable vendor libraries contain
audio transport support, but this does not provide an application talk session.

Relative PTZ movement, preset save/delete and vendor-specific focus, guard, sensitivity or smart
rule editing are not shared browser controls. Disabled controls describe unavailable application
operations; they are not a reason to grant broader access or enable an unsupported protocol.

## Troubleshooting

| Symptom                                    | What to check                                                                                                                               |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| Controls are disabled                      | The operation's role requirement, camera grant, capability evidence and current connection state.                                           |
| PTZ works in the vendor app only           | The configured HTTP port, device credentials, selected channel and supported KeepPeek adapter. RTSP playback alone does not validate these. |
| Motion is enabled but no events appear     | Camera schedule, regions, notification linkage and generic-motion retention; then inspect native event status.                              |
| A capability appears but its command fails | Reachability and the command's error. Cached discovery evidence is not a current successful command.                                        |
| Zoom changes only the displayed crop       | You are using Digital zoom. Optical zoom is a separate PTZ operation.                                                                       |

The [camera control reference](https://github.com/xnorpx/keeppeek/blob/main/docs/camera-controls.md)
details vendor differences and synthetic test coverage. Physical support still depends on the
camera model, firmware, account permissions and channel mapping.
