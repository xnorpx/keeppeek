# Local fourth-tier archive

Issue: [#130](https://github.com/xnorpx/keeppeek/issues/130). Base: main `dc76cec`, including
the merged #202 camera identity and runtime-profile recovery work from `54a7dcc`.

The implementation follows the local-first delivery and playback contract in #130. The
broader provider requirements remain open. RAM buffers, active local writers and finalized
local recordings retain their current meanings. ARCHIVE adds independently verified retained
copies; existing named-volume archive moves remain a separate operation.

## Ownership and bounds

One archive worker owns transfers and verification outside the recorder loop. The catalog
actor owns bindings, intents, checkpoints and manifest transitions. Filesystem targets use
existing confined roots and exact owned-file identities. An absent or changed root is an
archive error and never authorizes directory creation or fallback writes.

Copies use a 64 KiB buffer, configured object/queue byte limits, one concurrent transfer,
finite attempts and a cooperative deadline. Filesystem calls can still block in the OS;
the worker cannot promise hard cancellation of an unresponsive mounted filesystem.
Destination readback passes through an independent SHA-256 sink before manifest publication.
The source lease remains held until the readback and durable commit finish.

Archive-backed eviction retains recording, fragment, keyframe and coverage rows. It has an
explicit intent and receipt; holds and readers fence destructive work. Ordinary retention
cannot cascade away an archive-backed recording. Playback opens a pinned archive version
directly and retains its lease. Archive startup/seeking may be slower; no deliberate delay
or whole-file warmup is added to available local playback.

## Implementation order and evidence

1. Target trait, filesystem adapter, durable binding and checkpoint recovery. Verify changed
   roots, links, replacement, cancellation, partial publication and conditional retirement.
2. Journal, admission and worker. Verify finalized-only admission, count/byte limits,
   independent verification, retry/cancel and restart boundaries while recording continues.
3. Archive playback, index preservation, explicit eviction and verified restore. Verify
   first access/repeated access, seeks after restart and eviction, offline/corrupt targets,
   held/read objects and unchanged timeline identity.
4. Administrator configuration, owned probe, selection/status/retry/restore controls and
   operator documentation. Preserve config references and unrelated sections. Protected
   additive WebRTC edits require the separately requested repository approval.
5. Focused tests, adversarial review, measured ingest/playback interference and canonical
   Windows `check.bat`. Record actual evidence here and map it to #130 before PR delivery.

Tasks and acceptance criteria remain tracked in #130; this file records implementation
decisions and verification evidence without marking unobserved issue criteria complete.

## Verification map

The current-delivery criteria below map to #130 in their original order. Broader provider,
selection, remote retention and disaster-recovery requirements remain open in that issue.
Passing local fixtures does not establish those capabilities.

| Criterion                                | Implementation and evidence                                                                            | Observed result                                                                                                                                                                                                                                                              |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Four storage tiers                       | `storage::archive`, configuration reference and recording/operator book chapters                       | Local replica lifecycle is separate from named-volume primary relocation.                                                                                                                                                                                                    |
| Trait and one local implementation       | `ArchiveTarget`, `FilesystemArchiveTarget`; filesystem ownership/version fixtures                      | All 89 archive tests in the canonical package graph pass at `3ac4c66`; no provider dependency or selector added.                                                                                                                                                             |
| Administrator configuration and test     | `server::storage_archive`, configuration round-trip/import/reference tests, archive Chromium workflows | Settings, authorization, revision conflicts and private references pass focused checks; real configuration, probe and restart pass.                                                                                                                                          |
| Independent verification before Archived | Worker transfer/checkpoint tests, catalog Ready admission and republished-source tests                 | Focused suite passes; independent length/SHA-256 readback and trusted chunk evidence precede Ready.                                                                                                                                                                          |
| Durable bounded recovery                 | Filesystem staging/retirement, worker cleanup/probe/restart/quota tests                                | Focused suite passes, including actual probe replay and health RED-to-GREEN regressions.                                                                                                                                                                                     |
| Safe explicit primary eviction           | Worker lifecycle, archive catalog hold/policy fences and reader tests                                  | Pre-existing holds/readers block admission. After durable retirement starts, new hold changes are rejected until recovery completes. Policy activation waits without quarantining or blocking unrelated recordings. Receipt-only replay preserves unverified/offline health. |
| Direct archive playback and restore      | Indexed range/reader fixtures, lifecycle eviction/restart/restore tests                                | Native functional tests and real browser archive playback, seek and restore pass after eviction/restart.                                                                                                                                                                     |
| Timeline playback, seek and measurements | Real-media isolated browser fixture; native indexed/sample/chunk timing fixtures                       | Partial: actual presented-frame and changed-source cold-seek observations are recorded below; sustained fleet nonstarvation remains unverified.                                                                                                                              |
| Outage isolation                         | Offline/replaced/corrupt target tests; invalid Windows ACL diagnostic                                  | Focused suite passes; the real invalid-ACL attempt retained its primary and created no staging file. Live frames and writer timestamps advance across browser copy/restore intervals.                                                                                        |
| Preview, holds and owned-object deletion | Eviction/restore confirmation, SQL intent guards and conditional retirement receipts                   | Partial: explicit primary-eviction/restore confirmation and owned cleanup pass. Independent archive expiry, general deletion preview and provider retention remain later work.                                                                                               |
| Deterministic and performance evidence   | 89 focused archive tests, two quiet native measurements, isolated browser fixture                      | Partial: focused correctness and limited observations pass. Sustained ingest latency, large-fleet interference, pure storage throughput budgets and additional filesystem qualification remain unmeasured.                                                                   |
| Documentation and canonical validation   | Book build, Paper export integrity guard, UI workflows, Windows `check.bat`                            | Book build and focused checks pass; final Windows gate pending.                                                                                                                                                                                                              |

## Native playback measurements

Environment: Windows 10.0.26200, AMD Ryzen 5 5600G (6 cores/12 logical processors), Rust
1.99.0, unoptimized test profile, private NTFS fixture directories on C:. The workspace is on
ReFS; these measurements do not qualify ReFS as an archive filesystem. Each measurement uses
30 samples with a warm or unknown operating-system cache. No other build or browser test ran
during the measured window.

| Operation                                                 | Local median / p95 (microseconds) | Fresh verified archive median / p95 | Fresh minus local median / p95 | Cached verified archive median / p95 |
| --------------------------------------------------------- | --------------------------------- | ----------------------------------- | ------------------------------ | ------------------------------------ |
| Indexed initialization and GOP, 8,510-byte two-sample MP4 | 38 / 75                           | 4,220 / 4,558                       | +4,182 / +4,483                | 1 / 2                                |
| Three sample seeks per iteration, same MP4                | 8 / 9                             | Not measured                        | Different reader lifecycles    | 2 / 2                                |
| 64-byte range in a 12,582,976-byte object                 | 28 / 45                           | 145,015 / 153,436                   | +144,987 / +153,391            | Below 1 / below 1                    |

The large-object archive read verifies one 4,194,304-byte chunk. An unrelated corrupted chunk
does not prevent the selected range from playing; corruption in the touched chunk fails. The
local sample-seek baseline opens a file per iteration, while the archive sample-seek measurement
uses an existing verified reader. These figures characterize their respective paths and do not
establish that archive seeking is faster than local playback.

The exact native binary SHA-256 and emitted measurements are retained locally in
`target/agent-artifacts/archive-playback-native-timings.json`. Both measurements pass **2/2 in
8.31 seconds** at measured code checkpoint `3ac4c66`, including compact catalog replies. The canonical
test binary was invoked directly with `archive_playback_records --nocapture --test-threads=1`,
without another build or browser run. Its SHA-256 is
`7C8B3E9CE43EECFE1B435D9194DD784C3D98BB900438093DBC3BD389973B1CAB`.
Rebuild and reproduce the source harness with:

```powershell
$env:RUSTUP_TOOLCHAIN = '1.99.0'
$env:CARGO_INCREMENTAL = '0'
cargo test --lib archive_playback_records -- --nocapture --test-threads=1
```

These are native I/O and hashing observations, not decoded first-frame, physical cold-cache,
remote retrieval, ingest latency or throughput guarantees. No latency budget is established;
tests enforce byte correctness, integrity boundaries and bounded work instead of a timing threshold.

## Real browser lifecycle and observations

The measured isolated Playwright lifecycle passes **1/1 in 30.7 seconds**, 39.6 seconds including
setup, at code checkpoint `3ac4c66`. Its release backend includes recovery-health, compact reply
and scrub-ownership fixes. The lifecycle exercises save, probe, primary playback,
verified copy, explicit eviction, restart, direct archive playback, changed-source cold seek,
repeat access, restore, another restart, and actual presented playback/seek from the restored
primary with no archive classification. Release recorder SHA-256:
`3DB6EAB6613B65011C9E03B0C3B88547676D432EF8CA2A89BD8566126D20446B`.

Twelve Chromium tests and five archive Bun tests pass, including cold-seek archive classification,
stale cursor callbacks, Close-to-drag recovery, retry, cancellation, LAN-safe UUID generation and
visible failure when no secure RNG is available. Actual RED regressions precede the receipt-health
and scrub/deadline fixes. The final canonical package graph passes all 89 archive tests, including
the receipt-only health replay regression.

Workload: two RTSP fixture cameras use the repository's H.264 640×360, 15 fps clip for both
main and sub streams. One sub stream records while a 40-second, 1,456,425-byte seeded recording
from the other camera is copied/restored. Browser: Chromium 156.0.8078.4, loopback networking,
1280×720 initial viewport; captures use 1280×900 desktop and 390×844 mobile. These small local
fixtures do not qualify a large camera fleet or another disk/filesystem.

| Browser observation (milliseconds, one run per path) | Normal primary | Initial archive after eviction/restart | Repeat archive | Restored primary after restart |
| ---------------------------------------------------- | -------------- | -------------------------------------- | -------------- | ------------------------------ |
| Navigation to actual presented frame                 | 1,486.800      | 1,415.166                              | 1,445.166      | 833.730                        |
| Media load to actual presented frame                 | 18.400         | 16.466                                 | 16.066         | 11.230                         |
| Cold seek to presented frame on a new media source   | 55.800         | 486.700                                | 606.896        | 55.300                         |
| Cold-seek delta from local baseline                  | Baseline       | +430.900                               | +551.096       | -0.500                         |

The first navigation includes Vite/page initialization and is not a controlled before/after
startup comparison. The archive values do not establish faster startup. A cold seek is outside
the current player buffer; it does not mean an operating-system cold cache. Every timing ends
at a real `requestVideoFrameCallback` presentation. There are no percentile or latency-budget
claims from these single browser samples.

Copy completion was first observed after 934.407 ms; restore after 931.790 ms, each for
1,456,425 bytes. These are UI submission-to-observed-completion intervals including control
and refresh polling overhead, not disk bandwidth. Live frame counters advance 1→28 across
copy and 1→13 across restore, and actual writer last-write timestamps advance in both intervals.
The recording-byte counter does not grow in those intervals. This establishes interval progress,
not uninterrupted ingest latency or sustained transfer throughput.

A separate read-only sampler records 52 backend process observations at 500 ms intervals across
four process instances. Resident working set ranges from 74,039,296 to 93,196,288 bytes; largest
sampled private committed memory is 192,184,320 bytes. Observed per-process CPU counter deltas
sum to 3.8125 CPU-seconds; startup and sampling gaps are excluded. Twenty-one unavailable-process
samples occur around process replacement/termination. These samples can miss short
resource spikes and do not establish resource ceilings or ingest budgets.

Complete local observations, server log, ten real screenshots, stopped fixture metadata and
resource samples are preserved in `target/agent-artifacts/archive-lifecycle-3ac4c66/`.
The [UI handoff](130-local-archive-ui-design.md) records Paper comparisons and adaptations.
The full Windows gate at `3601b4a` remains pending until its result is recorded. Focused regressions pass:
receipt cleanup retains unverified/offline health;
Close followed by timeline dragging restores archive/error/retry ownership and rejects stale
completion. Held scrubbing has no normal startup deadline; release or cancellation restores it.

## Measurement applicability at `3601b4a`

The measurements and binary hashes above identify checkpoint `3ac4c66`. They are retained as
measured evidence, not relabeled as new measurements or binary hashes for `3601b4a`. No files
under `src/` or `api/` changed between those checkpoints, so the native 30-sample I/O and hashing
paths are unchanged.

Checkpoint `1530fb5` adds the missing non-Timeline `playerError` alert. Checkpoint `3601b4a`
uses pure predicates for visible media and an empty playback notice. In portrait layout, media
and notice share one grid cell and one maximum height instead of stacking their heights. Media
uses z-index 0 and the notice uses z-index 1; the video stays mounted with nonzero layout dimensions.
While an empty notice covers media controls, those controls are inert. Positive decoded width
and height, a still preview, or a camera-switch preview establish visible media and restore the
controls. Functional component assertions check focus before and after a reported frame.
JavaScript and CSS use the same `47.999rem` breakpoint.

These changes do not alter the measured healthy/archive desktop flow at 1280×720: it has no
player error or empty notice and does not enter the portrait layout rule. The prior native and
desktop observations therefore describe equivalent measured paths at `3601b4a`;
they do not establish identical UI source or new timing observations for the updated checkpoint.

## Final validation

The full Windows gate uses Rust 1.99.0, `CARGO_INCREMENTAL=0` and
`KEEPPEEK_RUN_SLOW_TESTS=1`. The authentication fixture is prebuilt and passed through
`KEEPPEEK_AUTH_E2E_BINARY` so its startup does not regenerate UI artifacts during Playwright.
The full gate at `3ac4c66` passed **3,450/3,450 Rust tests in 610.023 seconds**, with four slow
tests and 27 existing fixtures/benchmarks skipped. Clippy, static checks and the frontend suites
also passed: **417 Bun tests, 295 Chromium component tests and 71 compatibility tests**, with zero
Svelte errors or warnings. Its browser E2E phase ended with **291 passed, 2 skipped and 3 failed**:
recording-history geometry at 320px and 390px, and the absent-footage alert in the export view.
The full gate did not pass. Actual RED evidence is preserved in
`target/agent-artifacts/archive-check-windows-e2e-red.log` and the
`target/agent-artifacts/archive-e2e-red/` directories.

The UI-only fix in `1530fb5` adds absent-footage assertions across all four recording-link modes,
error/retry/close visibility assertions at both mobile widths, and a retained-decoded-frame
component regression. Its full Windows rerun passed **3,450/3,450 Rust tests in 455.796 seconds**,
Clippy and the frontend suites: **417 Bun tests, 296 Chromium component tests and 71 compatibility
tests**. That run was stopped during release E2E preparation to add valid slow-retry coverage.
It produced no browser E2E result and did not complete the full gate.

Checkpoint `3601b4a` preserves the original initial geometry checks and failed-playback actions
and geometry checks. It uses the existing fixture `storedOpenGates` with a resolved first gate
and a held retry gate to keep Retry preparing while the timeline is cached. Actual RED produced
two preparing-state overflow failures; at 390px the event card reached 823px while navigation
started at 766px. Evidence is preserved in
`target/agent-artifacts/archive-mobile-preparing-red.log` and
`target/agent-artifacts/archive-mobile-preparing-red/`.

Focused browser E2E checks at `3601b4a` pass **28/28 in 43.9 seconds** in
`target/agent-artifacts/archive-e2e-regressions-green.log`; focused notice component checks pass
**8/8 in 6.73 seconds** in `target/agent-artifacts/archive-notice-layout-green.log`. Six initial,
failed and preparing captures at 320px and 390px are retained in
`target/agent-artifacts/archive-mobile-notice-green/`; the 320px preparing capture was also
visually inspected. The full canonical rerun at `3601b4a` is in progress; its final outcome
remains pending.

An earlier full Rust run passed **3,450/3,450 in 484.871 seconds**, with 27 existing ignored
fixtures/benchmarks, before Clippy reported archive lint and reply-size findings. Those fixes pass
all-target Clippy with warnings denied, plus **89/89 archive tests in 27.996 seconds**. The refreshed
release lifecycle was measured at `3ac4c66` as recorded above.

The first full run exposed an existing Windows fixture problem in
`ordinary_configuration_load_keeps_the_rollback_guard`. Canonicalizing the temporary directory
produces a verbatim Windows path, which the unchanged storage-root validator rejects. The
fixture-only fix in `ce747b1` keeps a normal drive path on Windows and canonicalizes on other
platforms. Both rollback assertions remain. Actual RED is recorded in
`target/agent-artifacts/archive-config-rollback-path-red.log`; the subsequent configuration suite
passes **95/95 in 5.81 seconds**, and direct profile/identity guards pass **9/9 in 0.09 seconds**.
No production path restriction was weakened. These test-only changes do not alter the measured
release runtime or native playback paths.

The WebRTC responsiveness fixture now sends its Close request inside the documented
`ControlEnvelope`. The foreground reply is obtained while the background media handler is still
blocked, then the worker is released before response assertions. This preserves responsiveness and
in-flight reset checks while avoiding a stranded worker on assertion failure. Actual RED is in
`target/agent-artifacts/archive-webrtc-background-red.log`; the complete canonical-graph WebRTC
group passes **59/59 in 0.432 seconds** after test-only commit `69b432c`. Reproduce with:

```powershell
cargo nextest run --locked --all -E 'package(keeppeek) & test(webrtc::)'
```
