# Implementation Plan: Validated Backup, Restore, and Migration

## Overview

Implement issue #128 as an Administrator-only recovery workflow with a protobuf-defined ProtoJSON
control plane over HTTP and separately streamed ZIP artifacts. The workflow creates reference-only,
manifest-driven snapshots; validates uploaded bundles without mutation; produces immutable dry-run
plans; stages every selected target; activates before application state opens; verifies startup
health; and retains a bounded rollback point.

## Architecture Decisions

- Define `keeppeek.backup.v1` in `api/backup.proto`. Use canonical ProtoJSON for control requests,
  responses, and errors. Stream ZIP bodies as `application/zip` to avoid base64 overhead and large
  JSON allocations.
- Keep format 1 inspection compatible and make format 2 the current manifest. Reject future formats
  and unsupported section schemas before extraction.
- Sanitize raw configuration before secret resolution. Preserve exact references; replace inline or
  unknown private strings with deterministic external references. Sanitize notification databases
  offline and reject uploaded snapshots that retain resolved destinations or operational state.
- Snapshot live Turso databases through native `VACUUM INTO` commands on their owning serialized
  threads. Do not copy live database files.
- Treat recording catalog and event metadata as one physical consistency boundary. Event metadata
  is a logical selector, and thumbnail sections contain inventory evidence instead of JPEG bytes.
- Bind every plan to the archive digest, canonical target paths, target configuration revision,
  selected sections, dependencies, capacity evidence, migrations, required secrets, and ten-minute
  expiry.
- Stage owner-only targets beside their destinations. Persist a versioned journal before activation.
  Ordinary files use exact staging-time before-image digests; mutable databases receive native
  before-images after shutdown so normal writes through restart remain rollback-safe.
- Recover the journal before configuration or databases open. Mark completion only after
  configuration, HTTP, and camera-worker startup succeed. Retain before-images for 30 minutes.
- Advertise only sections with current durable owners. Generic StateStore and server-defined groups
  remain unavailable until their owning features implement persistence.
- Reuse the same HTTP surface from Settings and `keeppeek backup`. Keep remote credentials only in
  `KEEPPEEK_ACCESS_KEY` and never accept them in URLs or CLI arguments.

## Task List

### Phase 1: Contract and Archive Boundary

- [x] Define the additive backup ProtoJSON contract and generate Rust/TypeScript bindings.
- [x] Bound and validate ZIP size, expansion, members, paths, types, checksums, and schemas.
- [x] Preserve format 1 inspection and report explicit format migration.
- [x] Create deterministic format 2 reference-only bundles.

### Phase 2: Consistent State and Secret Safety

- [x] Snapshot recording and notification databases through their owning threads.
- [x] Sanitize configuration, access activity, notification destinations, and provider state.
- [x] Validate access, layout, template, event, thumbnail, and notification section semantics.
- [x] Inventory external thumbnails without copying media bytes.

### Phase 3: Dry Run, Activation, and Rollback

- [x] Produce immutable plans with dependency, migration, conflict, secret, canonical path,
      capacity, merged-configuration, and restart evidence.
- [x] Stage and verify all targets before mutation.
- [x] Activate through a crash-recoverable startup journal.
- [x] Preserve latest pre-activation database state and exact ordinary-file before-images.
- [x] Roll back partial activation, failed startup health, or confirmed completed restores.
- [x] Preserve stable camera, credential, event, layout, and notification identities in a full
      supported-domain round trip.

### Phase 4: Product and Automation

- [x] Add Administrator-only HTTP create/list/upload/download/inspect/plan/activate/status/rollback/delete.
- [x] Add stable typed errors, no-store responses, CORS, idempotent request IDs, and audit evidence.
- [x] Add `keeppeek backup` machine-readable automation without secret arguments.
- [x] Add capability-gated Settings UI with mobile layout, draft retention, explicit confirmation,
      verification, progress, warnings, and persisted health evidence.
- [x] Add public API, operator, book, changelog, and migration/rollback documentation.

### Checkpoint: Publication

- [x] Complete final fresh-context correctness and security review.
- [x] Run focused Rust, CLI, UI, HTTP, browser, and benchmark validation on the final tree.
- [ ] Run `./check.sh` from the repository root.
- [ ] Publish a PR with one evidence row per issue criterion and final-commit CI links.

## Acceptance Criteria Verification

| Criterion                                                                             | Testable outcome                                                                                                                                        | Verification                                                                                                                  | Current evidence                 |
| ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | -------------------------------- |
| Create and inspect a versioned checksummed backup                                     | Administrator creation and upload return a manifest only after section and archive verification                                                         | `backup::manager` and `server::tests::backup_http_*`; `ui/e2e/backup.e2e.ts`                                                  | Focused Rust and Playwright pass |
| Default bundles contain references but no resolved secrets, sessions, or keys         | Configuration and notifications sanitize before packaging; uploaded sections reject resolved/private runtime state                                      | `creates_deterministic_reference_only_configuration_sections`, notification backup tests, full round trip                     | Focused tests pass               |
| Dry run reports compatibility before mutation                                         | Plans report formats, schemas, dependencies, canonical paths, target revision, secrets, capacity, thumbnail evidence, merged config, and restart impact | `backup::restore::tests::dry_run_*`                                                                                           | Focused tests pass               |
| Activation is staged, crash-safe, and rollback-bounded                                | Live state is unchanged through staging; startup journal applies before owners open and restores before-images on failure                               | Restore lifecycle, partial activation, interrupted database, changed-target, and failed-health tests                          | Focused tests pass               |
| Same/older restores preserve identities and references                                | Format 2 full-domain and format 1 migration paths retain stable records and explicit external references                                                | `full_supported_round_trip_preserves_ids_references_and_mapped_paths`, `legacy_format_one_runtime_config_migrates_atomically` | Focused tests pass               |
| Future schemas and invalid selections fail without mutation                           | Unsupported formats/schemas, duplicate sections, missing dependencies, stale revisions, changed digests, and blocked plans fail closed                  | Archive adversarial suite and stale-plan tests                                                                                | Focused tests pass               |
| CLI/UI/cross-platform/security/migration/failure/rollback/full-round-trip checks pass | One generated contract drives Rust, browser, and automation; path fixtures cover Unix, Windows drive, and UNC sources                                   | ProtoJSON tests, CLI E2E, Settings E2E, restore failure suite, canonical gate                                                 | Final canonical gate pending     |

## Performance Evidence

Command:

```sh
cargo test --release --locked --lib \
  backup::restore::tests::backup_restore_performance_benchmark \
  -- --ignored --exact --nocapture
```

Environment: macOS 26.6.2 arm64, Apple M5 Max, Rust 1.97.1. Workload: 10 runs of a 14,595-byte
bundle containing every currently supported section and one camera/event/recording/notification
fixture.

| Metric                        | Result p50 | Result p95 | p95 delta from baseline |        Budget |
| ----------------------------- | ---------: | ---------: | ----------------------: | ------------: |
| Stopped-service raw file copy |   4.794 ms |  22.918 ms |                Baseline | Baseline only |
| Validated online creation     | 187.041 ms | 583.204 ms |  +560.286 ms (+2444.7%) |  2,000 ms p95 |
| Non-mutating dry run          |  76.984 ms | 162.267 ms |                     N/A |    500 ms p95 |
| Staging                       | 449.072 ms | 677.259 ms |                     N/A |  2,000 ms p95 |

The final workflow is slower than raw copying because it adds consistent live snapshots,
sanitization, schema/checksum validation, migration planning, and rollback evidence. It remains
within all declared budgets.

## Risks and Mitigations

| Risk                                        | Impact   | Mitigation                                                                                                |
| ------------------------------------------- | -------- | --------------------------------------------------------------------------------------------------------- |
| A bundle smuggles secrets or provider state | Critical | Fail-closed raw sanitization, owning section validators, artifact scans, and generic public errors        |
| A crash exposes partially restored state    | Critical | Durable journal, activation before owners open, reverse-order reconciliation, and failure-injection tests |
| Live database writes invalidate rollback    | Critical | Native startup-time database before-images after prior owners stop                                        |
| A mapped path escapes through a symlink     | High     | Canonicalize into the immutable plan and re-resolve before staging                                        |
| Selective restore breaks cross-references   | High     | Manifest dependency closure and owning schema validation                                                  |
| Media is mistaken for bundle content        | High     | Explicit omission, path mapping, catalog rewrite, and thumbnail inventory warnings                        |
| Recovery work blocks recording too long     | Medium   | Owner-thread native snapshots, bounded waits, serialized operations, streaming buffers, and p95 budgets   |

## Open Questions

- None. The user authorized an additive protobuf model carried as HTTP ProtoJSON with binary ZIP
  transfer bodies.
