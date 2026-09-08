# Recording maintenance

Recording maintenance provides Administrator previews, confirmed deletion jobs,
and bounded catalog reconciliation over the WebRTC control connection. It is
under qualification in [issue #133](https://github.com/xnorpx/keeppeek/issues/133),
not yet a completed or production-qualified feature. The macOS synthetic-media
workflow has passed end-to-end testing; native Windows validation and other
acceptance criteria remain outstanding.

Automatic storage retention remains separate from manual deletion. Manual claims
exclude their recordings from retention and new playback/export resolution until
the claim reaches a terminal outcome. Never delete recording files directly as a
substitute for maintenance: that can leave stale catalog entries and unavailable
evidence links.

## Review and delete recordings

1. Sign in as an Administrator and open **Recording integrity**, then **Maintenance**.
   **Review deletion** in Keep's export-range view carries its camera, stream, and
   interval into maintenance. Closing Keep also releases its playback cursor.
2. Select the camera, stream, UTC start and end, and operator or privacy reason.
   Choose **Preview deletion**. A day or hour is an ordinary UTC time range.
3. Review every whole-recording boundary, byte count, gap, bookmark, related
   export, and stated consequence. An active, protected, retention-pending, or
   identity-unavailable selection cannot be confirmed.
4. Type the exact confirmation text, such as `DELETE 1`, and choose
   **Delete permanently**. **Keep recordings** closes the confirmation without
   starting deletion; **Cancel job** cancels the prepared intent.
5. Follow the per-object results. Use **Refresh job** after a connection failure
   and **Download report** to retain the result. Reports contain no confirmation
   nonce or host filesystem path.

One deletion worker runs at a time. Close stored playback and wait for active
exports before confirming. The server rechecks authorization during execution;
revoking the session can leave a recoverable staged object rather than granting
an old request unlimited permission to continue. A failed confirmation consumes
its private nonce. Choose **Refresh preview**, review the new result, and type
the confirmation again. The textbox is disabled while the refresh is pending.
Losing the Administrator role or maintenance capability discards the review,
including late preview responses. Already-removed bytes cannot be restored by
cancellation.

## Select a complete scope

Maintenance uses stable source, stream, and recording identifiers, never a
display name or a caller-supplied filesystem path. Catalog inspection accepts
one recording or a nonempty half-open time range on one source and logical stream.
The start is included and the end is excluded.

A range selects whole recording objects that overlap it. It does not trim media
at the requested timestamps. The snapshot retains each object's full start and
end bounds so an eventual destructive preview can show any expansion explicitly.
The confirmation uses those whole-object bounds; it does not rewrite partial
boundary fragments. Inspect the listed boundaries even when the requested
interval is much shorter than the selected recording.

Selections contain at most 128 recording objects. Time ranges span at most
31 days, and indexed history inspection is bounded to 4,096 rows. If a complete
selection cannot be established within those limits, the request fails instead
of returning a partial result that could be mistaken for the full scope.

Counts and bytes in a catalog snapshot are recorded metadata. They do not prove
that a file is intact, that its media is playable, or that the selected interval
has no gaps. Active, protected, and cleanup-pending objects remain visible rather
than disappearing from the selection.

## Understand durable intent

Preparation reads an authoritative snapshot and checks its catalog revision. It
rejects empty selections, active or unfinalized recordings, protected recordings,
pending automatic cleanup, and recordings without a known end time. The stored
intent records the requester, scope, operator or privacy reason, and timestamps.

The snapshot also retains an opaque fingerprint of each recording's catalog
device/file identifier. Confirmation and restart preserve this preparation-time
binding; a later catalog refresh cannot substitute another file into the plan.
Missing preparation evidence stays unavailable even if the current catalog later
gains an identifier. The fingerprint does not verify media contents or prevent
filesystem identifier reuse.

Confirmation requires the same requester, a server-generated nonce, and the
unchanged preview revision. The catalog stores a hash of the nonce rather than
the nonce itself. Prepared plans expire after ten minutes using both wall-clock
and monotonic time; restarting the catalog requires a new preparation.

| State     | Meaning                                                                                          |
| --------- | ------------------------------------------------------------------------------------------------ |
| Prepared  | A short-lived intent is awaiting confirmation; no recording is reserved.                         |
| Expired   | Preparation must be repeated before confirmation.                                                |
| Queued    | Confirmation and a per-recording ledger are durable. Execution may reserve the selection.        |
| Working   | A live execution attempt owns the object.                                                        |
| Staged    | The selected file has moved into private maintenance storage, with a durable staging checkpoint. |
| Deleted   | File removal and catalog completion have been recorded.                                          |
| Failed    | Removal did not finish; retained evidence and a retry or inspection are required.                |
| Cancelled | Admission of unstarted objects has stopped. Objects already deleted stay deleted.                |

Confirmation creates exactly one ledger entry per selected recording in snapshot
order, in the same transaction as the queued state. Retrying the same confirmation
after a lost reply or restart returns that job rather than duplicating work.
Queued and cancelled intents survive restart. Cancellation is idempotent and
cannot assign a time before creation or confirmation.

The catalog admits at most 128 prepared plans and 256 confirmed or retained
cancelled jobs. Expired preparation and terminal history older than 30 days
can be pruned; unresolved execution is not evicted to make room. Cancelling before
confirmation does not create work entries.

Reservations and per-object checkpoints survive restart. An authenticated retry
can resume an abandoned attempt; it cannot run concurrently with a still-live
worker. A file that disappeared before any staging evidence is not reported as
deleted. The staging checkpoint binds recovery to the original token directory's
filesystem identity. A missing or replacement directory, renamed staged recording,
or unexpected entry requires inspection instead of a successful result. Once
unlink has completed, recovery can still finish if the empty original source
directory has been removed.

Cancelling interrupted work does not release a claim merely because staging is
absent. The original selected file must be positively identified; otherwise the
object remains failed and reserved for inspection or recovery. **Retry failed
objects** is available for those failures even when the job is cancelled. After
restart, a previous worker's pending objects report failed instead of indefinitely
working; the checkpoint and reservations remain intact for authenticated retry.
Automatic startup reconciliation of every interrupted filesystem/catalog
combination remains a qualification requirement.

## Audit and coordination

Maintenance admission shares the configuration-update lock used by restore and
access configuration. An export retry cannot remove its existing job while
maintenance is admitted. The worker retains storage coordination during execution
and rechecks the current session before filesystem mutations.

The access audit links each execution outcome to its durable job ID. Structured
`recording_maintenance` log events include requester, source and stream, requested
and expanded interval bounds, operator or privacy reason, preview revision,
object and byte counts, relationship counts, and the observed result. Hold
override is always false: this workflow cannot release a hold. Results distinguish
successful deletion, partial failure, incomplete work, completed cancellation,
and cancellation with pending work. Paths, confirmation nonces, and raw failure
details are excluded.

## Reconcile the catalog

Open the **Reconciliation** view and choose **Inspect catalog**. The dry run reads
at most 4,096 catalog rows and 4,096 archive entries and returns at most 128
findings. Scan limits are explicit: an incomplete report cannot authorize a
remedy. Reports expire after ten minutes and belong to the requesting
Administrator. At most sixteen reports are retained in server memory.

The current inventory reports missing and unknown files, duplicate paths, size
or identity mismatch, rejected paths, temporary files, and interrupted work.
It is a metadata scan, not full container, checksum, or media-decoding validation.
Unknown files are not automatically adopted, moved, or deleted.

- **Acknowledge** records an explicit ignore decision in the retained report and
  leaves the file and catalog untouched.
- **Retain tombstone** is available only for a missing, unprotected cataloged
  file. It rechecks absence and catalog revision, then removes the stale row and
  playback indexes while retaining existing coverage-derived deletion evidence.
  No filesystem bytes are deleted. A reappeared file or changed catalog rejects
  the operation; inspect again before making another change.

Category filters and **Download reconciliation report** preserve inspection
results. A new scan is required after a catalog-changing remedy. Quarantine,
validated re-indexing, full corruption detection, and previewed deletion of
owned temporary files are not implemented yet.

## Read a preflight report

Preflight accepts an actor-owned queued job. Its caller must authenticate and
authorize the Administrator and supply the configured archive. These internal
storage methods are not themselves a network authorization boundary.

The job, ledger, current recording rows, and observed catalog revision share one
read transaction on the catalog search worker. Filesystem checks run only after
that transaction closes. Reports preserve snapshot order and do not expose host
paths or confirmation secrets.

| Status                | Meaning                                                                                                              |
| --------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `Present`             | Planned and current device/file fingerprints match the observed file, and its size matches. Content is not verified. |
| `IdentityChanged`     | The preparation-time fingerprint, current catalog identifier, and observed file do not all agree.                    |
| `IdentityUnavailable` | The preparation or current catalog lacks identity evidence; path and size cannot establish a match.                  |
| `MissingFile`         | The catalog-resolved file could not be found during inspection.                                                      |
| `MissingCatalog`      | The recording row is absent. Its file location and whether bytes remain are unknown.                                 |
| `CatalogChanged`      | Source, logical stream, recording bounds, or recorded size no longer match the plan.                                 |
| `ActiveRecording`     | The recording is no longer finalized.                                                                                |
| `ProtectedRecording`  | Current catalog protection blocks maintenance.                                                                       |
| `CleanupPending`      | Automatic retention already has pending work for the recording.                                                      |
| `SizeMismatch`        | The observed file size differs from the catalog byte count.                                                          |
| `PathRejected`        | The path is not eligible for inspection, including escapes, hidden paths, or links.                                  |
| `InspectionFailed`    | Another filesystem error prevented a usable observation.                                                             |

Malformed identity evidence or an inconsistent ledger rejects the report rather
than reconstructing work from current files. Known catalog blockers are reported
before inspecting the affected path. Both the planned and observed catalog
revisions are returned; a changed revision requires another preview.

Malformed persisted fingerprints also reject intent reads and confirmation retries
with a redacted error. Rejected values and internal paths are not included in
public diagnostic messages.

The entire request shares one two-second deadline across queueing, catalog reads,
and at most 128 filesystem checks. Paths are limited to 4,096 bytes and sixteen
components. The deadline is cooperative: it cannot interrupt an operating-system
or database call already in progress. A timeout returns no partial report.

## Know what file inspection proves

Inspection pins the configured archive directory and opens each descendant
directory without following links. It rejects hidden directories such as
`.exports`, traversal, alternate-stream syntax, leaf symlinks, nonregular files,
and files with multiple hard links. Checks before opening do not replace the
checks on the opened file.

Each observation retains a non-writable file handle until it is dropped. Unix
uses nonblocking opens to avoid waiting for a writer if a regular file is swapped
for a FIFO. Unix requires read permission; Windows requests file attributes.
Inspection does not read media bytes or request write, create, or truncate access.
Preflight releases each file observation as it advances to the next object.

A retained handle pins the observed object, not its pathname. Another handle can
still change the file's contents. Device/file identifiers can be reused outside
the lifetime of an open handle, and the available 64-bit Windows file index is not
a complete ReFS identity. These checks do not certify immutable recording ownership
or qualify Windows reparse-point races without runtime verification.

Even an all-`Present` report with a matching revision is not permission to delete.
Filesystem observations are not atomic with the catalog read or with one another.
Revalidating a file and then unlinking its pathname still leaves a replacement
race. Missing catalog metadata is never evidence that an unknown file is safe to
remove.

The removal path uses separate private staging directories, revalidates the
staged object, and synchronizes directory changes before recording completion.
Unix directories must be owned by the process user and deny untrusted mutation;
private staging denies group and other access. macOS and Linux use atomic
no-replace rename, so a destination introduced before the move is not overwritten.
Unix also checks the retained selected inode after unlink and rejects the attempt
if that inode is still linked.

These checks do not isolate maintenance from another process running as the same
operating-system account or root. Such a process can replace private staging
names immediately before Unix pathname-based unlink. Post-unlink checks can
detect some substitutions, but cannot prevent the replacement from being removed
or establish where a relocated recording went. Do not treat retries after external
staging manipulation as qualified recovery. A trusted service-account boundary
or stronger process isolation must be resolved before this feature is qualified.

The native Windows implementation targets NTFS with persistent ACLs, checked
directory flushes, and handle-based rename/disposition. Other filesystems,
including ReFS, are rejected before source-file mutation. ReFS requires a full
128-bit file identity and a separately justified namespace-durability strategy;
a successful directory-flush call alone is not sufficient. Windows compilation,
ACL/race tests, and crash-boundary execution remain unverified in the current
local environment.

## Qualification still required

Before issue #133 can be completed, qualification must cover:

- exact object ownership and adversarial replacement at every mutation boundary;
- Windows NTFS and Linux runtime tests, plus supported-filesystem failure behavior;
- holds, shares, operational investigations, moves, restores, and current access
  coordination, including audit completeness;
- partial failure, lost replies, cancellation, and crash recovery across all
  file/catalog/tombstone combinations;
- the remaining reconciliation categories and remedies;
- browser conflict regeneration, failed-job retry, access revocation, and truthful
  timeline results, beyond the existing desktop/mobile happy-path coverage;
- final-commit canonical checks and reproducible performance evidence.

Unknown files must never be adopted or deleted automatically. A ready export must
not be removed as a side effect of deleting its source, and a bookmark is not an
evidence hold. The implementation and acceptance criteria are tracked in
[issue #133](https://github.com/xnorpx/keeppeek/issues/133).

Use [Recording and evidence](./recording-and-evidence.md) for recording policies,
playable coverage, and evidence exports. See [Backup and restore](./backup-and-restore.md)
before changing storage, and [Release readiness and known limitations](./release-readiness.md)
for the qualification status of the wider system.
