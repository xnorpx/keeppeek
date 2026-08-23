# Product

<!-- impeccable:product-schema 1 -->

> Captured by `/impeccable init` on 2026-08-18. The structured question tool returned "user is
> not available to respond" on both attempts even while the owner was present, so the interview
> was conducted in plain chat instead. Facts from the repository and from the owner's direct
> answers are unlabelled. Facts I reasoned to rather than read are marked `[inferred]`. Questions
> still unanswered are recorded under Open Decisions instead of being invented.

## Platform

web

## Users

Two audiences, weighted equally (`features.md`, Target users):

- **Home and prosumer self-hosters.** Run KeepPeek on their own hardware, often alongside Home
  Assistant. Administer and watch the same system; there is no separate operations team.
- **Small-business operators.** Shop, workshop, yard, or office. Somebody on site reviews footage
  after an incident; somebody else may have installed it.

The jobs that matter, in the order they occur:

1. Get a mixed bag of IP cameras ingesting reliably, including vendor-specific ones.
2. Confirm the recorder is actually recording — continuously, with no silent gaps.
3. Find the moment something happened, fast, without scrubbing.
4. Get that moment out as a file somebody else can open.
5. Find out why something broke.

In both audiences the reviewer is frequently also the administrator. A browser on the recorder
or on the same LAN is Administrator without signing in. Remote access is the only path that
asks for a key. The interface cannot assume a separate privileged operator exists to fix
configuration problems, and it cannot assume an administrator is a specialist.

## Product Purpose

KeepPeek is a local-first network video recorder and media gateway for IP cameras, written in
Rust and running as a native service on Linux, macOS, and Windows. It records RTSP camera
streams to disk and exposes controlled live and recorded media sessions over WebRTC.

Recording, playback, review, and administration all work with no vendor cloud. Cloud services may
be optional extensions; they are never dependencies for core NVR operation.

Success is one unbroken workflow: add a camera, see live video, record and retain it reliably,
identify and locate an event, play it promptly, export evidence, alert someone, and diagnose
failures. `features.md` treats that chain as the definition of a complete product, and any scope
change is re-checked against it.

## Positioning

- **Local-first is architectural, not a marketing line.** Media stays on the LAN. There is no
  vendor relay and no mandatory port forwarding; remote access is the user's own VPN or reverse
  proxy. A neighbouring product that phones home cannot truthfully claim this.
- **A media gateway, not just a recorder.** External services connect over the same WebRTC
  control channel to subscribe to media and events _and to publish back_ — transcoded variants,
  detection events with attachments, participant audio. Capability discovery, variant lineage,
  and recording intent are part of the wire contract.
- **KeepPeek deliberately does not own detection.** It ingests events from cameras and from
  external detection services. This is the boundary that keeps the recorder dependable and lets
  detection be swapped, upgraded, or run on separate hardware.
- **Open protocols are a requirement.** ONVIF and RTSP ingest, documented and versioned
  integration APIs, and stable identifiers are architectural commitments, not roadmap items.
- **Blue Iris is the capability benchmark, not the template.** Preserve the flexibility that keeps
  its users loyal; fix the reliability, review speed, portability, and integration problems that
  drive them away. Legacy Windows-only paths, proprietary containers, and vendor relays are
  explicitly rejected rather than reproduced.

## Operating Context

- Runs on hardware the user owns, on their own network. Native service on all three desktop
  operating systems; an OCI container supplements rather than replaces native packages.
- Reached through a browser, locally or remotely via user-managed VPN or reverse proxy.
- Cameras are a mixed fleet by assumption, not by exception: Reolink over the proprietary
  Baichuan protocol, ONVIF devices, and generic RTSP URLs coexist in one install. Vendor-specific
  overrides must survive.
- Real integration consumers already exist in the design: Prometheus scraping `/metrics`, Home
  Assistant Lovelace cards holding a dedicated token and talking directly to KeepPeek, MQTT and
  webhooks for automation.
- `[inferred]` Review happens after something has already gone wrong, often with someone waiting.
  Latency and confidence in the review path matter more than they would in a browsing tool.

## Capabilities and Constraints

**Ingest.** ONVIF discovery (5 s window, up to 200 parallel probes); RTSP via the vendored Retina
crate; Reolink via the vendored `reo-proto` (Baichuan) crate; TCP or UDP; explicit RTSP URLs
override discovery. Main and sub streams per camera. H.264, H.265 video; AAC, G.711 A-law/µ-law,
ADPCM audio. Streams are recorded without re-encoding.

**Live.** WebRTC only. `POST /create` exchanges a gzip-compressed SDP offer for an answer;
KeepPeek is an ICE-lite answerer, so that single exchange is the whole negotiation. Three
pre-negotiated data channels carry control, reliable data, and unreliable data. A
`ServerCapabilities` snapshot is sent complete on every state change, never as a delta. Media
variants are selected by `quality_rank` (auto/high/low) or by exact `variant_id`. PTZ supports
continuous, relative, presets, and zoom where the camera reports it. Groups provide always
full-duplex audio: KeepPeek arbitrates nothing, and push-to-talk and mute are purely client-side.

**Storage.** Three tiers — a short-term in-process buffer, a medium-term rolling tier bounded by
age, and a long-term archive bounded by size. Fragmented MP4, keyframe-aligned. A catalog records
recordings and fragments with byte ranges and random-access flags.

**Events.** One `Event` message covers live and stored. It carries `event_type`, `revision`,
optional `text` description, an arbitrary `payload` struct, `confidence`, `zone`, `bounding_box`,
and zero or more attachments (`snapshot`, `story-frame`, `text-summary`). Events mutate by
revision. Events may arrive with no imagery at all. Stored events are queried through a timeline
API returning bucketed availability plus event records.

**Health.** A `ServerHealthResponse` JSON document (system, storage, per-camera, per-stream,
plus a severity-scoped issues list), a Prometheus `/metrics` endpoint, and an SSE `/logs` stream.
Camera lifecycle states are Starting, Connected, Degraded, Reconnecting, ShuttingDown, Stopped.

**Authentication as it stands.** The running server does not yet check a key. The contract is:
local requests skip authentication; remote requests present one shared bearer UUID. Every key
has identical access. There is no user model, no role, no per-camera restriction, and no audit
trail.

**Access model, as decided.** Network position decides whether a request authenticates. Role
decides what an authenticated remote principal may do.

- **Local** — loopback, link-local, and private LAN source addresses. No sign-in, no key.
  The request is Administrator. This is how the person at the machine watches and configures
  their own recorder.
- **Remote** — every other source, including a request that arrived through a reverse proxy
  or a forwarded client address. Authentication is required. Today that is the shared bearer
  UUID. The target is an identity with one of the two fixed roles.

Two fixed roles, deliberately not granular:

- **Administrator** — full access, including every setting.
- **User** — may watch live and recorded video, and may operate cameras: PTZ, presets, zoom,
  talk. May change no settings at all. The line is operation versus configuration, not read
  versus write. There is no per-camera or per-group scoping, and no operator-defined custom
  roles.

First-run still creates an administrator so remote access has someone to sign in as. Local
use does not wait on that account. A User identity exists only for remote people who should
operate cameras without configuring the server.

**Scale target.** Up to **127 sources** on a single server. A source is not necessarily a physical
camera: external services publish inputs too, and anything an external service can look at counts
against the same ceiling. One server, one location. Multi-site aggregation is explicitly out of
scope, so no site switcher and no cross-site identity. At this ceiling, virtualised lists,
first-class search, and grouping are structural requirements rather than refinements, and no
screen may assume every source fits on one page.

**Frontend constraints the design must respect.** Client-side rendering only — SvelteKit with the
static adapter, `ssr: false` and `prerender: false`. All live video shares one
`RTCPeerConnection` held in context. State is Svelte 5 runes and context only, with no store
library. Bun is the only package manager and script runner. `./check.sh` is the gate.

**Designed but not implemented.** Shared camera-default inheritance; recording policies; rules
and action sets; zone/mask geometry storage; clip and snapshot export; users, roles and audit;
offsite archive targets. Camera create/update, storage paths, logging, and restart already have
write APIs and stay live. Two known internal gaps: stored-media indexing is not wired end to end,
so fast seek does not yet have its byte ranges, and event persistence currently holds one mutable
row and one thumbnail per event, so revisions and multi-image stories need new tables.

The target UI gates every missing backend-owned command on an exact-version server capability.
Missing capabilities leave current values visible and replace the command with "Server update
required"; failed commands preserve the user's draft and must not partially apply. Clip export is
required for MVP because the evidence workflow is incomplete without it.

**Scope boundary.** KeepPeek performs no object detection, no face recognition, and no licence
plate recognition. It has no model selection, no detector hardware configuration, and no
enrollment or training. **Zone and mask geometry is owned entirely by the detection service.**
KeepPeek ships no drawing tools, no mask editor, and no motion tuner; it renders the `zone` an
event already carries and nothing more. It stores and presents what cameras and external
detection services report. This supersedes `features.md` ranks 8, 11, 31 and 33, which describe
detection and zone authoring as in-scope; that document is a working proposal whose owner
decisions are all still `TBD`, and the owner instructed otherwise directly.

## Brand Commitments

- Name: **KeepPeek**. Logo at `assets/readme_logo.jpeg`.
- **Peek** is the live view and **Keep** is the recordings view. These names are kept, each
  carrying a plain-language subtitle. They are already load-bearing in routes and e2e tests.
- Licensed AGPL-3.0.
- Existing brand tokens in `ui/src/app.css`: rust `#8b3a20`, bright rust `#b7410e`, light rust
  `#d67b53`, iron `#2c2c2c`, oxidized `#5e3a2b`, paper `#f4ebd8`.
- The owner has bound the component and icon sources: **shadcn-svelte** for components and
  **Lucide** for icons, no mixing.

## Evidence on Hand

Real, and usable:

- `features.md` — 34 ranked features with an explicit scoring model and a genuine evidence
  register: page-level citations to the Blue Iris v6 manual, nine dated Reddit threads with vote
  and comment snapshots, and the official Pushover API.
- `reference/` — 273 annotated competitor screenshots: 53 from a production Frigate deployment,
  153 from a local Frigate install covering every settings and system screen, 67 from Scrypted
  including its layout editor and timeline.
- A Paper design file, "KeepPeek — NVR Design System & Spec", holding 80 registered design tokens
  and 34 in-scope boards covering desktop, mobile web, empty, degraded, capability-gated, export,
  diagnosis, keyboard, waiting, focused history, and light-theme states. Its versioned source snapshot and
  reviewed references are stored in `ui/design/paper/keeppeek-nvr-v34/`. Native iOS and Android
  viewer boards are intentionally outside this Svelte implementation scope.
- `docs/` — written design documents for the state store, groups, transcoding, event forwarding,
  Home Assistant integration, viewer, and the event-loop runtime.

Absences that future work must not paper over:

- **No customers, no testimonials, no case studies, no press, no logos.**
- **No benchmarks and no tested capacity profile.** `features.md` explicitly refuses to publish a
  camera-count ceiling until real profiles exist.
- **No pricing, licensing tiers, support commitments, or uptime claims.**
- The README states plainly that KeepPeek is a proof of concept and not production-ready. Nothing
  built may imply otherwise.

## Product Principles

1. **Local-first is a constraint, not a feature.** If something only works with an external
   service, it is optional by construction.
2. **KeepPeek records and indexes; it does not interpret.** Detection, recognition, and
   description arrive from elsewhere and are presented with their provenance intact.
3. **Silence is the failure mode.** A recording gap, a stalled writer, a degraded stream, or a
   dropped detector connection must be visible before somebody needs the footage, not after.
4. **Time is the primary index.** Every artefact — clip, event, alert, export — resolves to a
   moment on one shared timeline, and getting to that moment is the operation to optimise.
5. **Everything is reachable by another program.** Documented contracts, versioned messages, and
   stable identifiers, so the product composes with Home Assistant, Prometheus, MQTT, and
   detectors nobody has written yet.

## Accessibility & Inclusion

No product-specific standard has been established. Current state, for honesty rather than as a
target: ARIA labels and roles are present in places, there are no live regions for the streaming
data that updates constantly, contrast has not been tested, and there is no i18n framework — all
copy is hardcoded English with `lang="en"`.

## Open Decisions

Recorded rather than invented. Each one changes future design work.

- **Audit trail.** Not addressed. `features.md` bundles it with users and roles at rank 22; the
  chosen two-role model does not require it, but the small-business audience may.
- **Accessibility conformance target.** No standard named; whether one is required is unknown.
