> **Status:** KeepPeek is currently a proof of concept (POC) and is not yet production-ready.

<hr>

<div align="center">
	<img src="assets/readme_logo.jpeg" alt="KeepPeek logo" width="480">
</div>

<hr>

KeepPeek is a local-first network video recorder (NVR) and Media Gateway for IP cameras.

Written in Rust, it is built for low-overhead, predictable, memory-safe concurrent media handling
on local hardware.

It runs on Linux, macOS, and Windows.

It records RTSP camera streams and exposes controlled live media sessions through its Media Gateway
over WebRTC. Viewers, clients, and services can connect locally or remotely. Privacy and
performance are core design goals: KeepPeek keeps camera media local without requiring a cloud
relay and handles it efficiently on local hardware. Transcoding, object detection, analytics, and
other services can consume or publish media.

## Documentation

Read the [KeepPeek Book](https://xnorpx.github.io/keeppeek/).

## Ecosystem

The Media Gateway API is implemented over WebRTC and is flexible enough to support a variety of
viewers, clients, and services. KeepPeek's NVR core and included viewer will remain open source and
free to use under the AGPL, with no limit on the number of cameras. Third-party clients and services
that interoperate with the Media Gateway may be open or closed source.

## Development

KeepPeek is developed with AI assistance through an iterative, human-directed process of
implementation, validation, review, and refinement.

## HomeKit

HomeKit discovery and pairing are opt-in. Add this to `config.toml` and restart KeepPeek:

```toml
[homekit]
enabled = true
bind = "0.0.0.0"
port = 32000
name = "KeepPeek"
```

KeepPeek exports every configured camera as an independent HomeKit IP camera. `port` is the
base HAP port, so cameras listen on `32000`, `32001`, and so on. Private state, setup codes, and
one QR SVG per camera are written under `homekit/` beside the configuration file and are also
shown on the Settings page. Add each camera separately in Apple Home.

Each camera keeps its own accessory identity and pairing database. A camera supports up to 256
controller records and concurrent encrypted connections from multiple phones and home hubs.
KeepPeek provides discovery, pairing, encrypted HAP control, str0m WebRTC negotiation, live
H.264/H.265 video, and characteristic notifications. Opus audio and SFrame remain unsupported.
See the [hap-video integration guide](crates/hap-video/README.md) for details.

## Docker

The image supports both `linux/amd64` and `linux/arm64` when built with Docker Buildx. It runs
the `keeppeek` application directly.

KeepPeek stores its configuration, recordings, recording catalog, thumbnails, and logging settings
under `/config/keeppeek` in the container. Mount `/config` from the host to persist them. The first
start creates the configuration there:

```sh
mkdir -p keeppeek-data
docker run --rm --name keeppeek -p 8081:8081 \
  -v "$(pwd)/keeppeek-data:/config" \
  ghcr.io/xnorpx/keeppeek:latest
```

The container runs as UID `65532`; ensure the host directory is writable by that user. On Linux,
run `sudo chown 65532:65532 keeppeek-data` if necessary.

Publish a multi-architecture image with:

```sh
docker buildx build --platform linux/amd64,linux/arm64 \
  --tag ghcr.io/xnorpx/keeppeek:latest --push .
```

## License

Copyright (C) 2026 Marcus Asteborg.

KeepPeek is licensed under the [GNU Affero General Public License version 3](LICENSE) only
(`AGPL-3.0-only`).
