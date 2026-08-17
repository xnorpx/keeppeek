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

## License

Copyright (C) 2026 Marcus Asteborg.

KeepPeek is licensed under the [GNU Affero General Public License version 3](LICENSE) only
(`AGPL-3.0-only`).
