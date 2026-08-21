# Test Camera

`test-camera` serves deterministic main and sub video profiles from two MP4
sources. Each source must contain one H.264 or H.265 video track. The camera
starts a minimal ONVIF service so its printed configuration can be used by the
normal KeepPeek camera loader.

## RTSP

Start a generic RTSP camera with RTP over TCP:

```sh
cargo run -p test-camera -- rtsp --main main.mp4 --sub sub.mp4
```

Add `--transport udp` to exercise RTP/UDP instead. The process prints a TOML
entry with `backend = "retina"`, ephemeral ONVIF and HTTP UI ports, and the
supplied credentials. The HTTP port serves a small fake built-in camera UI, so
the Camera page's LAN-only UI link is testable. Add the entry to a camera
configuration, then connect through the regular application or
`keeppeek-camera test`.

## Reolink Baichuan

Start a Reolink-compatible Baichuan camera with TCP:

```sh
cargo run -p test-camera -- reo-proto --main main.mp4 --sub sub.mp4
```

Add `--transport udp` to exercise Baichuan UDP discovery and payload transport.
This mode identifies itself as a Reolink device through ONVIF and serves
Baichuan TCP on port `9000` plus UDP discovery on ports `2018` and `2015`.
The printed entry selects `backend = "reo-proto"` and includes its dynamic
ONVIF port, HTTP control port, and UID. Its fake Reolink HTTP API reports
deterministic device, encoder, audio, capability, and motion-detection data.
KeepPeek can read and toggle the fake motion state through the normal camera
information page.

Use `--username`, `--password`, `--uid`, and `--name` to customize the generated
entry. Use `--bind-ip` when the host has another address configured and a second
test camera is needed.

## Recording Seed

Seed deterministic H.264 history for browser tests:

```sh
cargo run -p test-camera --bin test_camera -- seed-recording \
	--source testdata/cc-4k-640x360-h264.mp4 \
	--recordings /tmp/keeppeek-recordings \
	--catalog /tmp/keeppeek-recordings.db \
	--stream-id e2e-h264/main
```

This mode replaces the former standalone E2E seeder and exits after finalizing
the recording and catalog entries.
