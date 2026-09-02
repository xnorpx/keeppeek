# Issue #128 Tasks

## Contract and bundle

- [x] Define `keeppeek.backup.v1` and generate Rust/TypeScript ProtoJSON bindings.
- [x] Create deterministic, versioned, checksummed reference-only ZIP bundles.
- [x] Bound archive bytes, expansion, members, section sizes, metadata, and parser work.
- [x] Reject traversal, unsafe paths, symlinks, encrypted/duplicate members, malformed sections,
      corrupt databases, bad checksums, and unsupported formats or schemas.

## Durable state and secrets

- [x] Snapshot recording and notification databases through their owning serialized threads.
- [x] Preserve camera, event, credential, layout, template, notification, and integration identities.
- [x] Remove resolved secrets, sessions, access activity, delivery history, and provider state.
- [x] Declare required target secret references and omitted media/cache data.
- [x] Advertise only sections backed by implemented durable owners.

## Restore safety

- [x] Validate dependencies, canonical mappings, capacity, permissions, secrets, merged
      configuration, migrations, conflicts, restart impact, and external thumbnail evidence in dry run.
- [x] Bind plans to one digest, target revision, selected section set, mapping set, and expiry.
- [x] Stage every selected target before mutation and verify all staged digests.
- [x] Apply before owners open and retain crash-safe before-images.
- [x] Restore the latest closed database state and exact ordinary-file state on rollback.
- [x] Roll back partial activation and failed startup health automatically.
- [x] Retain a confirmed rollback point for 30 minutes.

## Product surfaces

- [x] Add Administrator-only HTTP ProtoJSON lifecycle endpoints and streamed ZIP transfers.
- [x] Add typed errors, no-store responses, strict CORS, idempotency, and audit events.
- [x] Add `keeppeek backup` machine-readable CLI commands without secret arguments.
- [x] Add capability-gated desktop/mobile Settings workflows and preserve failed drafts/mappings.
- [x] Display checksum, size, per-section verification, warnings, progress, and final health checks.
- [x] Document the API, operator procedure, CLI, sections, limits, migration, and rollback.

## Verification

- [x] Focused Rust backup suite passes.
- [x] Focused UI unit tests pass.
- [x] Focused production-process Playwright backup suite passes.
- [x] Release benchmark passes all p95 budgets with reproducible output.
- [x] Final strict Clippy, format, dependency, build, and complete test gates pass.
- [x] Fresh-context review has no unresolved correctness or security findings.
- [x] `./check.sh` passes on executable commit `55ea282`.
- [x] PR draft contains the complete acceptance evidence table and final-head CI links.
