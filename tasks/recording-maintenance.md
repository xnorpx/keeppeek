# Recording Maintenance: Issue #133

## Scope and Status

Tasks are tracked in [issue #133](https://github.com/xnorpx/keeppeek/issues/133).
This record does not replace the existing plans for other features.

The backend now includes read-only catalog inspection and durable deletion-intent
preparation, confirmation, inspection, and cancellation, with a per-recording
work ledger and a read-only preflight report. Queued intent does not claim a file
or enable deletion. The protected `api/` contract still has no recording-maintenance
commands; it remains unchanged. No network command or UI action is enabled by this
work.

The [Recording maintenance book chapter](../book/src/recording-maintenance.md)
documents the available backend behavior and the unfinished user-facing workflow.

Backward compatibility is not a requirement for this pre-release feature, per
the owner's instruction. Do not add legacy wire adapters or migration layers for
unreleased maintenance formats. Existing recordings and unrelated application
state must still be preserved; compatibility scope does not waive data safety.

## Capability Map

| Module            | Responsibility                                                               | Depends on                                                   |
| ----------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------ |
| catalog-selection | Bounded, consistent inspection by stable source/stream/object or time range  | Existing recording catalog                                   |
| deletion-jobs     | Confirmed, durable deletion with confined filesystem operations and recovery | catalog-selection, filesystem identity and protection checks |
| reconciliation    | Bounded drift inspection and category-specific remedies                      | catalog-selection, deletion-jobs                             |
| maintenance-ui    | Administrator preview, confirmation, progress, and reports                   | Authorized protocol work, deletion-jobs, reconciliation      |

Build order: catalog-selection, deletion-jobs, reconciliation, maintenance-ui.

## Recorded Identity and File Pinning

Preflight now compares the catalog's recorded device/file identifier with metadata
from the opened archive file. The existing decimal `device:file` representation
is bounded to 41 bytes, with both components parsed as `u64`. SQL withholds
oversized values before they cross into Rust, and malformed identifiers fail
closed. A mismatch reports `IdentityChanged`; absent evidence reports
`IdentityUnavailable`, not `Present`. Both remain read-only observations.

Each `Observation` retains a non-writable file handle and its initial metadata
until dropped. A no-follow metadata precheck preserves existing rejection
semantics; the subsequent no-follow open independently validates the handle's
regular-file type and single-link count. Unix uses the existing `cap-fs-ext`
nonblocking option to avoid waiting on a FIFO substituted after the precheck.
No dependency is added. Windows requests only file attributes and allows ordinary
read/write/delete sharing; it is not a lock against concurrent changes.

Revalidation compares retained-handle and fresh-path metadata. Internal
`inspect_until` and `revalidate_until` helpers preserve a caller's earlier
deadline; the public standalone methods retain their own two-second call budget.
Preflight drops one observation before advancing to the next object. Unix now
requires read permission, but no media bytes are read. Public errors and reports
do not disclose paths or raw filesystem identifiers.

These identifiers can be reused before opening or after closing the handle, and
another writer can still change bytes in place. The incomplete 64-bit Windows
ReFS identity is not strengthened by holding the handle. This increment does not
establish immutable recording ownership or permit pathname-based deletion.

- [x] Ten new tests cover recorded identity, handle lifetime, post-check races, and deadlines.
- [x] Same-size replacement fails before the identity comparison and passes after it.
- [x] Existing archive rejection and drift tests pass without weakened assertions.
- [x] The new book chapter builds and is linked from the book navigation and evidence chapter.
- [x] Fresh-context review findings are reconciled, including shared-budget revalidation.
- [x] Final canonical validation passes with identity checks, pinning, and the book chapter.
- [ ] Windows runtime and reparse-point replacement qualification passes.

## Job Preflight

`recording_deletion_preflight` inspects one actor-owned queued job. It validates
the persisted job and ledger, reads the current catalog revision, and resolves
current recording metadata in one transaction on the search worker. It does not
wait for writer-queue capacity. The caller must authenticate and authorize the
Administrator and supply a trusted configured archive handle.

Filesystem checks start after the transaction closes. The report preserves
snapshot order and distinguishes present metadata, missing files, missing catalog
rows, active/protected/cleanup-pending recordings, changed scope or bounds,
size or identity mismatch, unavailable identity, rejected paths, and inspection failures. Missing catalog rows have
unknown file locations, whether or not the original bytes still exist. No unknown
file is enumerated, adopted, repaired, or removed.

Both planned and observed catalog revisions are returned. A changed revision
requires another preview. Even a matching revision and all-present report are
not a content check, file reservation, current authorization, or deletion proof.
Filesystem observations are not atomic with the catalog or one another.

The report contains at most 128 objects, with an indexed join bounded to 129 rows
including an overflow sentinel. SQL withholds paths over 4,096 bytes. Catalog and
filesystem work share the original two-second cooperative deadline; a per-file
budget cannot extend it. A timeout returns no partial report. Public reports and
errors exclude host paths. Neither job state nor recording state is changed.

- [x] Fifteen new tests pass on macOS, including four public integration tests.
- [x] Current blockers, missing catalog/file combinations, corruption, and rollback are tested.
- [x] Search-worker isolation, queue admission, snapshot consistency, and deadlines are tested.
- [x] Ancestor replacement and public error/path redaction are tested.
- [x] Strict Clippy, formatting, the function-size bound, and fresh-context review pass.
- [x] Canonical validation passes with the preflight increment.

## Per-Object Ledger

Confirmation inserts one queued work entry per snapshot recording and queues the
parent job in the same writer transaction. Entries retain the snapshot order and
recording IDs. `Job.objects` is empty before confirmation and for plans cancelled
before confirmation. A retry returns the stored ledger, including after restart
or a lost reply. It does not consult current recording rows to reconstruct work.

Reads reject missing, extra, reordered, substituted, or inconsistent entries.
Cancellation updates all queued entries and the job atomically. A new cancellation
cannot predate creation or confirmation; an already cancelled request remains
idempotent. Pruning removes child entries with their eligible expired parent,
never a queued job. No foreign-key cascade or legacy-format repair is assumed.

There are at most 128 entries per confirmed job and 32,768 entries across the
existing 256-job quota. Reads use the job/ordinal index and a 129-row bound.
Inserts reuse one prepared statement. Every operation retains the original
two-second request deadline, with checks between row operations. In-flight
database I/O remains cooperative, not preemptible. The ledger adds no media or
network I/O and does not alter recording rows, retention claims, or playback.

This is work admission only. It has no executing, deleted, or successful-removal
state and cannot authorize an unlink. Exact file ownership, evidence protection,
per-object execution results, and crash recovery remain executor requirements.

- [x] Atomic confirmation, cancellation, rollback, pruning, and restart are tested.
- [x] Concurrent confirmations and a dropped worker reply retain one ledger.
- [x] Thirteen new tests pass, including the 128-object limit and clock regression.
- [x] Strict Clippy, formatting, and the 70-line function bound pass.
- [x] Fresh-context review is reconciled against the implementation and tests.
- [x] Canonical validation passes with the ledger increment.

## Filesystem Inspection

The read-only `long_term::inspection::Archive` pins a configured directory handle
and inspects catalog-resolved paths one component at a time. The caller controls
the configured root and its ancestors when opening it. Descendant traversal uses
no-follow directory opens, and leaf inspection rejects symlinks, non-files, and
multiple hard links. Hidden names, alternate-stream syntax, traversal, paths over
4,096 bytes, and paths over sixteen components are rejected. Each inspection has
a cooperative two-second deadline; an operating-system call is not preemptible.

Observations compare file identifiers, size, permissions, modification time, and
creation time when available. They remain bound to one archive instance and keep
paths out of Debug output. No media bytes are read, and no files are modified.
Observations retain a non-writable file handle on all supported platforms. Successful revalidation is not a
content checksum, catalog-membership proof, reservation, or deletion capability.
The library's 64-bit Windows file index is not a full ReFS identity. The executor
must establish its own exact object ownership and race-safe removal sequence.

`cap-std` and `cap-fs-ext` are pinned to public crates.io version 4.0.3. These
libraries provide confined directory operations and cross-platform metadata
without introducing handwritten syscall wrappers. The locked additions were
reviewed, and `cargo audit --deny warnings` passes.

References:

- [Confined directory operations](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.Dir.html)
- [No-follow directory opening](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.DirExt.html#tymethod.open_dir_nofollow)
- [No-follow leaf opening](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.OpenOptionsFollowExt.html)
- [Nonblocking file options](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.OpenOptionsSyncExt.html#tymethod.nonblock)
- [Metadata identifier limitations](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.MetadataExt.html)

- [x] Nine integration tests pass on macOS, including same-size replacement,
      permission and size drift, missing files, path bounds, hidden/export paths,
      native path separators, hard links, instance isolation, redaction, and
      ancestor/leaf symlinks.
- [x] The permission-change regression failed before adding the comparison.
- [x] Strict Clippy and the locked dependency audit pass.
- [x] Canonical validation passes with the filesystem inspection increment.
- [ ] Windows runtime and reparse-point replacement tests pass.

The adversarial review's pathname-provenance, content-atomicity, and ReFS concerns
are limits of metadata inspection, not deletion guarantees. Source inspection
confirmed that Windows `stat_unchecked` constructs metadata from an opened file
and propagates query errors; no demonstrated panic path was found. Reinspection
reopens each ancestor with no-follow semantics, and the final deadline check is
after the leaf metadata checks. Both points were misread in the review. The
documented creation-time comparison is conditional on platform availability.
Cross-model review is skipped in this unattended run.

## Current Increment Contract

- Accept stable source and logical stream identifiers, plus one recording ID or
  a nonempty half-open time range. Never accept a filesystem path as a selector.
- Validate identifiers and time bounds before queuing work.
- Return a catalog revision and at most 128 recording metadata rows from one
  consistent read transaction. Reject a broader selection rather than silently
  returning an incomplete deletion scope. Limit time ranges to 31 days.
- Use the existing search worker, not the writer. Read at most 4,096 indexed
  source-history rows plus one overflow sentinel. Apply overlap filtering after
  this bounded scan. If older history cannot be ruled out, reject the request;
  even an apparently empty range cannot return a partial answer. An exact
  recording-ID lookup avoids the history scan.
- Keep identifiers at or below 512 UTF-8 bytes. Reject a full search queue
  immediately. Use the existing two-second catalog wait budget, carry its
  deadline through the queue, and check it before a transaction and between
  rows. This is cooperative cancellation: Turso 0.7.2 has no public interrupt
  method, so an in-flight row or operating-system I/O is not preempted.
- Include active, protected, and cleanup-pending objects instead of hiding them.
- Report full recording start/end bounds and catalog byte counts. These are
  metadata observations, not verified filesystem sizes or gap-free coverage.
- Preserve source/stream boundaries even for an exact recording ID.
- Do not return host paths, mutate catalog state, read media, or remove files.
- A catalog snapshot is not a deletion plan: authorization, file identity,
  confinement, bookmarks/exports/holds, nonce, expiry, and execution remain future
  requirements. A matching catalog revision cannot authorize deletion by itself.

## Verification

### Durable Intent Increment

Keep manual deletion intent separate from `cleanup_pending`, which is owned by
automatic retention. Store at most 128 prepared plans and 256 confirmed job
records (including retained cancellations) in the existing recording catalog.
Preparation accepts a scope, expected catalog revision, and operator/privacy
reason. It obtains an authoritative snapshot on the search worker, validates the
selection, and rechecks the revision inside the writer transaction. It does not
accept a caller-provided list of recordings. Reject active, protected,
already-pending, or unknown-end recordings before creating a plan.

Prepared plans expire after ten minutes on either UTC or the current catalog's
monotonic clock, whichever rejects first. Evaluate time after acquiring the write
transaction. Reject confirmation if UTC moves behind creation. Prepared plans
from an earlier catalog instance cannot be confirmed; callers must reprepare
after restart. Return `Expired` for those plans on inspection. Queued and
cancelled jobs remain durable across restart. Future preparation prunes old-epoch
or expired unconfirmed plans and cancellations older than 30 days, never queued
work. Snapshots are bounded to 512 KiB when persisted; nonces are stored as hashes.

Confirmation must match the plan owner, random nonce, and expected catalog
revision. The same confirmation returns the same durable queued job across
restart, even when unrelated catalog revisions subsequently change. Cancelling
queued intent does not change a recording or media. Reject forged/replayed
credentials and stale unconfirmed plans. Redact nonces from Debug output.

This increment records intent only. It neither enables automatic deletion nor
claims that catalog state alone proves filesystem confinement, evidence
relationships, or current authorization. The future executor must verify those
boundaries and claim objects atomically before removing media. No public wire
command or UI is added, and no protected API source is edited.

- [x] Prepare/confirm records persist in the existing catalog with bounded quotas.
- [x] Nonce, actor, revision, expiry, invalid-state, and retry cases are tested.
- [x] Restart preserves queued and cancelled state without cleanup or media changes.
- [x] Focused tests and canonical validation pass for the combined increment.

Use existing Rust catalog worker and Turso patterns. Keep public storage types
explicit and documented; use no new dependencies or diagnostic suppressions.

```sh
cargo test --locked -p keeppeek --lib storage::catalog::maintenance:: -- --test-threads=1
cargo clippy --locked -p keeppeek --lib --tests -- -D warnings
cargo fmt --all -- --check
./check.sh
```

All tests use generated synthetic recording files. Do not inspect, modify, or
delete private camera footage or application configuration for verification.

## Checklist

- [x] Read-only exact-object snapshot preserves catalog and media state.
- [x] Half-open range selection returns whole-recording bounds and blocked states.
- [x] Source/stream isolation and invalid/oversized scopes are tested.
- [x] Read consistency, overload, and error paths are verified.
- [x] Focused tests, Clippy, formatting, and canonical validation pass for the latest intent increment.
- [x] Fresh-context adversarial review is reconciled for the latest intent increment.
- [ ] Resolve the protected API contract before wiring the user-facing workflow.
- [ ] Implement confined, confirmed, durable deletion jobs and crash recovery.
- [ ] Implement reconciliation and Administrator UI with end-to-end tests.

Issue #133 remains open until its full acceptance criteria pass.

## Local Evidence

### Recorded Identity and File Pinning

- The final canonical `./check.sh` run passes with the combined uncommitted
  maintenance changes: 2,348 Rust tests (21 existing skips), 297 Bun tests,
  141 browser/visual tests, 57 compatibility tests, and 221 Playwright tests
  (two expected codec skips). The completion marker is
  `ISSUE133_IDENTITY_CHECK_EXIT=0`. Full output is retained locally in
  `target/issue133-identity-check.log`.
- The public same-size replacement regression first returned `Present`. It now
  returns `IdentityChanged` while preserving the original and replacement bytes.
  Three additional preflight tests cover missing identifiers, malformed/oversized
  values, and file/catalog rebinding after the consistent catalog read.
- Six focused file-inspection tests cover bounded/redacted parsing, retained
  read-only handle lifetime, shared-deadline revalidation, and FIFO/symlink/hard-link
  substitution after the metadata precheck. The FIFO test uses a bounded receiver
  and the production leaf opener, not a separate implementation.
- The first retained-handle implementation changed the leaf symlink error from
  `PermissionDenied` to an OS-specific loop error. The precheck now preserves the
  established classification, while independent post-open checks still reject
  races. All nine existing archive integration tests pass.
- Review identified the missing parent-deadline revalidation entry point. It is
  implemented and tested without changing the standalone public method. A second
  review asked for a deadline check after the fixed-size instance comparison;
  the existing pre-I/O and final checks meet the cooperative contract. No check
  can prevent scheduler delay between an instruction and a system call. Cross-model
  review was offered; the user was unavailable, so no external CLI ran.
- Thirty local 128-recording runs measured pinned preflight median/P95/maximum at
  5.490/5.766/22.270 ms. The preceding metadata-only preflight measured
  3.716/3.871/3.950 ms, an observed P95 increase of 1.895 ms. The paired ledger-only
  read in the new run measured 0.932/1.059/1.306 ms. These are added safety checks,
  not a speedup claim, and remain within the unchanged 2,000 ms budget.
- Environment matches the preflight baseline: Apple M5 Max, macOS 26.6.2,
  Rust 1.97.1, default test profile, locked Turso 0.7.2, in-memory catalog and
  local synthetic 64-byte files. Queue contention, durable database I/O,
  cold/network filesystems, and Windows remain outside this measurement.
- `mdbook build book --dest-dir "$PWD/target/issue133-book"` builds the new chapter.
  The existing installed Mermaid preprocessor reports its mdBook 0.5.0 build
  version against mdBook 0.5.4; rendering succeeds. Book Markdown formatting passes.

### Job Preflight

- The canonical `./check.sh` run passes on base commit
  `58edafeb1565a1460ba6c9598f0b6db3785dab3d` plus the combined uncommitted
  maintenance changes: 2,338 Rust tests (21 existing skips), 297 Bun tests,
  141 browser/visual tests, 57 compatibility tests, and 221 Playwright tests
  (two expected codec skips). The completion marker is
  `ISSUE133_PREFLIGHT_CHECK_EXIT=0`. Full output is retained locally in
  `target/issue133-preflight-check.log`. Final diff hygiene is clean.
- The first public regression failed because the preflight API did not exist.
  Eleven focused local tests and four public integration tests now pass, using
  synthetic files only. They include read-transaction consistency during a write,
  a symlink swap between catalog and filesystem phases, and absent catalog rows
  with either retained or missing media.
- A fresh-context reviewer found no actionable issue. Its ledger-substitution
  caveat is covered by persisted-ID validation and the corruption regression.
  Cross-model review was offered; the user was unavailable, so no external CLI ran.
- Thirty paired runs compared a read-only ledger load with the complete preflight
  for 128 synthetic 64-byte recordings: ledger median/P95/maximum were
  0.903/1.073/1.161 ms; preflight was 3.716/3.871/3.950 ms. The observed P95 cost
  of current-catalog and filesystem checks was +2.798 ms, within the unchanged
  2,000 ms request budget. This is added functionality, not a claimed speedup.
- Environment: Apple M5 Max, macOS 26.6.2, Rust 1.97.1, default test profile,
  locked Turso 0.7.2, in-memory database and local temporary files. The measurement
  excludes queue contention, durable database I/O, cold/network filesystems, and
  Windows qualification. No media bytes are read by preflight.
- The formatted preflight source SHA-256 is
  `36b9b9acb73527cd16bc4b32eeafb0691bf009bc674986d661a0857d8bfe0d17`.

```sh
cargo test --locked -p keeppeek --lib storage::catalog::maintenance::jobs::preflight::tests::maximum_preflight_is_complete_and_keeps_unknown_files_untouched -- --exact --test-threads=1 --nocapture
cargo test --locked -p keeppeek --test recording_deletion_ledger deletion_preflight -- --test-threads=1
```

### Per-Object Ledger

- The canonical `./check.sh` run passes with the combined uncommitted snapshot,
  intent, inspection, and ledger changes: 2,323 Rust tests (21 existing skips),
  297 Bun tests, 141 browser/visual tests, 57 compatibility tests, and 221
  Playwright tests (two expected codec skips). The completion marker is
  `ISSUE133_LEDGER_CHECK_EXIT=0`. Full output is retained locally in
  `target/issue133-ledger-check.log`. Final diff hygiene is clean.
- Eight new local ledger tests, one catalog-worker lost-reply test, and four
  public integration tests pass. The cancellation-clock test first failed by
  accepting a terminal time before confirmation; the check now precedes writes.
- The review's partial-commit scenario does not apply to the local transaction:
  ledger and job changes commit together, and queued retries bypass insertion.
  The locked Turso source shows that `is_autocommit` reads local handle state,
  not network or disk state; error context retains the original failure. Existing
  prepared-cancellation coverage is now supplemented by a public restart test.
  Cross-model review was offered; the user was unavailable, so no external CLI ran.
- The 30-run in-memory transaction measurement uses 128 recordings of 64 catalog
  bytes each, on Apple M5 Max, macOS 26.6.2, Rust 1.97.1, the default test profile,
  and locked Turso 0.7.2. It excludes media I/O, queue contention, and disk fsync.

| Implementation                    | Median (ms) | P95 (ms) | Maximum (ms) | Budget (ms) |
| --------------------------------- | ----------- | -------- | ------------ | ----------- |
| Prepare insert for each row       | 32.487      | 49.846   | 50.376       | 2,000       |
| Reuse one insert per confirmation | 28.282      | 42.938   | 43.358       | 2,000       |

Observed P95 changed by -6.908 ms (-13.9%). This is a local SQL measurement, not
production deletion throughput or cross-platform qualification. The baseline
ledger source SHA-256 was
`e65e57a2dc6c0b8c76de4e04fe9b49fb643759fff33bca1792d529046ae0f3b6`; the formatted
prepared-statement version is
`12cf8372eae3f406b5d9ccfc1f86bf064c617c4985b388c826ef8e1118d206c0`.
The [official Turso Rust reference](https://docs.turso.tech/sdk/rust/reference#prepared-statements)
and locked `Connection::execute` source document the statement API.

```sh
cargo test --locked -p keeppeek --lib storage::catalog::maintenance::jobs::tests::ledger_tests::maximum_scope_confirmation_preserves_every_object_within_the_wait_budget -- --exact --test-threads=1 --nocapture
cargo test --locked -p keeppeek --test recording_deletion_ledger -- --test-threads=1
```

### Filesystem Inspection

- The canonical `./check.sh` run passes with the combined uncommitted snapshot,
  intent, and filesystem inspection changes: 2,310 Rust tests (21 existing skips),
  297 Bun tests, 141 browser/visual tests, 57 compatibility tests, and 221
  Playwright tests (two expected codec skips). The completion marker is
  `ISSUE133_INSPECTION_CHECK_EXIT=0`. Full output is retained locally in
  `target/issue133-inspection-check.log`.
- All nine focused inspection tests pass on macOS. Strict Clippy, formatting,
  and `cargo audit --deny warnings` pass; the audit covers 640 locked dependencies.
- This evidence verifies read-only inspection, not race-safe removal. Windows
  runtime tests, durable execution, crash reconciliation, authorization, and the
  user-facing workflow remain pending. Issue #133 is not complete.

### Durable Intent

- Sixteen new job tests pass, including real catalog restart, actor/nonce checks,
  stale revisions, cancellation, quotas, hash-only nonce persistence, injected
  confirmation failure and rollback, both expiry clocks, and catalog-instance
  fencing. Media bytes and automatic-retention flags remain unchanged.
- The clock-rollback regression failed before the fix by accepting confirmation
  with a timestamp before creation. It passes with UTC/monotonic lease checks.
- An adversarial review identified time sampling before transaction acquisition
  and backwards-clock expiry risk. Time is now sampled inside the transaction;
  separate tests prove sampling order and monotonic expiry with apparently valid
  wall time. Overflow during expiry construction is checked and rejects the plan.
- Cross-model review was offered but the user was unavailable. It is skipped;
  no external CLI is invoked.
- The second review identified clock validation after pruning. Invalid clock
  values are now rejected before any intent query inside the transaction. All
  sixteen job tests pass after the repair and an added cancellation restart test.
  The neighboring catalog suite passed 56 tests before that additional case,
  with one pre-existing benchmark ignored; the canonical run includes it.
- The canonical `./check.sh` run passes on base commit
  `58edafeb1565a1460ba6c9598f0b6db3785dab3d` plus the combined uncommitted
  snapshot and intent changes: 2,301 Rust tests (21 existing skips), 297 Bun
  tests, 141 browser/visual tests, 57 compatibility tests, and 221 Playwright
  tests (two expected codec skips). Strict Clippy and formatting pass. Full
  output is retained locally in `target/issue133-intents-check-verified.log`.
- The cancelled-job restart test first exposed a redundant clone under the
  canonical Clippy rules, then a formatter difference after its removal. Both
  were repaired; no lint, test, timeout, or threshold was weakened.
- The earlier snapshot-only evidence below is retained separately and is not
  substituted for the combined increment's verification.

### Snapshot Baseline

- The first regression failed because `Scope` and the snapshot method did not
  exist. It passed after the implementation.
- Eleven focused tests pass. They cover unchanged media/catalog state, source and
  logical-stream isolation, half-open intervals, blocked-state visibility,
  selection and scan limits, queue admission, deadline expiry, corrupt metadata,
  transaction recovery, and snapshot consistency while another connection writes.
- Strict package Clippy passes for the library and tests.
- The neighboring catalog suite passes: 41 tests and one pre-existing ignored
  benchmark. No test or performance budget was skipped or reduced by this work.
- The canonical `./check.sh` run passes on base commit
  `58edafeb1565a1460ba6c9598f0b6db3785dab3d` plus this uncommitted increment:
  2,285 Rust tests (21 existing skips), 297 Bun tests, 141 browser/visual tests,
  57 compatibility tests, and 221 Playwright tests (two expected codec skips).
  Full output is retained locally in `target/issue133-check-final.log`.
- An earlier gate attempt failed on an `EILSEQ` write while importing an unchanged
  UI test on the mounted temporary volume. The isolated suite passed, and the
  full gate passed with a fresh temporary directory on the normal filesystem.
  No source change or diagnostic suppression was used to resolve that failure.
- The first adversarial review identified writer-thread blocking and the
  difference between a caller timeout and bounded query work. Both were
  addressed by the read-worker routing and indexed scan bound. An expired queued
  request does not start a transaction. A dropped reply is expected and does not
  require a log entry containing the selection.
- A second adversarial review identified loss of the primary error if the
  transaction-state check fails during recovery. The error now preserves both
  contexts; the catalog suite passes after the repair.
- Cross-model review is skipped in this unattended run. No external CLI is used.
- No physical-camera recordings, private settings, or protected API files were
  changed. No user-facing deletion or reconciliation operation is available yet.
