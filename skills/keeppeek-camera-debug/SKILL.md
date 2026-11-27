---
name: keeppeek-camera-debug
description: Capture decrypted camera network traffic and bitstreams through KeepPeek MCP, triage protocol issues, and reproduce fixes offline with ReplayCameraServer.
---

# KeepPeek Camera Debug

Use this skill to capture live camera protocol traffic and raw media bitstreams from a running KeepPeek server, diagnose stream or protocol failures, export a sanitized offline reproduction fixture, and verify a minimal root-cause fix before filing an issue or opening a pull request.

## 1. Opt In Safely (`[mcp].allow_camera_debugging`)

Camera debugging is disabled by default. Stop KeepPeek before editing `config.toml` manually, enable `[mcp]` and `allow_camera_debugging`, and restart KeepPeek:

```toml
[mcp]
enabled = true
allow_camera_debugging = true
debug_camera_ids = []             # Optional allowlist (max 256 IDs); empty = all configured cameras
max_debug_capture_bytes = 67108864  # 64 MiB default (allowed: 1048576..=268435456)
max_debug_storage_bytes = 536870912 # 512 MiB default (allowed: 16777216..=4294967296)
```

> **Security warning:** While `[mcp].allow_camera_debugging = true`, Administrator MCP clients and AI agents have **full raw access to live video bitstreams, audio streams, decrypted network captures (PCAPNG), and camera credentials** for the configured `debug_camera_ids` (or all cameras when empty). KeepPeek emits a startup warning log and a `ServerHealthResponse` warning issue while enabled. Always use an **Administrator** credential (without `--read-only`), scope `debug_camera_ids` to the single affected camera when possible, delete captures when done, and set `allow_camera_debugging = false` as soon as debugging finishes.

## 2. Capture Live Protocol and Bitstream Evidence

Call the Administrator-only `camera_debugging` MCP tool in sequence:

1. **Inspect the active pipeline (`inspect_pipeline`):**
   Read the camera's configured backend, active stream state, health status, and profile overrides first:

   ```json
   {
     "action": "inspect_pipeline",
     "camera_id": "192.0.2.50"
   }
   ```

2. **Run a bounded live capture (`capture`):**
   Collect decrypted PCAPNG traffic, raw video/audio elementary bitstreams, a KeepPeek-muxed MP4, and a protocol transcript:

   ```json
   {
     "action": "capture",
     "camera_id": "192.0.2.50",
     "stream": "main",
     "artifacts": [
       "pcapng",
       "video_bitstream",
       "audio_bitstream",
       "muxed_mp4",
       "protocol_transcript"
     ],
     "duration_seconds": 10,
     "max_bytes": 67108864,
     "include_raw_auth": true,
     "trigger_probe": false
   }
   ```

   - `stream` accepts `"main"`, `"sub"`, or `"both"`.
   - Omit `protocols` to auto-select the camera's primary protocol, or pass a subset of `["rtsp", "audio", "onvif", "isapi", "reolink_http", "reo_proto"]` supported by that camera (listed protocols run sequentially and `capture` fails if any requested protocol is unsupported).
   - `duration_seconds` is bounded to `1..=60`; `max_bytes` is bounded to `65536..=268435456`.
   - `include_raw_auth` defaults to `true` on `capture` and `compare_transports` (pass `false` to redact); `inspect_pipeline` defaults `include_raw_auth` to `false`.
   - KeepPeek stores media files and atomically written `capture.json` metadata under `<config-dir>/camera-debug/<capture-id>/` (including `rtp_packets.bin` when raw RTP dump is enabled; failed capture directories are deleted), returning the `capture_id`, saved paths, and automated bitstream anomaly counts (missing SPS/PPS/VPS/IDR, FU-A fragmentation errors, RTP sequence gaps, DTS/PTS regressions, and A/V drift).

3. **Compare transports (`compare_transports`):**
   Compare TCP vs UDP on the camera's resolved primary media protocol (`mode: "media_transports"`) or compare event channels (`mode: "event_channels"`). To compare Reolink `reo_proto` against `rtsp`, run separate `capture` calls:

   ```json
   {
     "action": "compare_transports",
     "camera_id": "192.0.2.50",
     "mode": "media_transports",
     "duration_seconds": 10
   }
   ```

4. **List existing captures (`list_captures`):**
   Inspect retained captures, byte sizes, and `created_at_unix_secs` timestamps (expired 24-hour TTL entries are pruned on list, and oldest captures are evicted FIFO under `max_debug_storage_bytes`).

Note: `capture`, `compare_transports`, and `delete_capture` reject `--read-only` sessions (`X-KeepPeek-MCP-Read-Only: true`).

## 3. Triage Saved Artifacts With Standard CLI Tools

Inspect the saved files inside `<config-dir>/camera-debug/<capture-id>/` directly on the local host:

```sh
# Inspect RTSP, RTP, HTTP, ISAPI, and ONVIF XML control exchanges
tshark -r capture.pcapng -Y "rtsp || rtp || http || xml"

# Follow a decrypted TCP control or RTSP-interleaved stream
tshark -r capture.pcapng -z follow,tcp,ascii,0

# Inspect RTP sequence numbers, timestamps, and H.264 NAL unit types
tshark -r capture.pcapng -T fields -e rtp.seq -e rtp.timestamp -e h264.nal_unit_type

# Inspect muxed MP4 packets and decoded frame timestamps
ffprobe -show_packets -show_frames muxed.mp4

# Check raw Annex-B elementary video bitstream for decode errors
ffmpeg -v error -i video.h264 -f null -
```

## 4. Reproduce and Fix Offline With `ReplayCameraServer`

1. **Clone or fork KeepPeek locally:**

   ```sh
   git clone https://github.com/xnorpx/keeppeek.git
   cd keeppeek
   ```

2. **Export a sanitized `keeppeek.camera-repro.v1` fixture:**
   Call `camera_debugging` with `action: "export_repro_fixture"` and keep credential stripping and network anonymization enabled:

   ```json
   {
     "action": "export_repro_fixture",
     "capture_id": "<capture-id>",
     "strip_credentials": true,
     "anonymize_network": true,
     "trim_media": "headers_and_minimal_failing_slice"
   }
   ```

   - `trim_media` accepts `"headers_and_minimal_failing_slice"`, `"synthetic_payloads"`, or `"keep_raw_frames"`.
   - Always inspect the returned `sharing_safety_report` (`safe_to_share_publicly`, `sanitized`, `strip_credentials`, `anonymize_network`, `trim_media`, `warnings`) before copying the fixture into the repository or an issue.

3. **Write a failing offline reproduction test or replay interactively:**
   - Add a unit or integration test using `ReplayCameraServer::from_file(path)` / `ReplayCameraServer::start(fixture)` from `crates/test-camera/src/replay.rs` (schema `keeppeek.camera-repro.v1`), proving the defect fails before your change.
   - Or run the standalone replay server interactively against a local KeepPeek test instance:

     ```sh
     cargo run --bin keeppeek-camera -- replay --fixture /path/to/repro.json --bind 127.0.0.1:0 --loop
     cargo run --bin keeppeek -- --config /path/to/test-config.toml
     ```

   - Live binary hot-swap via `keeppeek-supervisor` is coming separately; build and test locally against an isolated test configuration or `keeppeek-camera replay`.

4. **Implement the minimal root-cause fix and verify:**
   Fix the root cause in the affected crate (`crates/retina`, `crates/onvif`, `crates/isapi`, `crates/reo-proto`, `src/cameras`, etc.) and verify locally:

   ```sh
   cargo nextest run --locked
   cargo clippy --locked --all-targets -- -D warnings
   cargo fmt --check
   ```

## 5. Submit Sanitized Issue/PR and Disable Debugging

1. **Verify zero sensitive data before sharing:**
   Confirm `sharing_safety_report.safe_to_share_publicly` is `true` and verify that zero credentials, zero tokens, and zero private IP addresses remain in the fixture, logs, issue text, or pull request diff.
2. **File the bug issue and pull request:**
   Follow `.github/ISSUE_TEMPLATE/bug.yml` with the sanitized fixture, failing `ReplayCameraServer` reproduction test, and verification results, then open a PR with the minimal fix and regression test.
3. **Delete local captures and turn off camera debugging:**
   - Call `camera_debugging` with `action: "delete_capture"` (`capture_id` or `delete_all: true`) to remove saved PCAPNG and bitstream artifacts.
   - Stop KeepPeek, set `allow_camera_debugging = false` (or remove it) under `[mcp]` in `config.toml`, and restart KeepPeek.
