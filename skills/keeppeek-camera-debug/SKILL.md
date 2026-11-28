---
name: keeppeek-camera-debug
description: Capture decrypted camera network traffic, traces, and bitstreams through KeepPeek MCP (local or remote), triage protocol issues, reproduce offline with ReplayCameraServer, and hot-swap live dev builds with keeppeek-supervisor.
---

# KeepPeek Camera Debug

Use this skill to debug live cameras on a local or remote HTTPS KeepPeek server identically, stream correlated protocol traces and decrypted PCAPNG/bitstream artifacts, export a sanitized offline reproduction fixture, and verify a minimal root-cause fix (including live binary hot-swap via `keeppeek-supervisor`) before filing an issue or opening a pull request.

## 1. Opt In Safely (`[mcp].allow_camera_debugging`)

Camera debugging is disabled by default. Stop KeepPeek before editing `config.toml` manually, enable `[mcp]` and `allow_camera_debugging`, and restart KeepPeek (also set `allow_writes = true` if you plan to use `set_logging_filter` or `keeppeek mcp ... logs --filter`):

```toml
[mcp]
enabled = true
allow_writes = false              # Set true only when allowing set_logging_filter / keeppeek mcp logs --filter
allow_camera_debugging = true
debug_camera_ids = []             # Optional allowlist (max 256 IDs); empty = all configured cameras
max_debug_capture_bytes = 67108864  # 64 MiB default (allowed: 1048576..=268435456)
max_debug_storage_bytes = 536870912 # 512 MiB default (allowed: 16777216..=4294967296)
```

> **Security warning:** While `[mcp].allow_camera_debugging = true`, Administrator MCP clients and AI agents have **full raw access to live video bitstreams, audio streams, decrypted network captures (PCAPNG), and camera credentials** for the configured `debug_camera_ids` (or all cameras when empty). KeepPeek emits a startup warning log and a `ServerHealthResponse` warning issue while enabled. Always use an **Administrator** credential (without `--read-only` for capture, delete, or filter-boost actions), scope `debug_camera_ids` to the single affected camera when possible, delete captures when done, and set `allow_camera_debugging = false` as soon as debugging finishes.

Remote HTTPS servers (`https://keeppeek.example.net`) and local servers (`http://127.0.0.1:8081`) use the exact same workflow over `POST /mcp`, `GET /logs`, `GET /logs/snapshot`, and `keeppeek mcp`. Authenticate remote commands with `KEEPPEEK_ACCESS_KEY` or `--credential-file <PATH>`, and pass `--server`, `--config`, `--credential-file`, and `--read-only` **before** the `logs` or `pull-capture` subcommand.

## 2. Remote-First Streaming and Live Capture

1. **Stream live camera traces over SSE (`keeppeek mcp logs`):**
   Monitor live server traces from any local or remote workstation without SSHing to the host. When `--filter` is passed, `keeppeek mcp logs` calls `get_logging_settings` and `set_logging_filter` over `POST /mcp` (requires Administrator + `[mcp].allow_writes = true`, without `--read-only`) and restores the server's unboosted `base_filter` (falling back to `active_filter`) automatically on exit or `Ctrl-C` so concurrent `TraceFilterGuard` boosts aren't persisted on disk:

   ```sh
   # Stream live DEBUG logs for one camera over HTTPS SSE (GET /logs) and auto-restore the filter on exit
   keeppeek mcp --server https://keeppeek.example.net --credential-file ./keeppeek-mcp.key \
     logs --follow --camera 192.0.2.50 \
     --filter "info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug"

   # Fetch the latest bounded snapshot (GET /logs/snapshot) filtered by target and severity
   keeppeek mcp --server https://keeppeek.example.net --credential-file ./keeppeek-mcp.key \
     logs --level debug --target keeppeek::rtsp --camera 192.0.2.50 --limit 100
   ```

   - Available `logs` flags: `--level <trace|debug|info|warn|error>`, `--target <substr>`, `--camera <id>`, `--query <substr>`, `--filter <EnvFilter>`, `--follow`, and `--limit <N>` (default `100`, max SSE tail `1000`).
   - Prints compact JSONL `LogEntry` lines (`sequence`, `timestamp_ms`, `level`, `target`, `message`, `fields`, optional `file`/`line`) to `stdout`.

2. **Inspect the active pipeline (`inspect_pipeline`):**
   Call the Administrator-only `camera_debugging` MCP tool to read the camera's configured backend, active stream state, health status, and profile overrides first:

   ```json
   {
     "action": "inspect_pipeline",
     "camera_id": "192.0.2.50"
   }
   ```

3. **Run a bounded live capture with correlated `traces.jsonl` (`capture`):**
   Collect decrypted PCAPNG traffic, raw video/audio elementary bitstreams, a KeepPeek-muxed MP4, a protocol transcript, and capture-correlated `traces.jsonl` in one call:

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
       "protocol_transcript",
       "traces"
     ],
     "trace_filter": "info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug",
     "duration_seconds": 10,
     "max_bytes": 67108864,
     "include_raw_auth": true,
     "trigger_probe": false
   }
   ```

   - `stream` accepts `"main"`, `"sub"`, or `"both"`.
   - `artifacts` defaults to all six artifacts (including `"traces"`) when omitted.
   - `trace_filter` (optional, `1..=512` bytes, validated as `EnvFilter`) temporarily boosts tracing during the capture under an RAII guard and restores the prior filter automatically on finish, error, or drop. Omitting `trace_filter` applies the built-in `TRACE` protocol boost (`retina=trace,reo_proto=trace,onvif=trace,keeppeek::rtsp=trace,keeppeek::reolink=trace,keeppeek::isapi=trace,keeppeek::cameras=trace,keeppeek::camera_events=trace,keeppeek::camera_debug=debug,keeppeek::media_time=trace,keeppeek::storage::medium_term=trace,keeppeek::keeppeek=trace`).
   - Omit `protocols` to auto-select the camera's primary protocol, or pass a subset of `["rtsp", "audio", "onvif", "isapi", "reolink_http", "reo_proto"]` supported by that camera (listed protocols run sequentially and `capture` fails if any requested protocol is unsupported).
   - `duration_seconds` is bounded to `1..=60`; `max_bytes` is bounded to `65536..=268435456` (`traces.jsonl` is bounded to `1 MiB` inside the capture budget).
   - `include_raw_auth` defaults to `true` on `capture` and `compare_transports` (pass `false` to redact); `inspect_pipeline` defaults `include_raw_auth` to `false`.
   - KeepPeek stores media files, `traces.jsonl`, and atomically written `capture.json` metadata under `<config-dir>/camera-debug/<capture-id>/` (including `rtp_packets.bin` when raw RTP dump is enabled; failed capture directories are deleted), returning the `capture_id`, saved filenames, and automated bitstream anomaly counts (missing SPS/PPS/VPS/IDR, FU-A fragmentation errors, RTP sequence gaps, DTS/PTS regressions, and A/V drift).

4. **Compare transports (`compare_transports`):**
   Compare TCP vs UDP on the camera's resolved primary media protocol (`mode: "media_transports"`) or compare event channels (`mode: "event_channels"`). To compare Reolink `reo_proto` against `rtsp`, run separate `capture` calls. `compare_transports` does not take an `artifacts` toggle; it always uses `CaptureArtifacts::default()` (`traces: true`, recording `traces.jsonl`) and accepts optional `trace_filter`:

   ```json
   {
     "action": "compare_transports",
     "camera_id": "192.0.2.50",
     "mode": "media_transports",
     "duration_seconds": 10
   }
   ```

5. **List existing captures (`list_captures`):**
   Inspect retained captures, byte sizes, filenames, and `created_at_unix_secs` timestamps (expired 24-hour TTL entries are pruned on list, and oldest captures are evicted FIFO under `max_debug_storage_bytes`).

6. **Download artifacts locally (`keeppeek mcp pull-capture` or `read_artifact` fallback):**
   Pull all or selected capture artifacts over `POST /mcp` to your local workstation with automatic SHA-256 verification:

   ```sh
   # Download every artifact for a capture into ./debug-out
   keeppeek mcp --server https://keeppeek.example.net --credential-file ./keeppeek-mcp.key \
     pull-capture <capture-id> --output ./debug-out

   # Or download specific artifacts only
   keeppeek mcp --server https://keeppeek.example.net --credential-file ./keeppeek-mcp.key \
     pull-capture <capture-id> --output ./debug-out \
     --artifact traces.jsonl --artifact capture.pcapng
   ```

   For pure-MCP agents without local CLI shell access, read any artifact in bounded chunks via `camera_debugging` (`action: "read_artifact"`), looping with `offset = next_offset` until `eof` is `true` and checking `sha256`:

   ```json
   {
     "action": "read_artifact",
     "capture_id": "<capture-id>",
     "artifact": "traces.jsonl",
     "offset": 0,
     "length": 65536,
     "encoding": "auto"
   }
   ```

   - `artifact` must be a single-segment safe filename (`1..=128` bytes; no `/`, `\`, `..`, or NUL).
   - `offset` defaults to `0`; `length` is bounded to `1..=131072` (default `65536`); `encoding` accepts `"auto"`, `"base64"`, or `"utf8"` (default `"auto"`).
   - Returns `total_bytes`, `sha256`, `eof`, `next_offset`, `encoding` (`"utf8"` or `"base64"`), and `data`. Works even in `--read-only` mode.

Note: `capture`, `compare_transports`, and `delete_capture` reject `--read-only` sessions (`X-KeepPeek-MCP-Read-Only: true`).

## 3. 3-Tier Tracing Filter Cookbook

KeepPeek enforces a **zero-secret logging rule** across all tiers: passwords, Digest responses, WS-Security nonces, tokens, and inline `rtsp://user:pass@` credentials are never emitted. Session spans (`rtsp_session`, `reolink_session`, `isapi_events`, `onvif_pullpoint`, `camera_events_consumer` with `camera_id` + `camera_ip`) propagate `camera_id`, `camera_ip`/`ip`, and `stream`/`channel` (while `event_snapshot` carries `camera_ip` on the span and `camera_id` on inner events) through `LogHub`, `search_logs`, `GET /logs`, and `traces.jsonl`, and `tracing-log` bridges `retina`'s `log` events automatically.

| Tier  | Directive                                                                                                                                                                                                                           | When to use                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| INFO  | `info,keeppeek=info`                                                                                                                                                                                                                | Production baseline on `stderr` (or `keeppeek-service.log`, not `stdout`): sparse stream connect/disconnect, negotiated codecs/resolution/transport summary, failover, and warnings/errors (`fmt::layer()` skips `camera.rtsp.source_timing` so `LogCaptureLayer::with_formatted_timing` writes its redacted JSON `LogEntry` to `stderr`/service log; `camera.events.status`, `camera.isapi.status`, discovery/probe summaries, Reolink first-audio/session notices, and `retina::codec` no-depacketizer logs are demoted to `DEBUG`). |
| DEBUG | `info,keeppeek=debug,retina=debug,reo_proto=debug,onvif=debug`                                                                                                                                                                      | Resolves ~75% of issues: auth challenges/retries, capability & profile selection, SDP negotiation, SPS/PPS/VPS changes, keyframe cadence, transport fallback, depacketizer drops, RTP gaps, A/V drift, mux errors.                                                                                                                                                                                                                                                                                                                     |
| TRACE | `retina=trace,reo_proto=trace,onvif=trace,keeppeek::rtsp=trace,keeppeek::reolink=trace,keeppeek::isapi=trace,keeppeek::cameras=trace,keeppeek::camera_events=trace,keeppeek::media_time=trace,keeppeek::storage::medium_term=trace` | Resolves 100% of protocol, depacketizer, and muxer issues: RTSP/RTCP headers, per-RTP packet & FU-A/STAP-A internals, Baichuan binary headers & XML, ISAPI Digest/multipart, ONVIF SOAP, per-sample DTS/PTS.                                                                                                                                                                                                                                                                                                                           |

Reference targets by subsystem:

- `retina` (`retina::client::core`, `retina::client::rtp`, `retina::codec::{h264,h265,aac,simple_audio}`)
- `reo_proto` (`reo_proto::session`)
- `onvif` (`onvif::discovery`, `onvif::soap::client`, `onvif::event::client`, `onvif::event::protocol`)
- `keeppeek::rtsp` (`keeppeek::rtsp::blocking`, `keeppeek::rtsp::timing`)
- `keeppeek::reolink` (`keeppeek::reolink`, `keeppeek::reolink::speaker`)
- `keeppeek::isapi` (`keeppeek::isapi`, `keeppeek::isapi::callbacks`)
- `keeppeek::cameras` (`keeppeek::cameras`, `keeppeek::cameras::reolink`, `keeppeek::cameras::hikvision`)
- `keeppeek::camera_events` (`keeppeek::camera_events::pullpoint`, `keeppeek::camera_events::consumer`, `keeppeek::camera_events::snapshot`)
- `keeppeek::media_time`
- `keeppeek::storage` (`keeppeek::storage::medium_term`, `keeppeek::storage::engine`)

## 4. Triage Saved Artifacts (`./debug-out`)

Inspect the downloaded files inside `./debug-out` (or `<config-dir>/camera-debug/<capture-id>/` on the host), correlating packet timing, bitstream decoding, and JSONL traces:

```sh
# Inspect RTSP, RTP, HTTP, ISAPI, and ONVIF XML control exchanges
tshark -r ./debug-out/capture.pcapng -Y "rtsp || rtp || http || xml"

# Follow a decrypted TCP control or RTSP-interleaved stream
tshark -r ./debug-out/capture.pcapng -z follow,tcp,ascii,0

# Inspect RTP sequence numbers, timestamps, and H.264 NAL unit types
tshark -r ./debug-out/capture.pcapng -T fields -e rtp.seq -e rtp.timestamp -e h264.nal_unit_type

# Inspect muxed MP4 packets and decoded frame timestamps
ffprobe -show_packets -show_frames ./debug-out/muxed.mp4

# Check raw Annex-B elementary video bitstream for decode errors
ffmpeg -v error -i ./debug-out/video.h264 -f null -

# Inspect correlated trace lines by camera, target, or sequence
cat ./debug-out/traces.jsonl
```

## 5. Reproduce Offline and Hot-Swap Live Dev Builds

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

3. **Write a failing offline reproduction test (`ReplayCameraServer`):**
   - Add a unit or integration test using `ReplayCameraServer::from_file(path)` / `ReplayCameraServer::start(fixture)` from `crates/test-camera/src/replay.rs` (schema `keeppeek.camera-repro.v1`), proving the defect fails before your change.
   - Or run the standalone replay server interactively against a local KeepPeek test instance:

     ```sh
     cargo run --bin keeppeek-camera -- replay --fixture /path/to/repro.json --bind 127.0.0.1:0 --loop
     cargo run --bin keeppeek -- --config /path/to/test-config.toml
     ```

4. **Implement the minimal root-cause fix and verify offline:**
   Fix the root cause in the affected crate (`crates/retina`, `crates/onvif`, `crates/isapi`, `crates/reo-proto`, `src/cameras`, etc.) and run offline tests:

   ```sh
   cargo nextest run --locked
   cargo clippy --locked --all-targets -- -D warnings
   cargo fmt --check
   ```

5. **Hot-swap and re-verify on the real camera (`keeppeek-supervisor update --channel local`):**
   KeepPeek's slim supervisor supports zero-friction local binary swaps while stateless `POST /mcp` clients stay connected:

   - Start the supervised host once with `--allow-local`:
     - CLI supervisor: `keeppeek-supervisor run --root <dev-supervisor-root> --config <path/to/config.toml> --allow-local --probation-ms 500`
     - Windows service: pass `--allow-local` via Windows SCM `binPath` arguments (per `docs/windows-service.md`, e.g. `keeppeek-service.exe --allow-local` in the service `binPath`)
     - KeepPeek Desktop dev mode: `keeppeek-desktop --manage-local-server --allow-local`
   - Connect your MCP client directly to stateless `POST /mcp` (`http://127.0.0.1:8081/mcp` or HTTPS reverse proxy). On Windows, prefer `POST /mcp` over running `target/debug/keeppeek.exe mcp` directly so Cargo can relink without file locks.
   - Rebuild and swap the live child in one command:

     ```sh
     cargo build --bin keeppeek
     keeppeek-supervisor update --root <dev-supervisor-root> --channel local --binary target/debug/keeppeek --apply --wait --json
     ```

     (Use `target/debug/keeppeek.exe` on Windows.)

   - **Probation and automatic rollback:** If the new binary crashes or fails readiness/probation, `keeppeek-supervisor` automatically rolls back to the previous working slot: `keeppeek-supervisor update --apply --wait` exits non-zero with a rollback error on `stderr`, and `keeppeek-supervisor status --root <dev-supervisor-root> --json` reports `live.core.last_control_rolled_back: true`. You can also trigger `keeppeek-supervisor rollback --root <dev-supervisor-root> --component core --allow-local --apply --wait`.
   - **Re-verify live immediately over `POST /mcp`:** Confirm the new child PID/executable with `get_server_health` (`system.process.pid` and `system.process.executable`), tail live traces with `keeppeek mcp ... logs --follow --camera <id>`, and re-run `camera_debugging` (`capture` + `pull-capture`) against the real camera.

## 6. Submit Sanitized Issue/PR and Clean Up Safely

1. **Verify zero sensitive data before sharing:**
   Confirm `sharing_safety_report.safe_to_share_publicly` is `true` and verify that zero credentials, zero tokens, and zero private IP addresses remain in the fixture, logs, issue text, or pull request diff.
2. **File the bug issue and pull request:**
   Follow `.github/ISSUE_TEMPLATE/bug.yml` with the sanitized fixture, failing `ReplayCameraServer` reproduction test, and verification results, then open a PR with the minimal fix and regression test.
3. **Clean up captures, reset filters, and lock down debugging/MCP:**
   - Call `camera_debugging` with `action: "delete_capture"` (`capture_id` or `delete_all: true`) to remove saved PCAPNG, bitstream, and trace artifacts.
   - If you changed the persistent log filter manually outside `keeppeek mcp logs --filter` or `capture.trace_filter`, reset it via `set_logging_filter` (e.g. `{"filter": "info,keeppeek=debug"}`).
   - Stop KeepPeek, set `allow_camera_debugging = false` (or remove it) under `[mcp]` in `config.toml`, and restart KeepPeek.
   - If the operator wants MCP turned off after the debugging session, call `disable_mcp_access` with `{"confirm": true, "reason": "camera debugging complete"}` to persist `[mcp].enabled = false` and return `404` on `/mcp` live.
