# Adaptive playback (#131)

Issue: https://github.com/xnorpx/keeppeek/issues/131

Branch: `feat/131-adaptive-playback`, based on `7f9e3267d26a3d1fc5cc42b6cc3884ef71a9fe9f`.

## Starting behavior and implementation scope

The shared WebRTC API runtime dropped `EgressBitrateEstimate` events and did not
set its desired send bitrate. The native RTP Auto controller therefore
started on Sub without receiving the information needed to upgrade. The legacy
single-stream path already used that controller. Reuse it and make one shared
session own the bandwidth budget; do not introduce a competing client controller.

This workflow connects transport feedback to native live Auto, reserves
audio and explicit video demand, shares the remaining capacity among Auto
subscriptions, and preserves codec, privacy, keyframe, MID and manual-choice
rules. Reliable data delivery has no equivalent adaptation and must report that
limitation. Session capacity, a camera's allocated budget and received bitrate
have different meanings and must remain separate in diagnostics.

The focused Viewer previously pinned Auto to an exact Main variant. After its
startup preview, it now delegates native selection when all advertised choices
are browser compatible. Explicit choices, codec-limited cameras and the existing
single visible runtime fallback remain exact. Source changes keep the same peer,
MID and mounted video element.

The user approved optional typed subscription diagnostics in existing result
and state messages, synchronized protocol docs, generated bindings and tests.
No new command, feedback endpoint or historical alignment contract is covered.

## Bounds and verification

- Preserve the existing 20 Mbps maximum desired bitrate, three-second upgrade
  hold, one-second downgrade hold and one-second desired-bitrate refresh.
- Count a switching subscription once. Reserve explicit quality and audio
  demand before dividing remaining network capacity among Auto video tracks.
- TWCC reports session capacity; REMB applies to its own MID. Missing evidence
  must not become invented session capacity.
- Keep work proportional to the negotiated session topology. Review existing
  admission bounds before introducing a new externally visible limit.
- Use explicit clocks in deterministic good/congested/recovered and threshold
  traces. Exercise actual subscriptions, negotiated MIDs and keyframe admission.
- First record a failing regression against the existing shared feedback path.
  Report repeated simulated control timings separately from transport delivery,
  browser decoding and physical-camera performance.
- Validate unknown telemetry, replacement cleanup, manual choices and accessible
  diagnostics. Create Paper designs before changing the interface.
- Root owns Cargo and the final Windows `check.bat` gate. Do not overlap builds
  with generated frontend files or full browser suites.

The existing create-session route bounds the decompressed SDP request to 4 MiB.
The bandwidth pass visits the topology admitted by that route; it adds no new
track-count limit. str0m 0.23.1's bandwidth smoother emits changed estimates, not
periodic heartbeats. Keep the latest estimate for the owning RTC lifetime rather
than expiring healthy stable feedback on a guessed timeout. Runtime replacement
starts with unknown capacity; MID removal also removes its REMB limit.

Fixed/manual handovers reserve the larger of the current source's delivery demand
and its requested replacement until admission. Both calculations use the existing
headroom and burst policy. Zero capacity and manual overrides interrupt Auto
upgrade headroom; zero capacity continues downgrade pressure. Automatic H.264
handoffs require a known profile on both the cached candidate and the admitting
keyframe. The packetization/profile predicate is shared with ordinary frame
writing, whose startup and explicit-choice unknown-profile behavior is preserved.

Initial bandwidth feedback can precede SCTP. The application output queue waits
until the control channel opens and initial capabilities have been queued.
Capabilities precede diagnostics on the wire; pre-open deferral retains queue
accounting, and channel failures remain terminal after application readiness.

## Local qualification evidence

The original forwarding baseline failed both initial allocation regressions:
real Auto subscriptions retained a zero budget after feedback. A later regression
also observed an actual str0m event through the shared poll hook and failed with
the same missing-allocation result. The first timed trace fixture did not honor
str0m's timeout/payload scheduling; its output is invalid baseline evidence and
must not be described as a censored timing measurement.

After fixing the owner and fixture, all 11 shared-feedback tests and both
protobuf compatibility tests passed. The control trace replays 20 seeds over
20 simulated seconds with 100 ms scheduling and 500 ms keyframes. These cover
five quantized keyframe phases; they are not independent network benchmarks.

| Simulated control transition |   Median |      p95 | Completed replays |
| ---------------------------- | -------: | -------: | ----------------: |
| Initial promotion            | 3,200 ms | 3,400 ms |             20/20 |
| Congestion downgrade         | 1,200 ms | 1,400 ms |             20/20 |
| Recovery promotion           | 3,200 ms | 3,400 ms |             20/20 |

Every replay observed exactly three source transitions, each on a keyframe.
Queue high-water was two source frames and at most two source subscribers per
switching track. The trace holds source bitrate demand constant as an input;
its small synthetic encoded payloads do not measure received video quality,
ingest bitrate estimation, CPU latency or capture-to-display delay.

Run `cargo nextest run --all -E 'test(shared_feedback_)' --nocapture` to reproduce
the control checks and the `SHARED_AUTO_CONTROL_TRACE` JSON output.

The focused regression sequence reproduced and fixed retained manual handover
capacity, interrupted upgrade holds, incompatible cached and arriving H.264
profiles, and stale subscription state. The last pre-codec run passed 35 tests
and failed exactly the four new codec regressions. After those failures, the
guarded handover patch passed all 98 WebRTC and API checks.

The full browser gate then reproduced premature control-channel flushing in
36 native workflows. The readiness fix passes eight focused native cases:
Camera preview, decoded stored pixels, logging, real workflow operations,
focused Auto, Home Assistant bootstrap budgets, and desktop/mobile TLS login.
The paired-RTC regression checks pre-SCTP retention, real capabilities-first
delivery, exact drained counters, and terminal remote channel closure. Its
fixture follows str0m's input/output drain contract; the earlier loopback
timeout is not native-handshake performance evidence.

Viewer and Dashboard route tests pass all 20 cases, including Auto delegation,
manual choices, compatible exact fallback and return navigation. The focused
policy suite passes 12 cases. Svelte reports zero errors and warnings, and E2E
typechecking passes. Paper boards 61–62 and their native exports are recorded in
`ui/design/paper/131-live-auto/`.

The final Windows `check.bat` passes with Rust 1.99.0, incremental compilation
disabled and `KEEPPEEK_RUN_SLOW_TESTS=1`: 3,490 Rust tests pass with 27 existing
skips; all 817 UI checks pass; and all 297 end-to-end cases pass with two existing
skips. Clippy passes with warnings denied, along with dependency, formatting,
Python, protocol, Paper and accessibility checks. The complete local log is
`target/agent-artifacts/131-check-final4.log`. This run includes the final
behavior-preserving control-handler and browser-assertion helper extractions.

A serial replay confirmed the same 429 Bun tests across all 72 selected files
after Bun printed internal directory-cache diagnostics. No runner configuration,
assertion, test selection or failure threshold was weakened.

The paired-RTC case passes with production server settings and a peer that
disables SNAP, matching the ordinary browser SCTP handshake. Its explicit SDP
and channel-readiness checks prevent a shortcut handshake from bypassing the
pre-open condition. Repeated before/after packet impairment and Auto-specific
decoded-playback performance remain unmeasured.

## Full issue acceptance remains the contract

| Issue requirement                                                   | Evidence needed beyond the current workflow                                                                                     |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 1. Authorized, compatible, aligned same-lineage choices             | Live authorization/codec fences; historical capture-clock and content-view proof remain needed.                                 |
| 2. Stable pressure adaptation with hysteresis                       | Shared native network controller traces; decoder, buffer and viewport pressure remain needed.                                   |
| 3. Live and recorded clocks and intent remain truthful              | Live keyframe/MID checks; aligned recorded handoff and absolute playhead checks remain needed.                                  |
| 4. Explicit quality choices remain respected                        | Auto/High/Low regression coverage; exact historical variant behavior remains needed.                                            |
| 5. Network, decoder, server, source and derived limits are distinct | Typed live diagnostics; full historical and derived diagnostics remain needed.                                                  |
| 6. Derived-service failures cannot break recording                  | Existing publication boundary is not an implemented derived worker; separate implementation and failure evidence remain needed. |
| 7. Tests, accessibility, bounds and performance evidence            | Deterministic control traces and UI checks are distinct from packet impairment and repeated decoded-playback benchmarks.        |

Historical variants currently lack a proven shared capture epoch and bounded
mapping uncertainty. Independent session normalizers and equal UTC ranges do not
prove matching captured content. EventBoost can also alias Main requests to Sub
recordings. Do not enable historical Auto using those assumptions, and do not
claim this workflow closes #131 or replaces #120's exact-frame provenance work.
