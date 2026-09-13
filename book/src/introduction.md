# KeepPeek

> **Status:** KeepPeek is in Alpha qualification and is not yet production-ready.
> The active [Alpha gate](https://github.com/xnorpx/keeppeek/issues/145) tracks promotion evidence.

KeepPeek is a local-first network video recorder and WebRTC media gateway for IP cameras. It runs
on Linux, macOS, and Windows and keeps camera media on hardware controlled by the user without
requiring a vendor cloud relay.

The core service has a deliberately focused job:

- discover and connect supported cameras;
- ingest and record their encoded media without re-encoding it;
- store recordings as standard MP4 files;
- provide live and recorded media through WebRTC;
- keep camera, stream, recording, and server health observable;
- accept and store events from supported cameras or independent services;
- locate, review, and export bounded evidence clips;
- deliver rule-driven notifications and forward normalized events to MQTT.

KeepPeek does not require object detection or another AI service to record and review video.
Camera-native analytics can publish events directly, while optional inference, transcoding, home
automation, and commercial services connect through the open API. Recording remains useful when
those services are absent or unavailable.

The server is written in Rust for predictable, memory-safe concurrent media handling. The
first-party interface uses Svelte and runs in the browser, relying on the browser and operating
system for compatible video decoding. KeepPeek remains codec-aware without bundling a video codec
pack into the core service.

KeepPeek is developed with AI assistance through a human-directed process. AI is used as a tool for
implementation, validation, documentation, and review; design decisions and accountability remain
with people.

KeepPeek aims to remain a compact native service. Optional services own their detector runtimes,
models, and codec dependencies, so an installation can choose the extra work it needs.

## Reading the book

- [Feature guide and navigation](./feature-guide.md) maps the current screens and settings to
  everyday tasks and distinguishes implemented features from unavailable controls.
- [How KeepPeek works](./how-it-works.md) connects media, recording, events, persistent state,
  clients, and integrations in one overview.
- [Users and design choices](./users-and-design-choices.md) explains who KeepPeek is for and why its
  product, protocol, platform, licensing, and integration boundaries exist.
- [Reporting bugs](./reporting-bugs.md) describes how to file a reproducible defect and where to
  take setup questions or feature proposals.
- [Get started](./get-started.md) covers installation, persistent data, secrets, and the first
  camera.
- [Authentication](./authentication.md) explains roles, credentials, and camera access;
  [Camera controls](./camera-controls.md) covers discovery, setup, editing, and device operations.
- [Live wall](./live-wall.md) covers saved dashboards, layout sharing, kiosk display, and browser
  resource choices. [Digital zoom](./digital-zoom.md) explains local inspection controls.
- [Camera and stream health](./camera-health.md) defines the authoritative health model and evidence
  used across the server, API, metrics, and interface.
- [Recording and evidence](./recording-and-evidence.md) explains recording policies, coverage,
  event presentation, and durable evidence exports.
- [Notifications and integrations](./notifications-and-integrations.md) covers server-owned rules,
  Pushover delivery, MQTT 5 forwarding, retries, and failure isolation.
- [Native camera events](./native-camera-events.md), [External analysis](./external-analysis.md),
  and [Home Assistant](./home-assistant.md) explain the independent integration paths.
- [Visual configuration management](./configuration-management.md) and the
  [configuration reference](./configuration-reference.md) cover supported settings and safe edits.
- [Backup and restore](./backup-and-restore.md), [Upgrades](./upgrades-and-migrations.md),
  [Recording maintenance](./recording-maintenance.md), and
  [Archive recovery](./recording-archive-recovery.md) cover ongoing operation and recovery.
- [Release readiness and known limitations](./release-readiness.md) separates automated checks from
  the physical mixed-fleet, recovery, and soak evidence required for Alpha promotion.
- [Demo videos](./demo-videos.md) shows complete workflows against the real application.
- [Open source and licensing](./open-source-and-licensing.md) credits the projects KeepPeek builds
  on and explains the AGPL server and MIT API split.
- [Contributing](./contributing.md) explains scope review, interoperability contributions,
  AI-assisted work, pull requests, and validation.
