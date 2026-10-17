# Checked runtime recording admission

Owner: #202 from #147. Base: main `e76079e` after merged #306.

## Contract

Report synchronous storage configuration rejection as per-camera Degraded while continuing the
profile batch and enforcing camera availability. Camera startup keeps live media available when
recording admission fails. Configure storage once per camera per profile transition.

Use the existing recording configuration method with a checked result for admission and command
queue acceptance. A successful return does not acknowledge writer completion or later disk I/O.
Invalid settings, the existing 127-event-camera limit, a full queue and a disconnected writer stop
new recording admission for that source. The control enqueue is nonblocking. Preserve fail-closed
Off semantics; do not roll back to a previously permissive admission policy.

Share an enabled marker through the existing event-generation fence. The writer reclaims disabled
sources before admitting a new event configuration and during its existing 25 ms tick, so a lost
Off command cannot strand a writer slot. Already admitted/buffered output is not a synchronous
no-more-bytes barrier; privacy has its separate gate. Retry the accepted profile after queue recovery.

## Verification

- Reproduce Base Sub/post=0 overridden to EventOnly reporting Applied instead of Degraded.
- Cover startup, per-camera batch continuation, capacity, invalid settings, full/disconnected
  queues, restrictive Off, source fencing, writer-slot reclamation and successful reapply.
- Use real StorageHandle admission/queues with bounded test consumers and existing writer fixtures.
- Measure the existing 127-camera/30-transition fixture and checked configuration workload.
- Run profile/storage event/watch tests, all-target Clippy, formatting, book build and independent
  review. Keep all unqualified #202 acceptance criteria open; do not poll hosted CI.

No protected API, serialized schema, configuration file or activation path changes.

## Results

The corrected Sub/post=0 -> EventOnly fixture fails Applied versus Degraded when the loop discards
the storage error, as the old implementation did. Restoring checked handling passes the regression.
Base Off still prevents a profile from enabling recording; that precedence is unchanged.

- 68 profile tests, 25 storage event tests and 16 watch tests pass on the same final production code.
- Coverage includes live startup with a disconnected writer, batch continuation, single enqueue,
  full-queue recovery, invalid settings, the 128th event source, old-input fencing, lost Off cleanup
  before capacity checks/on the writer tick, and same-source retry with a fresh marker.
- Independent loop and storage reviews found no blocking issues. All changed functions fit 70 lines.
- All-target Clippy, benchmark-feature compilation, Rust/Markdown formatting, whitespace checks,
  protected-API diff and the book build pass.

Commands use Rust 1.99.0, `CARGO_INCREMENTAL=0`, `--locked --offline` and the verified cached public
camera-database archive through `KEEPPEEK_CAMERA_DATABASE_ARCHIVE`:

```powershell
cargo test --locked --offline -p keeppeek --lib profile -- --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe storage::engine::event::tests:: --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe state_store_watch::tests::
cargo clippy --locked --offline --all --all-targets -- -D warnings
cargo check --locked --offline -p keeppeek --features event-preroll-benchmark-current --benches
```

Windows x64 10.0.26200, AMD64 Family 25 Model 80, 12 logical processors, debug build. Five standalone
processes before and after, each running `runtime_profile_disabled_camera_scale --nocapture` with
127 disabled cameras and 30 transitions, without a storage handle:

| Metric (us)                       | Before main e76079e |     After |
| --------------------------------- | ------------------: | --------: |
| Median of five process medians    |                1106 |      1094 |
| Median of five process p95 values |                1161 |      1133 |
| Range of process medians          |           1101-1123 | 1083-1108 |
| Range of process p95 values       |           1126-1179 | 1121-1981 |

The new `event_configuration_admission_scale --nocapture` fixture configures 127 EventOnly sources
30 times through real admission and writer configuration, draining its one-slot queue each time.
Five standalone processes measured medians of 6 us, p95 values of 6-7 us, and maxima of 254-334 us. These are post-change observations only, without media input or recording writes in the measured phase.
Before/after writer throughput, retained memory and the complete release performance budget remain
unqualified. The existing profile measurements do not establish a performance improvement.

Full canonical `check.bat`, main-only slow tests, hosted CI and the original #202 live/browser/release
qualification remain open. Admission success does not acknowledge writer completion or later I/O.
