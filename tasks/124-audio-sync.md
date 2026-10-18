# Audio synchronization (#124)

Issue: https://github.com/xnorpx/keeppeek/issues/124

Branch: `feat/124-audio-sync`, starting from refreshed main `7f9e326`.
Roadmap #147 ranks this as the next new product feature after archive and guest
sharing. Earlier issues retain their incomplete qualification; this work does not
close or waive #95, #130, #132 or #202. Guest sharing remains separate in draft
PR #312. This task starts locally without pushing or opening another PR.

## Contract

Positive signed milliseconds delay audio. Negative values advance audio. Correction
belongs at the presentation/remux boundary and must preserve source bytes and video
timing. Existing zero-offset entry points retain their behavior. A missing source
clock must remain unavailable or identified as normalized/fallback evidence.

The issue's old video-only live baseline is obsolete: main already has shared live
audio publication/subscriptions and a 256 KiB, 250 ms audio queue. Reuse that owner;
do not enlarge it to implement multi-second correction. Live latency activation
requires a separate reviewed budget.

The proposed recorded setting is -5000..+5000 integral milliseconds, warning above
1000 ms. The owner approved these recorded bounds and additive recorded-audio settings,
preview, diagnostics/export metadata, protocol docs and regenerated bindings/tests.
Live correction and its additional latency budget remain separately gated.

## First increment

1. Add deterministic known-skew audio/video remux fixtures and independently inspect
   emitted sample timestamps, composition offsets, durations and payload identities.
2. Add one checked signed mapper, rounding correction to the nearest track tick with
   symmetric handling of negative values. The maximum rounding error is half a tick.
3. Route export remux audio through it. Clip complete audio samples whose corrected
   decode or presentation start precedes zero; never clamp timestamps or manufacture
   compressed silence. Keep the video anchor and original recordings intact.
4. Prove positive/negative corrections, zero behavior, fragment/recording gaps,
   cancellation, overflow and missing-audio handling before exposing settings.

Whole-sample leading clipping is a proposed implementation boundary, not a claim of
sample-accurate content trimming or approved end-user precision. Inspect actual MP4
output because the existing writer can rewrite predecessor sample durations.

## Following increments

- Persist inherited/effective per-main/sub recorded offsets through existing camera
  configuration, full-candidate validation and its atomic writer.
- Reuse the mapper for bounded stored-media delivery with seek/rate/generation
  handling and correction-aware cache identity.
- Snapshot effective timing and revision in export attempts/history for stable Retry.
- Add approved settings, preview, diagnostic and export metadata contracts; regenerate
  bindings and contract tests. Do not overload unrelated payloads.
- Design camera Audio and Keep calibration states in Paper before UI changes.
- Integrate live correction with explicit latency/memory budgets, reset semantics
  and truthful provenance; complete decoded fixture, browser, hardware and repeated
  before/after performance qualification.

## Acceptance evidence

All seven original issue criteria remain open: operator settings, preserved original
timing evidence, transition stability, bounded observable live latency, verified
corrected exports, truthful diagnostics and complete fixture/config/playback/export/
reconnect/hardware verification. A pure mapper or green repository gate alone does
not complete these outcomes.

The initial source audit found independent audio/video timestamp normalization,
receive-time fallback in recording, uncorrected stored fragments, and source-only
browser-remux cache names. These limitations must stay visible until qualified.

## Sources and validation

The [W3C ISO BMFF byte-stream format](https://www.w3.org/TR/mse-byte-stream-format-isobmff/)
defines track decode times in media fragments. The
[MSE coded-frame processing algorithm](https://www.w3.org/TR/media-source-2/#sourcebuffer-coded-frame-processing)
applies buffer timestamp offsets; one multiplexed buffer is not an independent
per-audio correction control. Server remux keeps the correction in the owning track.

The first private remux checkpoint used Rust 1.99.0 and disabled incremental
compilation. `cargo test --locked -p keeppeek --lib storage::playback::audio_ --
--nocapture` first passed ten tests and failed five expected behavioral cases because
the export path ignored correction. After implementation, all 15 cases passed in
0.10 seconds. The positive-source-leading fixture and explicit zero timeline were
refined after the first red run; that run retains its original fixture meaning.

The broader `cargo test --locked -p keeppeek --lib storage::playback -- --nocapture`
passed 25 tests in 2.51 seconds. `cargo clippy --locked -p keeppeek --lib -- -D warnings`
passed in 57.56 seconds. Independent mapping and remux reviews found no blocker.
Logs are retained under `target/agent-artifacts/124-audio-remux-{red,green}.log`,
`124-playback-checkpoint.log` and `124-clippy-checkpoint.log`.

These tests inspect emitted MP4 DTS/PTS, durations, composition offsets and source
SHA-256. AAC marker payloads qualify structural byte-preserving remux; they do not
qualify decoded lip synchronization. The correction entry point remains private;
operator settings, preview, stored playback and export snapshots follow next.

Full issue completion still requires the canonical Windows gate, real decoded/browser
timing and the original performance, live and hardware evidence.
