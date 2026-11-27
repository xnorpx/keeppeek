# Demo videos

Audio-paced H.264/AAC walkthroughs covering KeepPeek's real-server workflows and interactive browser
controls. Each video is regenerated from canonical scenarios with English WebVTT captions and
embedded directly in its matching book chapter.

These are recorded demonstrations, not a live preview of the current build. Check the
[published manifest](https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/manifest.json)
for the source commit, generation timestamp, content hashes, and scenario metadata for every asset.

## Video index

| Video                                                                                                                     | Chapter                                                                                              | Recorder              | Asset ID                               |
| ------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | --------------------- | -------------------------------------- |
| [Complete first-run setup](#complete-first-run-setup)                                                                     | [Get started](./get-started.md)                                                                      | Storybook interactive | `setup.desktop.first-run`              |
| [Add a verified camera](#add-a-verified-camera)                                                                           | [Get started](./get-started.md)                                                                      | Real server + RTSP    | `cameras.desktop.add-wizard`           |
| [Set up a camera with catalog context](#set-up-a-camera-with-catalog-context)                                             | [Camera controls](./camera-controls.md)                                                              | Storybook interactive | `cameras.desktop.catalog-guided-setup` |
| [Manage credentials, camera grants, and sessions](#manage-credentials-camera-grants-and-sessions)                         | [Authentication](./authentication.md)                                                                | Storybook interactive | `settings.desktop.access`              |
| [Operate PTZ, motion detection, and Talk](#operate-ptz-motion-detection-and-talk)                                         | [Camera controls](./camera-controls.md)                                                              | Storybook interactive | `camera.desktop.details-ptz`           |
| [Add nine cameras and watch the live wall](#add-nine-cameras-and-watch-the-live-wall)                                     | [Introduction](./introduction.md), [Live wall](./live-wall.md)                                       | Real server + 9 RTSP  | `peek.desktop.live-wall`               |
| [Edit dashboard layouts, wall display, and audience scope](#edit-dashboard-layouts-wall-display-and-audience-scope)       | [Live wall](./live-wall.md)                                                                          | Storybook interactive | `peek.desktop.layout-editor`           |
| [Review what just happened](#review-what-just-happened)                                                                   | [Feature guide](./feature-guide.md)                                                                  | Storybook interactive | `peek.desktop.history-keep`            |
| [Inspect details with digital zoom and pan](#inspect-details-with-digital-zoom-and-pan)                                   | [Digital zoom and pan](./digital-zoom.md)                                                            | Storybook interactive | `viewer.desktop.digital-zoom`          |
| [Review recorded footage and export evidence in Keep](#review-recorded-footage-and-export-evidence-in-keep)               | [Recording and evidence](./recording-and-evidence.md)                                                | Storybook interactive | `keep.desktop.timeline-export`         |
| [Filter events, inspect detail evidence, and review Activity](#filter-events-inspect-detail-evidence-and-review-activity) | [Recording and evidence](./recording-and-evidence.md), [Native events](./native-camera-events.md)    | Storybook interactive | `events.desktop.search-review`         |
| [Configure, issue, preview, and revoke a guest review link](#configure-issue-preview-and-revoke-a-guest-review-link)      | [Guest review links](./guest-review.md)                                                              | Storybook interactive | `share.desktop.guest-review`           |
| [Triage fleet health and diagnose a camera](#triage-fleet-health-and-diagnose-a-camera)                                   | [Camera and stream health](./camera-health.md)                                                       | Storybook interactive | `health.desktop.triage-diagnosis`      |
| [Manage camera templates and bulk fleet changes](#manage-camera-templates-and-bulk-fleet-changes)                         | [Visual configuration management](./configuration-management.md)                                     | Storybook interactive | `cameras.desktop.fleet-config`         |
| [Export, stage, and validate a configuration ZIP](#export-stage-and-validate-a-configuration-zip)                         | [Export and import](./configuration-export-import.md), [Backup and restore](./backup-and-restore.md) | Storybook interactive | `settings.desktop.config-zip`          |
| [Preview recording deletion and reconcile catalog drift](#preview-recording-deletion-and-reconcile-catalog-drift)         | [Recording maintenance](./recording-maintenance.md)                                                  | Storybook interactive | `recordings.desktop.maintenance`       |
| [Configure notification rules, mobile push, and MQTT 5](#configure-notification-rules-mobile-push-and-mqtt-5)             | [Notifications and integrations](./notifications-and-integrations.md)                                | Storybook interactive | `settings.desktop.notifications-mqtt`  |
| [Configure the direct Home Assistant Lovelace card](#configure-the-direct-home-assistant-lovelace-card)                   | [Home Assistant card](./home-assistant.md)                                                           | Storybook interactive | `home-assistant.desktop.card`          |
| [Inspect appearance, live logs, diagnostics, and bug reports](#inspect-appearance-live-logs-diagnostics-and-bug-reports)  | [Reporting bugs](./reporting-bugs.md)                                                                | Storybook interactive | `settings.desktop.logs-diagnostics`    |

Pure reference, policy, architecture, CLI-only, and native-installer chapters (`desktop-app.md`,
`configuration-reference.md`, `upgrades-and-migrations.md`, `recording-archive-recovery.md`,
`external-analysis.md`, `mcp.md`, `groups.md`, `how-it-works.md`, `users-and-design-choices.md`,
`release-readiness.md`, `open-source-and-licensing.md`, and `contributing.md`) intentionally omit
embedded UI walkthroughs.

## Complete first-run setup

Verify storage write checks, inspect local timezone context, claim the one-time initial remote
Administrator key, and enter the empty-fleet ready state at `/setup`.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.mp4">Download the first-run setup demo</a>.
</video>

## Add a verified camera

Add and verify a deterministic H.264 RTSP camera through the real KeepPeek server and production
`/cameras/new` wizard, confirming video and keyframe evidence before the first save.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.mp4">Download the add-camera wizard demo</a>.
</video>

## Set up a camera with catalog context

Discover a model, review embedded CCTV Database catalog facts, open the outbound catalog link for
deeper research, search a quiet camera manually, and review editable stream suggestions before the
first configuration write.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.catalog-guided-setup.mp4">Download the catalog-guided camera setup demo</a>.
</video>

## Manage credentials, camera grants, and sessions

Create a named User credential in **Settings > Access**, scope its access to selected groups and
cameras, and revoke an active session.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.access.mp4">Download the access control demo</a>.
</video>

## Operate PTZ, motion detection, and Talk

Drive continuous PTZ with explicit **Stop PTZ**, recall a saved preset, toggle camera motion
detection with verified read-back, and inspect the Administrator **Talk** speaker boundary on
`/camera`.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/camera.desktop.details-ptz.mp4">Download the camera controls demo</a>.
</video>

## Add nine cameras and watch the live wall

Start from an empty KeepPeek server, configure nine independently paced RTSP sources, watch all
nine feeds join the shared WebRTC live wall, and inspect one stream's diagnostics.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4">Download the nine-camera live-wall demo</a>.
</video>

The virtual-camera source is _Big Buck Bunny_, © 2008 Blender Foundation,
[licensed under CC BY 3.0](https://creativecommons.org/licenses/by/3.0/).

## Edit dashboard layouts, wall display, and audience scope

Apply a 12-column dashboard preset, pin a primary tile, place an unplaced camera, adjust wall
display settings, and inspect dashboard audience scope.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.layout-editor.mp4">Download the live-wall layout editor demo</a>.
</video>

## Review what just happened

Focus a live camera on Peek, then use **History** to open its timeline in Keep.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.history-keep.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.history-keep.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.history-keep.mp4">Download the Peek history demo</a>.
</video>

## Inspect details with digital zoom and pan

Zoom a live or recorded viewport from `1.0x` to `4.0x`, pan within picture bounds by drag and
keyboard, and reset to `1.0x` without moving physical PTZ hardware.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/viewer.desktop.digital-zoom.mp4">Download the digital zoom and pan demo</a>.
</video>

## Review recorded footage and export evidence in Keep

Inspect recorded spans and gaps on the UTC timeline, browse **Stories & Calendar** and
**Swimlanes**, create an evidence export with burn-in timestamps and SHA-256 verification, and
inspect bounded **Timelapse** review and silent MP4 export.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.mp4">Download the Keep timeline and export demo</a>.
</video>

## Filter events, inspect detail evidence, and review Activity

Build a structured boolean filter in **Events**, inspect a matched card's canonical thumbnail,
bounding box, and bookmark actions, then open **Activity** to inspect bucket charts, weekday×hour
heatmaps, and CSV/JSON export.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4">Download the events search and activity demo</a>.
</video>

## Configure, issue, preview, and revoke a guest review link

Enable canonical HTTPS guest sharing policy, issue a one-time bounded link from a ready export,
preview the isolated `/share` guest surface, and revoke the grant with an audit reason.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.mp4">Download the guest review link demo</a>.
</video>

## Triage fleet health and diagnose a camera

Inspect fleet verdicts and evidence dimensions on `/system-health`, drill into an offline camera,
and apply **Switch to TCP** on a UDP-degraded camera.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/health.desktop.triage-diagnosis.mp4">Download the camera health diagnosis demo</a>.
</video>

## Manage camera templates and bulk fleet changes

Inspect inherited versus overridden camera fields on `/cameras`, review templates preserving
`{secret:...}` references, preview a server-owned bulk diff, and apply it with per-camera results.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.mp4">Download the fleet configuration demo</a>.
</video>

## Export, stage, and validate a configuration ZIP

Export the two-file `config.toml` + `secrets.toml` ZIP archive, stage a verified upload pending
explicit server restart, and observe rejection handling for an invalid archive.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.mp4">Download the configuration ZIP demo</a>.
</video>

## Preview recording deletion and reconcile catalog drift

Preview bounded whole-recording deletion with typed `DELETE N` confirmation and durable ledger
tracking, then inspect and reconcile catalog drift on `/recordings/maintenance`.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.mp4">Download the recording maintenance demo</a>.
</video>

## Configure notification rules, mobile push, and MQTT 5

Inspect notification rules, **Inbox**, and **History**, test bounded **Keeppeek Service** mobile
push signaling, and inspect and test the **MQTT** 5 forwarder.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.notifications-mqtt.mp4">Download the notifications and MQTT demo</a>.
</video>

## Configure the direct Home Assistant Lovelace card

Load camera sources in the Home Assistant visual card editor, switch grid columns and single-camera
layout, and focus and unfocus a live tile.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/home-assistant.desktop.card.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/home-assistant.desktop.card.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/home-assistant.desktop.card.mp4">Download the Home Assistant card demo</a>.
</video>

## Inspect appearance, live logs, diagnostics, and bug reports

Switch browser-local appearance themes, filter and pause live logs, download diagnostics, and walk
through the redacted Administrator **Report a bug** review flow.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.logs-diagnostics.mp4">Download the logs and bug reporting demo</a>.
</video>
