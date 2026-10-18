# Recording write failure status

Owner: #202 from #147. Base: main `7fd1585` after merged #308.

## Contract

An active input write failure after writer configuration acceptance must change the matching
Applied camera to Degraded through the existing loop-owned profile status publisher. Use the
explicit camera source identity, never a storage-label prefix or a raw health error. Keep the
status reason fixed, bounded and free of paths, credentials and labels.

Extend the existing configuration token with acknowledgement and failure flags. The writer arms
it only after accepting a non-Off configuration and before replying. New requests, including
same-revision retries and legacy mode changes, invalidate older tokens. A failure racing a new
request cannot arm or degrade its replacement. The writer handles input and configuration
serially; event configuration retires its old buffered outputs before acknowledgement. Legacy
buffers may retain older media, so this observes active writes after acceptance rather than
claiming per-frame generation attribution.

Keep the canonical recording policy as the single owner. Share it with the writer and perform
source/token lookup only on error paths. Add no atomics, allocations or queue slots to the healthy
steady frame path. Creating a pipeline clones its identity once so the incoming source remains
available for failure attribution; existing pipelines do not clone it.
Poll from the main loop at most every 100 ms, including when media/events keep the loop busy.
Examine at most 127 status entries per poll, using a rotating ordered cursor for larger fleets
or retained missing targets. Do not impose a new camera limit. Update only Applied, recording-enabled
entries for the loop's current revision and active profile. Preserve newer Pending state and
independent Degraded reasons. Release status locks before one coalesced publication, and do not
publish again for unchanged failures.

The server's delayed activation completion must preserve a matching live Degraded observation
instead of replacing it with an earlier Applied result for the same revision and policy.

The failure stays latched until explicit reapplication installs a new accepted configuration.
Reapplication acknowledges configuration only; it does not establish later disk recovery or
playable footage. A successful write must not erase other runtime failure reasons automatically.
The existing UI disables reselecting the active Base/profile. After storage repair, recovery of
the observation uses restart or authorized WebRTC reapplication; direct UI retry remains open.

## Scope and limits

Cover legacy direct input writes, buffered flushes triggered by input, named-storage routing
errors during input, and active event-frame writes. Replacement cleanup/finalization, periodic
FlushAll, shutdown, producer queues and global storage-safety pauses retain their existing
diagnostics and remain outside this status slice. No protected API, serialized settings, new
dependency, activation route or UI change is required.

## Verification plan

- RED on main: configure a real writer, observe Applied, replace an empty recording directory
  with a file, submit a source-aware keyframe under a different recording label and run the
  actual KeepPeek loop. Verify that storage observes failure while profile status stays Applied.
- GREEN: the same integration reports Degraded and one status publication after failure.
- Cover failure after ack but before receipt consumption, pre-ack failure, current-token retry,
  Off, event writes, source isolation, newer revision/active profile, Pending, independent
  degradation, repeated observations and poll throttling under a busy loop.
- Measure identical 127-camera activation and the no-storage control before/after, plus healthy
  and failed 127-camera polling work. Keep the original external 250 ms p95 qualification open.
- Run focused profile/storage/watch tests, Clippy, benchmark-feature compilation, Rust/Markdown
  formatting, book build and independent review. The owner monitors hosted CI.

Full #202 acceptance, canonical and slow gates and live/browser/release qualification stay open
until their own final-build evidence is available.

## Results

The real-writer regression failed on exact main `7fd1585`: storage recorded the forced input I/O
error while the running loop kept Applied. The delayed-completion regression separately failed
against the unchanged server projection: its stale Applied result replaced the later Degraded
observation. Both pass with the changes. The profile suite passes 85 tests; 35 storage event tests
also pass, including a buffered input-triggered flush and an event write with a different label.

Independent storage, loop and fresh server-status reviews found no blocking issue. The final
server merge holds the status write fence, matches revision/active profile/original policy and
preserves a live Degraded observation only against a returned Applied result. Current privacy is
then projected under the same status-to-privacy lock order already used by profile activation.

### Measurements

Windows x64 10.0.26200, AMD64 Family 25 Model 80, 12 logical processors; Rust 1.99.0 debug with
`CARGO_INCREMENTAL=0`. Run five standalone native processes per workload, 30 samples each, without
concurrent builds. Baseline is main `7fd1585` with only the failing regression added.

```powershell
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_real_writer_acknowledgement_scale --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_disabled_camera_scale --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe runtime_profile_recording_health_poll_scale --nocapture
```

| Metric (us)                                          |                    Before |         After |
| ---------------------------------------------------- | ------------------------: | ------------: |
| Real writer: median of process medians               |                      4352 |          4121 |
| Real writer: median of process p95 values            |                      4612 |          4385 |
| Real writer: process median range                    |                 4307-4410 |     4066-4159 |
| Real writer: process p95 range                       |                 4525-7778 |     4292-4488 |
| No storage: median of process medians                |                      1127 |          1097 |
| No storage: median of process p95 values             |                      1189 |          1153 |
| No storage: process median range                     |                 1107-1132 |     1089-1104 |
| No storage: process p95 range                        |                 1154-1610 |     1113-1213 |
| Healthy poll: median of process medians / p95 values |       No observation path |       30 / 37 |
| Failed poll: median of process medians / p95 values  | Incorrectly stays Applied |       49 / 49 |
| Healthy poll: process median / p95 ranges            |       No observation path | 30-30 / 31-38 |
| Failed poll: process median / p95 ranges             |       No observation path | 49-49 / 49-53 |

The unchanged real-writer fixture has 127 EventOnly camera definitions, existing worker slots and
a real StorageEngine thread, with no frames, camera startup, disk throughput or browser delivery.
The no-storage fixture uses 127 disabled cameras. The polling fixture has 127 current Sub
configurations; it forces each poll and restores Applied between failure samples to measure the
full transition cost, without watch transport. Separate timing runs are local debug evidence,
not a performance improvement claim or complete external 250 ms p95 qualification.

The token adds two flag bytes within its existing allocation; the command-slot size regression
passes. Existing healthy pipelines add no token lookup, atomics or allocations. New pipelines
clone their identity once for source attribution. The loop has one scheduling cursor, clones at
most one bounded source ID per poll and allocates a fixed reason for each changed observation.
Its measured struct size is 1032 bytes on this platform. Complete retained-memory and media
throughput qualification remain open.

### Validation status

```powershell
cargo test --locked --offline -p keeppeek --lib profile -- --nocapture
target/debug/deps/keeppeek-80bac74c1e257cad.exe storage::engine::event::tests --nocapture
$env:PSExecutionPolicyPreference='Bypass'
target/debug/deps/keeppeek-80bac74c1e257cad.exe --test-threads=4
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo check --locked --offline -p keeppeek --features event-preroll-benchmark-current --benches
cargo fmt --all --check
bun run --cwd ui format:markdown:check
git diff --check
mdbook build book
```

The focused runs pass. At publication preparation, the broader library run has 1863 passes and
11 ignored tests, with only the existing durable-state value-quota qualification still running.
That full run is not claimed complete. The native artifact predates only the final `const fn`
and semicolon lint cleanup; final-source Clippy and benchmark-feature compilation pass. No lint
suppression or test weakening was added. Windows ACL fixtures use process-only PowerShell policy
outside the sandbox. The Markdown check also needs access to an existing ACL fixture in `target/`.
The book retains its existing mdbook-mermaid 0.5.0 / mdbook 0.5.4 warning. The protected `api/`
diff is empty. Canonical `check.bat`, slow and full feature/release gates remain unqualified.
