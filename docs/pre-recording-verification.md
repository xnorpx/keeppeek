# Event pre-recording acceptance evidence

This record maps issue [#172](https://github.com/xnorpx/keeppeek/issues/172) to
reproducible checks. Behavioral and performance qualification is complete; the
full canonical Windows gate passed on 2026-09-27 UTC.

The implementation base is `e4f8289`. The additive contract and opt-in EventBoost
persistence delay are approved as recorded in
[the contract proposal](pre-recording-contract-proposal.md). This evidence uses
synthetic encoded media and loopback browser fixtures; it does not claim physical
camera certification. Production implementation commit
`8a6525394ce3261ad54ea5a63b2cdcf179f6b9a3` is equivalent to the measured release
build; `2e9dd84` adds the FFmpeg CI prerequisite without changing production code.
The later generated binding update changes only its generator-version comment;
API documentation formatting changes only line endings. Both exceptions and
their hashes are recorded in the performance build manifest.

## Acceptance criteria verification

| Criterion | Observable outcome and verification                                                                                                                                                                        | Observed evidence                                                                                                                                                                                                                                                                                                             |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| AC-1      | `every_legacy_recording_mode_upgrades_with_preroll_disabled`; `event_only_routes_selected_stream_and_zero_preroll_boost_uses_legacy_queue`; `configuring_zero_preroll_preserves_an_existing_legacy_writer` | Legacy zero-pre routing and existing writer preservation pass. Old fields load with zero pre and main selection. Disabled history instances/bytes remain zero; all controlled aggregate latency cases show no added overhead.                                                                                                 |
| AC-2      | `selected_replay_bypasses_short_term_aging_and_silent_deadline_finalizes`, both selected streams; real H.264/H.265 event clips                                                                             | Idle creates no writer/media directory or catalog rows. Event output produces one finalized record; deadline clamping and silent-source finalization pass.                                                                                                                                                                    |
| AC-3      | `event_preroll_h264_is_independently_decodable`; `event_preroll_h265_is_independently_decodable`                                                                                                           | Both pass with AAC. Requested pre is 1.3 seconds, trigger is 2.5 seconds, and retained video begins at the 2-second keyframe under the hard duration limit. Whole audio packets stay within video coverage. Independent FFmpeg decoding and catalog-fragment seek decoding pass.                                              |
| AC-4      | `enabled_boost_preserves_one_recording_across_real_h264_h265_audio_transitions`; `boost_replays_main_under_continuous_sub_identity_without_overlap`                                                        | One logical sub recording, decoder descriptions H.264/H.265/H.264, ordered samples and bounded AAC coverage pass. The opt-in persistence horizon and pressure release are documented.                                                                                                                                         |
| AC-5      | `overlapping_media_event_storm_writes_each_frame_once_to_one_catalog_recording`                                                                                                                            | 1,001 triggers from 2.500 to 3.500 seconds extend the exclusive deadline to 5.500 seconds. Exact output receive timestamps equal fixture inputs from 2.000 seconds through the last sample before 5.500, in order and once each. One finalized catalog record, exact MP4 sample count, and independent decoding pass.         |
| AC-6      | `storage::pre_record::tests`, runtime pressure/shutdown tests, actual writer lease tests, 127-camera benchmark                                                                                             | Whole-GOP duration/byte eviction, detached payload ownership, metadata bounds, oversized-GOP rejection, and deterministic oldest-first eviction pass. Peak global history is 268,435,420 / 268,435,456 bytes; peak stream history is 16,752,406 / 67,108,864 bytes. All measured queues drain to zero.                        |
| AC-7      | Buffer reason/recovery tests; `pre_recording_health_wire_preserves_server_coverage_and_failure_reasons`; TypeScript control decoding; desktop/mobile health browser tests                                  | All 13 server reason codes survive protobuf serialization. Requested 30 seconds, available 4 seconds, sub selection and global-pressure state render from the server fixture without an active-event false positive.                                                                                                          |
| AC-8      | Configuration inheritance/round-trip/invalid-write tests; control update reload through `load_cameras`; editor/setup component and browser tests                                                           | 48 focused UI tests, 18 compatibility tests, and five final browser flows pass. Valid fields persist through disk reload; browser save/reload restores values. Invalid 31 seconds prevents update and focuses the error summary. Desktop/mobile screenshots below show final states.                                          |
| AC-9      | Engine privacy/reconnect/pause/writer-failure/optional-main-pressure tests; bounded no-media storm; malformed optional main and continuous-sub tests                                                       | Failure fencing, queue-byte release, writer-held history progress, preservation of committed media, recovery with fresh input and unaffected continuous-camera output pass. 10,000 no-media events cannot accumulate finish markers.                                                                                          |
| AC-10     | Same-host release baseline/current, fixed 1080p/4K H.264/H.265 fixtures, 1/10-second GOPs, 1/127 cameras, three process pairs, three warmups and 90 retained measured runs per workload                    | All 16 aggregate workloads pass the 5% median/p95 budget; worst aggregate change is 0%. All eight actual-writer workloads pass 30 runs. One paired block exceeds 5%; the complete aggregate retains it. See [the performance report](pre-recording-performance.md) for raw evidence, 100 ns timer limits and block variation. |

## Executed checks

The final `check.bat` run exited successfully with `KEEPPEEK_RUN_SLOW_TESTS=1`
and `NEXTEST_TEST_THREADS=4`: 2,674 Rust tests passed (21 existing skips), strict
Clippy, dependency checks, Rust/TOML/Python formatting, UI registry/static checks,
394 Bun tests, 199 browser component tests, 57 compatibility tests, and 271
Playwright end-to-end tests passed (two existing skips). Svelte reported zero
errors and warnings. End-to-end tests used default local parallelism, unchanged
assertions and no retries. The complete local log is
`target/preroll-canonical-final-check.log`.

The final admission allocation change passed all 39 focused engine tests:
`cargo test --locked --lib storage::engine:: -- --nocapture`.
The policy lookup borrows the existing camera ID and only allocates a key when
inserting a previously unseen source. Configuration locking and enqueue ordering
remain covered by the existing concurrency tests. Its performance qualification
is included in the final 90-run comparison below.

The explicit upgrade fixture is
`tests/event_preroll_configuration.rs::every_legacy_recording_mode_upgrades_with_preroll_disabled`.
It passed for `off`, `sub`, `main`, `both`, and `event-boost` through disk loading,
serialization and disk reload, using
`cargo test --locked --test event_preroll_configuration -- --nocapture`.
The serialized field diff is the same for each mode:

```diff
 recording_mode = "<unchanged legacy mode>"
 event_recording_duration_secs = 45
+event_pre_recording_duration_secs = 0
+event_recording_stream = "main"
```

Final storage qualification: `cargo test --locked --lib storage:: -- --nocapture`
passed 270 tests with one pre-existing ignored benchmark. The channel layout test
first failed because optional event input enlarged each command slot from 176 to
240 bytes. Boxing only the queued event frame restores the legacy slot ceiling;
the full run verifies reservation release, ordering, and failure recovery.
The two decoder-memory
regressions were observed failing before the fix; both now pass. Decoder epochs
retain 32-byte SHA-256 fingerprints, including every AVC/HEVC configuration field,
instead of parameter vectors that could bypass the encoded-history budget.
Oversized H.265 parameters now return `InvalidData` before the parser's former
panic. The same run covers identical/changed decoder epochs, real codec/AAC
transitions, event storms, and concurrent configuration ordering.
The final log is `target/preroll-channel-layout-storage.log`; the observed failing
layout regression is in `target/preroll-channel-layout-red.log`.

The host is Windows 11 Pro, AMD Ryzen 5 5600G, 16 GB RAM, Rust/Cargo 1.98.1.
Browser workflows use the repository's Playwright Chromium fixture at 320px and
1440px. Dedicated temporary recording output uses C: NTFS; build outputs use E:
ReFS. No camera credentials or private recordings are required.

- `cargo test --locked --lib preroll -- --nocapture`: five passed.
- The same Cargo-built test executable, filter `storage::event_recording`:
  16 passed, including all four real-media/storm tests.
- The same executable, filter `pre_recording_health_wire`: one passed.
- `KEEPPEEK_RUN_SLOW_TESTS=1 cargo test --locked -p keeppeek --test storage_pipeline -- --nocapture`:
  four passed, 570 ingested frames and six inspected MP4 outputs per pipeline
  scenario. This explicitly enabled the slow assertions.
- Benchmark-feature strict Clippy passed; the monotonic failure-counter
  regression passed and proves recovered errors remain observable.
- Final focused UI capture/diagnostic run: five passed; E2E typecheck and lint
  passed. Screenshots were visually inspected.

Local logs are `target/preroll-final-focused.log`,
`target/preroll-final-runtime.log`, `target/preroll-final-health-wire.log`,
`target/preroll-slow-storage.log`, `target/preroll-bench-clippy.log`,
`target/preroll-bench-counter-test.log`, and
`target/preroll-ui-final-captures.log`.

The first integrated storage run exposed an AAC switch-boundary bug and a
legacy-writer fixture configuration mistake. Both corrected regressions pass in
the final focused run. The first complete Windows check passed 1,620 tests before
an unchanged ONVIF deadline fixture observed one request instead of two while
other release builds were running. The unchanged test passed immediately in
isolation. A later complete Rust run passed all 2,674 tests with slow assertions
enabled. Its local browser stage passed 266 tests but failed five navigation or
page-initialization checks, with an observed Svelte runtime initialization error.
All five passed unchanged in isolation; cold-cache parallel repetitions did not
reproduce the original errors. Two repeated backup restores correctly returned
HTTP 409 after a prior repetition had staged a pending restore. No production
change, assertion relaxation, or worker-count change followed this investigation.
The original log is `target/preroll-canonical-browser-transient.log`.

The extended [Main workflow, run 36282572698](https://github.com/xnorpx/keeppeek/actions/runs/36282572698),
passed all eight jobs: Windows x64/ARM and Linux ARM platform qualification,
Windows/macOS browser suites, coverage, CodeQL and slow Rust tests. Its first
macOS attempt exceeded the existing 100 ms Dashboard return budget and also
encountered a touch-fixture image-decoding race. The unchanged second attempt
passed. These initial failures remain visible in the workflow history.

## Browser evidence

The screenshots use the public documentation address `192.0.2.42` and synthetic
configuration/health fixtures. They contain no camera credentials or imagery.

| State              | Desktop, 1440px                                                                   | Mobile, 320px                                                                    |
| ------------------ | --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Enabled            | [Controls](verification/event-preroll/1440-event-pre-recording-enabled.png)       | [Controls](verification/event-preroll/320-event-pre-recording-enabled.png)       |
| Disabled, zero pre | [Controls](verification/event-preroll/1440-event-pre-recording-disabled.png)      | [Controls](verification/event-preroll/320-event-pre-recording-disabled.png)      |
| Invalid duration   | [Error state](verification/event-preroll/1440-event-pre-recording-validation.png) | [Error state](verification/event-preroll/320-event-pre-recording-validation.png) |
| Server diagnostics | [Coverage](verification/event-preroll/1440-event-pre-recording-diagnostics.png)   | [Coverage](verification/event-preroll/320-event-pre-recording-diagnostics.png)   |

Run the checked-in `ui/e2e/camera.e2e.ts`, `add-camera.e2e.ts`, and `health.e2e.ts`
through the repository's E2E preparation and runner scripts to reproduce these
flows. The canonical repository entry point remains `./check.sh` or `check.bat`.
