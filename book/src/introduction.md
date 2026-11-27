# KeepPeek

This page introduces KeepPeek's scope, architecture principles, and chapter map. KeepPeek is in **Alpha qualification** and is not yet production-ready; active promotion evidence is tracked in [Alpha gate #145](https://github.com/xnorpx/keeppeek/issues/145).

KeepPeek is a local-first network video recorder (NVR) and WebRTC media gateway for IP cameras. It runs natively on Linux, macOS, and Windows and keeps camera media on hardware you control without requiring a vendor cloud relay or an AI runtime.

## Core capabilities

| Area                             | What KeepPeek provides                                                                                                                                                                                                                                                     |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Camera ingest and controls**   | Discover and connect Reolink (`reo-proto`), ONVIF/RTSP (Retina), Hikvision/Annke (SADP/ISAPI), and manual RTSP cameras. Supports advertised PTZ controls and Administrator browser **Talk** on cameras with a confirmed speaker.                                           |
| **Zero-re-encode MP4 recording** | Write camera-native H.264/H.265 video and supported audio directly to fragmented ISO MP4 segments across named storage volumes and an optional fourth-tier local archive.                                                                                                  |
| **Live and recorded WebRTC**     | Serve multi-camera live walls, kiosk display, focused single-camera viewing, UTC day timelines, synchronized multi-camera review, bounded timelapse review, bounded MP4 clip exports, and isolated guest review links (`/share`).                                          |
| **Events and analytics**         | Store camera-native events (Reolink, Hikvision/Annke ISAPI, ONVIF PullPoint, RTSP metadata) and external WebRTC analytics; search with boolean expressions and hybrid semantic ranking (`/events`) and inspect bounded temporal summaries (`/activity`).                   |
| **Alerts and integrations**      | Evaluate server-owned notification rules (browser alerts, Pushover, webhooks, and opt-in bounded Keeppeek Service mobile push signaling), forward normalized events over MQTT 5, expose Prometheus `GET /metrics`, and embed cameras directly via the Home Assistant card. |
| **Operations and health**        | Track authoritative camera, stream, storage, and server health across the web UI, foreground terminal dashboard (TUI), and metrics; manage state in `config.toml` and `secrets.toml` with validated ZIP export/apply, backups, and recording maintenance.                  |

## Video walkthrough

Watch nine real-time RTSP cameras configured on an empty KeepPeek server and streamed into the live wall with WebRTC health evidence; see [Demo videos](./demo-videos.md) for all narrated workflow walkthroughs.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/peek.desktop.live-wall.mp4">Download the nine-camera live-wall demo</a>.
</video>

The virtual-camera source is _Big Buck Bunny_, © 2008 Blender Foundation, [licensed under CC BY 3.0](https://creativecommons.org/licenses/by/3.0/).

## Architecture and development principles

- **Compact native server, browser-decoded UI:** The server is written in Rust for memory-safe, predictable concurrent media handling without bundling video codecs or a desktop browser runtime. The Svelte 5 web interface relies on the browser and OS media stack for decoding; an optional Tauri desktop app (`keeppeek-desktop`) wraps the same UI on Windows and macOS by connecting to an existing server.
- **Modular analytics and transcoding:** Recording and playback do not depend on object detection, external AI services, or transcoders. Optional inference services, transcoders, and automation bridges connect through the open MIT-licensed API while the recorder remains independent.
- **Human-directed, AI-assisted engineering:** KeepPeek uses AI tools for implementation, validation, documentation, and review; people remain accountable for architecture, security, and release decisions.

## Book map

| Section                          | Chapter                                                               | Use it for                                                                                                    |
| -------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| **Overview**                     | [Feature guide and navigation](./feature-guide.md)                    | Map every route, Settings section, workflow, and unavailable control to its owning page.                      |
| **Getting started**              | [Get started](./get-started.md)                                       | Install on Linux/Docker, macOS, or Windows; complete `/setup`; run the foreground TUI; add a first camera.    |
|                                  | [Desktop application](./desktop-app.md)                               | Install and connect `keeppeek-desktop` (Windows/macOS) to an existing KeepPeek server.                        |
|                                  | [Authentication and access control](./authentication.md)              | Configure local trust, named credentials, OIDC, roles, camera grants, and session revocation.                 |
|                                  | [Camera controls](./camera-controls.md)                               | Run discovery, catalog matching, stream verification, PTZ, browser **Talk**, and runtime profiles.            |
|                                  | [Demo videos](./demo-videos.md)                                       | Watch end-to-end workflows recorded against the live application.                                             |
| **Watching & investigating**     | [Live wall and kiosk display](./live-wall.md)                         | Manage saved dashboards, layout editing, audience sharing, kiosk mode, and browser decode limits.             |
|                                  | [Recording and evidence](./recording-and-evidence.md)                 | Investigate UTC timelines, synchronized review, timelapse, event search, `/activity`, bookmarks, and exports. |
|                                  | [Guest review links](./guest-review.md)                               | Issue, scope, open, and revoke time-bounded external clip review links (`/share`).                            |
|                                  | [Digital zoom and pan](./digital-zoom.md)                             | Use browser-side digital zoom and pan without moving physical PTZ hardware.                                   |
|                                  | [Camera and stream health](./camera-health.md)                        | Diagnose camera, stream, storage, and recorder states via web UI, TUI, and `/metrics`.                        |
| **Administration & recovery**    | [Visual configuration management](./configuration-management.md)      | Edit settings safely in `/settings`, distinguish save vs. apply vs. restart, and manage profiles.             |
|                                  | [Configuration reference](./configuration-reference.md)               | Look up every `config.toml` section, field, default, bound, ownership rule, and `{secret:KEY}` pattern.       |
|                                  | [Export and import configuration](./configuration-export-import.md)   | Export and atomically apply two-file configuration ZIP bundles (`config.toml` + `secrets.toml`).              |
|                                  | [Backup and restore](./backup-and-restore.md)                         | Create backups, plan restores, verify rollback, and check what survives a server restart.                     |
|                                  | [Upgrades and migrations](./upgrades-and-migrations.md)               | Perform manual platform upgrades, rollbacks, startup migrations, and storage relocation.                      |
|                                  | [Recording maintenance](./recording-maintenance.md)                   | Inspect integrity (`/recordings`) and confirm destructive maintenance (`/recordings/maintenance`).            |
|                                  | [Recording archive recovery](./recording-archive-recovery.md)         | Recover offline or incomplete recording archives, catalogs, and guest share metadata.                         |
| **Events & integrations**        | [Native camera events](./native-camera-events.md)                     | Configure Reolink, Hikvision/Annke ISAPI, ONVIF PullPoint, and RTSP metadata event ingestion.                 |
|                                  | [External analysis](./external-analysis.md)                           | Subscribe to WebRTC media and publish normalized detections from an external inference service.               |
|                                  | [Notifications and integrations](./notifications-and-integrations.md) | Configure rules, Pushover, webhooks, Keeppeek Service push signaling, and MQTT 5 forwarding.                  |
|                                  | [Home Assistant card](./home-assistant.md)                            | Embed direct browser WebRTC camera views in Home Assistant dashboards.                                        |
|                                  | [MCP server and LLM configuration](./mcp.md)                          | Connect AI assistants via `keeppeek mcp` stdio bridge or opt-in `POST /mcp` with explicit write approval.     |
|                                  | [Groups and two-way audio: current status](./groups.md)               | Distinguish camera access labels, dashboard audiences, shipped **Talk**, and unavailable live groups.         |
| **Understanding & contributing** | [How KeepPeek works](./how-it-works.md)                               | Trace runtime media/event paths, persistence boundaries, HTTP/WebRTC surfaces, and code entry points.         |
|                                  | [Users and design choices](./users-and-design-choices.md)             | Review target users, brand/protocol support policy, architecture trade-offs, and product non-goals.           |
|                                  | [Release readiness and known limitations](./release-readiness.md)     | Check Alpha qualification criteria, supply-chain policy, soak checks, and known limitations.                  |
|                                  | [Reporting bugs](./reporting-bugs.md)                                 | Capture logs (`/settings/logs`), diagnostics packages, bug-reporting grants, and issue evidence.              |
|                                  | [Open source and licensing](./open-source-and-licensing.md)           | Review third-party credits and the AGPL server vs. MIT API licensing split.                                   |
|                                  | [Contributing](./contributing.md)                                     | Follow repository checks, API contract rules, tooling policies, and pull request workflow.                    |
