# Issue #136 Tasks

## Task 1: Add the configuration v1 contract

**Acceptance criteria:**

- [x] Snapshot, template CRUD, plan, apply, import, and export actions are typed and versioned.
- [x] Plans carry a revision, exact targets, expiry, impact, validation, and semantic changes.
- [x] Existing protocol fields and behavior remain compatible.

**Verification:**

- [x] Regenerate protobuf bindings.
- [x] Run focused Rust and UI type checks.

## Task 2: Resolve defaults, overrides, and templates

**Acceptance criteria:**

- [x] Shared camera defaults remain inherited rather than copied.
- [x] Effective values identify their source and applied state.
- [x] Template documents and fields are bounded and never contain resolved secrets.

**Verification:**

- [x] Run focused config resolution and template-store unit tests.

## Task 3: Preview exact bulk and template changes

**Acceptance criteria:**

- [x] Explicit IDs, filtered snapshots, and groups resolve to an authoritative server count.
- [x] Preview reports old/effective/new values, skipped targets, validation, capabilities, and impact.
- [x] A partial visible page cannot become an implicit fleet target.

**Verification:**

- [x] Run focused server planning tests.

## Task 4: Apply plans safely

**Acceptance criteria:**

- [x] Stale, expired, changed, or capability-incompatible plans fail without writes.
- [x] Valid plans preserve unknown values and secret references in one atomic configuration write.
- [x] Camera activation outcomes and restart/reconnect recovery are explicit.

**Verification:**

- [x] Run focused server apply, conflict, rollback, and round-trip tests.

## Task 5: Manage and exchange templates

**Acceptance criteria:**

- [x] Create, duplicate, edit, apply, delete, import, and export are bounded and revision-aware.
- [x] Applying creates documented explicit overrides; edits and deletion never mutate cameras silently.
- [x] Imports are fully validated and previewed before mutation.

**Verification:**

- [x] Run focused template lifecycle and import/export tests.

## Task 6: Implement the typed client

**Acceptance criteria:**

- [x] Client types and protobuf conversions preserve optional set/clear semantics.
- [x] Structured conflict and validation evidence reaches field-addressable UI state.
- [x] Capability loss leaves readable values available.

**Verification:**

- [x] Run focused control-client tests.

## Task 7: Build the fleet configuration UI

**Acceptance criteria:**

- [x] Users can inspect defaults, overrides, effective sources, templates, and applied state.
- [x] Template and bulk workflows require preview and preserve drafts on every failure.
- [x] Search covers setting, camera, capability, and source; controls fit desktop and 390 px mobile.

**Verification:**

- [x] Run focused Svelte browser tests and accessibility checks.

## Task 8: Verify end to end

**Acceptance criteria:**

- [x] Playwright covers CRUD, preview/apply, conflict, capability loss, import/export, keyboard, and mobile.
- [x] Existing configuration workflows do not regress.
- [x] The canonical repository gate passes.

**Verification:**

- [x] Run focused Playwright tests.
- [x] Run `./check.sh` from the repository root.
