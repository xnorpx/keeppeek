# Event pre-recording performance qualification

The final controlled 90-run aggregate meets the 5% admission budget in all 16 workloads, with no aggregate disabled overhead. Actual-writer qualification passes all eight workloads with 30 runs each. One enabled process pair exceeds the budget; the aggregate, that exception, uncontrolled failures, and measurement limits are all retained below.

## Final measured results

| Gate                             | Result                                                                                                                            |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| Enabled median/p95 overhead <=5% | All 16 aggregate workloads pass; worst aggregate change 0%                                                                        |
| Disabled added overhead          | All 16 aggregate workloads pass; all 127-camera medians/p95s match baseline in all three pairs                                    |
| Disabled history                 | Zero constructed history instances and retained bytes                                                                             |
| Actual writer                    | All 8 workloads pass; every source has finalized media/catalog fragments, zero recorded health failures, zero pending queue bytes |

Disabled distributions are not claimed identical: five single-camera workloads improve. The final executable is `ec67771afe5532c5f4abe027b0a50b420c416fcad9c29dd33d6532a30af6330b`, equivalent to implementation commit `8a6525394ce3261ad54ea5a63b2cdcf179f6b9a3`. All 1,802 production/build/harness source-file hashes were captured before compilation and matched when rechecked immediately after timings. Later changes comprise the CI prerequisites in commit `2e9dd84` and canonical regeneration of `ui/src/lib/proto/webrtc_pb.ts`, which updates only the generator-version comment from `protoc-gen-es v2.14.1` to `v2.15.0`, plus CRLF-to-LF normalization of `api/webrtc.md` with identical text. Generated binding code, schemas, Rust runtime, and benchmark logic are unchanged. The original measurement hashes remain intact; the manifest records both new file hashes as narrow post-measurement exceptions. The other 1,800 captured source-file hashes still match. The [full build manifest](verification/event-preroll/optimization-5/build.json) maps this equivalent build to the source.

| Fixture              | Cameras | Disabled median/p95 change | Enabled median/p95 change |
| -------------------- | ------: | -------------------------: | ------------------------: |
| 1080p-h264-gop1.mp4  |       1 |              +0.0% / +0.0% |           -66.7% / -50.0% |
| 1080p-h264-gop1.mp4  |     127 |              +0.0% / +0.0% |            -33.3% / +0.0% |
| 1080p-h264-gop10.mp4 |       1 |            -50.0% / -20.0% |           -50.0% / -60.0% |
| 1080p-h264-gop10.mp4 |     127 |              +0.0% / +0.0% |           -33.3% / -25.0% |
| 1080p-h265-gop1.mp4  |       1 |            -25.0% / -20.0% |           -75.0% / -60.0% |
| 1080p-h265-gop1.mp4  |     127 |              +0.0% / +0.0% |           -33.3% / -25.0% |
| 1080p-h265-gop10.mp4 |       1 |             -50.0% / +0.0% |           -50.0% / -60.0% |
| 1080p-h265-gop10.mp4 |     127 |              +0.0% / +0.0% |           -33.3% / -25.0% |
| 2160p-h264-gop1.mp4  |       1 |              +0.0% / +0.0% |           -66.7% / -50.0% |
| 2160p-h264-gop1.mp4  |     127 |              +0.0% / +0.0% |           -33.3% / -25.0% |
| 2160p-h264-gop10.mp4 |       1 |             -50.0% / +0.0% |           -50.0% / -50.0% |
| 2160p-h264-gop10.mp4 |     127 |              +0.0% / +0.0% |            -33.3% / +0.0% |
| 2160p-h265-gop1.mp4  |       1 |              +0.0% / +0.0% |           -66.7% / -50.0% |
| 2160p-h265-gop1.mp4  |     127 |              +0.0% / +0.0% |            -33.3% / +0.0% |
| 2160p-h265-gop10.mp4 |       1 |             -50.0% / +0.0% |           -50.0% / -50.0% |
| 2160p-h265-gop10.mp4 |     127 |              +0.0% / +0.0% |            -33.3% / +0.0% |

The predeclared enabled pair 1 misses for 4K H.265/GOP10 with 127 cameras: its median of run p95s is 0.45 us versus 0.40 us (+12.5%). Pairs 2 and 3 pass all 16 workloads. The reported aggregate includes every one of the 90 measured runs, including that pair; no run or block was removed. [Disabled blocks and A/A controls](verification/event-preroll/optimization-5/affinity/summary.json) and [enabled blocks, aggregate, and spread](verification/event-preroll/optimization-5/affinity/enabled-summary.json) link the complete evidence.

## Actual writer

The final sink uses normal full processor affinity, preserving producer/writer concurrency. Values below use 30 measured runs per workload; p95 is nearest-rank over those 30 runs. Every individual source must have a finalized, sample-bearing MP4 and catalog fragments, with no active files left. The monotonic failure counter must remain zero.

| Sink fixture         | Cameras | First commit median/p95 (ms) | Flush median/p95 (s) | Median MiB/s |
| -------------------- | ------: | ---------------------------: | -------------------: | -----------: |
| 1080p-h264-gop1.mp4  |       1 |                    4.11/4.44 |          0.029/0.030 |        251.8 |
| 1080p-h264-gop10.mp4 |       1 |                    6.18/6.53 |          0.018/0.020 |        379.2 |
| 1080p-h265-gop1.mp4  |     127 |                    6.07/7.33 |          3.797/3.901 |        153.3 |
| 1080p-h265-gop10.mp4 |       1 |                    5.46/5.78 |          0.019/0.020 |        259.1 |
| 2160p-h264-gop1.mp4  |       1 |                    4.38/5.55 |          0.032/0.041 |        579.5 |
| 2160p-h264-gop10.mp4 |       1 |                    8.59/9.63 |          0.022/0.025 |        825.2 |
| 2160p-h265-gop1.mp4  |     127 |                 95.57/116.58 |          3.811/3.893 |        346.4 |
| 2160p-h265-gop10.mp4 |       1 |                    8.00/9.54 |          0.021/0.024 |        738.9 |

Both 127-camera workloads flush the submitted 6-second live-media span in less than 4.65 seconds in every measured run. Bounded producer waits are included in the [raw sink results](verification/event-preroll/optimization-5/sink.json); [sink summaries](verification/event-preroll/optimization-5/sink-summary.json) include ranges. This demonstrates the tested burst workload keeping up, not arbitrary camera or disk throughput.

The largest retained encoded history across final enabled blocks was 268,435,420 bytes against the 268,435,456-byte global cap; the largest stream was 16,752,406 bytes against 67,108,864. No pending input remained after any measured run.

## Controlled comparison and limits

Admission measurements use logical CPU 2 (affinity mask 0x4) from process creation, inherited Normal priority, and the same unchanged baseline/candidate executables and fixture hashes. The disabled sequence was predeclared A/B,B/A,A/B; enabled used A/E,E/A,A/E, each process with three warmups and 30 measured runs per workload. A is baseline, B disabled, E enabled. Raw reports for all 90 runs per mode are retained. Aggregate values are the median of all run medians and the median of all run p95s, as in the original comparison; they are not batch-average substitutes for frame latency.

Process-block state is correlated. The report shows paired-block differences and median-of-block results alongside the aggregate and does not bootstrap 90 contiguous runs as independent observations. Baseline A/A blocks 1 and 3 match every metric; block 2 has one single-camera 1080p H.265/GOP10 p95 step from 0.4 to 0.5 us, then returns. This warns against claiming narrow statistical certainty from the per-run samples.

Windows QPC is 10 MHz on this host, a 100 ns tick. Five percent of a 0.4 us baseline is 20 ns, below one tick. The unchanged 5% criterion passes for the measured aggregate, but this clock cannot establish a universal 20 ns upper bound. The single paired-block miss and uncontrolled earlier regressions remain visible; fixed affinity does not erase them. Five single-camera disabled cases have favorable differences, so the result means no measured added overhead, not identical distributions.

The enabled first block overlapped a 4.99-second implementation commit/push during 00:11:20-00:11:40 UTC on 2026-09-27, without hooks or source edits. A 0.34-second YAML formatting check occurred near the end of the enabled window. These brief activities are disclosed, and affected runs were retained. CI-only commit/push at 00:27:48-00:27:54 UTC occurred while timings were stopped. Remote CI execution did not consume this host's compute.

## Workloads and boundaries

`benches/event_preroll.rs` consumes fixed encoded H.264 and H.265 MP4 fixtures at
1920x1080 and 3840x2160, 15 frames/s, with 1-second and 10-second GOPs. The fixture
preparation script records SHA-256 hashes, encoder commands, FFmpeg version, and
ffprobe output. Each workload has three warmups and at least 30 measured runs.
Admission workloads exercise both one camera and 127 cameras, with overlapping
triggers at 6, 7, and 8 seconds. Sources reuse immutable `Bytes` payloads from
the fixed fixtures; encoded-byte accounting still charges each retained frame
to its source. This isolates recording work from capture and codec generation.

The per-frame timer surrounds production `RecordingAdmission::ingest_at` only.
Fixture cloning, queue draining, history selection, and `ShortTermBuffer` work
occur outside this timer. The report includes total simulated-pipeline work separately, including admission,
fixture and identity cloning, triggers, consumer processing, and byte-counter observation.
The historical baseline is `e4f8289`, with the identical measurement-only harness
added in an isolated worktree. Both builds use the release profile, including optimization level 3 and overflow
checks. Cargo benchmark targets use unwinding instead of the production binary's
abort panic strategy; this is identical for the baseline and candidate. The
benchmark profile is not used.

A second workload uses the actual `StorageEngine`, media writer, and filesystem.
It measures trigger-to-first-committed-frame observation and completed MP4 bytes
per elapsed flush second, including finalization. Commit observation polls the real catalog for a committed fragment
after each one-millisecond pause; it is an observation upper bound, not a durable-fsync claim.
The actual sink retains the fixture's original receive-clock cadence on one
shared origin 12 seconds in the past. Frames before offset 6 seconds fill history;
the accepted event is timestamped at offset 6 seconds, then the remaining frames
are submitted in bursts. The measured event-to-commit interval starts when the
event command is submitted, so this is delayed/backlog submission, not paced live
capture. Sink history is set to 30 seconds to retain the first keyframe of the
10-second GOP fixture until submission; admission workloads retain their fixed
10-second setting. Both use the same 64/256 MiB byte caps.
The producer waits for the bounded input queue between camera bursts, and this
wait is reported. A benchmark-only monotonic health counter detects writer and queue failures even
if later progress recovers them. Any such failure, missing finalized output or
catalog fragments for an individual source, or pending input at shutdown fails
the workload. Admission latency alone does not qualify writer
throughput.

History counters include retained encoded bytes, not allocator overhead. Every
admission workload asserts the 64 MiB per-stream and 256 MiB global ceilings.
Disabled mode asserts that no pre-recording history instance is constructed.
The source-level lazy construction invariant supplements this counter; the
counter is not a general-purpose heap profiler.

## Reproduction

Run builds serially. Stop compilation and other CPU-intensive workloads before
collecting timings. Use the same host, fixtures, toolchain, release settings, and
filesystem for every comparison.

```powershell
python benches/event_preroll/prepare_fixtures.py
cargo bench --locked --profile release --bench event_preroll --features event-preroll-benchmark-current --no-run
$env:KEEPPEEK_PREROLL_BENCH_RUNS = '30'
$env:KEEPPEEK_PREROLL_BENCH_MODE = 'disabled' # repeat with enabled
$env:KEEPPEEK_PREROLL_BENCH_OUTPUT = 'target/preroll-disabled.json'
$env:KEEPPEEK_PREROLL_FIXTURES = (Resolve-Path target/event-preroll-fixtures).Path
# Save the reported candidate executable as target/event-preroll-binaries/candidate5.exe.
cmd /c 'start "" /b /wait /affinity 4 target\event-preroll-binaries\candidate5.exe'
```

In the baseline worktree, copy only the benchmark harness and feature/bench
registration, expose the measurement module in `storage/engine.rs`, and build
with `--features event-preroll-benchmark`. Set mode to `baseline` and point
`KEEPPEEK_PREROLL_FIXTURES` at the same generated fixture directory. Save its executable as `baseline.exe` beside the candidate and launch it with the same `start /b /wait /affinity 4` command. Run the predeclared A/B, B/A, A/B and A/E, E/A, A/E process sequences described above, saving a distinct JSON output for each process. Every process uses 30 measured runs per workload; retain all 90 runs for each mode.

For actual writer measurements, set mode to `sink` and set
`KEEPPEEK_PREROLL_SINK_ROOT` to a dedicated empty directory on the documented
recording filesystem. Launch the sink executable directly, without an affinity restriction. The harness creates unique per-run child directories and
removes only those children after successful verification.

```powershell
python benches/event_preroll/compare.py target/preroll-baseline.json target/preroll-disabled.json target/preroll-enabled.json target/preroll-comparison.json
```

`compare.py` reproduces the initial single-process summaries, including deterministic bootstrap intervals. Those intervals are not used as a pooled confidence claim for the final correlated 90-run experiment. The final block JSON summaries report the median of all 90 run medians/run p95s, median-of-block values, and every paired-block difference. The 5% gate remains unchanged. Favorable disabled differences are reported separately from equality; neither a zero point estimate nor a confidence interval containing zero proves strict equivalence.

## Host and results

The current qualification host has an AMD Ryzen 5 5600G (6 cores, 12 logical
processors), 16,473,337,856 bytes of physical memory, and Windows 11 Pro x86-64 (10.0.26200, build 26200). Rust is 1.98.1
(48a229cea, 2026-09-01), and Cargo is 1.98.1 (797e8a9bc, 2026-08-05). The build
and fixture cache is on E: ReFS. Actual sink qualification uses an explicitly
selected C: NTFS directory. Exact OS/toolchain versions, fixture hashes, filesystem details, raw results, and build identities are retained under [verification/event-preroll](verification/event-preroll/host.json).

## Initial measured result

The [baseline](verification/event-preroll/baseline.json) and
[initial candidate raw runs](verification/event-preroll/before-optimization/enabled.json)
use matching fixture hashes and 30 runs per workload after three warmups.
[Full comparisons](verification/event-preroll/before-optimization/comparison.json)
include deterministic bootstrap intervals and minimum, quartiles, and maximum.

Disabled mode passed all 16 workloads: median and p95 changes were 0%, both
bootstrap intervals contained zero, and retained history/instance counts were
zero. Enabled mode passed all eight single-camera cases and failed p95 in all
eight 127-camera cases. The 5% threshold remains unchanged.

| Enabled workload     | Cameras | Baseline p95 (us) | Candidate p95 (us) | Change |
| -------------------- | ------: | ----------------: | -----------------: | -----: |
| 1080p-h264-gop1.mp4  |       1 |              0.40 |               0.30 | -25.0% |
| 1080p-h264-gop1.mp4  |     127 |              0.40 |               0.50 | +25.0% |
| 1080p-h264-gop10.mp4 |       1 |              0.40 |               0.30 | -25.0% |
| 1080p-h264-gop10.mp4 |     127 |              0.40 |               0.50 | +25.0% |
| 1080p-h265-gop1.mp4  |       1 |              0.40 |               0.30 | -25.0% |
| 1080p-h265-gop1.mp4  |     127 |              0.40 |               0.50 | +25.0% |
| 1080p-h265-gop10.mp4 |       1 |              0.40 |               0.30 | -25.0% |
| 1080p-h265-gop10.mp4 |     127 |              0.40 |               0.50 | +25.0% |
| 2160p-h264-gop1.mp4  |       1 |              0.40 |               0.30 | -25.0% |
| 2160p-h264-gop1.mp4  |     127 |              0.40 |               0.55 | +37.5% |
| 2160p-h264-gop10.mp4 |       1 |              0.40 |               0.30 | -25.0% |
| 2160p-h264-gop10.mp4 |     127 |              0.40 |               0.50 | +25.0% |
| 2160p-h265-gop1.mp4  |       1 |              0.40 |               0.30 | -25.0% |
| 2160p-h265-gop1.mp4  |     127 |              0.40 |               0.50 | +25.0% |
| 2160p-h265-gop10.mp4 |       1 |              0.40 |               0.30 | -25.0% |
| 2160p-h265-gop10.mp4 |     127 |              0.40 |               0.50 | +25.0% |

All enabled median changes were zero or negative. The largest retained encoded
history was 268,435,420 bytes against the 268,435,456-byte global budget; the
largest stream was 16,752,406 bytes against its 67,108,864-byte budget. No queued
input bytes remained after a run. Windows measurements are quantized at roughly
0.1 microsecond in these results; the observed 0.1-0.15 microsecond p95 increase
still exceeds the relative budget and is not treated as a pass.

The initial actual-writer warmup exposed a qualification lookup bug: recordings
use `camera-000/main`, while the validator queried `camera-000`. Inspection found
a finalized MP4 and committed catalog fragments. The validator now derives the
key through `RecordingStreamIdentity`; no sink throughput result is claimed from
that failed warmup.

[Host metadata](verification/event-preroll/host.json),
[fixture commands and hashes](verification/event-preroll/fixtures.json),
[baseline build hashes](verification/event-preroll/baseline-build.json), and
[initial candidate build hashes](verification/event-preroll/before-optimization/build.json)
preserve the measured setup. No binaries or media are checked in.

## First admission optimization

Removing duplicate enabled-path lookups and success-path string allocations
improved every enabled median. The 127-camera median fell from 0.3 to 0.2 us,
and the one-camera median fell to 0.1 us. One-camera p95 fell to 0.2 us.
However, every 127-camera p95 remained 0.5 us against the 0.4 us baseline (+25%),
so the candidate still fails the unchanged 5% gate. Disabled mode remained
unchanged in all 16 workloads. [Raw runs and comparisons](verification/event-preroll/optimization-1/comparison.json)
are preserved with the candidate build identity.

The corrected actual-writer harness then detected a writer/queue failure during
its first warmup. Its catalog contained committed fragments but an unfinished
active recording. No sink throughput qualification is claimed from that run;
bounded first-failure capture is being used to diagnose the failure.

The captured sink error was `event deadline does not follow the last video sample`.
Compressing each live GOP's receive times into microseconds let the writer's
fallback sample timing extend beyond shutdown's real-time endpoint. The harness
now preserves fixture cadence on the shared past origin described above. Writer
validation remains unchanged.

The second candidate completed six actual-sink workload groups, including the
127-camera 1080p H.265 group, without a writer failure. Its run was deliberately
interrupted during the 127-camera 4K H.265 group because a subsequent memory
audit required a production fix. The interrupted log and build hashes are
preserved under `verification/event-preroll/optimization-2`; no complete timing
report or final qualification is claimed for that candidate.

## Borrowed legacy-policy lookup

The fourth candidate removes the per-frame legacy source-string allocation on
a policy hit. Enabled admission passes all 16 workloads against both the original
and adjacent baseline. Disabled admission still exceeds the budget in five
127-camera workloads; favorable one-camera changes also exclude zero in some
confidence intervals. Those improvements do not hide the five regressions.
Both baseline comparisons and all 30-run raw reports are preserved under
`verification/event-preroll/optimization-4`. No additional sink run was started
for this candidate because the admission gate failed.
