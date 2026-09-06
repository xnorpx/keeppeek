# Native event recording performance

## Final Build Confirmation

The final implementation was remeasured on 2026-09-06 using the same workload,
one excluded warm-up pair, ten measured pairs, and unchanged guards below.
Runtime was 75.517 seconds; compilation took another 58.69 seconds. All 40
finalized recordings and 150 enabled events validated. An independent JSON audit
recomputed the percentiles and deltas from all 20 measured samples.

| Metric (ms)                            | Disabled p50 | Enabled p50 | Disabled p95 | Enabled p95 | p95 delta |
| -------------------------------------- | -----------: | ----------: | -----------: | ----------: | --------: |
| First durable fragment on both streams |     1062.537 |    1064.488 |     1071.287 |    1072.710 |    +1.423 |
| Maximum durable-fragment gap           |     1006.922 |    1007.713 |     1014.465 |    1017.661 |    +3.196 |
| Shutdown and storage finalization      |      214.807 |     180.065 |      230.000 |     224.790 |    -5.210 |

The primary p95 delta is +3.196 ms (+0.315%), below the 86.667-ms fixture guard.
This is a same-build policy comparison, not a speedup claim or a pre-PR commit
comparison. The workload and statistical caveats below still apply.

```sh
TMPDIR=/Volumes/KeepPeekCheckTmp cargo bench --locked -p test-camera --bench native_events
```

Final raw samples, summaries, budgets, and completion record are in
`target/issue96-native-event-performance-final.log`.

| Artifact                 | SHA-256                                                            |
| ------------------------ | ------------------------------------------------------------------ |
| Final release executable | `45641334ebe78bbaae3663bdf8cf2926b9f2e8ba47fd4a8f5f76ecd93b578737` |
| Final benchmark log      | `a67f7ddd2f80b3e65c3c988373da8499bf35a2044fc6fbf6aa1779daa9801313` |
| Cargo lockfile           | `b4c7a57af0a32935733a432a527dcd95f54a8ee4d6924096f35481956d82338a` |

## Earlier Measurement

Completed on **2026-09-05 PDT**, with the final log closed at
**2026-09-06 06:45:51 UTC**. The release harness completed **10 measured pairs**
(10 samples per condition), plus one excluded warm-up pair, in **75.242 seconds**.
Every correctness check and engineering guard passed. Compilation took another
59.79 seconds and is not included in the runtime or latency measurements.

The baseline is `EventMode::Disabled`; the result is `EventMode::RtspMetadata`.
Both use the **same current-worktree release binary** and the same input bytes.
This is an event-policy overhead comparison, **not a pre-PR baseline**, a
comparison between commits, or evidence of a performance improvement.

All times below are milliseconds. Each condition has `n = 10`. Deltas are enabled
minus disabled, calculated before rounding. The primary metric is the maximum
observed durable-fragment gap across both streams in each run.

| Metric                                 | Disabled p50 | Enabled p50 |        p50 delta | Disabled p95 | Enabled p95 |        p95 delta |
| -------------------------------------- | -----------: | ----------: | ---------------: | -----------: | ----------: | ---------------: |
| First durable fragment on both streams |     1053.273 |    1051.220 | -2.053 (-0.195%) |     1064.303 |    1065.669 | +1.366 (+0.128%) |
| Maximum durable-fragment gap           |     1000.833 |    1007.597 | +6.764 (+0.676%) |     1009.507 |    1012.230 | +2.722 (+0.270%) |
| Shutdown and storage finalization      |      196.679 |     196.459 | -0.220 (-0.112%) |      210.765 |     215.046 | +4.280 (+2.031%) |

The p95 cadence delta is **+2.722 ms**, below the predeclared **86.667 ms** guard.
Small differences near the 10 ms polling resolution do not establish a speedup
or a general production overhead estimate. This short single-camera fixture does
not cover physical-camera jitter, fleet scale, sustained event pressure, other
native-event transports, or snapshot work. CPU time and RSS were not measured;
these are wall-clock recording metrics, not a parser microbenchmark.

## Distribution and paired deltas

Nearest-rank percentiles sort all samples and select the one-based rank
`ceil(n * percentile / 100)`. Thus p50 selects the fifth sample and p95 selects
the maximum of ten samples. There is no interpolation or outlier removal. The
Rust implementation uses checked `usize` multiplication and checked rank bounds.

Paired deltas below subtract the disabled sample from the enabled sample within
each pair, then summarize those ten differences. Their percentiles are distinct
from the differences between condition percentiles in the first table.

| Metric                                 |  Disabled min..max |   Enabled min..max | Paired delta p50 | Paired delta p95 | Paired delta min..max |
| -------------------------------------- | -----------------: | -----------------: | ---------------: | ---------------: | --------------------: |
| First durable fragment on both streams | 1045.273..1064.303 | 1040.403..1065.669 |           -1.482 |          +20.162 |      -16.458..+20.162 |
| Maximum durable-fragment gap           |  995.684..1009.507 |  997.960..1012.230 |           +4.372 |          +14.623 |       -8.925..+14.623 |
| Shutdown and storage finalization      |   112.641..210.765 |   160.636..215.046 |           +4.332 |          +78.052 |      -22.207..+78.052 |

## Contract

This issue #96 benchmark compares `EventMode::Disabled` with
`EventMode::RtspMetadata` in the same compiled executable. It does not compare
different commits or remove metadata traffic from the baseline.

Both conditions use one parent-owned loopback camera fixture at `127.0.0.1`,
with ephemeral ports. Main and sub use the same 640x360 Constrained Baseline H.264
file: 15 FPS, 15 frames, a one-second loop and GOP, and no audio. Both profiles
advertise and transmit the same 30 raw ONVIF XML documents at a nominal 10 Hz
(the fixture enforces at least 100 ms between documents). This is 22,335 XML
bytes per stream and 60 documents per run. Every new RTSP session starts its own
metadata sequence. Both policies SETUP all advertised tracks; disabled mode does
not remove the metadata network workload.

The documents alternate motion start and clear for source token `source-2`,
producing 15 complete events. Snapshot work is disabled and generic motion
retention is enabled in both conditions. The recording policy is `Both`, not
event-boost. Only the event mode changes between conditions.

Each run uses a new recorder subprocess, fresh catalog, and temporary recording
directory. Sharing the input fixture does not share recorder or catalog state.
The observation window is 3,150 ms, starting immediately before
`KeepPeekLoop::add_camera`. Fixture construction and initial catalog creation are
outside the measured latencies. They remain inside the overall suite runtime.
Catalog polling is every 10 ms. Both streams must expose at least three advancing,
nonempty, random-access fragments. Startup is measured separately from later
fragment cadence. Shutdown timing starts immediately before cancellation and ends
after the recorder worker joins and `StorageEngine::shutdown` returns; it excludes
the subsequent MP4 validation and fixture teardown.

After shutdown, every catalog fragment range must fit its finalized MP4, and both
files must parse with at least 45 samples. Enabled runs must persist exactly 15
cleared camera-motion events before shutdown; disabled runs must persist none.
Each event must have revision 2, the expected camera identity, and no snapshot
attachments. Shutdown must preserve the same event rows.

The completed comparison contains one excluded warm-up pair and 10 measured
pairs, alternating Disabled/Enabled and Enabled/Disabled order. Every sample in
that completed dataset is retained. The earlier incomplete harness attempt is
preserved separately below and is not pooled with the final dataset.

## Budgets and correctness

The predeclared candidate cadence budget is an enabled p95 increase of at most
**86.667 ms**: one 15 FPS frame period (66.667 ms), plus two 10 ms observation
intervals. The absolute startup and gap guards are 2.5 and 1.5 nominal GOP periods.
These are engineering guards for this fixture, not previously approved product
SLAs. They were declared before measurement and were not relaxed after failure.

| Check                                                  | Fixed requirement                    | Observed final result                       | Status |
| ------------------------------------------------------ | ------------------------------------ | ------------------------------------------- | ------ |
| First fragment on both streams, every run              | <2500 ms                             | Maximum 1065.669 ms                         | Pass   |
| Maximum fragment gap, every run                        | <1500 ms                             | Maximum 1012.230 ms                         | Pass   |
| Enabled p95 gap minus disabled p95 gap                 | <=86.667 ms                          | +2.722 ms                                   | Pass   |
| Persisted events per disabled / enabled run            | 0 / 15, unchanged by shutdown        | 0 / 15 in all 10 pairs                      | Pass   |
| Advancing catalog fragments per stream before shutdown | >=3                                  | 3 in every stream                           | Pass   |
| Finalized MP4 sample count per stream                  | >=45, readable file and valid ranges | 48..50, all 40 measured files valid         | Pass   |
| Child deadline                                         | 6 seconds                            | Slowest case including warm-up: 3418.187 ms | Pass   |
| Total runtime including warm-up and fixture lifecycle  | <90 seconds                          | 75.242 seconds                              | Pass   |

Each condition finalized 20 MP4 files and 80 catalog fragments. Both main and sub
had sample-count p50/p95 of **48/50** under both policies. Disabled recorded 964
video samples and 2,603,824 bytes in total; enabled recorded 968 samples and
2,650,968 bytes. Enabled retained 150 measured events; disabled retained zero.
In-flight media can drain during cancellation, so the small tail-frame count
difference is not a throughput improvement claim.

## Bounds

- One active parent-owned camera and one recorder child at a time, always on loopback.
- Two real video streams; no synthetic camera-identity multiplication.
- Exactly 30 metadata documents per profile, each at most 4 KiB.
- A maximum of 400 polls and eight catalog fragments per stream.
- A six-second child deadline and a 90-second total runtime deadline.
- Child output reads are limited to 64 KiB; failures and timeouts exit nonzero.
- Parent-owned temporary directories are removed after children are reaped.
- The 36,481-byte source loops twice per second across both streams: approximately
  73 KB/s of source media plus metadata and protocol overhead. At 3.15 seconds per
  run, media storage is approximately 230 KB before MP4 and catalog overhead.

## Raw measured pairs

D means disabled; E means RTSP metadata. Values are rounded to three decimal
places; the JSON log retains full precision and per-stream observations.

| Pair | Order | First both D / E (ms) |  Max gap D / E (ms) | Finalize D / E (ms) |
| ---- | ----- | --------------------: | ------------------: | ------------------: |
| 1    | D, E  |   1056.861 / 1040.403 | 1003.225 / 1007.597 |   196.679 / 215.046 |
| 2    | E, D  |   1048.942 / 1046.852 | 1000.833 / 1012.230 |   208.595 / 186.388 |
| 3    | D, E  |   1055.454 / 1051.220 |  997.042 / 1003.413 |   194.444 / 188.431 |
| 4    | E, D  |   1054.028 / 1052.546 | 1000.436 / 1009.275 |   197.252 / 196.459 |
| 5    | D, E  |   1050.169 / 1050.808 | 1006.323 / 1007.803 |   114.035 / 192.087 |
| 6    | E, D  |   1053.273 / 1055.050 |  1000.618 / 998.743 |   112.641 / 160.636 |
| 7    | D, E  |   1045.507 / 1065.669 |  1002.464 / 997.960 |   210.765 / 203.118 |
| 8    | E, D  |   1045.273 / 1053.711 | 1009.507 / 1000.582 |   122.948 / 200.161 |
| 9    | D, E  |   1056.831 / 1049.105 | 1005.259 / 1009.714 |   201.890 / 207.502 |
| 10   | E, D  |   1064.303 / 1064.048 |  995.684 / 1010.307 |   209.373 / 213.705 |

## Environment and identity

- Host: Apple M5 Max, model `Mac17,6`, 18 logical CPUs, 64 GiB RAM.
- OS: macOS 26.6.2 build `25G83`; Darwin 25.6.0, `arm64`.
- Rust: `rustc 1.97.1 (a7727ddedd 2026-08-13)`, packaged version
  `1.97.1-ms-20260814.2+a7727ddedd`; LLVM 22.1.6, host `aarch64-apple-darwin`.
- Cargo: `1.97.1 (1.97.1-ms-20260814.2+a7727ddedd)`; bench profile reported
  `optimized + debuginfo`; no dependency changes were made for this benchmark.
- HEAD: `aa99b11fdf54649cd3e36aef627dc1a6c2afbcc9`, with pre-existing staged,
  unstaged, and untracked production and fixture changes. HEAD alone does not
  identify the measured implementation.
- The release executable was 21,701,152 bytes. Its SHA-256 identifies the actual
  current-build artifact used by both policies. The input manifest hashes an
  explicit list of public files, including the untracked benchmark modules;
  it is not a claim of a complete worktree hash.

| Artifact                    | SHA-256                                                            |
| --------------------------- | ------------------------------------------------------------------ |
| Measured release executable | `99139ae66d210302fd2ba1230792c7b67978159eb02cec33d8470887f2e2d104` |
| Source MP4                  | `94d9415835fa2c41397b85852d94781f3f33ae23a5b9c9ae81d106ccb0bba811` |
| Cargo lockfile              | `b4c7a57af0a32935733a432a527dcd95f54a8ee4d6924096f35481956d82338a` |
| Final raw benchmark log     | `436757bcd9ab1c9d6f2f4087e63519aa1366895d56f411af2e33d78d2eca0bcd` |

## Commands and evidence

Run from the repository root. All one-shot commands below were run synchronously.
The smoke run is one pair and is not a percentile measurement. The full command
must print `NATIVE_EVENT_PERF_COMPLETE` with 10 pairs, 10 samples per condition,
`smoke: false`, and `gates_passed: true`.

```sh
set -o pipefail
cargo bench --locked -p test-camera --bench native_events -- --smoke 2>&1 | tee target/native-event-performance-smoke.log
cargo bench --locked -p test-camera --bench native_events 2>&1 | tee target/native-event-performance.log

rustc --edition=2024 --test crates/test-camera/benches/native_events/stats.rs -o "$PWD/target/native-event-perf-stats-tests"
"$PWD/target/native-event-perf-stats-tests"
cargo clippy --locked -p test-camera --bench native_events -- -D warnings
rustfmt --edition 2024 --check --config skip_children=true crates/test-camera/benches/native_events.rs crates/test-camera/benches/native_events/recorder.rs crates/test-camera/benches/native_events/stats.rs
bunx --no-install prettier --check docs/native-event-performance.md
```

Environment and public-input identity commands:

```sh
uname -smr
sw_vers
sysctl hw.model hw.ncpu hw.memsize machdep.cpu.brand_string
rustc -Vv
cargo -V
git rev-parse HEAD
ffprobe -v error -show_entries stream=codec_name,profile,width,height,r_frame_rate,nb_frames,duration -of json crates/test-camera/testdata/cc-4k-640x360-h264.mp4
shasum -a 256 Cargo.toml Cargo.lock crates/test-camera/Cargo.toml crates/test-camera/benches/native_events.rs crates/test-camera/benches/native_events/recorder.rs crates/test-camera/benches/native_events/stats.rs crates/test-camera/testdata/cc-4k-640x360-h264.mp4 target/release/deps/native_events-9780fe0a81d4deab target/native-event-performance.log target/native-event-performance-smoke.log target/native-event-performance-attempt-1.log target/native-event-performance-audit.log | tee target/native-event-performance-identity.sha256
```

The executable filename above is the path printed by Cargo for this measured
build. A different build may print a different filename. Evidence is local,
generated output under `target/`; it contains fixture data, not camera secrets.

| Evidence                                          | Outcome                                                                                                 |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `target/native-event-performance.log`             | Complete 10-pair run and passing budget/completion records                                              |
| `target/native-event-performance-smoke.log`       | Parent-fixture smoke passed both conditions in 8.027 seconds                                            |
| `target/native-event-performance-audit.log`       | Independent Node built-in JSON audit matched all percentiles, deltas, counts, order, and reported gates |
| `target/native-event-performance-identity.sha256` | Exact binary, selected public-input, and evidence-file hashes                                           |
| `target/native-event-performance-attempt-1.log`   | Preserved incomplete draft run: eight measured pairs, then the shared 90-second deadline during pair 9  |

The incomplete draft created and destroyed the full RTSP, ONVIF, and web-UI
fixture in every child. Reusing one unchanged input fixture in the parent removed
that repeated lifecycle cost while retaining independent recorder subprocesses,
fresh catalogs, the full observation window, exact event assertions, and all
deadlines. This is a harness lifecycle correction, not a production optimization.
The entire final measurement was rerun; no failed sample was replaced inside a
partially completed dataset.

Five standalone statistics tests pass, including checked-rank overflow and bounds,
nearest-rank behavior, exact sample counts, non-finite rejection, input preservation,
and signed deltas. The statistics module is used by the benchmark report. Its tests
are run explicitly with `rustc --test` because the Cargo bench uses `harness = false`.
Bench-specific Clippy passes with warnings denied, and the scoped Rust and Markdown
format checks pass. Repository-wide canonical validation is a separate caller-owned
gate and is not implied by these results.

This continuation changes only the benchmark source, its child modules, and this
report. The existing bench stanza is reused. It does not modify production code,
the RTSP fixture implementation, dependencies, API, UI, Git staging, or branches.
The benchmark does not use physical cameras or Internet services.
