# Get started

> **Status:** KeepPeek has completed its proof-of-concept gate and is undergoing MVP
> qualification. It is not yet production-ready.

Choose the installation that fits the host. Linux has first-class Docker support; macOS and Windows
use native packages and service integration.

## Linux with Docker

The published image supports `linux/amd64` and `linux/arm64`. It runs KeepPeek directly as the
non-root user `65532` and listens on port `8081`.

KeepPeek stores configuration, owner-only secrets, recordings, the recording catalog, thumbnails,
and logging settings under `/config/keeppeek` in the container. Mount `/config` from the host so
that data survives container replacement:

```sh
mkdir -p keeppeek-data
docker run --rm --name keeppeek -p 8081:8081 \
	-v "$(pwd)/keeppeek-data:/config" \
	ghcr.io/xnorpx/keeppeek:latest
```

On Linux, make the mounted directory writable by the container user when necessary:

```sh
sudo chown 65532:65532 keeppeek-data
```

Open `http://localhost:8081` after the server starts.

## macOS

Current macOS releases target Apple Silicon and use a signed and notarized application in a DMG.
Drag `KeepPeek.app` into Applications and open it once to install and start the per-user `launchd`
service. See the
[macOS installation guide](https://github.com/xnorpx/keeppeek/blob/master/docs/macos-installation.md)
for checksums, service operation, upgrades, and removal.

## Windows

Windows releases provide signed x86-64 and ARM64 binaries and a signed NSIS installer. The installer
can register KeepPeek with the Windows Service Control Manager and manages service upgrades and
removal. See the
[Windows service guide](https://github.com/xnorpx/keeppeek/blob/master/docs/windows-service.md) for
service commands and logging options.

## Configuration and secrets

The first start creates the KeepPeek configuration directory for the selected installation. Reusable
private values belong in owner-only `secrets.toml` beside `config.toml`:

| Installation | Default secrets path                                      |
| ------------ | --------------------------------------------------------- |
| macOS        | `~/Library/Application Support/keeppeek/secrets.toml`     |
| Linux        | `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/secrets.toml` |
| Windows      | `%APPDATA%\keeppeek\secrets.toml`                         |
| Docker       | `/config/keeppeek/secrets.toml`                           |

Never add `secrets.toml` to source control, logs, screenshots, support bundles, or command
arguments. Enter camera credentials and other private values directly on the machine running
KeepPeek. See the
[secrets guide](https://github.com/xnorpx/keeppeek/blob/master/docs/secrets.md) for references,
precedence, and migration behavior.

## Back up before relying on recordings

KeepPeek does not yet provide a validated online backup or restore workflow. Before treating an
installation as evidence storage, configure a host-level backup that includes the complete KeepPeek
configuration directory and every configured recording, catalog, thumbnail, and export path.

Stop the KeepPeek service before taking a file-level copy so the catalog, its write-ahead log, and
recording files represent one point in time. Preserve file ownership and permissions, keep backup
media encrypted, and test restoration on an isolated host before depending on it. A live copy can
capture mismatched database and media state; an untested copy may not be recoverable when evidence
is needed.

Validated online backup, restore, migration checks, and guided recovery are owned by
[issue #128](https://github.com/xnorpx/keeppeek/issues/128) for the Alpha milestone. Until that work
is complete, the supported workaround is a stopped-service copy managed and verified by the host
administrator.

## Add the first camera

Open **Cameras** and use **Add camera**. KeepPeek discovers common cameras where possible and keeps
manual RTSP entry available. Do not trust discovery alone: authentication, the requested main or
sub streams, keyframes, finalized MP4 recordings, and playback all need to validate.

Edit an existing camera from its **Camera** page. Use **Settings** only for server-wide
configuration. Camera-specific credentials and stream choices belong with the camera.

The [Users and design choices](./users-and-design-choices.md#cameras-brands-and-protocols) chapter
describes supported vendor paths and the pragmatic camera compatibility policy.

After the first stream is verified, continue with [Camera and stream health](./camera-health.md) and
[Recording and evidence](./recording-and-evidence.md). Review
[Release readiness and known limitations](./release-readiness.md) before relying on the installation
for evidence.
