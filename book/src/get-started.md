# Get started

> **Status:** KeepPeek is in Alpha qualification and is not yet production-ready.
> The active [Alpha gate](https://github.com/xnorpx/keeppeek/issues/145) tracks promotion evidence.

Choose the installation that fits the host. Linux has first-class Docker support; macOS and Windows
use native packages and service integration.

## Before installation

Choose a host with enough disk capacity for the camera bitrates and retention you need. KeepPeek
stores camera media without transcoding; a browser still needs to support the selected codec.
Start with a small camera set and measure it before increasing the workload.

Use the [release page](https://github.com/xnorpx/keeppeek/releases) to select a build for the host.
Record that version and retain its checksum information. By default, clients on private and local
networks receive Administrator access without signing in; review
[the access policy](./authentication.md) before exposing the listener to other devices.

## Linux with Docker

The published image supports `linux/amd64` and `linux/arm64`. It runs KeepPeek directly as the
non-root user `65532` and listens on port `8081`.

KeepPeek stores configuration, owner-only secrets, recordings, the recording catalog, thumbnails,
and logging settings under `/config/keeppeek` in the container. Mount `/config` from the host so
that data survives container replacement:

```sh
mkdir -p keeppeek-data
sudo chown 65532:65532 keeppeek-data
docker run --rm --name keeppeek -p 8081:8081 \
	-v "$(pwd)/keeppeek-data:/config" \
	ghcr.io/xnorpx/keeppeek:latest
```

The ownership command is for the newly created Linux directory. Use the permissions appropriate
to your existing storage if the directory already contains data. This example runs in the foreground
and removes the container when it stops; the mounted host data remains. Configure your deployment's
restart policy for unattended operation. Select an explicit release tag or digest instead of the
moving `latest` tag when you need to reproduce the installation.

Open `http://localhost:8081` after startup. If the container cannot create its configuration or
recordings, check the host mount and permissions for user `65532`.

## macOS

macOS packages target Apple Silicon (`arm64`). Download the release DMG and its matching `.sha256`
file, then verify the checksum:

```sh
shasum -a 256 -c keeppeek-<version>-macos-aarch64.dmg.sha256
```

Replace `<version>` with the downloaded version. Drag `KeepPeek.app` into `/Applications` and open
it once. The application installs and starts the per-user `com.keeppeek` launch agent, which starts
after that user logs in. Open `http://localhost:8081` in a browser.

Signing and notarization depend on the release workflow's configured credentials; check the chosen
artifact rather than assuming every DMG has both. The
[macOS installation guide](https://github.com/xnorpx/keeppeek/blob/main/docs/macos-installation.md)
explains packaging and service logs. Use [Upgrades and migrations](./upgrades-and-migrations.md#macos)
when replacing an existing installation.

## Windows

Windows releases provide signed x86-64 and ARM64 binaries and an installer. Choose the architecture
that matches the host and run the installer. Select **Run KeepPeek as a Windows service** for
background operation through the Service Control Manager. The service is named `KeepPeekService`.
Without that component, run the installed `keeppeek.exe` as a standalone process.

Open `http://localhost:8081` after startup. To inspect the service from PowerShell:

```powershell
sc.exe query KeepPeekService
sc.exe qc KeepPeekService
```

The service reads configuration in its own process account's environment, which can differ from
your interactive `%APPDATA%`. Inspect the service identity and actual configuration location before
moving data between standalone and service operation. See
[Upgrades and migrations](./upgrades-and-migrations.md#windows) and the
[Windows service guide](https://github.com/xnorpx/keeppeek/blob/main/docs/windows-service.md) for
service lifecycle and logging.

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
[configuration reference](./configuration-reference.md#passwords-and-secret-references) for references,
precedence, and migration behavior.

These paths belong to the account running KeepPeek. `--config <path>` selects a different
configuration file with `secrets.toml` beside it. Inspect the effective recording paths separately;
omitted storage paths still use the operating system's default KeepPeek directory.

On first startup, use a trusted-local browser to retrieve and save the **Initial Administrator** key
if you need remote access. The browser shows it once. Create named credentials for individual remote
users or integrations in **Settings > Access & roles**. See
[Authentication and access control](./authentication.md#first-run).

## Create a recovery backup

Review **Settings** storage and retention values before relying on the recorder. Confirm the actual
recording, catalog, and thumbnail paths, available space, and retention policy. Defaults are starting
values, not a sizing recommendation for every camera fleet.

Open **Settings → Backup and restore** as an Administrator and create a validated configuration
bundle before relying on the recorder. Download the ZIP and retain it separately from the recorder.
The ZIP contains exactly `config.toml` and plaintext `secrets.toml`, so handle it as sensitive data.
It intentionally omits `recordings.db`, sessions, MP4 recordings, and thumbnail JPEG bytes.

Test recovery on an isolated installation. Export uses `GET /config/export`; apply uses
`POST /config/apply` with the ZIP body. Apply validates both files, capacity, and target paths before
staging. Live files change only after a controlled restart. See
[Backup and restore](./backup-and-restore.md) for the complete workflow and limits.

For footage recovery, also create and test a
[separate catalog and media archive](./recording-archive-recovery.md). A successful ZIP export alone
does not prove recordings can be recovered after disk loss.

## Add the first camera

With no cameras configured, an Administrator can select **Add camera** directly from the
Dashboard's **No cameras yet** message. The same wizard is available from **Cameras → Add camera**.
KeepPeek discovers common cameras where possible and keeps manual RTSP entry available. Do not
trust discovery alone: authentication, the requested main or sub streams, keyframes, finalized MP4
recordings, and playback all need to validate.

Edit an existing camera from its **Camera** page. Use **Settings** only for server-wide
configuration. Camera-specific credentials and stream choices belong with the camera.

Set a recording policy that matches the footage you need: the built-in default is `event-boost`, not
continuous recording of both streams. Open live view, then review a new finalized interval in
**Keep**. An incompatible browser codec is different from absent footage; retain a compatible
substream when the camera provides one.

The [Users and design choices](./users-and-design-choices.md#cameras-brands-and-protocols) chapter
describes supported vendor paths and the pragmatic camera compatibility policy.

After the first stream is verified, continue with [Camera and stream health](./camera-health.md) and
[Recording and evidence](./recording-and-evidence.md). Review
[Release readiness and known limitations](./release-readiness.md) before relying on the installation
for evidence.

## Complete the first-use check

- Verify live view, recorded playback, recording integrity, and a small downloaded evidence export.
- Test each remote credential from an address that is not trusted-local. Check allowed and denied
  cameras, and the separate dashboard audience.
- If you enable native events, external analysis, notifications, or MQTT, test a controlled event
  through its intended destination and inspect failures.
- Restart once and confirm recording resumes, durable settings remain, and remote users can sign
  in again. Pending notification and MQTT work does not survive that restart.
