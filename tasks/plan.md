# Implementation Plan: Visual Configuration and Safe Bulk Editing

## Overview

Implement issue #136 as a versioned, typed configuration service and a camera-fleet UI. The first release builds on the existing runtime, camera, storage, access, integration, notification, and appearance editors instead of replacing them. It adds the missing shared camera defaults, effective-value evidence, reusable templates, authoritative bulk previews, revision-aware atomic configuration writes, activation reporting, and bounded import/export.

## Architecture Decisions

- Add an additive `keeppeek.configuration.v1` WebRTC control contract. Existing camera and runtime commands remain compatible.
- Keep `config.toml` authoritative for runtime configuration. Preserve unrelated TOML values by editing its parsed table and writing one validated candidate atomically.
- Persist versioned templates in a bounded JSON sidecar beside `config.toml`. Templates contain typed camera/recording fields and secret references only; resolved secrets never enter responses.
- Apply templates as explicit per-camera overrides. Template edits never mutate cameras silently, and deleting a template does not change previously applied values.
- Represent every inheritable value as configured default, optional template proposal, optional camera override, effective value, source, applied state, and validation/capability evidence.
- Create bulk/template plans on the server against one configuration revision and an authoritative target snapshot. Applying a plan rechecks its revision and digest before one atomic configuration write.
- Activate each changed camera after the configuration commit. Report immediate/reconnect/restart impact and per-camera activation; a failed activation leaves the atomic persisted configuration staged and reports the required recovery action.
- Put shared defaults, templates, and bulk editing on `/cameras`, which owns fleet configuration. Link existing server-owned domain editors instead of duplicating storage, layouts, backup, access, integrations, notifications, logging, or appearance behavior.

## Task List

### Phase 1: Contract and Resolution Foundation

- [x] Task 1: Add the versioned configuration snapshot, template, plan, and apply protobuf contract and regenerate bindings.
- [x] Task 2: Add bounded typed template persistence and pure inheritance/effective-value resolution with unit tests.

### Checkpoint: Foundation

- [x] Focused Rust model tests pass.
- [x] Generated Rust and TypeScript bindings compile.

### Phase 2: Authoritative Planning and Writes

- [x] Task 3: Produce revision-bound previews with exact target snapshots, semantic changes, capability gaps, validation, and impact.
- [x] Task 4: Apply valid plans with one unknown-field-preserving atomic TOML write and report per-camera activation outcomes.
- [x] Task 5: Add bounded versioned template CRUD and import/export through the same revision and validation boundary.

### Checkpoint: Server

- [x] Focused conflict, validation, unknown-field, secret-reference, target-snapshot, and activation-failure tests pass.
- [x] Existing camera and runtime update tests remain green.

### Phase 3: Typed Fleet UI

- [x] Task 6: Add client types and conversions for snapshots, effective values, templates, plans, and apply results.
- [x] Task 7: Add capability-gated shared-default, template, and bulk-edit workflows to `/cameras`, including search and complete draft preservation.
- [x] Task 8: Add Playwright coverage for preview/apply, conflict recovery, capability loss, import/export, keyboard use, and 390 px layout.

### Checkpoint: Complete

- [x] Contract, Rust, browser-unit, and Playwright tests pass.
- [x] `./check.sh` passes from the repository root.
- [x] Issue acceptance criteria have reproducible evidence.

## Acceptance Criteria Verification

| Criterion                                                                                   | Testable outcome                                                                                                                             | Verification                                                                                                                                          | Observed result                                                                            |
| ------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Common supported configuration is available through typed UI                                | Cameras exposes searchable effective evidence, shared defaults, templates, and bulk editing; existing domain owners remain linked            | `ui/e2e/configuration.e2e.ts`, `ui/e2e/cameras.e2e.ts`, `ui/e2e/camera.e2e.ts`                                                                        | All affected workflows passed; canonical Playwright passed 189 tests with 2 expected skips |
| Inherited, template, override, effective, and applied values are distinct                   | Omitted camera fields follow future defaults; explicit clear restores inheritance; snapshots report source and runtime state                 | `camera_policy_defaults_flow_to_fields_without_overrides`, `stale_apply_preserves_current_state_and_fresh_clear_restores_inheritance`, snapshot tests | Passed in the 1,562-test canonical Rust run                                                |
| Template and bulk operations preview exact targets, changes, gaps, and impact               | Every mutation requires a revision-bound authoritative plan with exact target count, redacted semantic changes, issues, and reconnect policy | Configuration planning/target tests and selected/filter/group/all browser workflows                                                                   | Passed; maximum plan is 64 cameras and responses are encoded-size checked                  |
| Writes are validated, revision-aware, atomic, and failure-safe                              | Startup-equivalent validation precedes one atomic TOML write; stale update/remove/apply/import fail; activation reports staged recovery      | CAS, invalid-candidate, unknown-field, activation-failure, and browser conflict tests                                                                 | Passed; no blind overwrite path remains                                                    |
| Unknown fields and secret references survive edits and exchange                             | Unrelated TOML values remain; exports contain references only; unknown import fields fail without mutation                                   | Config round-trip, secret-diff, template import/export, and strict unknown-field tests                                                                | Passed; resolved credentials never enter snapshots, plans, exports, or logs                |
| Missing capabilities disable only unavailable commands                                      | Fleet remains visible; an open draft survives capability loss and becomes actionable after capability recovery                               | `preserves an open bulk draft through capability loss and recovery`                                                                                   | Passed in Chromium                                                                         |
| Desktop/mobile, accessibility, conflict, restart, versioning, and secret-safety checks pass | Dialog fits 390 px, supports keyboard tabs/Escape, protects unsaved navigation, and exposes field issues                                     | Svelte check, Playwright configuration suite, canonical `./check.sh`                                                                                  | 0 Svelte errors/warnings; canonical gate passed                                            |

## Performance Evidence

`cargo test --release configuration_planning_benchmark --lib -- --ignored --nocapture` on macOS
26.6.2 arm64, Apple M5 Max, Rust 1.97.1, 30 runs, and 64 cameras measured snapshot p50/p95
`0.238/0.445 ms` and five-field plan p50/p95 `0.595/1.167 ms`. Plan p95 delta was `0.721 ms`
against a `250 ms` budget. The final plan encoded to `23,348 bytes` against the `65,536-byte`
control-message budget.

## Canonical Evidence

Final `./check.sh` results are retained in ignored `target/check-issue136-final.log`:

- Rust: 1,562 passed, 19 skipped.
- Bun unit tests: 110 passed.
- Browser and server-compat unit tests: 28 passed.
- Playwright: 189 passed, 2 expected skips.
- Clippy, cargo-machete, Rust/Python/TOML/Prettier formatting, Svelte diagnostics, Paper, visual harness, demo checks, and production build passed.

## Risks and Mitigations

| Risk                                                     | Impact | Mitigation                                                                                                                   |
| -------------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------- |
| A stale or partial target set changes unintended cameras | High   | Bind every plan to an exact server-resolved target snapshot, revision, expiry, and digest.                                   |
| Secrets leak through effective-value evidence or export  | High   | Return configured booleans or unchanged references only and test serialized responses for resolved values.                   |
| Multi-camera runtime activation cannot be transactional  | High   | Commit configuration atomically, report each activation, and classify staged restart/reconnect recovery explicitly.          |
| Existing TOML fields disappear during typed writes       | High   | Mutate the parsed source table, validate the complete candidate with startup loading, and round-trip unknown-field fixtures. |
| The feature duplicates linked issue ownership            | Medium | Reuse and link existing storage, layout, and backup editors; do not add those fields to this contract.                       |
| Large fleet previews exhaust memory or UI space          | Medium | Bound templates, targets, fields, serialized bytes, retained plans, and response detail; paginate or summarize UI rows.      |

## Open Questions

- None. The user explicitly authorized additive changes to the otherwise protected protobuf contract for this issue.
