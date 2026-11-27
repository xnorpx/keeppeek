# Camera controls

This chapter covers onboarding cameras (`/cameras`, `/cameras/new`), inspecting and editing camera details (`/camera`), operating PTZ and motion-detection switches, browser **Talk** and **Broadcast talkback**, and switching live streams or runtime profiles. Adding/editing cameras, toggling motion detection, using browser **Talk**, and managing runtime profiles require **Administrator**; **User** accounts can operate PTZ, recall presets, and play supported doorbell sounds on their granted cameras.

> **Evidence boundary:** A live video feed, manufacturer name, or capability badge does not guarantee a camera command will succeed. KeepPeek reports command errors separately from [camera and stream health](./camera-health.md).

## Add and manage cameras (`/cameras`, `/cameras/new`)

Open **Cameras** (`/cameras`) to search your fleet, filter **Not healthy** sources, select cameras for bulk **Configuration**, or launch **Add camera** (`/cameras/new`).

The `/cameras/new` wizard guides you through five steps:

1. **Find & connect:** Run subnet discovery (customizable `/24` prefixes, cancellable), enter a manual IPv4 or `rtsp://` address, or search the embedded **CCTV Database** catalog for model-specific credential-free RTSP hints (Reolink matches default to `reo-proto`, ONVIF port `8000`, HTTP port `80`).
2. **Connection options:** Enter per-camera credentials (or leave blank to inherit server defaults), optional ONVIF/HTTP ports, backend (`auto`, `retina`, `reo-proto`), and transport (`tcp`/`udp`). Unless you supplied a manual `rtsp://` URL, KeepPeek probes ONVIF across common ports automatically.
3. **Streams:** Review or edit **Main stream** (recording) and **Sub stream** (live) URLs, optionally **Restore catalog streams**, and run **Verify streams**. KeepPeek authenticates and checks streams in parallel; **every stream required by your recording policy must deliver decoded video and a keyframe (`VIDEO + KEYFRAME`) before saving**:
   - `event-boost` or `both`: both `main` and `sub` verified
   - `event-only`: selected event stream verified
   - `main` or `sub`: that stream verified
   - `off`: at least one stream verified
     Editing either RTSP URL invalidates prior verification.
4. **Recording:** Set the camera display name, recording policy (default `event-boost`), **Store generic motion events** (`false` by default), event duration (`1–3600` s, default `60` s), and optional pre-recording (`0–30` s) and event stream.
5. **Review & save:** Confirm settings and write the camera to `config.toml` (passwords remain write-only).

This walkthrough shows LAN discovery, embedded **CCTV Database** catalog lookup, and stream verification before the first configuration write:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.mp4">Download the catalog-guided camera setup demo</a>.
</video>

## Camera detail page (`/camera`)

Select any camera to open `/camera?camera=<id>`. Desktop navigation provides eight sections:

| Section           | Purpose                                                                                                                         |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| **Overview**      | Live preview (`sub` H.264 preferred) and PTZ control panel when supported and privacy is inactive                               |
| **Configuration** | **Edit settings** for credentials, ports, RTSP URLs, backend/transport, and recording policy                                    |
| **Privacy**       | **Edit schedule** for server-enforced privacy blocking across all live, recorded, and control paths                             |
| **Connection**    | Health state, hardware identity, **Manufacturer** override, **Test connection**, **Open camera UI**, and **CCTV Database** link |
| **Events**        | **Motion detection** status and **Enabled** switch (where controllable)                                                         |
| **Streams**       | **Configured media profiles** table and **Live stream health** metrics (auto-refreshes every 5 s)                               |
| **Audio**         | Discovered audio profiles plus recorded and live audio timing offset controls (with live preview)                               |
| **Advanced**      | **Reported capabilities** (`PTZ`, `Audio`, `Events`, `Recording`, `Analytics`, `Imaging`, `Two-way audio`)                      |

Use **Camera** (`/camera`) for per-camera edits and **Settings** (`/settings`) only for server-wide settings.

## PTZ movement and presets

On a PTZ-capable camera, the **Overview** panel exposes an 8-way directional pad, optical zoom (**Wide** / **Tele**), **Stop PTZ**, and device presets:

1. Verify the live image for the target camera.
2. Press and hold a direction or optical zoom button; release to send stop. Keyboard users can focus any movement button and hold `Space` or `Enter`.
3. Select **Stop PTZ** to halt movement explicitly.
4. Select a listed preset to recall that saved camera position.

**Ownership and vendor boundaries:**

- Continuous movement belongs to one controlling WebRTC session; transitions are serialized and concurrent commands are rejected while another is in flight.
- KeepPeek sends a stop on disconnect or uncertain movement acknowledgement. If stop cannot be confirmed, KeepPeek reports the error and retains ownership rather than assuming the camera is idle—inspect the camera directly and keep the camera's own movement timeout enabled.
- Control transport is independent of media backend: Hikvision/Annke uses ISAPI HTTP (signed per-axis speeds scaled to reported spaces), while Reolink uses Reolink HTTP on **channel 0** even when media uses `reo-proto` (directional movement + speed `1..64`; arbitrary NVR channel control is not supported). Speed scales differ across vendors, and missing axes or presets are never software-emulated.

For non-mechanical magnification on live views, recordings, or event images, use [Digital zoom and pan](./digital-zoom.md).

## Motion detection switch

On `/camera`, **Motion detection → Enabled** modifies the camera's hardware detector configuration (not whether motion is happening right now). KeepPeek reads the full alarm configuration, toggles only the enabled field while preserving masks and sensitivity, writes it back, and verifies via read-back. A failed write or mismatched read-back reports an error; KeepPeek never reboots a camera automatically.

| Setting                         | Scope and effect                                                                      |
| ------------------------------- | ------------------------------------------------------------------------------------- |
| **Motion detection → Enabled**  | Toggles the camera's on-device motion detector (Administrator only).                  |
| **Store generic motion events** | Controls whether KeepPeek persists unclassified motion events in storage.             |
| **Camera recording policy**     | Selects recorded streams (`event-boost`, `main`, `sub`, `both`, `event-only`, `off`). |

Disabling generic motion retention does not disable the detector or native event subscriptions that smart person/vehicle classifications rely on. Configure regions, schedules, sensitivity, and vendor filters in the camera's own UI. See [Native camera events](./native-camera-events.md) and [Recording and evidence](./recording-and-evidence.md).

## Two-way audio (Talk) and doorbell sounds

Listening to camera audio is independent of transmitting microphone audio to a camera speaker. **Talk** and **Broadcast talkback** require the **Administrator** role and a speaker confirmed by KeepPeek's metadata probe:

| Vendor / backend       | Confirmation requirement for browser **Talk**                                                                                                                                                                                                                                   | Transport & codec sent                                             |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| **Reolink**            | Powered, direct camera on `reo-proto` TCP (not battery/UDP); channel grants CGI talk permission AND read-only Baichuan probe confirms compatible mono ADPCM speaker profile at 8 or 16 kHz. Firmware answering only while a speaker stream is open leaves **Talk** unavailable. | Native Baichuan IMA ADPCM (8/16 kHz mono)                          |
| **Hikvision / Annke**  | ISAPI audio discovery reports an explicit enabled speaker channel (a microphone channel alone never enables **Talk**).                                                                                                                                                          | Raw ISAPI 8 kHz mono G.711 A-law or mu-law (bounded 300 s session) |
| **Other ONVIF / RTSP** | Browser **Talk** is unavailable unless a supported speaker adapter confirms a speaker channel.                                                                                                                                                                                  | Not supported                                                      |

- **Single-camera Talk:** In the focused live view, select the microphone (**Talkback**) button and grant browser microphone permission. Select it again (**Stop talkback**) or close the page to release the microphone. Incoming camera audio stays active while you talk.
- **Broadcast talkback:** On the Dashboard (`/`), Administrators with confirmed speaker cameras can pick **Target** (**All cameras** or a camera group) and select **Broadcast talkback** / **Stop broadcast** (one active talkback owner at a time, up to 128 targets). Losing speaker evidence, replacing credentials, or removing a camera automatically retires its active talkback route.
- **Quick replies and chimes:** Cameras reporting doorbell sounds show a `♫` (**Play doorbell sound**) menu in live view. Both Administrators and permitted Users can trigger quick replies and chimes (disabled while Talk is active).

## Video walkthrough

This walkthrough demonstrates continuous PTZ movement, **Stop PTZ**, preset recall, the **Motion detection → Enabled** switch distinction, and the browser **Talk** boundary on `/camera`:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.mp4">Download the camera details and PTZ demo</a>.
</video>

## Live stream switching and runtime profiles

- **Per-viewer stream switching:** In focused live view (`/` or `/viewer`), choose **Auto** (network-adaptive), **High**, **Low**, **Main**, or **Sub**. If a browser cannot decode the requested stream's codec, KeepPeek falls back to a compatible stream and shows a notice.
- **Server-wide runtime profiles:** Administrators manage up to 32 named profiles in **Settings → Runtime profiles** (`/settings#runtime-profiles`), overriding up to 127 cameras each across four levers: `availability`, `recording_mode`, `privacy_enforced`, and `notifications_suppressed`. Profile switches apply asynchronously with per-camera `pending`, `applied`, or `degraded` status; see [Runtime profiles](./configuration-reference.md#runtime-profiles).

## Unavailable controls

Relative PTZ movement, preset creation/deletion, focus, guard position, sensitivity, detection regions, schedules, and vendor smart-rule editing are not shared KeepPeek browser controls. Manage them in the camera's native UI (**Open camera UI** on `/camera`).

## Application and test coverage

KeepPeek's camera control stack is verified with synthetic integration and browser tests rather than physical-device certification across every model and firmware:

- **Hikvision/Annke (`test-hikvision`):** Covers HTTP Digest, motion config read-back, capability updates, PTZ session ownership and original-target disconnect cleanup, ISAPI audio busy/open/close, raw G.711 speaker writes, full-duplex progress, cancellation, expiry, and malformed responses.
- **Reolink:** Covers CGI talk permission + read-only Baichuan speaker-profile verification, talkback routing/retirement on config change or removal, idle activity vs enabled detector state, and channel-0 PTZ behavior.
- **Browser E2E:** Verifies Administrator-only Talk/Broadcast gating and User quick-reply/chime permissions in Peek.

Real-world speaker volume, latency, half- vs full-duplex behavior, and firmware quirks still vary by physical camera model, firmware version, account permissions, and network topology—test directly on your hardware before relying on controls. See the [camera control reference](https://github.com/xnorpx/keeppeek/blob/main/docs/camera-controls.md) for protocol-level vendor mappings.

## Troubleshooting

| Symptom                                      | What to check                                                                                                                                                            |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Controls are missing or disabled             | Role requirement (Administrator vs User), camera grant, active privacy schedule, confirmed capability evidence, and WebRTC connection state.                             |
| **Verify streams** fails in `/cameras/new`   | Camera credentials, ONVIF/RTSP port, TCP vs UDP transport, and whether the stream emits a valid video keyframe.                                                          |
| PTZ works in vendor app only                 | Configured HTTP port, device account permissions, channel mapping (Reolink HTTP targets channel 0), and supported adapter; RTSP alone does not verify HTTP controls.     |
| **Talk** button does not appear              | Requires Administrator role plus confirmed speaker evidence (Reolink powered TCP + CGI permission + 8/16 kHz ADPCM profile; Hikvision explicit enabled speaker channel). |
| Motion is enabled but no events appear       | Camera detector schedule, regions, notification linkage, `record_generic_motion_events`, and native event status.                                                        |
| Capability badge shows yes but command fails | Device reachability and the returned command error; cached discovery evidence does not guarantee current reachability.                                                   |
| Zoom only crops the picture                  | You are using [Digital zoom](./digital-zoom.md); optical zoom is in the PTZ panel.                                                                                       |
