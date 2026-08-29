# KeepPeek

> **Status:** KeepPeek has completed its proof-of-concept gate and is undergoing MVP
> qualification. It is not yet production-ready.

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

> **Small on purpose:** The current release binary is under 40 MB. KeepPeek aims to remain a compact
> native service, not a two-gigabyte spaghetti-script bundle held together by Python and JavaScript
> runtimes. Those ecosystems are welcome in optional services where they fit; the core should not
> make every installation carry them.

## Reading the book

- [Users and design choices](./users-and-design-choices.md) explains who KeepPeek is for and why its
  product, protocol, platform, licensing, and integration boundaries exist.
- [Reporting bugs](./reporting-bugs.md) describes how to file a reproducible defect and where to
  take setup questions or feature proposals.
- [Get started](./get-started.md) covers installation, persistent data, secrets, and the first
  camera.
- [Camera and stream health](./camera-health.md) defines the authoritative health model and evidence
  used across the server, API, metrics, and interface.
- [Recording and evidence](./recording-and-evidence.md) explains recording policies, coverage,
  event presentation, and durable evidence exports.
- [Notifications and integrations](./notifications-and-integrations.md) covers server-owned rules,
  Pushover delivery, MQTT 5 forwarding, retries, and failure isolation.
- [Release readiness and known limitations](./release-readiness.md) separates automated checks from
  the physical mixed-fleet and soak evidence required for MVP promotion.
- [Demo videos](./demo-videos.md) shows complete workflows against the real application.
- [Open source and licensing](./open-source-and-licensing.md) credits the projects KeepPeek builds
  on and explains the AGPL server and MIT API split.
- [Contributing](./contributing.md) explains scope review, interoperability contributions,
  AI-assisted work, pull requests, and validation.
