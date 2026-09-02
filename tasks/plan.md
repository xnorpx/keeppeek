# Implementation Plan: Validated Backup, Restore, and Migration

## Overview

Implement issue #128 as an Administrator-only HTTP API whose control and status documents use
the canonical ProtoJSON mapping from a dedicated `keeppeek.backup.v1` schema. Backup artifacts
stream as bounded ZIP bodies over HTTP. The implementation creates consistent reference-only
snapshots, validates uploaded bundles before planning, stages selected restores, activates them
through a crash-recoverable journal, retains one bounded rollback point, exposes the workflow in
Settings and the CLI, and audits every security-relevant operation without logging bundle data.

## Architecture Decisions

- Add `api/backup.proto` as the canonical HTTP JSON and manifest contract. Generate Rust
  ProtoJSON codecs with `pbjson` and TypeScript types with the existing Buf ES generator.
- Use lower-camel ProtoJSON with named enums and quoted 64-bit integers. Reject unknown fields at
  the HTTP boundary so unsupported clients and future schemas fail before mutation.
- Keep control traffic JSON. Stream ZIP upload and download bodies as `application/zip` to avoid
  base64 expansion and unbounded whole-artifact JSON allocations.
- Store managed artifacts under a bounded `.backups` directory beside `config.toml`. Use opaque
  UUID identifiers, deterministic member paths, per-section SHA-256, a whole-artifact SHA-256,
  and owner-only permissions.
- Export supported durable state into independent logical sections: runtime/camera/group and
  integration configuration, recording/event catalog metadata, notification state, access
  records, layouts, configuration templates, and thumbnail inventory. Omit caches and media bytes.
- Sanitize configuration and notification data from typed supported fields. Preserve secret
  references, replace inline sensitive values with explicit unresolved references, and fail closed
  if a selected section cannot prove reference-only output.
- Snapshot Turso databases through database-native consistent transactions or snapshot primitives;
  never copy a live database file directly. Bind all section revisions into one manifest.
- Make dry-run plans immutable, content-addressed, revision-bound, capacity-checked, path-mapped,
  dependency-validated, and short-lived. Activation accepts only an unexpired unchanged plan.
- Stage every target on its target filesystem. A persistent journal records before-images and each
  swap. Startup completes rollback after interruption, and the previous state remains available
  until post-restore validation passes or the rollback window expires.
- Keep recording media outside the bundle. Catalog restore reports missing media and remaps source
  archive roots explicitly without treating media absence as archive corruption.
- Put Backup and restore in server-wide Settings. The browser keeps uploaded `File` objects only in
  memory, preserves draft mappings after validation errors, and never stores bundle bytes or
  credentials in browser persistence.
- Add direct `keeppeek backup` commands that use the same core planner and ProtoJSON documents as
  HTTP. Non-interactive output is machine-readable and secrets never appear in arguments.

## Task List

### Phase 1: Contract and Safe Bundle Core

- [x] Task 1: Add the versioned backup ProtoJSON contract and generated Rust/TypeScript bindings.
- [ ] Task 2: Extend the existing inspector with whole-artifact bounds, semantic section
      validation, dependency rules, and typed manifest conversion.
- [ ] Task 3: Create deterministic reference-only bundles for file-backed sections with secret
      scans and cross-platform fixtures.

### Checkpoint: Bundle Foundation

- [x] ProtoJSON round trips and rejects unknown or malformed input.
- [ ] Adversarial archive, section dependency, path, checksum, size, and secret tests pass.

### Phase 2: Consistent State and Restore Planning

- [ ] Task 4: Add consistent recording and notification database snapshot/export adapters.
- [ ] Task 5: Build revision-bound dry-run plans with migration, conflict, ID, path, capacity,
      permission, media consequence, and restart evidence.
- [ ] Task 6: Stage selected sections and activate them through a crash-recoverable rollback journal.

### Checkpoint: Recovery Core

- [ ] Same-version and supported older-version round trips preserve identities and references.
- [ ] Failure injection before, during, and after activation restores the exact previous state.

### Phase 3: HTTP API, CLI, and Audit

- [ ] Task 7: Add bounded Administrator-only HTTP JSON lifecycle endpoints plus streaming ZIP
      upload/download and operation metrics/audit events.
- [ ] Task 8: Add `keeppeek backup` create, list, inspect, dry-run, restore, rollback, and delete
      commands with ProtoJSON output.

### Checkpoint: Automation

- [ ] HTTP and CLI authorization, cancellation, timeout, concurrency, restart, and secret tests pass.
- [ ] Linux, macOS, and Windows path fixtures produce equivalent plans.

### Phase 4: Administrator UI and End-to-End Evidence

- [ ] Task 9: Add the typed HTTP client and responsive Settings backup/restore workflow with
      progress, warnings, mapping drafts, confirmation, rollback, and deletion.
- [ ] Task 10: Add browser and real-process round-trip coverage for cameras, policies, groups,
      layouts, catalog metadata, users, notifications, and integrations.
- [ ] Task 11: Document bundle format, operator recovery, migration support, limits, security,
      CLI/API examples, and rollback behavior.

### Checkpoint: Complete

- [ ] Focused contract, Rust, UI, CLI, HTTP, migration, failure-injection, and browser tests pass.
- [ ] Performance benchmarks meet documented snapshot, dry-run, activation, and memory budgets.
- [ ] `./check.sh` passes from the repository root.
- [ ] Every issue criterion has final-commit evidence in the PR table.

## Risks and Mitigations

| Risk                                                   | Impact   | Mitigation                                                                                 |
| ------------------------------------------------------ | -------- | ------------------------------------------------------------------------------------------ |
| A default artifact leaks a supported or unknown secret | Critical | Export allowlisted typed fields, scan final ZIP/log/API/browser evidence, and fail closed  |
| Multi-file activation crashes between swaps            | Critical | Persist a before-image journal, use same-filesystem atomic renames, and recover on startup |
| A live database copy is inconsistent                   | Critical | Use database-native snapshots or one bounded read transaction, never raw file copy         |
| Partial restore breaks cross-references                | High     | Encode section dependencies and validate all references before creating a plan             |
| A stale plan overwrites newer state                    | High     | Bind plans to revisions, source digest, target paths, expiry, and exact selected sections  |
| Archive upload exhausts memory or disk                 | High     | Stream to an owner-only temporary file with compressed and uncompressed limits             |
| Catalog restore implies missing media exists           | High     | Validate exact media coverage, report gaps, and require explicit path mappings             |
| Restore invalidates the active process                 | High     | Stage first, restart only required owners, retain rollback, and gate completion on health  |
| ProtoJSON evolves incompatibly                         | Medium   | Version package/messages, add fields only, reject unknown input, and test old fixtures     |

## Open Questions

- None. The user authorized a protobuf-defined HTTP JSON API. Bulk ZIP bytes remain streamed HTTP
  bodies because encoding them in JSON would violate the feature's bounded-memory requirement.
