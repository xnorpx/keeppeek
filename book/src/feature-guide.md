# Feature guide and navigation

Use this chapter to find the part of KeepPeek that owns a task. It describes the current
application, including controls that are visible but unavailable. A design reference or generated
API type does not, by itself, mean that a feature works in the running server.

KeepPeek remains in [Alpha qualification](./release-readiness.md). Availability also depends on
your role, camera grants, device capabilities, recording coverage, and browser codec support.

## Find the right screen

The desktop shell and mobile navigation lead to the same workflows. On a phone, **More** opens
Settings; Administrator-only navigation is hidden for Users.

| Screen                      | Route                              | Use it for                                                                                                                                                     | Read next                                                        |
| --------------------------- | ---------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| Dashboard, also called Peek | `/`                                | Watch the fleet, select a saved dashboard, arrange cameras, and adjust wall display settings.                                                                  | [Live wall](./live-wall.md)                                      |
| Viewer                      | `/viewer`                          | Focus on a camera, switch feeds, listen to supported audio, and enter History.                                                                                 | [Live wall](./live-wall.md), [digital zoom](./digital-zoom.md)   |
| Keep                        | `/keep`                            | Investigate a recorded day, seek to an exact moment, inspect gaps and events, and create evidence exports.                                                     | [Recording and evidence](./recording-and-evidence.md)            |
| Events                      | `/events`                          | Search stored events by time, camera, type, source, zone, confidence, image availability, and text. Review or dismiss events and open their recording context. | [Recording and evidence](./recording-and-evidence.md)            |
| Cameras                     | `/cameras`                         | Inspect the inventory and open camera configuration. Start discovery or manual setup at `/cameras/new`.                                                        | [Camera controls](./camera-controls.md)                          |
| Camera details              | `/camera`                          | Inspect and edit the selected camera and use the device controls it advertises.                                                                                | [Camera controls](./camera-controls.md)                          |
| System health               | `/system-health`                   | Diagnose recorder, storage, camera, stream, and integration health. Open an individual camera for detailed evidence.                                           | [Camera and stream health](./camera-health.md)                   |
| Camera health               | `/system-health/camera/[cameraId]` | Inspect lifecycle, stream progress, and recording evidence for one camera.                                                                                     | [Camera and stream health](./camera-health.md)                   |
| Settings                    | `/settings`                        | Manage server configuration, dashboards, storage, backups, access, events, and integrations.                                                                   | [Visual configuration management](./configuration-management.md) |
| Logs                        | `/settings/logs`                   | Inspect live server and browser logs and collect diagnostics.                                                                                                  | [Reporting bugs](./reporting-bugs.md)                            |
| Recording integrity         | `/recordings`                      | Inspect recording coverage, gaps, and integrity before opening maintenance.                                                                                    | [Recording maintenance](./recording-maintenance.md)              |
| Recording maintenance       | `/recordings/maintenance`          | Inspect recording integrity and explicitly plan and confirm maintenance.                                                                                       | [Recording maintenance](./recording-maintenance.md)              |
| Initial setup               | `/setup`                           | Complete the first-run setup workflow.                                                                                                                         | [Get started](./get-started.md)                                  |

Camera and event links carry the selected identity and, when appropriate, a timestamp. Keep those query parameters when sharing a
deep link. A link does not grant its recipient permission to view the camera.

## From live view to evidence

1. Select a camera in Dashboard or Viewer and inspect whether its live stream is advancing.
2. Use **History** to enter Keep, or open an event's recording context from Events.
3. Check the selected camera, date, and UTC time. A gap means recorded coverage is absent for that
   interval; a loaded player or thumbnail is not proof of continuous recording.
4. Seek, play, and zoom to investigate. Digital zoom changes the browser view; PTZ moves a physical
   camera and has separate capability and permission requirements.
5. Review the event, add the supported bookmark or evidence selection, and export the needed
   interval. Check the export result and retained coverage before relying on the file.

For controls, limits, time handling, and failure states, use
[Recording and evidence](./recording-and-evidence.md). For missing footage, start with
[camera health](./camera-health.md), then use [maintenance](./recording-maintenance.md) or
[archive recovery](./recording-archive-recovery.md) when the evidence points to stored data.

## Find a setting

Settings presents sections on desktop and a focused section index on mobile. Saving a draft,
applying a change to a running camera, and restarting the server are distinct actions; inspect the
reported result rather than assuming that a successful file write proves activation.

| Settings area       | What it controls                                                                                                                                         | Documentation                                                                                                |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Dashboards          | Saved layouts, camera placement, audience, and wall presentation/resource choices.                                                                       | [Live wall](./live-wall.md)                                                                                  |
| Storage & retention | Recording paths, retention, pressure thresholds, policies, and maintenance.                                                                              | [Configuration reference](./configuration-reference.md), [recording maintenance](./recording-maintenance.md) |
| Backup & restore    | Backup lifecycle, uploads, restore planning, activation, and rollback evidence.                                                                          | [Backup and restore](./backup-and-restore.md)                                                                |
| Event sources       | Read-only catalog and origin context. The live source registry is unavailable; configure native adapters through the camera/configuration workflows.     | [Native events](./native-camera-events.md), [external analysis](./external-analysis.md)                      |
| Groups              | The declared group model and its current unavailable runtime controls.                                                                                   | [Groups: current status](./groups.md)                                                                        |
| Notifications       | Rules, delivery destinations, history, and errors.                                                                                                       | [Notifications and integrations](./notifications-and-integrations.md)                                        |
| Access              | Named credentials, Administrator/User roles, camera grants, and session revocation.                                                                      | [Authentication](./authentication.md)                                                                        |
| Integrations        | MQTT configuration and connection evidence, alongside reference cards whose configuration is unavailable. Home Assistant setup is documented separately. | [Notifications and integrations](./notifications-and-integrations.md), [Home Assistant](./home-assistant.md) |
| Appearance & time   | Dark, Light, or Match system appearance and the browser's time and reduced-motion context.                                                               | [Visual configuration management](./configuration-management.md)                                             |
| System & updates    | Server version and health context and the explicit restart action. Follow the documented upgrade procedure to replace the server.                        | [Upgrades](./upgrades-and-migrations.md)                                                                     |
| Logs & diagnostics  | Live logs and downloadable diagnostic evidence.                                                                                                          | [Reporting bugs](./reporting-bugs.md)                                                                        |

## Set appearance and inspect system context

For appearance, open **Settings > Appearance & time** and select **Dark**, **Light**, or
**Match system**. The choice applies immediately and is saved in this browser when local storage
is available; it does not change another browser or the server configuration. Video surfaces stay
dark. Without a saved preference, the interface starts in Dark mode.

The browser's reported time zone and reduced-motion preference are context, not server settings.
Server time zone, clock format, week-start preferences, and an alternate interface language are
not configurable through this panel. Keep's UTC selection and export timestamps retain their
documented meaning. **System & updates** shows server evidence and supports restart; an automatic
update check or update-channel selector is not available. Use the [upgrade procedure](./upgrades-and-migrations.md).

## Choose an event or integration path

- **Record without analytics:** No detector or notification service is required for recording and
  playback.
- **Use camera analytics:** Enable a supported native adapter and verify real events and its
  connection health. Camera support differs by model and firmware.
- **Use external analysis:** Run a separate service that subscribes to media and publishes events
  through the API. KeepPeek does not install or manage detector models in its core.
- **Send alerts or automate:** Match committed events with notification rules, use Pushover where
  configured, or forward normalized events through MQTT. A rule match and successful delivery are
  different states. The notification rule's Forwarder action is currently unavailable; the MQTT
  integration is configured separately.
- **Embed a camera in Home Assistant:** Use the direct browser card with an appropriate named
  credential and browser-to-recorder connectivity.

The integration chapters explain their requirements and failure behavior. Protocol scenarios that
describe future client possibilities are not an installation checklist for a shipped service.

## Understand unavailable controls

Read the reason shown by an unavailable control. Do not interpret an unavailable count as zero,
a saved configuration as a healthy connection, or advertised camera support as a successful
operation. Browser Talk and live participant groups remain unavailable.

Camera access labels, dashboard audiences, and planned live participant groups are different
concepts. [Groups](./groups.md) explains that distinction. The
[release-readiness chapter](./release-readiness.md) records remaining qualification boundaries;
[reporting bugs](./reporting-bugs.md) explains how to capture a reproducible failure safely.
