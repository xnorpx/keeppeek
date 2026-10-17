# Recording writer acknowledgement

Owner: #202 from #147. Base: main `955619b` after merged #307.

## Contract

Keep profile recording status Pending until the storage writer acknowledges its configuration.
Queue acceptance alone must not report Applied. Resolve all camera receipts against one 200 ms
deadline after enqueuing the batch and applying camera availability; never wait under profile-status
or storage-admission locks. Startup batches share the same deadline, and live workers start even
when recording fails. Preserve per-camera failure reporting and the existing revision fences.

Retain the public queue-admission method and add a crate-private writer-receipt path for the loop.
Use a one-shot bounded channel. Retain one current token per configured recording policy across
all modes, including Off and legacy modes. New configuration invalidates the old token. Timeout,
rejection and disconnected receipts close admission only when their token is still current. A late
old result cannot disable a newer policy. Drop of a queue-only caller's receipt does not cancel.

The writer acknowledges only after checking its current token and installing the configuration.
Rejection clears partial runtime state. A timed-out command cannot later re-enable ingress or an
event source. Writer configuration acceptance does not prove later disk I/O or playable footage;
the full privacy/media-leak and release qualification matrices remain open.

## Verification

- RED: a real live camera with a queued but stalled storage consumer reports Applied on main.
- Cover successful writer reply, rejection, disconnected reply, timeout, late reply, Off->On,
  same-source retry, a newer pending revision, and startup/profile batches sharing one deadline.
- Test actual writer rejection/cleanup and conditional admission closure with real storage fixtures.
- Measure the unchanged no-storage 127-camera fixture before/after, plus healthy 127-camera
  acknowledgement and stalled-batch timing. Preserve the original 250 ms p95 activation target.
- Run focused profile/storage/watch tests, all-target Clippy, formatting, book build and independent
  review. Do not poll hosted CI or check unqualified #202 acceptance criteria.

No protected API, serialized configuration, new dependency, or activation surface changes.

## Results

The stalled-writer regression failed on exact merged main `955619b`: the camera stayed live, but
recording reported Applied instead of Degraded. The same test passes with writer receipts.
The final library build passes 75 profile tests. The same native artifact passes 31 storage event
tests, 31 legacy/named engine tests, 16 status-watch tests and the queue-slot size regression.
Two existing opt-in local disk benchmarks remain ignored. Independent storage and loop
reviews found no remaining blocker, including mixed legacy/event mode changes and stale receipts.

Run with Rust 1.99.0, `CARGO_INCREMENTAL=0`, `--locked --offline` and the verified cached public
camera archive. Focused commands:

```powershell
cargo test --locked --offline -p keeppeek --lib profile -- --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe storage::engine::event::tests --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe server::state_store_watch::tests --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe optional_event_input_does_not_enlarge_legacy_channel_slots
$env:PSExecutionPolicyPreference='Bypass'
target/debug/deps/keeppeek-80bac74c1e257cad.exe storage::engine::tests --nocapture
```

The engine suite initially failed in eight Windows ACL fixtures under the sandbox or restricted
PowerShell policy. It passes outside the sandbox with process-only execution policy; no source
change or test weakening was needed. The two ignored disk benchmarks were not requested.

The following checks pass:

```powershell
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo check --locked --offline -p keeppeek --features event-preroll-benchmark-current --benches
cargo fmt --all --check
bun run --cwd ui format:markdown:check
git diff --check
mdbook build book
```

The book build retains the existing mdbook-mermaid 0.5.0 / mdbook 0.5.4 warning. The protected
`api/` diff is empty. The Markdown check needs access to an existing ACL fixture under `target/`.

### Timing

Windows x64 10.0.26200, AMD64 Family 25 Model 80, 12 logical processors; debug build.
Run five standalone native processes per workload, 30 revisions each, without concurrent builds.
The real-writer fixture uses 127 EventOnly camera definitions, existing worker slots and a real
StorageEngine thread. It excludes camera startup, frame processing, disk I/O and browser delivery.
The baseline is main `955619b` plus the identical fixture. Main completes at queue admission;
this branch completes at writer configuration acceptance, so the measured completion boundary changes.

```powershell
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_real_writer_acknowledgement_scale --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_disabled_camera_scale --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_recording_startup_batch_shares_one_ack_deadline --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_recording_activation_batch_shares_one_ack_deadline --nocapture
```

| Metric (us)                                  |                            Before |         After |
| -------------------------------------------- | --------------------------------: | ------------: |
| Real writer: median of process medians       |                               998 |          4073 |
| Real writer: median of process p95 values    |                              1067 |          4391 |
| Real writer: process median range            |                          991-1703 |     4055-4119 |
| Real writer: process p95 range               |                         1023-1761 |     4309-4640 |
| No storage: median of process medians        |                              1104 |          1131 |
| No storage: median of process p95 values     |                              1157 |          1167 |
| No storage: process median range             |                         1090-1117 |     1110-1139 |
| No storage: process p95 range                |                         1131-1223 |     1154-1201 |
| Stalled 127-camera startup: elapsed range    | Incorrect Applied without waiting | 205012-215012 |
| Stalled 127-camera activation: elapsed range | Incorrect Applied without waiting | 207203-218046 |

The healthy writer fixture retains its 250,000 us local p95 ceiling. The stalled tests use a
two-second functional ceiling to catch 127 separate waits. These local results do not qualify the
complete 250 ms external WebRTC p95 target. Each receipt has one bounded reply slot; current tokens
add no per-frame atomic read. Recording throughput, retained memory and the live fleet remain unqualified.

### Remaining qualification

This is a draft follow-up. Canonical `check.bat`, main-only slow Rust tests and the complete #202
live/browser/privacy/restart/release matrices remain open. All original #202 criteria stay unchecked.
Hosted CI is left to the owner to monitor.
