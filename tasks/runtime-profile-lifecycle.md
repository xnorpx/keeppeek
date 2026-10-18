# Runtime profile activation and recovery

Issue: [#202](https://github.com/xnorpx/keeppeek/issues/202). Baseline: `ce40a4b`.

Deliver one operator workflow: activate a profile, observe a real recording failure, repair storage,
reapply the same desired policy, restore Base, and recover the accepted policy after restart.
Existing Administrator, compare-and-set, privacy and WebRTC-only boundaries remain required.

The owner approved persistent camera identity across IP changes, with a fresh identity after
delete/re-add. The camera configuration owner stores UUID v4 `profile_identity` metadata in the
existing camera table. Public camera control IDs remain unchanged; the profile namespace exposes
an Administrator-only bounded identity catalog for target selection.

## Implementation and verification

- [x] Add an explicit recovery action for the active policy. Preserve draft, conflict, schema and
      authorization checks; keep the accepted profile selected during retry.
- [x] Migrate legacy targets atomically, preserve identity on IP edits, leave missing targets
      unavailable, and prove same-IP replacement does not inherit overrides. Preserve privacy
      before runtime changes and through failed acknowledgements.
- [x] Exercise the real StateStore dispatcher, camera loop and storage writer through failure,
      recovery, Base restoration and fresh-owner startup. Check recorded progress separately from
      configuration acknowledgement.
- [x] Cancel timed-out or superseded queued notification suppression commands. Prove that later
      execution cannot apply those abandoned requests; retain the limit for already executing work.
- [x] Verify UI activation, multiple clients, ordered updates and reconnect in Chromium. Exercise an
      isolated recorder restart if the existing fixture supports it without changing shared tests.
- [x] Update the operator recovery procedure and consolidate current implementation evidence.
- [x] Execute `secret_backed_camera_ip_uses_one_identity_and_camera_snapshot` after the final
      secret-IP compatibility change. Verify migration, raw-reference preservation, the shared
      loaded secrets snapshot and identity preservation when the companion secret changes.
- [x] Run focused regressions, independent review, the complete Windows `check.bat`, Markdown
      validation and the book build. Hosted CI monitoring stays with the owner.

## Bounds and acceptance limits

Retry reuses the existing revisioned activation operation and adds no queue, scheduler, endpoint,
schema or dependency. Notification cancellation retains only one latest request per shared handle;
the existing 256-command queue and five-second acknowledgement timeout stay bounded.

Camera deletion and ordinary edits remain allowed. An active target with ambiguous source routing
fails validation before media starts. A missing legacy IP becomes a tombstone and is never converted
again when that IP is reused. Migration advances affected definition and registry revisions once;
failure leaves the original file unchanged.

Supported camera IP references such as `{secret:CAMERA_IP}` remain raw in `config.toml`.
Migration, the profile identity map and actual camera definitions resolve against one loaded disk
`secrets.toml` snapshot. A later secrets-file edit does not change that loaded snapshot; the next
configuration load uses the changed address while retaining the camera UUID. The regression
passes for migration, raw-reference preservation, the loaded snapshot and a fresh load after rotation.

Migrated profile configuration carries `identity_schema = 1`. Older binaries reject it before
media starts. Configuration rewrites preserve it, including after every camera is removed;
WebRTC profile documents retain their existing shapes.

The complete #202 contract also requires bounded asynchronous convergence, the full privacy byte
matrix, maximum-capacity transport and retained-memory evidence, and release-build activation and
browser latency measurements. These remain acceptance items until their specific evidence passes.

## Local measurements

Final-source Windows `check.bat` passes with `KEEPPEEK_RUN_SLOW_TESTS=1`, Rust 1.99.0 and
`CARGO_INCREMENTAL=0`: 3,385 Rust tests pass (four slow, 27 skipped) in 475.406 seconds, followed
by strict workspace Clippy, dependency, formatting and Python checks. UI quality passes 410 Bun,
283 Chromium and 71 compatibility tests. The fresh-release E2E run passes 293 tests in 2.9 minutes;
two existing H.265 capability-dependent tests are skipped on this browser.

All six runtime-profile E2E tests pass in that final run, including two Settings clients, CAS
conflict recovery, reconnect, an isolated recorder restart, persisted policy, Base restoration and
desktop/mobile CRUD. Separate final focused runs pass 108 profile regressions and the secret-IP
snapshot regression; one opt-in timing benchmark is ignored by default. The camera CLI shared-loader
regression was reproduced red on the old path and green after the fix. Independent configuration,
backend, UI and activation-surface reviews found no remaining blocker; protected `api/` is unchanged.
Markdown validation and the book build pass. Raw logs are retained under
`target/agent-artifacts/profile-lifecycle-*`; hosted CI remains with the owner.

Windows x64 10.0.26200, AMD Ryzen 5 5600G, Rust 1.99.0 debug, `CARGO_INCREMENTAL=0`.
Timing ran without a concurrent build. These measurements do not qualify the release-build,
127-camera/32-profile external WebRTC and browser budgets from #202.

The notification comparison substitutes the `ce40a4b` notification implementation into the same
branch and identical benchmark harness. Each of five fresh native processes makes 100 sequential
setter calls against an empty outbox. The table reports medians of process percentiles; startup,
external delivery and already executing work are outside this measurement.

| Notification setter acknowledgement |       Before |        After |
| ----------------------------------- | -----------: | -----------: |
| Median of process p50 values        |      16.2 us |      17.2 us |
| Median of process p95 values        |      21.2 us |      19.4 us |
| Process p50 range                   |  3.5–17.5 us | 15.7–18.0 us |
| Process p95 range                   | 16.3–22.2 us | 16.6–21.1 us |
| Maximum across all five processes   |     190.0 us |     232.5 us |

The small timing differences do not establish an improvement. The important behavioral change is
the deterministic rejection of timed-out and superseded queued requests.

For 127 UUID-mapped cameras, five processes each force 30 healthy and 30 failed health polls.
The median process median/p95 is 53/55 us for healthy polls and 71/74 us for failed polls.
Process p95 ranges are 54–82 us and 71–79 us respectively. Historical IP-based measurements in
[#309](https://github.com/xnorpx/keeppeek/pull/309) were 30/37 us and 49/49 us. That baseline was
not rerun here; identity representation differs. Each poll still examines at most 127 entries per
100 ms. These local measurements exclude transport and media throughput.

The 127-source identity index has a 48-byte header and 18,796 bytes of owned string capacity in
the IPv6 fixture; BTree node allocations are excluded. The loop header is 1,088 bytes. This is
not a complete retained-memory measurement.

The actual 127-target UUID Degraded fixture encodes a 60,410-byte value, 60,533-byte Get,
60,582-byte snapshot and 60,609-byte watch; watch admission succeeds. This exceeds the original
16 KiB document/48 KiB snapshot budgets, so the multipart acceptance work remains open.

Reproduce the committed fixtures with:

```powershell
cargo test --lib secret_backed_camera_ip_uses_one_identity_and_camera_snapshot -- --nocapture
cargo test --lib notification_profile_suppression_latency -- --ignored --nocapture
cargo test --lib runtime_profile_recording_health_poll_scale -- --nocapture
cargo test --lib profile_identity_index_records_owned_string_capacity_for_127_sources -- --nocapture
cargo test --lib runtime_profile_full_degraded_status_fits_get_and_watch_transport -- --nocapture
```
