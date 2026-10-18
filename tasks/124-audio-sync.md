# Audio synchronization (#124)

Issue: https://github.com/xnorpx/keeppeek/issues/124

Branch: `feat/124-audio-sync`, starting from refreshed main `7f9e326`.
Roadmap #147 ranks this as the next new product feature after archive and guest
sharing. Earlier issues retain their incomplete qualification; this work does not
close or waive #95, #130, #132 or #202. Guest sharing remains separate in draft
PR #312. Work began locally; publishing this recorded-only increment keeps the
remaining #124 criteria open.

## Contract

Positive signed milliseconds delay audio. Negative values advance audio. Correction
belongs at the presentation/remux boundary and must preserve source bytes and video
timing. Existing zero-offset entry points retain their behavior. A missing source
clock must remain unavailable or identified as normalized/fallback evidence.

The issue's old video-only live baseline is obsolete: main already has shared live
audio publication/subscriptions and a 256 KiB, 250 ms audio queue. Reuse that owner;
do not enlarge it to implement multi-second correction. Live latency activation
requires a separate reviewed budget.

The approved recorded setting is -5000..+5000 integral milliseconds, warning above
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

Whole encoded sample clipping is the implemented leading-edge boundary. It does not
claim sample-accurate content trimming or end-user synchronization precision. Actual
MP4 output is inspected because the writer can rewrite predecessor sample durations.

## Recorded extensions

- Inherited/effective per-main/sub recorded offsets use existing camera configuration,
  full-candidate validation and its atomic writer.
- Bounded stored-media delivery reuses the mapper with seek/rate/generation handling.
  Corrected delivery is uncached; zero-offset cache behavior remains unchanged.
- Export attempts/history snapshot effective timing and revision for stable Retry.
- Approved settings, preview, diagnostic and export metadata contracts include
  regenerated bindings and contract tests.
- Camera Audio and Keep calibration states were designed in Paper before UI changes.

Live correction still needs explicit latency/memory budgets, reset semantics and
truthful provenance. Broader transition/reconnect, hardware and repeated before/after
performance qualification remain open.

## Acceptance evidence

The seven original issue criteria cover operator settings, preserved original timing
evidence, transition stability, bounded observable live latency, verified corrected
exports, truthful diagnostics and complete fixture/config/playback/export/reconnect/
hardware verification. The recorded evidence below qualifies part of that contract;
live correction, original clock provenance and broader qualification remain open.
A green repository gate alone does not complete the full issue.

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

These first-checkpoint tests inspect emitted MP4 DTS/PTS, durations, composition
offsets and source SHA-256. AAC marker payloads qualify structural byte-preserving
remux; they do not qualify decoded lip synchronization. That checkpoint kept the
correction entry point private. The recorded workflow below adds operator settings,
preview, stored playback and export snapshots.

The recorded checks below qualify the exercised synthetic decoded and browser media
paths; they do not complete #124 or establish acoustic/hardware synchronization.
The final canonical Windows gate passed as recorded below. Live latency, broader
transition/reconnect, hardware and repeated before/after performance evidence remain open.

## Recorded workflow

The recorded increment now includes validated camera default/Main/Sub settings, the
approved additive contracts and generated bindings, offset-only settings persistence,
stored playback and preview generations, immutable export snapshots and recovery.
Resolution follows the actual stored stream, including EventBoost's Sub recordings.
Settings writes retain the complete configuration revision, private references and
camera identity. A partial nested reset replaces the numeric settings table so a
removed stream override cannot survive on disk.

Corrected playback reads pinned neighboring audio fragments, keeps one video clock
across refills, and uses timestamp-based MSE segments. Fresh generations reset the
clock. Cursor incarnation checks reject work from a closed/reopened public ID;
initialization and fragment identifiers increase across refills. Zero-offset
delivery retains source fragment bytes. Two workers, 32 source periods, a 40-second
covered span, 64 MiB source/output bounds and one 30-second work deadline limit each
corrected batch; these are not a measured RSS or playback-latency guarantee.

Each client cursor serializes at most eight active/queued server operations. This
prevents pause, refill, seek and preview commands from rejecting each other's stale
snapshots. The video element pauses immediately; its server update waits behind
already-active bounded work. Closing disposes local media immediately and sends
Close directly. Queued commands cannot reopen or mutate a disposed cursor.

An export captures the actual recording stream and recorded timing policy together.
Retry retains that policy after offset changes and rejects a changed source stream
before retiring the previous attempt. Unknown legacy snapshots remain unknown.
Diagnostics use earliest original container PTS, distinguish inspected empty audio
from unknown data, and do not claim preserved RTP clocks or measured lip-sync.

Paper boards 57-60 and their desktop/390/320 references are recorded in
[the UI design note](124-audio-sync-ui-design.md). The existing 40 board records and
all earlier artifact bytes remain unchanged. Preview never saves settings or selects
an export mode. Initial-open recovery requires the recorded-audio capability and an
explicit click to open a temporary zero-offset preview.

Independent review found and reproduced partial-reset persistence and low-bitrate
AAC-description failures. The fixes keep runtime/reload settings consistent and
register source descriptions before interleaved media. Their original assertions
and source-integrity checks remain in place. Decoded qualification uses the small
checked-in flash/tone recording and strict decoded frame/PTS pairing; its log parser
also handles carriage-return progress output. Final validation evidence follows.

The first final media pass completed 41 tests in 2.57 seconds, including independent
AAC decoding. The browser calibration passed one real WebRTC/MSE test in 18.8
seconds, with AAC bytes decoded, video frames presented across two initialization
periods on one SourceBuffer, signed previews, reset, seek and a fresh second-period
cursor. These are media transport and decoded timing checks, not a browser acoustic
measurement or camera hardware qualification.

A later independent review reproduced an export edge-selection bug: three of four
new tests failed because shifted audio needed neighboring source intervals and
corrected packet selection at the exclusive end. The zero-offset byte-parity case
passed. The original failure is retained in `124-export-context-red.log`.

The product paths support nonzero correction for AAC with zero audio composition
offset. Other AAC composition offsets are unsupported because the time index cannot
bound their neighboring-source lookup. Signed video composition offsets and the
private mapper's signed arithmetic remain supported. Export edge lookup has a
32-period and 64 MiB indexed-range limit, with at most 512 total source periods.
Overlapping indexed source periods and conflicting descriptions of one period fail
explicitly for corrected exports. A second regression run reproduced all three
missing checks: overlap rejection, conflicting metadata and a 513-period union.
The six existing contextual export cases still passed; original zero-overlap
behavior remains covered. Evidence: `124-export-overlap-red.log`.

UI static validation, mdBook, Mermaid assertions, Wiki types, source checks and the
66-page/82-file Wiki build passed. The additional Wiki unit suite passed 82 tests
and failed three existing case-sensitive filesystem assertions on Windows; this
work does not change those tests or Wiki publication code. The canonical Windows
gate remains the required final repository check.

## Final recorded qualification

The implementation is committed locally at
`37cc3dcf5fabf98cba709a4d370c3be32a23b3cc`, on main base
`7f9e3267d26a3d1fc5cc42b6cc3884ef71a9fe9f`. Windows validation uses Rust 1.99.0,
`CARGO_INCREMENTAL=0`, `KEEPPEEK_RUN_SLOW_TESTS=1`, Bun 1.4.0, Node 24.22.0,
Python 3.12 and FFmpeg 9.0.2.

The final contextual media tests passed 52 cases in 2.33 seconds, including independent
AAC decoding, preserved source hashes, neighboring audio, exclusive-end selection,
overlap/conflicting-metadata rejection and the total-period limit. Their original
failure evidence remains in `124-export-context-red.log` and
`124-export-overlap-red.log`; the final pass is `124-export-overlap-green.log`.

The first full browser run exposed an asynchronous UTC-day URL assertion and a
playback alert after successfully decoded media. A deterministic held-refill/pause
test independently reproduced the stale-cursor rejection. The serialized client
fix preserves current errors, latest intent, bounded admission and immediate local
disposal. Its original failing test remains in `124-cursor-operations-red.log`;
all 40 shared-client tests passed in `124-cursor-operations-all-green.log`.
The first browser artifact did not include the raw control error, so it does not
prove the attribution of that incident. Subsequent evidence includes bounded
control-error capture without recording authentication or media payloads.

The original exact export-card geometry also exposed clipped Download content.
The fix retains captured timing inline and uses the existing footer for unknown
legacy timing. The original lane assertions remain, with stronger descendant-bound
checks. Six focused Chromium tests passed in `124-export-story-green.log`.

Full UI quality passed static/type/format checks, 435 Bun tests, 313 Chromium tests
and 93 compatibility tests. Three new protobuf test-fixture objects needed canonical
message construction; the full typecheck passed after that correction, with no
casts, suppressions or removed assertions. Evidence: `124-ui-all-final.log`.

Both failed browser cases then passed three parallel repetitions each, six tests in
24.6 seconds (`124-e2e-regressions-final.log`). The complete native/browser suite
passed 298 tests in 2.8 minutes with two existing skips (`124-e2e-all-final.log`).
The calibration case passed all six media phases with decoded AAC, presented frames
and no surfaced media error. Its artifact is retained in
`124-recorded-audio-full-green-artifacts/recorded-audio-observations.json`.
The bounded control probe retained five other REJECTED replies with omitted text
and no capture overflow. Existing Administrator-only health polling explains expected
role denials for this User session, but exact attribution of all five replies is
not established. This evidence does not claim a protocol-error-free session.

On 2026-10-18, the complete required `check.bat` rerun finished with exit 0:

- Rust: 3,534 passed in 489.174 seconds, including four slow tests; 27 existing skips.
- Strict all-targets Clippy, unused-dependency and Rust/TOML/Python formatting checks passed.
- UI: static checks, 435 Bun, 313 Chromium and 93 compatibility tests passed.
- Native/browser E2E: 298 passed in 2.9 minutes; two existing skips.

Evidence: `target/agent-artifacts/124-check-windows.log` and
`124-recorded-audio-canonical-green-artifacts/recorded-audio-observations.json`.
The final result comes from the complete entry point, with the slow-test flag enabled;
it does not combine partial runs or waive the earlier failures.

Live correction and its reviewed latency budget, original capture-clock provenance,
broader reconnect/hardware testing and repeated before/after performance qualification
remain open. This recorded increment does not close #124 or claim physical lip-sync
or browser audio-device latency qualification.
