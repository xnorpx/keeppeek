# Recording Maintenance: Issue #133

## Scope and Status

Tasks are tracked in [issue #133](https://github.com/xnorpx/keeppeek/issues/133).
This record does not replace the existing plans for other features.

The continuation now implements catalog reservations, per-object execution and
recovery checkpoints, an approved WebRTC maintenance command, Administrator
deletion and reconciliation views, and explicit missing-row/ignore remedies.
Confirmation can remove selected media; the feature is still under qualification
and does not yet satisfy every #133 acceptance criterion. Earlier increment
descriptions and measurements below are historical evidence, not final readiness.

The [Recording maintenance book chapter](../book/src/recording-maintenance.md)
documents the implemented workflow and its remaining safety and qualification limits.

## Workflow Qualification Status

### PR 232 Follow-Up

The owner rebased the continuation onto `origin/main` at `4977bdd`. Draft
[PR #232](https://github.com/xnorpx/keeppeek/pull/232) now runs cross-platform
qualification and remains incomplete. `4b6a4c9` fixes both initial direct CI
failures: the missing newline in `.config/nextest.toml` and the generated WebRTC
binding's actual protoc-gen-es provenance. `bun x @bufbuild/protoc-gen-es@2.14.1`
resolves the real pinned executable even though the full local Bun install and
direct curl download failed. Explicit Buf plugin invocation with that executable
regenerated the binding; the code was identical apart from generator provenance.
No version check or manifest pin was weakened.

CI run `34267658608` passes Format, Ubuntu UI, Linux Rust tests, both native builds,
and four E2E shards. The unchanged macOS lease-pressure test failed once and passed
on rerun without changing its 1,600 ms budget. Native Windows testing then exposed
`ERROR_INVALID_PARAMETER` in staging. Commit `5683cd6` adds operation-level
diagnostics and a focused native primitive test. They localize the failure to the
relative `FileRenameInfo` call; NTFS qualification, ACL checks, private-directory
creation and directory flush already pass. `d8ba6b4` uses a handle-derived absolute
destination and null `RootDirectory`, with exclusive source ownership and
no-replacement semantics unchanged. Run `34275029245` passes all native Windows,
macOS and Linux tests and all other required CI checks on `d8ba6b4`. The follow-up
reindex commit `cec91de` is also fully green in the PR status checks. The original
reported failures are resolved without changing budgets or suppressing errors.

The next local reconciliation slice adds bounded container validation, distinct
corrupt/duplicate-identity/index-drift findings, and explicit catalog-owned reindex.
Reindex uses retained filesystem handles and checks identity before transaction
mutation and commit. Exact fingerprints compare fragment/keyframe offsets and
timing, coverage ranges, and coverage totals. Tests cover equal-count corruption,
missing coverage, replaced files before commit, overflow decode times, sample
offsets into headers, and duplicate track runs before shared parsing. Unknown files
are never admitted by this remedy. Mutating remedies share the restore/configuration
coordination lock. Focused results: 23 public catalog tests, four parser regressions,
queued-reindex regression, eight server tests and two Chromium remedy tests pass.
The canonical `KEEPPEEK_RUN_SLOW_TESTS=1 ./check.sh` now passes on macOS: 2,402 Rust
tests (21 existing skips, 101.249 seconds), 361 Bun tests, 193 browser/visual tests,
57 compatibility tests and 242 Playwright tests (two existing capability skips,
58.2 seconds). Evidence: `target/issue133-reindex-check.log`, marker
`ISSUE133_REINDEX_CHECK_EXIT=0`. This includes all parser, index, and coordination
repairs. The later Windows-only rename change is not a macOS runtime difference.

The independent Home Assistant container job reported `unknown_command` from its
visual editor on `5683cd6`. All five exact pinned-image container tests pass locally
in 34.6 seconds with their unchanged zero-console-error assertion; evidence is
`target/issue133-ha-reproduction.log`, marker `ISSUE133_HA_REPRO_EXIT=0`. No error
exception or skip was added. Final exact-SHA native CI still gates this draft.

### Continuation Safety Checks

Startup now invokes non-destructive deletion reconciliation before recording
pipelines launch. It marks abandoned pending objects failed and retains their
claims; a recorded, matching staging directory with no selected source or staged
bytes permits atomic catalog completion only. Present bytes are never moved or
deleted by recovery. Transactions preserve original checkpoints after injected
catalog failure, respect the original deadline and remaining database wait budget,
and skip same-epoch live executors including siblings. Four focused startup tests
and the complete 17-test execution slice pass. Three bounded review cycles are
reconciled, with no further material findings in the final review.

The canonical startup gate passes: 2,406 Rust tests (21 existing skips,
101.886 seconds), 361 Bun tests, 193 browser/visual tests, 57 compatibility tests,
and 242 Playwright tests (two existing capability skips, 57.0 seconds). Evidence:
`target/issue133-startup-check.log`, marker `ISSUE133_STARTUP_CHECK_EXIT=0`.
The subsequent extraction of the unchanged startup hook into a small helper passes
all four startup regressions and strict Clippy; it does not change execution order.

The legacy suffix-based `cleanup_stale_active_files` pass is removed. A regression
first reproduced deletion of unowned bytes, then verified preservation in both media
roots with and without a catalog. Interrupted recording bytes and catalog rows now
survive startup, as do files behind directory symlinks. All 20 storage-engine tests
pass; normal finalized-media retention remains unchanged. Previewed remediation of
proven abandoned temporary data still needs its explicit workflow.

The latest continuation adds immutable 32-byte staging-directory identity evidence
in place of the unqualified `was_staged` boolean. The field is bounded on load and
cannot be rebound. Recovery rejects replaced directories and unexpected contents,
including a recording renamed inside staging. Legitimate unlink/catalog-failure
recovery tolerates removal of an empty original source parent and synchronizes its
last surviving ancestor. Cancelled uncheckpointed objects retain reservations when
the original selected file cannot be identified or staging contents conflict.

The focused catalog integration suite now has 19 passing tests; execution has
13 passing tests. Atomic no-replace rename and post-unlink retained-inode checks
have separate macOS regressions. Every new failure regression was observed red
before its corresponding fix. Strict Clippy passes on the current macOS slice.

The server maintenance slice has eight passing tests. Export retry now shares
the configuration-update lock with maintenance admission and rejects maintenance
before mutating export history. Restore receives the same lock through `app.rs`
and `BackupManager::open_with_config_update`. Tests cover busy coordination and
revoked authorization before filesystem work. Audit status never labels unresolved
zero-failure work as success; structured tracing tests verify requester, exact
bounds, reason, revision, counts, hold-override false, and private-data redaction.

Restarted execution progress now projects previous-epoch working/staged objects
as failed without changing the durable checkpoint. A public restart test verifies
the file survives and explicit retry completes. Ten Chromium page tests cover
cancelled-failure retry, blocked-preview controls, stale-preview regeneration,
fresh typed confirmation after deferred refresh, role/capability loss, batched
capability loss/restoration, and stale asynchronous results. The existing
`CapabilityState` loss latch is reused. Three client tests verify nonce redaction,
scope bounds, and one-attempt confirmation consumption. Explicit dismissal also
invalidates a pending preview so its late response cannot reopen the dialog.
Three bounded UI reviews were reconciled with these regression tests; no external
model CLI or fourth review cycle was run while the user was unavailable.

Current verification evidence:

- `target/issue133-final-continuation-check.log`: the final source tree repeats
  the canonical build, all 2,388 Rust tests (21 existing skips, 89.168 seconds),
  strict Clippy, dependency and formatting checks successfully. UI quality again
  stops at protoc-gen-es 2.14.0 versus required 2.14.1, marker
  `ISSUE133_FINAL_CONTINUATION_CHECK_EXIT=1`. The subsequent task-record update is
  prose only. This is a failed canonical gate, not a completion result.
- `target/issue133-continuation-check.log`: canonical build, 2,388 Rust tests
  (21 existing skips), strict Clippy, dependency checks and formatting passed.
  The test phase took 100.692 seconds. UI quality then failed on actual generator
  2.14.0 versus required 2.14.1, marker `ISSUE133_CONTINUATION_CHECK_EXIT=1`.
  These Rust results are from the final backend tree; the last dialog-only fix
  follows that run, so this is not a full final-tree canonical pass.
- `target/issue133-continuation-ui.log`: final UI has 300 Bun tests, 151
  browser/visual tests and 57 compatibility tests passing. The isolated real-server
  desktop/mobile maintenance E2E passes in 9.5 seconds, with a 2.6-second test body.
  Marker: `ISSUE133_CONTINUATION_UI_EXIT=0`. The rebuilt release backend is the
  same backend source used by `target/issue133-continuation-e2e.log`.
- Final desktop 1440x900 and mobile 390x844 confirmation screenshots were inspected;
  text and controls fit, typed confirmation is required, and document overflow
  remains guarded by E2E. Fixtures contain generated media only.
- Oxlint, Svelte diagnostics and E2E typechecks pass with installed tooling.
  Book builds and tracked diff whitespace checks pass. The backup protobuf
  binding is unchanged; generator provenance has not been edited by hand.

Three fresh-context recovery reviews produced actionable findings. The tested
fixes address directory replacement, unexpected contents, missing source parents,
premature cancellation, and destination overwrite. The third review also exposes
an unresolved Unix same-account namespace race: no portable conditional-inode
unlink exists in this implementation. A process with the service account's access
can substitute private staging names just before unlink. Retained-inode checks
prevent some false success reports but cannot prevent deletion of a replacement,
and subsequent retries after such manipulation are not qualified. Acceptance of
the service-account/root trust boundary, or approval of stronger process isolation,
is outstanding. The unavailable user was not treated as approval. No fourth review
cycle or external model CLI was run.

The public npm install retry still fails with `ConnectionClosed` before generation.
Manifest pins, registry configuration, and the generator-version check are unchanged.
Historical full-suite counts below do not certify these newest changes.

### Earlier Workflow Evidence

- [x] Synthetic public integration tests cover reservations, deletion, terminal path reuse,
      owner-bound reconciliation, and rejection of reappeared files (16 tests).
- [x] Execution tests cover interrupted catalog completion, cancelled started work, live-worker
      exclusion, missing staging directories, evidence drift, claimed media readers, and
      two-object cancellation (nine tests).
- [x] Server tests cover Administrator rejection, typed confirmation, durable progress, and
      actor-owned reconciliation remedies (three tests).
- [x] The isolated real-server Playwright workflow passes desktop and mobile preview,
      cancellation, deletion, nonce-free report download, and unknown-file preservation.
      Local evidence is in `target/issue133-e2e.log` and Playwright screenshot artifacts.
- [x] Svelte typecheck, Oxlint, and strict macOS Rust Clippy passed before the latest
      Windows-only safety repairs; ACE/SID bounds have a platform-independent passing test.
- [x] The full Rust suite passes with `KEEPPEEK_RUN_SLOW_TESTS=1` and the canonical
      macOS crypto feature: 2,374 passed, 21 existing skips, 89.922 seconds.
      Evidence: `target/issue133-workflow-rust.log`, marker `ISSUE133_FULL_RUST_EXIT=0`.
- [x] UI unit suites pass with the installed tooling: 299 Bun tests, 141 browser/visual
      tests, and 57 compatibility tests. Evidence: `target/issue133-workflow-ui-tests.log`,
      marker `ISSUE133_UI_UNIT_EXIT=0`. These results do not replace the generator-version gate.
- [x] Updated book chapters build, all new handwritten functions fit within 70 source lines,
      and generated-file whitespace is normalized without changing generator provenance.
- [ ] Regenerate WebRTC TypeScript with the required actual protoc-gen-es 2.14.1 package.
      Installed 2.14.0 generated the current binding. Public npm requests fail with
      `ConnectionClosed` / TLS socket errors; no mirror, pin change, or header spoofing is allowed.
- [ ] Full canonical gate passes after resolving that generator mismatch. The failed run is
      `target/issue133-workflow-check.log`, marker `ISSUE133_WORKFLOW_CHECK_EXIT=1`.
- [ ] Native Windows NTFS compilation, ACL/reparse/race tests, and durability/recovery tests pass.
      ReFS is deliberately rejected; Unix tests do not qualify the Windows implementation.
- [ ] Complete ownership and crash-boundary review, including stale staging and in-place changes.
- [ ] Resolve the Unix service-account namespace trust boundary; the current post-unlink
      detection does not protect against a malicious process with the same OS identity.
- [ ] Complete reconciliation corruption/content-duplicate/interrupted-work classification,
      validated re-indexing, quarantine, and owned-temporary cleanup remedies.
- [ ] Complete hold/share/investigation and move/restore protection coordination and audit fields.
- [ ] Verify browser stale-preview recovery, partial failure/retry, revocation, timeline result,
      and accessibility beyond the existing happy-path test.
- [ ] Update final measurements and open the completion PR only when these requirements pass.

The current Windows helper uses native NTFS ACL and handle APIs. Review identified
and repaired raw disposition flag construction, premature ACE/SID pointer use, and
unvalidated nested directories. Runtime qualification is still required. The
installed custom Rust toolchain has no `rustup` Windows target management; the
repository's Windows CI remains necessary. No private footage or configuration
was used for verification.

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

## Continuation After PR #229

Continue on `feat/recording-maintenance-workflow`, branched from foundation commit
`9cbed56001b680b295da315b77f0f30a80435bfd` in
[PR #229](https://github.com/xnorpx/keeppeek/pull/229). After #229 merged, the
continuation was rebased onto `main` at
`4a56c8d1000a6f5f7a9598e9eb056c0e9d83aa45`. The rebase preserved the continuation
patch unchanged and retained the merged Windows test-startup fix. The original
foundation branch remains unchanged.
Issue #133 remains the completion tracker; its acceptance criteria are unchanged.

The owner requested a completion PR after the remaining work is finished. Keep
the book chapter synchronized with each implemented behavior and verify its final
instructions before opening that PR. The completion PR must include every #133
acceptance criterion, final-commit validation and performance evidence, and any
remaining qualification limits. Do not claim completion or close #133 while any
required outcome remains unimplemented or unverified.

The existing capability map still governs the remaining implementation order:

1. `deletion-jobs`: establish exact object ownership and protection coordination,
   then implement confined removal, durable per-object outcomes, cancellation,
   and recovery. Verify replacement races and every file/catalog presence
   combination with synthetic fixtures before enabling execution.
2. `reconciliation`: implement bounded drift inspection and explicit remedies.
   Verify missing, orphaned, duplicate, corrupted, escaping, stale-temporary,
   and interrupted-job fixtures; unknown bytes must never be deleted or adopted
   implicitly.
3. `maintenance-ui`: integrate the authorized protocol, Administrator preview,
   confirmation, progress, retry, audit, and truthful timeline results. Verify
   role rejection and desktop/mobile workflows with real-browser tests.
4. Complete cross-platform qualification, before/after performance evidence,
   the book chapter, and the full canonical validation for the final tree.

The owner explicitly approved changes to `api/webrtc.proto` for #133 on
2026-09-08. This removes the API-editing approval blocker for that file; the
maintenance contract and user-facing workflow still need implementation. Define
and validate deletion and reconciliation commands before implementing their
server handlers and frontend client. Follow the approval rules in `AGENTS.md`
and ask before expanding the approved API scope. Do not introduce an undocumented
HTTP endpoint or tunnel maintenance operations through an unrelated command.
Backend-only work does not complete #133.

## Prepared Identity Binding

The continuation preserves the catalog file identity in each preparation snapshot
as an opaque `FileIdentity`. It hashes the two parsed `u64` identifiers in big-endian
order with SHA-256 and serializes exactly 32 bytes. The digest is metadata evidence,
not a media checksum, secret, keyed authentication value, or immutable ownership
proof. Diagnostics redact it, and public snapshots do not expose the raw numbers.

Preflight requires the preparation-time fingerprint, current catalog identity,
and opened file to agree before reporting `Present`. A later catalog refresh
cannot rebind a confirmed job to a replacement, including after restart. Missing
preparation evidence stays unavailable; the loader does not reconstruct it from
the current catalog. Malformed snapshot identities fail with `Failure::Invalid`
without echoing rejected input through public intent reads or confirmation retries.

The same source/stream, row, serialized-snapshot, and cooperative deadline bounds
remain in force. No new filesystem operation, dependency, deletion state, network
command, or UI control is added. Exact object ownership and safe removal remain
executor requirements, not properties of this fingerprint.

- [x] The same-size replacement plus catalog-refresh regression fails before the change.
- [x] The replacement remains rejected after reopening the catalog; both files are preserved.
- [x] Missing planned/persisted evidence and catalog-only changes cannot become `Present`.
- [x] Fingerprint encoding, canonicalization, redaction, bounds, and corrupt-row recovery pass.
- [x] The public malformed-snapshot error regression fails before loader redaction and passes after it.
- [x] Fresh-context review is reconciled and the follow-up review finds no further issues.
- [x] Strict Clippy, formatting, the function-size bound, and focused tests pass before rebasing.
- [x] Thirty-run measurements retain the existing 2,000 ms budget.
- [x] Canonical validation passes on the rebased continuation.

Before rebasing, 55 maintenance unit tests and 20 public integration tests pass.
The first review found that Serde errors could disclose malformed persisted
identity values through public intent reads and confirmation retries. The public
regression confirmed that leak, and the shared loader now maps decoding failures
to `Failure::Invalid`. The follow-up review found no further issue in this slice.
Cross-model review was skipped because the user was unavailable; no external CLI
ran. The book chapter builds and its Markdown formatting passes.

Thirty 128-recording runs measured ledger-read median/P95/maximum at
2.087/2.774/2.848 ms and complete preflight at 9.179/11.696/28.757 ms. Confirmation
measured 32.700/51.866/56.991 ms. The #229 foundation run at
`9cbed56001b680b295da315b77f0f30a80435bfd` measured preflight P95 at 6.933 ms,
so the observed P95 increase is 4.763 ms. Both use Apple M5 Max, macOS 26.6.2,
Rust 1.97.1, the default test profile, locked Turso 0.7.2, an in-memory catalog,
and local synthetic 64-byte files. These separate-run observations do not isolate
host contention or prove production throughput, cold-filesystem, or Windows
performance. The existing 128-object harnesses below reproduce the measurements.

### Rebased Validation

The first full rebased gate timed out in file inspection during the 30-run
maximum-preflight measurement. The failing log is retained locally in
`target/issue133-rebased-check.log`. The same workload passed alone under Nextest
with a 22.014 ms maximum, and all 55 maintenance tests also passed with the
canonical macOS crypto feature and ordinary parallel scheduling.

The latency workload now requests `num-test-threads` for its exact test name in
`.config/nextest.toml`, following the
[Nextest per-test scheduling contract](https://nexte.st/docs/configuration/threads-required/).
This prevents unrelated tests in the same invocation from overlapping the
measurement. It does not isolate other processes on the host. All 30 iterations,
128 objects, assertions, and the 2,000 ms request budget remain unchanged; no test
is skipped or retried. Ordinary maintenance correctness and deadline tests retain
their parallel scheduling. Failure diagnostics now include the iteration and
catalog/total elapsed times.

Nextest 0.9.143 accepts the exact-test override. The maintenance group completed
in 3.312 seconds with the override versus 2.726 seconds in the preceding parallel
run, an observed 0.586-second scheduling cost; these separate runs do not isolate
host noise. The fresh-context scheduling review found no issue in the override,
tracking exception, diagnostics, or unchanged test requirements.

The final `KEEPPEEK_RUN_SLOW_TESTS=1 ./check.sh` run passes on the rebased
implementation and scheduling repair: 2,355 Rust tests (21 existing skips),
297 Bun tests, 141 browser/visual tests, 57 compatibility tests, and 221 Playwright
tests (two expected codec skips). The Rust suite completed in 103.887 seconds.
The completion marker is `ISSUE133_REBASED_VERIFIED_CHECK_EXIT=0`; full local
output is retained in `target/issue133-rebased-check-verified.log`.
The validated tree is `ead13f016ad937b7cca24796722e3077ce813e7d`, based on
`2e0c6c67a0c833e2f65321a6c127e653cc915c24`. Only this task record changes after
that gate; its formatting is checked separately. The updated book chapter builds.

## Recorded Identity and File Pinning

Preflight compares the prepared binding and the current catalog's recorded
device/file identifier with metadata from the opened archive file. The existing
decimal `device:file` representation is bounded to 41 bytes, with both components
parsed as `u64`. SQL withholds
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
- [x] Obtain explicit approval to change `api/webrtc.proto` for #133.
- [ ] Define and validate the maintenance API contract before wiring the user-facing workflow.
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
