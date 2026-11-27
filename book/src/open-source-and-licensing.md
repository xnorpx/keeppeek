# Open source and licensing

This chapter defines KeepPeek's dual-license boundary (`AGPL-3.0-only` server vs `MIT` public API), credits the open-source libraries, datasets, and media fixtures KeepPeek builds on, and summarizes how KeepPeek's scope compares with peer open-source NVR projects.

## Licensing model

KeepPeek separates the shared recorder core from its public protocol at an explicit license boundary:

| Area                                                                                                                                                        | License                       | Purpose                                                                                            |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------- | -------------------------------------------------------------------------------------------------- |
| Server, first-party UI, desktop wrapper, reference examples (`examples/`), and most workspace crates                                                        | `AGPL-3.0-only`               | Keeps the shared NVR core open—including modified network deployments—with no camera-count limits. |
| Public HTTP, WebRTC, protobuf, and SDP contracts under [`api/`](https://github.com/xnorpx/keeppeek/tree/main/api), plus bindings generated solely from them | `MIT`                         | Lets independent clients and services implement the protocol under any open or commercial license. |
| Permissive protocol library (`crates/isapi`) and local upstream forks (`crates/retina`, `crates/onvif`, `crates/mp4`, `crates/rouille`)                     | Upstream `MIT` / `Apache-2.0` | Retains original upstream authorship and licenses in each crate.                                   |

See the root [`LICENSE`](https://github.com/xnorpx/keeppeek/blob/main/LICENSE) for complete `AGPL-3.0-only` terms and [`api/LICENSE`](https://github.com/xnorpx/keeppeek/blob/main/api/LICENSE) for the `api/` MIT terms.

### Protocol-plugin boundary (free and paid services)

Because the recorder is a network service, `AGPL-3.0-only` ensures operators and users retain source access to the server running for them. Meanwhile, licensing `api/` and its generated bindings under `MIT` establishes a **protocol-plugin model** rather than an in-process plugin ABI:

- Independent free, community, or commercial services (object detection, transcoding, workflows, alternate viewers) run as separate processes and talk to KeepPeek through the public API without inheriting AGPL.
- KeepPeek retains sole ownership of camera credentials, recording paths, and internal databases rather than exposing them to third-party in-process plugins.

| Stakeholder                  | What the split provides                                                                                            |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| **Self-hosters & consumers** | Free, auditable recorder and first-party viewer with no camera-count paywall; optional services only when desired. |
| **Small businesses**         | Ability to purchase specialized external services at the API boundary without a proprietary NVR fork.              |
| **Open-source contributors** | Core improvements stay open under AGPL while permissive API clients remain easy to build.                          |
| **Service authors**          | Sustainable commercial or open offerings without closing the shared recorder core.                                 |

## Code foundations and upstream forks

Local workspace forks preserve upstream authorship and licenses; carrying a fork is a maintenance decision, not a claim of original authorship.

| Project / crate                                                                                                                                                           | Upstream authors                  | License                 | Role in KeepPeek                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **[Retina](https://github.com/scottlamb/retina)** ([`crates/retina`](https://github.com/xnorpx/keeppeek/tree/main/crates/retina))                                         | Scott Lamb & contributors         | `MIT OR Apache-2.0`     | Local fork providing the RTSP client, RTP depacketization, and codec framing.                                                                                                                                                                                                      |
| **[ONVIF-rs](https://github.com/lumeohq/onvif-rs)** ([`crates/onvif`](https://github.com/xnorpx/keeppeek/tree/main/crates/onvif))                                         | Chris Bruce, Lumeo & contributors | `MIT`                   | Local fork providing ONVIF discovery, XML schemas, authentication, and camera control operations.                                                                                                                                                                                  |
| **[str0m](https://github.com/algesten/str0m)**                                                                                                                            | Martin Algesten & contributors    | `MIT OR Apache-2.0`     | Upstream Sans-I/O Rust WebRTC stack (RTP/frame APIs, data channels, explicit time/network inputs).                                                                                                                                                                                 |
| **[mp4-rust](https://github.com/alfg/mp4-rust)** ([`crates/mp4`](https://github.com/xnorpx/keeppeek/tree/main/crates/mp4))                                                | Alf & contributors                | `MIT`                   | Local fork providing ISO BMFF / MP4 reading and writing.                                                                                                                                                                                                                           |
| **[rouille](https://github.com/tomaka/rouille)** ([`crates/rouille`](https://github.com/xnorpx/keeppeek/tree/main/crates/rouille))                                        | Pierre Krieger & contributors     | `MIT/Apache-2.0`        | Local fork providing the synchronous HTTP server layer.                                                                                                                                                                                                                            |
| **[`crates/isapi`](https://github.com/xnorpx/keeppeek/tree/main/crates/isapi)** & **[`crates/reo-proto`](https://github.com/xnorpx/keeppeek/tree/main/crates/reo-proto)** | KeepPeek contributors             | `MIT` / `AGPL-3.0-only` | Plain-Rust Sans-I/O Hikvision ISAPI ([provenance](https://github.com/xnorpx/keeppeek/blob/main/crates/isapi/UPSTREAM.md)) and Reolink Baichuan protocol (informed by [neolink](https://github.com/QuantumEntangledAndy/neolink) and thirtythreeforty's reverse-engineering notes). |

## Third-party catalogs and test media fixtures

| Asset                                                                   | Attribution / source                                                                  | License                                                         | Usage in KeepPeek                                                                                                                                                                      |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **CCTV Camera Database** (`v2.8.0`)                                     | [`ch-bas/cctv-camera-database`](https://github.com/ch-bas/cctv-camera-database)       | Upstream release archive (verified by SHA-256 in `build.rs`)    | Embedded offline camera model catalog for guided camera setup.                                                                                                                         |
| **_Big Buck Bunny_**                                                    | © 2008 Blender Foundation                                                             | [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/)       | 48-second 4K H.264 excerpt (`crates/test-camera/testdata/`) for the nine-camera demo and benchmarks.                                                                                   |
| **_4K resolution sample_**                                              | Jihei (2011), Wikimedia Commons                                                       | [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) | 1-second H.264/H.265 derivatives (`crates/test-camera/testdata/cc-4k-*.mp4`) for backend tests.                                                                                        |
| **Intel IoT DevKit sample videos** (`person-bicycle-car-detection.mp4`) | [`intel-iot-devkit/sample-videos`](https://github.com/intel-iot-devkit/sample-videos) | [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/)       | Fetched at pinned commit in CI (not committed; see [`data/README.md`](https://github.com/xnorpx/keeppeek/blob/main/data/README.md)) for real-model object-detection integration tests. |

See [`crates/test-camera/testdata/README.md`](https://github.com/xnorpx/keeppeek/blob/main/crates/test-camera/testdata/README.md) for exact fixture hashes and encoding commands.

## Peer open-source NVR projects

[Scrypted](https://www.scrypted.app/) and [Frigate](https://frigate.video/) are strong open-source projects with distinct design centers—thank you to their maintainers and communities for advancing self-hosted camera software.

| Project                                   | Distinct design center                                                                                                                              | Strongest fit when                                                                                                             |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| **[Scrypted](https://www.scrypted.app/)** | Video integration platform and NVR with an extensible plugin model, smart detections, and low-latency HomeKit, Google Home, and Alexa bridges.      | Broad smart-home bridges, an in-process plugin ecosystem, and consumer-platform integration belong in one system.              |
| **[Frigate](https://frigate.video/)**     | Local NVR centered on real-time AI object detection, accelerator-aware inference, zones, event review, Home Assistant, and MQTT.                    | Tightly integrated, accelerator-tuned local AI detection and home automation are the NVR's central job.                        |
| **KeepPeek**                              | Focused recorder and WebRTC media gateway that ingests camera events and delegates optional inference, transcoding, and bridges across an open API. | Recording must stay isolated from analytics, and clients or services should compose cleanly over an open media/event boundary. |

See [Contributing](./contributing.md) for how licenses apply to submitted changes.
