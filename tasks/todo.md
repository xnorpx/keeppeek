# Issue #128 Tasks

## Task 1: Define the HTTP ProtoJSON contract

**Acceptance criteria:**

- [x] Backup capabilities, records, manifests, create/inspect/plan/activate/rollback/delete requests,
      progress, warnings, mappings, and stable errors are versioned protobuf messages.
- [x] Rust and TypeScript use generated types; HTTP JSON follows canonical ProtoJSON.
- [x] Unknown fields, invalid enums, out-of-range values, and malformed values fail closed.

**Verification:**

- [x] Run focused Rust ProtoJSON round-trip tests.
- [x] Regenerate and type-check TypeScript bindings.

## Task 2: Harden bundle inspection

**Acceptance criteria:**

- [ ] Compressed input, members, manifest, sections, paths, and total expanded bytes are bounded.
- [ ] Every selected section is parsed and schema-validated before a plan can exist.
- [ ] Future formats, unsupported schemas, invalid dependencies, collisions, and malformed databases
      fail without extraction or live-state mutation.

**Verification:**

- [ ] Run focused adversarial backup inspector tests.

## Task 3: Create reference-only bundles

**Acceptance criteria:**

- [ ] Supported file-backed sections serialize deterministically with stable IDs and references.
- [ ] Inline secrets become explicit unresolved references; resolved values, sessions, cookies,
      tokens, and private keys never enter the default artifact.
- [ ] Derived caches and recording media are omitted and declared as omitted.

**Verification:**

- [ ] Run deterministic round-trip and artifact secret-scan tests.

## Task 4: Snapshot database-backed sections

**Acceptance criteria:**

- [ ] Recording/event and notification state use consistent database-native snapshots.
- [ ] Concurrent writes yield one declared revision or a bounded retry, never a torn copy.
- [ ] Routine metadata backup does not stop recording.

**Verification:**

- [ ] Run concurrent-write and live-snapshot integration tests.

## Task 5: Build complete dry-run plans

**Acceptance criteria:**

- [ ] Plans report source, sections, omitted secrets, capabilities, migrations, conflicts, IDs,
      paths, permissions, capacity, media effects, dependencies, and restart impact.
- [ ] Plans bind exact live revisions, bundle digest, mappings, selections, and expiry.
- [ ] Same-version and supported older schemas preserve compatible identities and references.

**Verification:**

- [ ] Run path, capacity, conflict, migration, selection, and stale-plan tests.

## Task 6: Stage, activate, and roll back

**Acceptance criteria:**

- [ ] Every target is validated in staging before mutation.
- [ ] Activation uses a persistent journal and exact before-images; any injected failure restores
      previous state and startup recovery is idempotent.
- [ ] A bounded rollback snapshot remains until health verification succeeds or its window expires.

**Verification:**

- [ ] Run failure injection at each journal transition and post-restore health check.

## Task 7: Expose the Administrator HTTP API

**Acceptance criteria:**

- [ ] All control/status endpoints consume and produce ProtoJSON and require Administrator role.
- [ ] ZIP upload/download streams over HTTP with content-length, timeout, concurrency, and disk bounds.
- [ ] Create, inspect, download, plan, activate, rollback, delete, rejection, and failure are audited
      without bundle contents or secrets.

**Verification:**

- [ ] Run HTTP route, auth, CORS, body-limit, cancellation, and audit tests.

## Task 8: Add non-interactive CLI automation

**Acceptance criteria:**

- [ ] CLI supports create, list, inspect, dry-run, restore, rollback, and delete.
- [ ] Machine-readable output is ProtoJSON and errors use stable exit semantics.
- [ ] No command accepts a secret or passphrase argument.

**Verification:**

- [ ] Run real-process CLI lifecycle and output conformance tests.

## Task 9: Build the Settings workflow

**Acceptance criteria:**

- [ ] Administrators can create, inspect, download, upload, dry-run, confirm, restore, roll back,
      and delete while seeing per-section progress and final health.
- [ ] Mapping drafts survive recoverable validation errors; bundle bytes remain memory-only.
- [ ] User role, capability loss, mobile layout, keyboard, focus, and screen-reader behavior are safe.

**Verification:**

- [ ] Run focused Svelte browser tests and Playwright workflow tests at desktop and 390 px.

## Task 10: Prove full round-trip recovery

**Acceptance criteria:**

- [ ] A clean fixture recovers cameras, policies, groups, layouts, catalog/events, access records,
      notifications, integrations, and selected durable state.
- [ ] macOS, Linux, and Windows path fixtures converge through explicit mappings.
- [ ] Artifact, logs, HTTP responses, browser storage, and diagnostics pass secret scans.

**Verification:**

- [ ] Run cross-platform integration, failure-injection, and end-to-end suites.

## Task 11: Document and qualify

**Acceptance criteria:**

- [ ] Operator docs specify format, limits, dependencies, migration matrix, secret requirements,
      recovery procedure, rollback window, restart effects, and media non-goals.
- [ ] Snapshot, planning, activation, memory, and artifact-size benchmarks have reproducible budgets.
- [ ] The PR contains final-commit evidence for every #128 criterion.

**Verification:**

- [ ] Run docs checks, benchmarks, `./check.sh`, and a fresh-context adversarial review.
