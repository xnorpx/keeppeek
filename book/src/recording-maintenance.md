# Recording maintenance

Manual recording deletion and catalog reconciliation are not yet available in the
KeepPeek interface or public protocol. The backend supports bounded catalog
inspection, durable deletion intent, and read-only preflight checks. Confirming
an intent does not reserve a recording, change playback, or delete a file.

Automatic storage retention is separate and remains unchanged. Do not delete
recording files directly as a substitute for the unfinished maintenance workflow:
doing so can leave stale catalog entries and unavailable evidence links.

## Select a complete scope

Maintenance uses stable source, stream, and recording identifiers, never a
display name or a caller-supplied filesystem path. Catalog inspection accepts
one recording or a nonempty half-open time range on one source and logical stream.
The start is included and the end is excluded.

A range selects whole recording objects that overlap it. It does not trim media
at the requested timestamps. The snapshot retains each object's full start and
end bounds so an eventual destructive preview can show any expansion explicitly.

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

| State     | Meaning                                                                           |
| --------- | --------------------------------------------------------------------------------- |
| Prepared  | A short-lived intent is awaiting confirmation. No per-recording work is admitted. |
| Expired   | Preparation must be repeated before confirmation.                                 |
| Queued    | Confirmation and a per-recording work ledger are durable. No deletion runs yet.   |
| Cancelled | Intent was cancelled. No recording or media is changed by cancellation.           |

Confirmation creates exactly one ledger entry per selected recording in snapshot
order, in the same transaction as the queued state. Retrying the same confirmation
after a lost reply or restart returns that job rather than duplicating work.
Queued and cancelled intents survive restart. Cancellation is idempotent and
cannot assign a time before creation or confirmation.

The catalog admits at most 128 prepared plans and 256 confirmed or retained
cancelled jobs. Expired preparation and cancellation history older than 30 days
can be pruned; queued intent is not evicted to make room. Cancelling before
confirmation does not create work entries.

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

## Safeguards still required

The user-facing maintenance workflow remains unavailable until it provides:

- a complete destructive preview with exact or expanded intervals, gaps,
  bookmarks, exports, shares, holds, and expected catalog/event consequences;
- current authorization and coordination with active writers, moves, restores,
  and evidence protection before work is claimed;
- exact object ownership and race-safe removal, followed by durable catalog
  completion or a tombstone;
- per-object progress, errors, partial results, retry, and cancellation that stops
  before the next object without undoing an already committed removal;
- crash recovery for every file/catalog presence combination;
- reconciliation dry runs for missing, orphaned, duplicated, corrupted, escaping,
  stale-temporary, and interrupted-job data, with explicit category-specific remedies;
- Administrator controls, audit records, and equivalent desktop and mobile confirmation.

Unknown files must never be adopted or deleted automatically. A ready export must
not be removed as a side effect of deleting its source, and a bookmark is not an
evidence hold. The implementation and acceptance criteria are tracked in
[issue #133](https://github.com/xnorpx/keeppeek/issues/133).

Use [Recording and evidence](./recording-and-evidence.md) for recording policies,
playable coverage, and evidence exports. See [Backup and restore](./backup-and-restore.md)
before changing storage, and [Release readiness and known limitations](./release-readiness.md)
for the qualification status of the wider system.
