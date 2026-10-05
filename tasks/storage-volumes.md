# Named storage volumes (#129)

Branch: `feat/129-storage-volumes`. Draft PR: #269.

## Scope

The owner's October 4 clarification supersedes the original upgrade requirements:
this is new development, with no deployed storage layout to adopt or migrate.
Do not build legacy inventories, captured paths, adoption endpoints, or backward
compatibility workflows. Moves and drains operate between named volumes.

Use one named-volume model for recordings, exports, thumbnails, and metadata.
A fresh installation must have useful defaults without hand-written placement rules.
Settings remain in `config.toml`, with private references in `secrets.toml`.
The approved API scope includes `api/webrtc.proto`, `api/webrtc.md`, and generated bindings.

## Invariants

- Existing objects retain their recorded owner when placement policy changes.
- A missing bound root is offline. Never recreate it or write to its ancestor.
- Reserve capacity before writing, including shared-filesystem reservations.
- Unavailable destinations reject admission unless the rule explicitly permits fallback.
- Moves retain the source until the destination is verified and published.
- Readers, moves, and retention share ownership checks and durable job state.
- Metadata relocation requires a restart and preserves one catalog authority.
- Removal requires drain and no remaining objects, reservations, or cleanup work.
- Limits remain 32 volumes, 256 rules, and 8 candidates per rule.

## Remaining implementation and verification

- [x] Finish removing the adoption/captured-path implementation and its contract surface.
- [x] Initialize useful named defaults and route every production media writer through them.
- [x] Enable validated volume configuration and complete health/status reporting.
- [x] Complete bounded bulk move/drain preview, confirmation, progress, and cancellation UI.
- [ ] Verify export/thumbnail placement, restart recovery, unavailable volumes, and retention.
- [ ] Verify named metadata relocation and removal without compatibility prerequisites.
- [ ] Regenerate bindings, synchronize operational documentation, and review the final diff.
- [ ] Pass focused tests, the canonical Windows `check.bat`, and final-head platform CI.
- [ ] Record practical local-service scale evidence and make the PR ready when complete.

Historical test results from removed compatibility slices do not validate the simplified
implementation. The PR remains draft until the remaining work and checks are complete.

## Current verification

- The initial Windows gate passed 3,184 Rust tests with slow tests enabled, then
  found four Clippy errors. Those errors are fixed; the final gate is pending.
- All 13 storage-volume component tests passed in Chromium.
- The configured recording-seed integration test verifies named ownership, finalized
  MP4 samples, and refusal to recreate an offline metadata root.
- The configuration book builds with mdBook 0.5.4 and Mermaid 0.17.1.
- `named_writer_local_scale` compares the same writer with and without named
  ownership. Thirty runs after one warmup alternate execution order, each writing
  eight cameras with 64 synthetic keyframes per camera and verifying every output.
  Windows debug results: baseline median 2,743.857 ms / p95 3,660.614 ms;
  named median 6,013.121 ms / p95 9,476.487 ms. This is a synchronous durability
  workload, not a live-camera throughput result. Final isolated performance and
  platform evidence remain required before completion.
