# Named storage volumes (#129)

Branch: `feat/129-storage-volumes`. PR: #269.

## Scope

The owner's October 4 clarification supersedes the original upgrade requirements:
this is new development, with no deployed storage layout to adopt or migrate.
Do not build legacy inventories, captured paths, adoption endpoints, or backward
compatibility workflows. Moves and drains operate between named volumes.

Use one named-volume model for recordings, exports, thumbnails, and metadata.
A fresh installation has useful defaults without hand-written placement rules.
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
- Configuration restore retains target ownership and rejects conflicting root secrets.
- Windows owned roots require NTFS; other Windows filesystems remain unqualified.

## Implementation and verification

- [x] Remove the adoption/captured-path implementation and its contract surface.
- [x] Initialize useful named defaults and route every production media writer through them.
- [x] Enable validated volume configuration and complete health/status reporting.
- [x] Complete bounded bulk move/drain preview, confirmation, progress, and cancellation UI.
- [x] Verify export/thumbnail placement, restart recovery, unavailable volumes, and retention in Rust tests.
- [x] Verify named metadata relocation and removal without compatibility prerequisites in Rust tests.
- [x] Regenerate bindings, synchronize operational documentation, and review the remaining safety paths.
- [ ] Pass the complete canonical Windows `check.bat` and final-head platform CI.
- [ ] Obtain approval of numeric performance budgets and make the PR ready when qualified.

## Current verification

The current full Windows run passes all 3,189 Rust tests (including slow media tests),
with 26 skipped diagnostics/platform cases. The rest of the gate is still running.
Three new configuration-restore regressions passed after failing against the old code.
The configured recording-seed integration verifies named ownership, finalized MP4 samples,
and refusal to recreate an offline metadata root. The configuration book builds with
mdBook 0.5.4 and Mermaid 0.17.1.

Two ignored reproducible diagnostics run alone in Rust 1.99 debug builds on Windows 11,
Ryzen 5 5600G, using local NTFS disks. Thirty measured runs follow one warmup.

- `named_writer_local_scale`: eight cameras, 64 synthetic keyframes each; verifies every
  MP4 and named ownership. Baseline durable batch median/p95 2,601.324/2,735.698 ms;
  named 5,806.548/6,065.157 ms. Buffered-ingest median/p95 0.001/0.001 ms in both modes.
  This is synchronous durability evidence, not live-camera throughput qualification.
- `volume_drain_local_scale`: 8MiB export-kind object moved between C: NVMe NTFS and
  D: SATA SSD NTFS, with one worker and 64KiB copy buffer. Plain copy/fsync/hash median/p95
  343.308/380.181 ms; named drain 1,884.839/2,028.203 ms. Catalog query idle/during-drain
  p95 1.357/1.837 ms; owned 64KiB read p95 2.571/3.440 ms. CPU median/p95 328/360 ms
  for plain copy versus 1,547/1,781 ms for named drain plus concurrent readers.
  Sampled resident maximum 37,511,168 bytes. Digest, authority and unrelated sentinel verified.

Proposed acceptance budgets await the owner's answer: p95 buffered ingest <100ms,
p95 catalog/read operations during drain <100ms, and p95 8MiB drain <5s. The live issue
requires numeric budgets approved before acceptance; elapsed time is not approval.
Final CI, real-backend browser evidence and budget approval remain open. The PR and issue
must not claim completion based only on focused tests or earlier revisions.
