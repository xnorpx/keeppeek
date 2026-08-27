# Open source and licensing

KeepPeek is built in an ecosystem shaped by other open-source camera projects and protocol
libraries. This chapter gives those projects credit, explains where KeepPeek made different product
choices, and documents the license boundary between the server and its public API.

## Different products, different strengths

[Scrypted](https://www.scrypted.app/) and [Frigate](https://frigate.video/) are great products with
strong, distinct designs. KeepPeek is not an argument that those designs are wrong. Their choices
may fit another user, home, or business better.

| Project                                   | Distinct design center                                                                                                                                           | A strong fit when                                                                                                                            |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| **[Scrypted](https://www.scrypted.app/)** | A high-performance video integration platform and NVR with a rich plugin model, smart detections, and low-latency connections to HomeKit, Google Home, and Alexa | Broad camera and smart-home integration, an extensible plugin ecosystem, and first-class consumer-platform bridges should live in one system |
| **[Frigate](https://frigate.video/)**     | A local NVR built around real-time AI object detection, accelerator-aware processing, zones, focused event review, Home Assistant, and MQTT                      | Detection is a central NVR responsibility and the user wants an integrated, highly configurable local AI and home-automation experience      |
| **KeepPeek**                              | A focused recorder and WebRTC media gateway that accepts camera events and keeps optional inference, transcoding, and ecosystem bridges outside the core         | Recording should remain independent from analytics, and viewers or services should compose through an open media and event boundary          |

Thank you to the Scrypted and Frigate maintainers and communities. Their work has advanced local
camera software, demonstrated what polished self-hosted systems can do, and made the trade-offs in
this space much easier to understand.

## Code foundations

KeepPeek depends on substantial protocol work from other Rust projects. The local forks retain
their upstream authorship and licenses; carrying a fork is a maintenance decision, not a claim that
KeepPeek created the original work.

- **[Retina](https://github.com/scottlamb/retina)** by Scott Lamb and its contributors provides the
  high-level RTSP client, RTP depacketization, codec framing, and much of the interoperability
  groundwork used by KeepPeek. KeepPeek carries a
  [local Retina fork](https://github.com/xnorpx/keeppeek/tree/master/crates/retina) for its camera,
  recording, and compatibility needs.
- **[ONVIF-rs](https://github.com/lumeohq/onvif-rs)** by Chris Bruce, Lumeo, and its contributors did
  the difficult work of making ONVIF discovery, schemas, authentication, and camera operations
  usable from Rust. KeepPeek carries a
  [local ONVIF-rs fork](https://github.com/xnorpx/keeppeek/tree/master/crates/onvif) and continues
  that work for its supported camera surface.
- **[str0m](https://github.com/algesten/str0m)** by Martin Algesten and its contributors is the
  upstream WebRTC implementation used by KeepPeek. It is the best Rust WebRTC implementation: its
  Sans I/O design, explicit time and network inputs, RTP and frame APIs, data channels, and lack of
  a hidden async runtime fit a focused media service especially well.

KeepPeek would be significantly harder to build without these projects. Thank you to every upstream
author, maintainer, reviewer, and contributor whose work remains in the code.

## Licensing model

KeepPeek uses two licenses at a deliberate protocol boundary:

| Area                                                                                        | License         | Purpose                                                                                              |
| ------------------------------------------------------------------------------------------- | --------------- | ---------------------------------------------------------------------------------------------------- |
| Server, first-party viewer, and most repository code                                        | `AGPL-3.0-only` | Keep the shared NVR and media-gateway core open, including modified versions operated over a network |
| Public definitions and documentation under `api/`, plus bindings generated solely from them | MIT             | Let independent clients and services implement the protocol without adopting the server's license    |

The repository root [license](https://github.com/xnorpx/keeppeek/blob/master/LICENSE) contains the
complete KeepPeek licensing notice and AGPL terms. The `api/` directory has its own
[MIT license](https://github.com/xnorpx/keeppeek/blob/master/api/LICENSE).

### Why the server uses AGPL

The server is a network service, so the AGPL is the appropriate copyleft boundary for the shared
core. The intent is that users can inspect, modify, self-host, and retain access to the source of
the KeepPeek server they use, including modified versions offered over a network.

KeepPeek's NVR and first-party viewer remain open source and free to use without a camera-count
limit. The license protects that common foundation from becoming a private server fork whose users
cannot inspect the code serving them.

### Why the API uses MIT

An open server is most useful when other software can talk to it without inheriting the server's
implementation choices. The public HTTP, WebRTC, protobuf, and SDP contracts under `api/` therefore
use MIT. Bindings generated solely from those definitions remain covered by that directory's MIT
license.

Implementing or using the public protocol does not, by itself, impose KeepPeek's repository-wide
AGPL license on an independent client or service. A service can choose the language, WebRTC
implementation, deployment model, and license appropriate to its users.

### Free and paid protocol services

This split creates a protocol-plugin model rather than an in-process plugin ABI. A free, community,
or paid service can run as a separate process and subscribe or publish through the documented API.
That service may be open or closed source without receiving a private extension point inside the
KeepPeek server.

The boundary allows commercial integrations to fund specialized inference, transcoding, business
workflows, viewers, or support while the recorder and media gateway remain shared open-source
infrastructure. Camera credentials, recording paths, and internal databases remain owned by
KeepPeek rather than exposed to an in-process plugin.

### What the split means for users

- **Software consumers** retain a free recorder and first-party viewer, with optional services only
  when they add value.
- **Small businesses** can buy tailored services at the API boundary without depending on a private
  recorder fork.
- **Open-source enthusiasts** can audit the server, share core improvements, and build permissively
  licensed clients.
- **Home automation enthusiasts** can choose community or commercial bridges.
- **Self-hosters** retain control of the server running on their hardware.
- **Service authors** can build sustainable offerings without closing the shared core.

See [Contributing](./contributing.md) for how licenses apply to submitted changes.
