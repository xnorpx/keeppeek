# Performance benchmarks

## Runtime resource ceilings

KeepPeek applies hard limits at untrusted network and session boundaries:

- The HTTP listener accepts at most 256 active connections, applies 30-second socket read and
  write timeouts, and buffers at most eight parsed requests for the application.
- An HTTP request line and each header line contain at most 8 KiB. All non-empty header lines
  contain at most 64 KiB in total, including line delimiters, and a request contains at most 100
  headers.
- The default request executor runs at most 64 handlers. A configured worker pool queues at most
  eight requests per worker.
- `POST /delete` accepts at most 16 KiB. Public Baichuan framing accepts at most 16 MiB per body.
- BCUDP retains at most 256 receive-window packets and 4,096 in-flight send packets. Its command
  and delivered-payload channels each retain at most 64 items.
- One WebRTC session owns at most 16 stored-media cursors, and the server owns at most 1,024
  stored-media cursors across all sessions.

Requests that exceed a boundary fail instead of allocating or queueing without a limit. Operators
must treat repeated limit failures as load or abuse evidence; increasing a limit requires a new
memory and concurrency budget.

## Configuration planning

The ignored `configuration_planning_benchmark` test measures the maximum 64-camera visual
configuration batch. Its read-only baseline builds the first typed configuration snapshot page.
The result resolves all camera targets, applies five named policy fields to an in-memory TOML
candidate, validates the complete candidate, computes redacted semantic changes, and encodes the
authoritative plan.

```sh
cargo test --release configuration_planning_benchmark --lib -- --ignored --nocapture
```

Set `KEEPPEEK_CONFIGURATION_BENCH_SAMPLES` to change the default 30 measured runs. The benchmark
warms both paths, reports nearest-rank p50 and p95 wall time, fails above a 250 ms plan p95 budget,
and fails when the encoded plan exceeds the 64 KiB WebRTC control-message limit.

### Reference measurement

Captured on 2026-08-31 with macOS 26.6.2 arm64 on an Apple M5 Max developer workstation with
18 logical CPUs and 64 GiB RAM, Rust 1.97.1. The optimized process used 64 cameras and 30 runs.

| Path                                  | p50 ms | p95 ms | p95 delta ms | Budget ms |
| ------------------------------------- | -----: | -----: | -----------: | --------: |
| Typed configuration snapshot baseline |  0.238 |  0.445 |     baseline |       N/A |
| Five-field authoritative bulk plan    |  0.595 |  1.167 |        0.721 |       250 |

The final plan encoded to 23,348 bytes against the 65,536-byte transport budget. Snapshot pages
also have an executable 64 KiB regression with 127 cameras and a near-limit 16 KiB template
document.

## Recording coverage snapshot

The `recording_coverage` benchmark compares the bounded catalog snapshot used by the recording
integrity dashboard with the previous per-stream materialization pattern. The default deterministic
corpus contains 127 cameras, `main` and `sub` streams, 30 days, 128 keyframed fragments per
camera/day/stream, and 975,360 finalized playable fragments.

```sh
cargo bench --locked --bench recording_coverage
```

The benchmark seeds production catalog tables and maintained per-recording coverage summaries,
warms both paths, and reports five-sample median and nearest-rank p95 wall time. The baseline issues
one materializing fragment query per stream. The result uses the same finalized, indexed fragments
through the maintained summary tables, merges exact stream intervals, and emits deterministic
6-hour buckets for the 30-day range. It fails above a 500 ms p95 budget or if retained exact detail
exceeds 256 ranges per stream.

Environment variables override the camera count, day count, sample count, and p95 budget:

```sh
KEEPPEEK_COVERAGE_BENCH_CAMERAS=127 \
KEEPPEEK_COVERAGE_BENCH_DAYS=30 \
KEEPPEEK_COVERAGE_BENCH_SAMPLES=5 \
KEEPPEEK_COVERAGE_BENCH_P95_BUDGET_MS=500 \
cargo bench --locked --bench recording_coverage
```

### Reference measurement

Captured on 2026-08-28 with macOS 26.6.2 arm64 on an Apple M5 Max developer workstation with
18 logical CPUs and 64 GiB RAM, Rust/Cargo 1.97.1. Baseline and result use the same optimized
benchmark process, 975,360 queried fragments, five samples, and nearest-rank percentiles.

| Path                                      | Median ms |    p95 ms | Median delta | Budget ms |
| ----------------------------------------- | --------: | --------: | -----------: | --------: |
| Per-stream fragment materialization       | 1,301.497 | 1,588.955 |     baseline |       N/A |
| Maintained coverage snapshot with buckets |   124.157 |   174.117 |      -90.46% |       500 |

The final response retained 254 exact merged ranges for 254 streams, below the 65,024-range bound,
and owned 888,151 bytes against an 8 MiB snapshot budget. Browser DOM bounds are verified by
`ui/e2e/recordings.e2e.ts`, which holds a 127-camera result to 25 rendered camera rows and fewer
than 1,500 dashboard descendants per page.

## Event keyframe lookup

The `event_keyframe_lookup` benchmark measures the storage API path from an event ID and logical
stream to owned encoded keyframe bytes:

The target requires the opt-in `event-keyframe-benchmark` Cargo feature. Normal CI test and
coverage jobs do not enable it, so they never generate or execute the benchmark corpus.

1. Resolve the event-to-keyframe link through `RecordingCatalog`.
2. Resolve the recording path and exact keyframe byte range.
3. Open the MP4, seek to the indexed offset, allocate the result, and read the encoded bytes.

It does not use HTTP, WebRTC, protobuf serialization, video decoding, or frontend code.

### Prepare the corpus

The default corpus contains deterministic events for 127 cameras, both `main` and `sub` streams,
and 30 UTC days. Production event, search-term, event-keyframe, recording, fragment, and keyframe
tables and indexes are populated until the logical database size is at least 1 GiB. Each
camera/day/stream has 128 uniformly spaced indexed GOPs, producing roughly one million fragment
and keyframe rows. This samples 30-day index cardinality without pretending that 1 GiB can contain
metadata for every GOP from 127 continuously recording cameras.
The fragment/keyframe indexes impose a corpus floor of roughly 200 MiB even when `--target-mib`
requests a smaller smoke corpus; the target controls the minimum rather than an exact size.

```sh
cargo bench --locked --features event-keyframe-benchmark --bench event_keyframe_lookup -- --rebuild --prepare-only
```

Generated state is stored under `target/perf/event-keyframe-lookup/corpus/` and is not checked in.
Later runs reuse it when its manifest, schema contract, dimensions, and committed MP4 fixture hashes
still match. Use `--rebuild` to replace it explicitly.

A smaller corpus is useful while changing the benchmark:

```sh
cargo bench --locked --features event-keyframe-benchmark --bench event_keyframe_lookup -- \
  --rebuild \
  --prepare-only \
  --target-mib 16
```

### Run measurements

```sh
cargo bench --locked --features event-keyframe-benchmark --bench event_keyframe_lookup
```

The runner validates 10,000 exact recording IDs, fragment sequences, timestamps, byte ranges, and
keyframe reads, warms each workload for five seconds, and then
measures for 20 seconds at concurrency 1, 8, and 32. It measures both:

- `resolve`: catalog lookup and response construction without file I/O.
- `read`: catalog lookup plus MP4 open, seek, allocation, and exact read.

Short local runs can override durations:

```sh
cargo bench --locked --features event-keyframe-benchmark --bench event_keyframe_lookup -- \
  --target-mib 16 \
  --warmup-secs 1 \
  --measurement-secs 2 \
  --correctness-lookups 1000
```

Results include throughput, returned MiB/s, mean, p50, p90, p95, p99, p99.9, maximum latency, and
error count. JSON and Markdown reports are written to
`target/perf/event-keyframe-lookup/results/`, including `latest.json` and `latest.md`.

Timing is descriptive and has no machine-dependent pass threshold. Incorrect, missing, or
truncated keyframe data does fail the run.

### Cache interpretation

The corpus gives every camera/day/stream a unique path, but those paths hard-link four small,
committed H.264/H.265 fixtures. Media pages therefore become warm quickly. Results characterize a
large Turso catalog, its single connection-owning catalog thread, queueing, file open/seek/read,
allocation, and copying under the ambient OS cache. They do not characterize cold random reads
across 30 days of unique video bytes.

## Keep timeline interaction

The timeline benchmark mounts the production vertical and horizontal timeline components with a
deterministic 24-hour fixture containing 1,440 one-minute recording segments and 600 uniformly
distributed events. It measures initial render, zoom, pan, event-filter feedback, playhead drag,
and cold-seek feedback. The mobile component does not expose event filters, so filtering is measured
only on the desktop component.

```sh
cd ui
bun run perf:timeline
```

The runner builds the visual harness, launches its pinned headless Chromium, uses nearest-rank p95,
and fails when any interaction metric exceeds 150 ms. It collects 10 initial-render samples and 20
samples for each interaction by default. Reports are written to
`target/keep-performance/timeline/latest.json` and `latest.md`. Override the run count, budget, or
report label with `KEEPPEEK_TIMELINE_PERF_SAMPLES`,
`KEEPPEEK_TIMELINE_PERF_INITIAL_SAMPLES`, `KEEPPEEK_TIMELINE_PERF_BUDGET_MS`, and
`KEEPPEEK_TIMELINE_PERF_LABEL`.

Initial render is measured from mounting the component with the complete fixture through the second
animation frame. Interaction feedback is measured in the browser from event dispatch through the
second frame after the expected DOM state appears. Cold-seek feedback covers the immediate playhead
response; media arrival and decoder startup remain covered by `bun run perf:keep:real`. Long tasks,
total timeline descendants, and viewport callbacks during a 24-sample drag are recorded alongside
latency.

### Rendering strategy

Ticks, availability ranges, event markers, activity spans, and event cards remain DOM-based for
mouse, touch, keyboard, and accessibility behavior. Both timeline orientations render the visible
viewport plus one viewport of overscan on each side. The vertical timeline uses a 400 px estimate
before layout measurement, preventing its first render from expanding to the full day. Filtering and
clustering operate only on the resulting time window. The component regression test supplies all
600 fixture events, adds another 1,200 events entirely outside the window, and verifies that rendered
timeline primitives remain unchanged and below 800.

### Reference measurements

These measurements were captured on 2026-08-25 with headless Chromium 151.0.7922.34 on an Apple M5
Max developer workstation with 18 logical CPUs and 64 GiB RAM, running macOS/Darwin 25.6.0 arm64.
The baseline and result used the same production visual-harness build, fixture, browser process,
sample counts, and nearest-rank statistic. The baseline restored the prior unbounded initial window
and two-viewport overscan formulas; the result used the final implementation. Raw reports are
reproducible with labels `before` and `latest`.

| Viewport         | Metric             | Baseline p95 ms | Result p95 ms | Delta ms | Budget ms |
| ---------------- | ------------------ | --------------: | ------------: | -------: | --------: |
| Desktop 1440x900 | Initial render     |            40.0 |          31.2 |     -8.8 |       150 |
| Desktop 1440x900 | Zoom               |            24.8 |          24.0 |     -0.8 |       150 |
| Desktop 1440x900 | Pan                |            16.1 |          15.8 |     -0.3 |       150 |
| Desktop 1440x900 | Filter             |            24.1 |          24.2 |     +0.1 |       150 |
| Desktop 1440x900 | Cold-seek feedback |            24.0 |          24.1 |     +0.1 |       150 |
| Desktop 1440x900 | Playhead drag      |            23.9 |          24.0 |     +0.1 |       150 |
| Mobile 390x844   | Initial render     |            17.3 |          18.6 |     +1.3 |       150 |
| Mobile 390x844   | Zoom               |            24.3 |          23.8 |     -0.5 |       150 |
| Mobile 390x844   | Pan                |            15.8 |          15.3 |     -0.5 |       150 |
| Mobile 390x844   | Cold-seek feedback |            24.3 |          23.9 |     -0.4 |       150 |
| Mobile 390x844   | Playhead drag      |            23.8 |          24.1 |     +0.3 |       150 |

Desktop peak timeline descendants fell from 1,944 to 1,430 (-26.4%); mobile fell from 326 to 205
(-37.1%). Both runs reported a long-task p95 of 0 ms. A desktop drag produced no viewport callback,
and a mobile drag produced at most one, bounding query-triggering work during pointer movement.

### Issue 86 verification

| Acceptance criterion                                      | Verification                                                          | Observed result                                                                                                     |
| --------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Representative checked-in harness                         | `bun run perf:timeline`                                               | 24 hours, 1,440 segments, 600 events                                                                                |
| Initial render and feedback p95 at or below 150 ms        | Generated nearest-rank p95 report                                     | Highest final p95: 31.2 ms                                                                                          |
| Node count and render work stay bounded                   | Dense component regression and 1,600-descendant harness guard         | 1,200 added off-screen events changed rendered primitives by 0; peak descendants 1,430 desktop and 205 mobile       |
| Desktop and mobile interactions avoid visible stalls      | Zoom, pan, filter, drag, cold-seek feedback, and long-task collection | Every p95 at or below 31.2 ms; no long tasks observed                                                               |
| Existing timeline, visual, and Playwright behavior passes | `./check.sh`                                                          | 93 browser/visual tests, 17 compatibility tests, and 127 Playwright tests passed; 2 codec tests skipped as expected |
| Strategy and before/after evidence are recorded           | This section and generated JSON/Markdown reports                      | Initial-render p95 40.0 to 31.2 ms; desktop nodes 1,944 to 1,430; mobile nodes 326 to 205                           |
