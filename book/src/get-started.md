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

## Create a recovery backup

Open **Settings → Backup and restore** as an Administrator and create a validated reference-only
bundle before relying on the recorder. Download the ZIP and retain it separately from the recorder.
The bundle includes configuration and critical metadata but intentionally omits resolved secrets,
sessions, MP4 recordings, and thumbnail JPEG bytes.

Test recovery on an isolated installation. Inspection and dry run do not change live state. Dry run
requires explicit target path mappings and reports required external secrets, capacity, conflicts,
migrations, and restart consequences. Activation and rollback require confirmation and a controlled
restart. See [Backup and restore](./backup-and-restore.md) for the complete workflow and limits.

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
