# Runtime profile status watch delivery

Owner: [#202](https://github.com/xnorpx/keeppeek/issues/202), dispatched from #147.
Base: `2311446`. PR #302 supplies runtime admission and completion revision fencing.

## Contract

Publish camera startup results and later loop-owned profile completion through the existing
state-store status watch. Reuse the current session authorization, camera projection, sequence
acknowledgement, and bounded notification queues. Release the shared status lock before invoking
the publisher. Rejected superseded work must not publish an old result.

Attach the publisher in the application runtime wiring. Keep the existing configuration format,
profile desired revision, watch sequence, activation surface, and protected `api/` unchanged.
The publisher reads current shared state instead of retaining a potentially superseded snapshot.

## Verification

- Reproduce the missing startup publication with a disabled camera fixture.
- Decode notifications delivered to two staged WebRTC sessions after loop-owned completion.
- Preserve the existing supersession and authorization regressions.
- Run focused Rust tests, Clippy, formatting, documentation checks, and independent review before
  publishing a PR. Record remaining validation honestly; do not wait for hosted CI.

This adds one watch fan-out per startup camera batch and completed transition. A standalone
camera addition publishes its result separately. Serialization and
projection use the existing bounded status/watch machinery. Measure emitted frames/bytes and
completion latency in the integration fixture. This does not qualify the full live 127-camera,
privacy/restart, or stable-identity acceptance matrix.

## Local evidence

Windows x64, Rust 1.99.0, `CARGO_INCREMENTAL=0`, 2026-10-17:

- Startup regression reproduced before wiring: zero publications instead of one.
- `cargo test --locked -p keeppeek --lib profile_ -- --nocapture`: 40 passed.
- `cargo test --locked -p keeppeek --lib server::state_store_watch::tests::`: 16 passed.
- Two staged API-session queues received two decoded completion frames, 418 bytes total;
  one debug fixture completed in 458 microseconds. This excludes encryption and browsers.
- The separate 127-disabled-camera, 30-transition debug fixture measured median 1,494 us,
  p95 2,061 us. It does not establish a controlled performance delta or live-camera capacity.
- Markdown formatting, book build, and the protected-API/diff whitespace checks passed.
- `cargo fmt --all --check` and `cargo clippy --locked --all --all-targets -- -D warnings` passed.
- Independent review found no remaining blocking correctness or lock-order issue after batching
  and snapshot-registration fixes.

Full canonical, hosted CI, live-camera and release-build acceptance are not claimed here.
