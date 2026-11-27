# Get started

This chapter covers installing KeepPeek on Linux (Docker or binary), macOS, or Windows, monitoring a foreground server in the terminal dashboard, completing `/setup`, backing up configuration, and verifying your first camera. First-run setup requires Administrator access (granted automatically from trusted local networks by default).

> **Alpha status:** KeepPeek is in Alpha qualification and is not yet production-ready. Track promotion evidence in the [Alpha gate](https://github.com/xnorpx/keeppeek/issues/145).

## Choose an installation path

Select a host with enough disk capacity for your camera bitrates and retention policy. KeepPeek records and streams camera media without transcoding, so viewing browsers must support the camera's codec. Download builds and checksums from the [releases page](https://github.com/xnorpx/keeppeek/releases) and record the version and `.sha256` digest.

| Platform | Package / artifact                                                   | Architectures        | Runtime / service                           | Default config & secrets directory            |
| -------- | -------------------------------------------------------------------- | -------------------- | ------------------------------------------- | --------------------------------------------- |
| Docker   | `ghcr.io/xnorpx/keeppeek:<tag>`                                      | `amd64`, `arm64`     | Non-root UID `65532`, port `8081`           | `/config/keeppeek/`                           |
| Linux    | `keeppeek-<version>-linux-<arch>.tar.gz`                             | `x86_64`, `aarch64`  | Foreground `keeppeek` binary                | `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/` |
| macOS    | `keeppeek-<version>-macos-aarch64.dmg`                               | `aarch64`            | `KeepPeek.app` + `com.keeppeek` LaunchAgent | `~/Library/Application Support/keeppeek/`     |
| Windows  | `keeppeek-<version>-windows-<arch>-installer.exe`, standalone `.exe` | `x86_64`, `aarch64`  | `keeppeek.exe` or `KeepPeekService` (SCM)   | `%APPDATA%\keeppeek\`                         |
| Desktop  | Windows unified installer / `.exe`; macOS desktop `.dmg`             | Win x64/arm64, macOS | Native Tauri client only (no server)        | Reads/writes `[desktop]` in `config.toml`     |

On Windows and macOS, you can optionally install [KeepPeek Desktop](./desktop-app.md) to open an existing local or remote KeepPeek server in a dedicated window.

> **Security default:** Clients on private and local networks receive **Administrator** access without signing in. Review [Authentication and access control](./authentication.md) before exposing port `8081` beyond trusted devices.

## Linux with Docker

The container image supports `linux/amd64` and `linux/arm64`, runs KeepPeek as non-root user `65532`, and listens on port `8081`. Configuration, owner-only `secrets.toml`, recordings, `recordings.db`, thumbnails, and logging settings live under `/config/keeppeek`.

1. Create a host data directory owned by UID/GID `65532` (for a new directory; adjust permissions if reusing existing storage):

```sh
mkdir -p keeppeek-data
sudo chown 65532:65532 keeppeek-data
```

2. Run the container with `/config` mounted from the host:

```sh
docker run --rm --name keeppeek -p 8081:8081 \
  -v "$(pwd)/keeppeek-data:/config" \
  ghcr.io/xnorpx/keeppeek:latest
```

3. Open `http://localhost:8081`.

Use an explicit release tag or digest instead of `latest` for reproducible deployments, and add a restart policy (such as `--restart unless-stopped`) for unattended operation. If the container cannot write configuration or recordings, verify host mount permissions for UID `65532`. For native Linux hosts without Docker, extract `keeppeek-<version>-linux-<arch>.tar.gz`, verify its `.sha256` file, and run `./keeppeek`.

## macOS

macOS releases target Apple Silicon (`arm64`) only.

1. Download `keeppeek-<version>-macos-aarch64.dmg` and its `.sha256` file, then verify the checksum:

```sh
shasum -a 256 -c keeppeek-<version>-macos-aarch64.dmg.sha256
```

2. Open the DMG, drag `KeepPeek.app` to `/Applications`, and launch it once. It installs and starts the per-user `com.keeppeek` `launchd` agent, which starts at user login.
3. Open `http://localhost:8081`.

Code signing and notarization depend on the release workflow's configured credentials; verify the downloaded artifact rather than assuming both are present. See the [macOS installation guide](https://github.com/xnorpx/keeppeek/blob/main/docs/macos-installation.md) for service logs and [Upgrades and migrations](./upgrades-and-migrations.md#macos) before replacing an existing app.

## Windows

Official Windows releases publish Authenticode-signed, RFC 3161 timestamped `x86_64` and `aarch64` executables (including `uninstall.exe`), each with a `.sha256` file:

- **Unified installer:** `keeppeek-<version>-windows-<arch>-installer.exe` — select at least one of **KeepPeek Desktop** (`keeppeek-desktop.exe`), **KeepPeek Server with TUI** (`keeppeek.exe`), and **KeepPeek Windows Service** (`keeppeek-service.exe` / `KeepPeekService`).
- **Standalone assets:** `keeppeek-<version>-windows-<arch>.exe`, `keeppeek-service-<version>-windows-<arch>.exe`, `keeppeek-desktop-<version>-windows-<arch>.exe`, and server archive `keeppeek-<version>-windows-<arch>.zip`.

1. Download the unified installer for your host architecture and verify its `.sha256` hash and Authenticode signature (`Get-AuthenticodeSignature`).
2. Run the installer and select your desired components. For silent installs, combine `/S` with `/DESKTOP`, `/SERVER`, `/SERVICE`, `/NO_DESKTOP`, `/NO_SERVER`, or `/NO_SERVICE` (at least one component must remain enabled, or the installer exits with status `2`).
3. Open `http://localhost:8081` after startup.

Inspect the Windows service from PowerShell:

```powershell
sc.exe query KeepPeekService
sc.exe qc KeepPeekService
```

`KeepPeekService` reads configuration from its own service account's profile, which may differ from your interactive `%APPDATA%`. Check the service identity and active data paths before migrating between standalone and service runs. See [Upgrades and migrations](./upgrades-and-migrations.md#windows) and the [Windows service guide](https://github.com/xnorpx/keeppeek/blob/main/docs/windows-service.md) for lifecycle and logging details.

## Foreground terminal dashboard (TUI)

Running `keeppeek` in an interactive terminal opens a read-only live health dashboard by default (`--tui auto`). It polls server health every 2 seconds (marking samples `STALE` after 5 seconds and keeping the last snapshot if a check fails) and shows process CPU, RSS, viewer count, storage pressure, warnings, and per-stream rows (`Camera`, `Source` at >=115 columns, `Stream`, `Health`, `Codec`/`Resolution`/`FPS`/`Recording` at >=100 columns, `Recv kbit/s`, and `Age`) with a 4-line selected-stream detail pane.

| Flag / environment                                           | Behavior             | Notes                                                                             |
| ------------------------------------------------------------ | -------------------- | --------------------------------------------------------------------------------- |
| Foreground TTY (Linux, macOS, Windows Terminal / PowerShell) | Dashboard            | Default `--tui auto`; needs interactive stdin, stdout, stderr (`TERM != dumb`).   |
| Interactive SSH (`ssh -t`) or Docker (`docker run -it`)      | Dashboard            | Pass `--tui never` to emit plain logs instead.                                    |
| Non-TTY SSH/Docker, pipes, redirected stdio                  | Plain logs           | No escape sequences emitted; `--tui always` logs one diagnostic and keeps logs.   |
| `keeppeek config ...`, `--help`, `--version`                 | Plain output         | Subcommands and metadata flags never enter alternate screen, even `--tui always`. |
| `keeppeek-service` (Windows) and `com.keeppeek` (macOS)      | Plain / service logs | Background service entry points never start the terminal dashboard.               |

**Controls:** `Up`/`Down` or `Page Up`/`Page Down` select a stream row; `q` detaches to plain logs while recording continues; `Ctrl+C` requests a graceful shutdown and finalizes active recordings. After a hard kill, open a new terminal or run `reset` on Unix.

## First-run setup (`/setup`)

Open `http://localhost:8081/setup` from a trusted-local browser to complete the evidence-based first-run check:

1. **Where footage goes:** Confirm `storage.medium_term_path`, observed free capacity, and a `WRITE PROOF VERIFIED` status. Use **Verify storage again** to re-test or **Change recording storage** (`/settings?edit=storage#storage`) to pick another path.
2. **What time it is here:** Verify the browser-detected time zone. The server does not expose a timezone configuration command, so host OS time remains authoritative.
3. **Remote Administrator credential:** If you need remote access, select **Retrieve initial key**, save it with **Copy** or **Download** (`keeppeek-initial-administrator-key.txt`), then select **Hide permanently**. The key is displayed once and remote access stays unready until it is retrieved. Create named credentials per person or integration in **Settings → Access**; see [Authentication and access control](./authentication.md#first-run).
4. Select **Continue to camera setup** (enabled once storage write proof passes) to open `/cameras/new` (or `/cameras` if cameras already exist).

This walkthrough verifies first-run storage proof, browser time zone, and the one-time **Initial Administrator** key claim on `/setup`:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/setup.desktop.first-run.mp4">Download the first-run setup demo</a>.
</video>

## Configuration, secrets, and recovery backup

First startup creates `config.toml` and owner-only companion `secrets.toml` in the installation's default directory. Passing `-c <path>` / `--config <path>` selects a custom `config.toml` and looks for `secrets.toml` beside it; omitted storage paths still fall back to the OS default KeepPeek directory.

Never commit `secrets.toml` to source control or attach it to logs, screenshots, or support bundles. See [Passwords and secret references](./configuration-reference.md#passwords-and-secret-references) for `{secret:KEY}` syntax, precedence, and full configuration schemas.

Before relying on the recorder:

1. Review **Settings → Storage & retention** to confirm recording, catalog, and thumbnail paths, free space, and retention caps. Built-in defaults are starting values, not fleet sizing recommendations.
2. Open **Settings → Backup and restore** as an Administrator and export a validated configuration ZIP (`GET /config/export`, or `keeppeek config export --output <file>`). Store the ZIP securely off-host—it contains exactly `config.toml` and plaintext `secrets.toml` and omits `recordings.db`, sessions, MP4 footage, and thumbnails.
3. Verify restoration on an isolated instance (`POST /config/apply` or `keeppeek config apply <file> --confirm`). Apply validates files, capacity, and paths before staging; live files switch only on controlled restart. See [Backup and restore](./backup-and-restore.md).
4. Set up a separate media and catalog backup if footage must survive disk loss; see [Recording archive recovery](./recording-archive-recovery.md).

## Add and verify the first camera

With zero cameras configured, select **Add camera** from the Dashboard's **No cameras yet** prompt, or open **Cameras → Add camera** (`/cameras/new`).

1. Run subnet discovery, enter an IPv4 or `rtsp://` address manually, or match your model in the embedded **CCTV Database** catalog.
2. Provide camera credentials (or use server defaults) and run **Verify streams**—KeepPeek requires authenticated video and a keyframe on every stream needed by the selected recording policy before saving.
3. Choose a recording policy that fits your workload (the default is `event-boost`, not continuous dual-stream recording) and save.
4. Edit per-camera settings on its **Camera** page (`/camera`); reserve **Settings** (`/settings`) for server-wide configuration.

See [Camera controls](./camera-controls.md) for the full wizard and control details, and [Cameras, brands, and protocols](./users-and-design-choices.md#cameras-brands-and-protocols) for vendor paths and compatibility boundaries.

This walkthrough shows adding and verifying a camera through the production UI:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.add-wizard.mp4">Download the add-camera wizard demo</a>.
</video>

## First-use verification checklist

Before trusting an installation, verify all four checks:

1. **Media and evidence:** Confirm live playback, a newly finalized interval in **Keep** (`/keep`), and a small downloaded evidence export. Keep a browser-compatible H.264 substream available when main streams use unsupported codecs.
2. **Remote access boundary:** Test a disposable **User** credential from a non-local address across allowed cameras, denied cameras, and a shared dashboard audience.
3. **Events and integrations:** If you enabled native events, external analysis, notifications, or MQTT, trigger one controlled event and verify end-to-end delivery.
4. **Restart durability:** Restart KeepPeek once and confirm recording resumes, durable settings persist, and remote users can sign in again. Pending notification and MQTT deliveries do not survive a restart.

Next steps: [Camera and stream health](./camera-health.md), [Recording and evidence](./recording-and-evidence.md), and [Release readiness and known limitations](./release-readiness.md).
