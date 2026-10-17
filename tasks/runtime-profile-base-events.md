# Preserve Base event settings across profile transitions

Owner: #202, dispatched from #147. Base: main `8c966fe` after merged #305.

## Contract

An events-only camera edit must update the camera definition that profile transitions reuse.
After an availability override stops the camera, returning to Base must restore the edited event
configuration and generic-motion retention setting. The events-only fast path must retain its
existing media workers. Preserve the accepted profile revision and all API/configuration formats.

Use the existing camera-definition owner, not another settings cache. This adds one bounded camera
clone per events-only configuration update and no work per media frame. Compare a 31-edit loopback
fixture before and after the fix; it also cycles availability and reconnects the RTSP worker.

## Verification

- Reproduce the stale Base restore using real loop-owned media workers and a loopback listener.
- Verify an events-only edit keeps the media worker identity and Base restores the last edit.
- Run profile tests, relevant camera-event tests, all-target Clippy, formatting and docs checks.
- Review independently and publish a PR; leave hosted CI monitoring to the owner.
- Full #202 stable identity, storage failure, privacy/restart, browser and release qualification
  remain open; this slice does not change those acceptance criteria.

## Local evidence

Windows 11 Pro x64, AMD Ryzen 5 5600G, Rust 1.99.0 debug build, `CARGO_INCREMENTAL=0`,
2026-10-17. Builds used `--locked --offline` and the checksum-verified cached public camera database
via `KEEPPEEK_CAMERA_DATABASE_ARCHIVE`.

- RED: the regression failed after Away -> Base restored the old generic-motion retention setting.
- `cargo test --locked --offline -p keeppeek --lib profile -- --nocapture --test-threads=1`:
  64 passed, including the new real-worker regression.
- The same native test executable passed 16 `server::state_store_watch::tests::` tests and the
  `keeppeek::events::tests::` restart-selection test.
- `cargo clippy --locked --offline --all --all-targets -- -D warnings`, Rust formatting,
  Markdown formatting, book build, whitespace and protected-API checks passed. Book build emits
  the existing mermaid 0.5.0 / mdbook 0.5.4 warning.
- Independent review found no blocking issue in the definition update, worker cleanup or restored
  policy checks. The retention assertion reads registered policy, not an emitted physical event.

The isolated 31-edit fixture measured before/after median 41 / 47 us (+6 us), p95 70 / 60 us
(-10 us), and max 671 / 371 us (-300 us). Both runs include cold first-edit effects. Media worker
identity stayed unchanged through all edits. These are local debug observations, not evidence of a
performance improvement or qualification of the full live-camera/server/browser/release budget.

Full canonical, slow post-merge and hosted CI checks are not claimed here. Rollback is a code revert;
persisted formats do not change.
