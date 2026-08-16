---
name: keeppeek-camera-setup
description: "Discover, authenticate, test, validate, and configure IP cameras for KeepPeek. Use when: finding cameras, managing the private KeepPeek camera config, checking Reolink or ONVIF credentials, comparing reo-proto and Retina transports, validating main/sub MP4 recordings, diagnosing camera stream quality, or recommending camera encoder settings."
argument-hint: "[discover|validate|configure|optimize] [camera name or IP]"
---

# KeepPeek Camera Setup

Use this workflow to discover cameras, verify known credentials, test real streams, validate recorded MP4 output, and merge only proven camera configurations into KeepPeek.

## Safety Rules

1. Never request, paste, echo, log, or summarize passwords, tokens, or camera credentials in chat.
2. Never pass passwords through `--password`, shell variables, environment variables, process arguments, or generated reports during agent-driven runs.
3. Reuse known credentials only through `--credentials-from <local-config>`. If credentials are missing, tell the user to enter them directly in a local config file or terminal outside the agent conversation.
4. Do not guess, brute-force, derive, reset, or change camera passwords. Test only credentials the user already stores locally.
5. Never point `discover-cameras --output` at the live `config.toml`. Discovery output is a complete generated camera table and would replace application and storage settings.
6. Keep every credential-bearing config and generated camera setup artifact under the OS-private KeepPeek config directory, never in the source checkout.
7. Do not print or read a full generated config containing credentials. Inspect only redacted fields such as section names, IPs, models, backends, transports, and profile metadata.
8. Do not add a camera to the live config until authentication, requested streams, MP4 parsing, and FFmpeg decode all pass.
9. Do not mutate camera-side encoder, imaging, alarm, network, time, or recording settings without explicit user approval for the exact proposed changes and a tested rollback plan.
10. Optimization defaults to selecting the best KeepPeek backend/transport and making read-only recommendations. Preserve camera settings when evidence is incomplete.

## Paths

Resolve `<config-dir>` with `keeppeek::config::config_dir()` for the current host:

- macOS: `~/Library/Application Support/keeppeek`
- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/keeppeek`
- Windows: `%APPDATA%\keeppeek`

Use these paths and quote paths containing spaces:

- Live config: `<config-dir>/config.toml`.
- Detailed existing inventory: `<config-dir>/cameras_info.toml`.
- Staged discovery: `<config-dir>/camera-setup/discovered.toml`.
- Staged capabilities: `<config-dir>/camera-setup/cameras-info.toml`.
- Stream tests: `<config-dir>/camera-setup/streams/<camera>/<backend>-<transport>/`.
- Reports: `<config-dir>/camera-setup/report.md` with no secrets.

## Phase 1: Preflight

1. Read repository instructions and the current config structure without reproducing secret values.
2. Check for uncommitted user edits and preserve them.
3. Confirm required commands:

```bash
cargo build --bin discover-cameras --bin camera-stream-test --bin validate-recordings --bin configure-cameras
command -v ffprobe
command -v ffmpeg
```

4. Identify whether KeepPeek is running. Discovery may run alongside it, but stream comparison should be sequential and may require pausing KeepPeek to avoid duplicate high-bitrate sessions.
5. Create staging directories under `<config-dir>/camera-setup/` without touching the live config.

## Phase 2: Discover

Run local discovery using credentials already present in the live config:

```bash
cargo run --bin discover-cameras -- \
  --credentials-from "<config-dir>/config.toml" \
  --output "<config-dir>/camera-setup/discovered.toml" \
  --info "<config-dir>/camera-setup/cameras-info.toml"
```

Only add `--subnet <third-octet>` when the user identifies an additional `/24` network not attached to a local interface. Do not scan arbitrary networks.

Discovery is successful when it reports cameras from HTTP, Baichuan, ONVIF, or RTSP and stages both TOML files. Authentication success is separate from discovery success.

Summarize only:

- Camera IP
- Safe camera name
- Brand and model
- Discovery sources
- Whether known credentials authenticated
- ONVIF port when verified
- Main/sub profile codec, resolution, frame rate, bitrate, and GOP when available

Do not expose credential values or secret-bearing snapshot/stream URLs.

## Phase 3: Build the Candidate Set

1. Parse staged TOML with a structured TOML parser, never regex replacement.
2. Match cameras by IP first. Names are presentation labels and may change.
3. Preserve existing names, UIDs, credentials, storage settings, host, port, and unknown fields.
4. Exclude cameras with empty credentials from automated stream testing. Report them as discovered but unverified.
5. Test one physical camera at a time to avoid saturating the network or camera session limits.

## Phase 4: Validate Streams

For Reolink cameras, try `reo-proto` over TCP first:

```bash
cargo run --bin camera-stream-test -- \
  --config "<config-dir>/camera-setup/discovered.toml" \
  --camera <camera-ip> \
  --stream main,sub \
  --backend reo-proto \
  --transport tcp \
  --duration 20 \
  --output "<config-dir>/camera-setup/streams/<camera>/reo-proto-tcp"
```

Fallback order when that test fails:

1. `--backend retina --transport tcp`
2. `--backend retina --transport udp` only when TCP is unsupported or measured evidence favors UDP
3. For non-Reolink cameras, start with `--backend retina --transport tcp`

Do not run fallback tests after a candidate already meets all success criteria unless the user explicitly requests a transport benchmark.

A stream test succeeds only when:

- Authentication and profile query complete
- Every requested stream connects
- Video frames and keyframes arrive
- No persistent reconnect loop occurs
- One or more finalized `.mp4` files are produced per requested stream
- The process exits cleanly

Record safe metrics in the report: codec, resolution, measured FPS, keyframe FPS, bitrate, maximum frame gap, reconnect count, and selected backend/transport.

## Phase 5: Validate Media

Enumerate every finalized MP4 under the test output using the platform's filesystem tools. For each file:

```bash
cargo run --bin validate-recordings -- \
  --input "<config-dir>/camera-setup/streams/<camera>"
```

This must successfully read every indexed audio/video sample through KeepPeek's local MP4 reader. Then validate with independent tools:

```bash
ffprobe -v error \
  -show_entries format=format_name,duration,size:stream=codec_name,codec_type,width,height,r_frame_rate,duration \
  -of json <recording.mp4>

ffmpeg -v warning -i <recording.mp4> -map 0:v:0 -f null -
```

Capture FFmpeg stderr and treat any warning or error as a failed backend candidate even when
FFmpeg exits with status zero. Try the next backend/transport before proposing camera-side changes.

Reject files when:

- `ffprobe` fails or reports no video stream
- Duration is zero or implausible for the test window
- Dimensions or codec disagree with the queried profile
- FFmpeg reports decode errors
- Main/sub output is missing when requested

Do not add generated MP4 files to git.

## Phase 6: Select KeepPeek Settings

Choose settings from measured results:

- Reolink + successful Baichuan test: `backend = "reo-proto"`, `transport = "tcp"`
- Generic RTSP + successful TCP test: `backend = "retina"`, `transport = "tcp"`
- Use UDP only when TCP failed or a controlled comparison demonstrates a meaningful reliability advantage
- Keep both main and sub profiles when both validate; Peek uses a compatible lightweight live stream and Keep can expose Main/Sub recorded coverage

Read-only optimization recommendations may include:

- Keep a roughly one-second GOP for fast timeline seeks
- Prefer H.264 substream for broad browser live compatibility
- Keep the main stream at native resolution when storage/network measurements support it
- Avoid excessive bitrate reductions that destroy evidence quality
- Enable AAC only when audio is needed and validates cleanly

Do not apply these camera-side changes automatically. For each proposed mutation, show current value, proposed value, expected benefit, risk, validation command, and rollback value, then obtain explicit approval.

## Phase 7: Merge Verified Cameras

1. Stop the running KeepPeek process cleanly before changing its config.
2. Use a structured TOML merge that preserves all root application/storage fields and all existing camera-specific unknown fields.
3. Add or update only IPs that passed stream and media validation.
4. Preserve existing UID and name unless verified discovery provides a better user-approved name.
5. Never remove an existing camera merely because one discovery pass missed it.
6. Write atomically and preserve restrictive file permissions.
7. Never print the resulting secret-bearing config.

Use the repository merger after every listed camera has passed validation:

```bash
cargo run --bin configure-cameras -- \
  --candidates "<config-dir>/camera-setup/discovered.toml" \
  --verified <camera-ip>=reo-proto-tcp \
  --verified <camera-ip>=retina-tcp
```

Repeat `--verified` for each proven camera. The merger preserves the existing section key and UID for a known IP so historical recording paths remain connected.

## Phase 8: Verify KeepPeek

1. Start KeepPeek with the merged config.
2. Verify `/api/cameras` lists every configured camera without exposing secrets.
3. Open a fresh Peek page and verify every configured camera reaches decoded dimensions and the
   `live` state. Do not accept a tile that remains `Connecting` merely because its offer succeeded.
4. Let at least one segment finalize, then verify Keep lists timeline coverage and decodes Main/Sub as configured.
5. Confirm logs have no authentication loops, persistent reconnects, MP4 finalization errors, or retention failures.
6. Keep the backend running only when the user wants to inspect the result.

## Final Report

Report without secrets:

- Discovered camera count
- Authenticated camera count
- Fully validated camera count
- Per-camera model, streams, codec/resolution/FPS, selected backend/transport
- Failed phase and next safe action for unverified cameras
- Config path updated
- Exact test and media-validation results
- Any camera-side optimization recommendations still awaiting approval
