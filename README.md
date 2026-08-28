<div align="center">
	<img src="assets/readme_logo.jpeg" alt="KeepPeek logo" width="480">
</div>

> **Status:** KeepPeek is currently a proof of concept (POC) and is not yet production-ready.

KeepPeek is a local-first network video recorder (NVR) and Media Gateway for IP cameras. The
focused Rust service records camera streams as MP4, serves the Svelte browser interface, and lets
independent viewers, inference services, and integrations consume or publish media and events.

## Documentation

- [KeepPeek Book](https://xnorpx.github.io/keeppeek/)
- [Get started](https://xnorpx.github.io/keeppeek/get-started.html)
- [Authentication and access control](https://xnorpx.github.io/keeppeek/authentication.html)
- [Users and design choices](https://xnorpx.github.io/keeppeek/users-and-design-choices.html)
- [Public API](api/README.md)
- [Reporting bugs](https://xnorpx.github.io/keeppeek/reporting-bugs.html)
- [Contributing](https://xnorpx.github.io/keeppeek/contributing.html)

Reusable private values belong in owner-only [`secrets.toml`](docs/secrets.md) beside
`config.toml`; camera defaults, per-camera settings, and the remote access key reference those flat
keys. Add cameras from **Cameras**, edit an existing camera from its **Camera** page, and use
**Settings** only for server-wide configuration.

## Ecosystem

The Media Gateway API is implemented over WebRTC and is flexible enough to support a variety of
viewers, clients, and services. KeepPeek's NVR core and included viewer will remain open source and
free to use under the AGPL, with no limit on the number of cameras. Third-party clients and services
that interoperate with the Media Gateway may be open or closed source.

## Development

KeepPeek is developed with AI assistance through an iterative, human-directed process of
implementation, validation, review, and refinement.

Repository formatting and validation require Python 3.12 plus the tools listed in
`examples/object_detection_service/requirements.txt`, including Black. The root `fix` and `check`
scripts use `KEEPPEEK_PYTHON` when set, otherwise `python3.12`, then `python3`. Do not use a Python
virtual environment; `fix` installs the requirements into the resolved interpreter.

## Docker

The image supports both `linux/amd64` and `linux/arm64` when built with Docker Buildx. It runs
the `keeppeek` application directly.

KeepPeek stores its configuration, owner-only secrets, recordings, recording catalog, thumbnails,
and logging settings under `/config/keeppeek` in the container. Mount `/config` from the host to
persist them. The first start creates the configuration there:

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
