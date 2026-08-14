# KeepPeek

## UI development

The Svelte 5 UI uses Bun 1.3.14 on Windows, Linux, and macOS. Public package sources are enforced by `.npmrc`, `ui/bunfig.toml`, and `.cargo/config.toml`.

```sh
bun install --cwd ui --registry=https://registry.npmjs.org/
bun run --cwd ui quality
bun run --cwd ui test:e2e
```

Before completing any UI change, run the same complete pipeline with `./check-ui.sh` on macOS or
Linux, or `.\check-ui.bat` on Windows after installing dependencies and Chromium once. These are
the canonical local and CI entry points and run all quality and browser checks without requiring a
network connection.

Use `bun run --cwd ui dev` for local development and `bun run --cwd ui test` for the complete Vitest and Playwright suite. Install Chromium once with `bun run --cwd ui test:e2e:install`.

## Private configuration

KeepPeek stores its live configuration outside the source checkout by default:

- macOS: `~/Library/Application Support/keeppeek/config.toml`
- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek/config.toml`
- Windows: `%APPDATA%\keeppeek\config.toml`

Camera credentials belong only in that file. Detailed camera inventory is stored as
`cameras_info.toml` in the same directory. Keep both files private and never add copies to the
repository. `--config` and `--base` remain available when an explicit alternate path is needed.

## Camera stream test

`camera-stream-test` records selected streams from one configured camera and reports ingress
statistics every 10 seconds. The camera entry must contain valid credentials.

```sh
cargo run --bin camera-stream-test -- \
	--camera 192.168.137.6 \
	--stream main,sub \
	--duration 60 \
	--output camera-stream-test-output
```

Select a camera by its configuration name or IP address. `--stream` accepts `main`, `sub`, or
both. A duration of zero records until Ctrl+C. Reports include measured FPS, keyframe rate,
bitrate, frame-gap timing, reconnects, protocol-reported dropped frames, and errors. MP4 files are
finalized under the output directory when recording stops.

Camera entries accept `backend = "auto" | "retina" | "reo-proto"` and
`transport = "tcp" | "udp"`. Defaults are `auto` and `tcp`. Automatic selection uses reo-proto
for Reolink cameras and Retina for other cameras. Retina supports both transports; reo-proto
supports both TCP and direct BCUDP transport.

Set `http_port` when a camera exposes its direct HTTP API on a non-default port. KeepPeek uses
that port for camera metadata, motion-detection control, and the LAN-only link to the camera's
built-in UI.

Keep camera table keys stable because they identify recording directories. Set `display_name` on
the camera entry to change the label shown in Peek and Keep without disconnecting existing history.
Use the structured updater rather than editing secret-bearing TOML as text:

```sh
cargo run --bin configure-cameras -- \
	--display-name 'front_gate=Front Gate'
```

Audit Reolink motion and AI settings without changing camera configuration or exposing credentials:

```sh
cargo run --bin audit-camera-motion
```

The safe report is written to `target/camera-setup/motion-audit.json`.