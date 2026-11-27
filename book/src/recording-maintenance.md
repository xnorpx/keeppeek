# Recording maintenance

This chapter covers fleet recording-integrity checks (`/recordings`), explicit plan-then-confirm recording deletion and bounded catalog reconciliation (`/recordings/maintenance`), and Administrator-confirmed fourth-tier archive retention (**Settings → Storage & retention → Local archive**). Reading `/recordings` requires **User** access; all deletion, reconciliation, and archive-retention mutations require **Administrator** access and the relevant server capability. Never delete recording files directly on disk or manually clear catalog rows or private staging directories to force progress.

## Video walkthrough

Watch Administrator recording deletion preview, typed `DELETE N` confirmation, durable ledger execution, and bounded catalog reconciliation on `/recordings/maintenance`.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/recordings.desktop.maintenance.mp4">Download the recording maintenance demo</a>.
</video>

## Recording integrity and coverage (`/recordings`)

**Recording integrity** (`/recordings`, backed by `GET /recording-coverage`) joins effective camera policy, live writer telemetry, keyframe-indexed finalized fragments, and the deletion ledger to prove whether requested streams are actually recording. It auto-refreshes every 30 seconds while visible and links Administrators directly to **Maintenance** (`/recordings/maintenance`) and **Open Keep** (`/keep`).

| Element                        | Current contract & bounds                                                                                                                                                                                                                                                            |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Fleet states**               | `healthy`, `degraded`, `paused_by_policy`, `not_configured` (shown only when nonzero), `unknown`. Policy-disabled streams do not generate unexpected gaps.                                                                                                                           |
| **Playable coverage**          | Counts only finalized, random-access fragments with indexed keyframes (merges gaps ≤1 ms). Ongoing trailing gaps keep `end_ms: null` (`observed_end_ms` marks snapshot time).                                                                                                        |
| **Filters & intervals**        | `24h`, `7d`, `30d` (API max 31 days), camera search (≤128 chars), state, stream (`main`/`sub`), group, and minimum camera gap (`≥1m` to `≥1h`).                                                                                                                                      |
| **Pagination & detail bounds** | 25 cameras/page (API 1–50), bound to `catalog_revision` with 15-minute `page_token` expiry (HTTP `409` resets pagination). Up to 256 exact recent ranges and 256 deletion intervals per stream (10,000-entry ledger); aggregated strips use 15m / 1h / 6h buckets with exact totals. |

See [Recording and evidence](./recording-and-evidence.md) for coverage interpretation and [recording integrity guide](https://github.com/xnorpx/keeppeek/blob/main/docs/recording-integrity.md) for Prometheus `/metrics`.

## Storage safety invariants

- **Manual vs. automatic retention:** Automatic retention runs separately from manual deletion. Active manual claims exclude their recordings from automatic retention and new playback/export resolution until they reach a terminal state.
- **Named storage volumes:** Deletion follows each recording's current catalog owner (including relocated recordings), requires the bound root to be online and writable, and blocks while readers or unfinished moves hold the object. Interrupted jobs retain capacity ownership until durable checkpoints finish.
- **Active files (`.active`):** Startup preserves `.active` files and catalog rows in every root without assuming temporary names authorize deletion. Writers create `.active` files exclusively and fail rather than truncating or replacing an existing path.
- **No side-effect removals:** Bookmarks do **not** protect footage from deletion, ready exports are **not** deleted when source recordings are removed, and unknown files are **never** automatically adopted or deleted.

## Plan-then-confirm recording deletion (`/recordings/maintenance`)

Requires **Administrator** access and `keeppeek.recording-maintenance.v1` over WebRTC. Before deleting irreplaceable footage, retain and independently verify an evidence export or [archive recovery copy](./recording-archive-recovery.md).

### Deletion runbook

1. Open **Recording integrity → Maintenance** (`/recordings/maintenance`, **Recordings** tab), or choose **Review deletion** from Keep's export-range view to carry the camera, stream, and UTC interval over (closing Keep also releases its playback cursor).
2. Select **Camera**, **Stream** (`Sub` or `Main`), **Reason** (**Operator cleanup** or **Privacy request**), and half-open UTC **Start (UTC)** / **End (UTC)** bounds (`[start, end)`), then choose **Preview deletion**.
3. Review every listed whole-recording boundary, total bytes, catalog revision, gaps, bookmarks, related exports, and stated consequences. Selections containing active, protected, retention-pending, unknown-end, or identity-unavailable recordings cannot be confirmed.
4. Close competing playback and wait for active exports, type the exact prompt text (for example `DELETE 1`), and choose **Delete permanently**. Choose **Keep recordings** to dismiss without starting, or **Cancel job** to cancel prepared/queued admission.
5. Track per-object outcomes; use **Refresh job** after disconnects, **Retry failed objects** for recoverable failures, and **Download report** to save a redacted JSON record (contains no confirmation nonce or host paths).

A failed confirmation consumes its server nonce—choose **Refresh preview**, review the fresh snapshot, and re-enter the confirmation text. Losing Administrator access or the capability immediately discards the active review and any late preview responses.

### Scope, durability, and crash-recovery contract

- **Whole-object scope & hard limits:** Maintenance addresses stable source/stream/recording IDs only (never display names or caller paths) and selects every **whole recording** overlapping `[start, end)` without trimming boundary files. Limits: **≤128 objects/plan**, **≤31-day range**, **≤4,096 catalog rows inspected** (exceeding limits fails explicitly rather than returning partial scope), **10-minute** plan expiry (wall + monotonic; restart invalidates prepared plans), **≤128 prepared plans**, and **≤256 confirmed/cancelled jobs** (expired/terminal records older than 30 days may be pruned; unresolved claims are never evicted).
- **Identity binding & concurrency:** Preparation locks an opaque device/file fingerprint, catalog revision, requester identity, and hashed nonce. One deletion worker runs at a time, shares the configuration-update lock with restore/access/export retries, and rechecks session authorization before filesystem mutations. Confirmation is idempotent across lost replies and restarts.
- **Restart recovery:** Startup runs a **2-second bounded recovery pass** before recording pipelines start. It settles catalog completion **only** when staging-directory filesystem identity and absence checks prove unlink already finished—it never moves or deletes present media. Abandoned `Working` items report `Failed` and retain reservations for authenticated **Retry failed objects** (available even on cancelled jobs). Cancelling interrupted work never releases a claim unless the original selected file is positively identified.
- **Audit evidence:** Access audit links outcomes to the durable job ID; structured `recording_maintenance` logs record requester, scope, expanded bounds, reason, revision, counts, and outcome (`hold_override` is always `false`; paths, nonces, and raw failures are redacted).

| Job / object state | Meaning                                                                                                                     |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `Prepared`         | Short-lived preview awaiting confirmation; no recordings reserved.                                                          |
| `Expired`          | Plan timed out or catalog restarted; repeat **Preview deletion**.                                                           |
| `Queued`           | Confirmation and per-object ledger committed atomically; ready for execution.                                               |
| `Working`          | Live deletion worker owns the object.                                                                                       |
| `Staged`           | File moved into private maintenance staging with durable directory-identity checkpoint.                                     |
| `Deleted`          | Unlink verified and catalog completion recorded (irreversible).                                                             |
| `Failed`           | Removal did not finish; claim retained for inspection or **Retry failed objects**.                                          |
| `Cancelled`        | Unstarted objects will not execute; already-deleted objects stay deleted; failed/staged items remain inspectable/retryable. |

## Administrator-confirmed local archive retention

Fourth-tier local archive copies (**Settings → Storage & retention → Local archive**, requiring `keeppeek.local-archive-retention.v1`) protect the **last verified copy** during retention deletion. Setting **Archive retention days** (`retention_days`; blank = indefinite) makes verified replicas eligible for preview only—**expiry alone never deletes any archive copy**.

### Archive deletion runbook

1. Open **Settings → Storage & retention → Local archive**.
2. Choose **Preview archive deletion** on one ready job, or **Preview expired archive copies** to preview one page of up to **16** retention-eligible jobs.
3. Review the target ID, object IDs, bytes, verification timestamp, and per-item eligibility (`Eligible for deletion`, `Within retention period`, `Restore the primary copy first`, `Verified primary unavailable`, `Preservation hold or maintenance`, `An active reader is using this object`, `Awaiting fresh verification`).
4. Choose **Confirm archive deletion** to queue removal for **only** the eligible items on that page (each additional page requires its own preview and confirmation). Use **Cancel queued archive deletion** before execution starts if needed.

### Last-verified-copy protection rules

- **Two-copy verification & durable staging:** Before claiming a job, the worker verifies both complete copies within one `operation_timeout_secs`, then records retirement intent, quarantines the archive, records staging, and re-verifies both the staged archive and protected primary in the same turn before unlinking. Restart resumes from the last durable step.
- **Evicted primaries & expired exports:** Evicted recordings require a completed **Restore primary** before archive deletion is allowed. Expired exports whose primary was already retired retain their archive copy. Holds, active readers, changed media, or unavailable primary storage block removal (`archive_deletion_blocked`).
- **Staged restore fence:** Archive deletion claims are refused while a pending configuration restore would replace the database or change/disable the active `[storage.archive]` target ID or root.
- **Permanent catalog exclusion:** Once acknowledged, KeepPeek records the deletion in the catalog and permanently excludes that recording or export from automatic archiving and the manual copy picker while that catalog is in use.

| Server platform             | Surviving-primary protection requirement before archive removal                                                                                                                                                                                                                                                                                                                                                                  |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Windows (NTFS)**          | KeepPeek holds a writer-excluding read handle on the primary throughout archive removal.                                                                                                                                                                                                                                                                                                                                         |
| **Linux**                   | Primary **must** have both `fs-verity` and the immutable inode flag (`chattr +i`); ordinary mutable Linux primaries retain both copies (`archive_primary_write_protection_required`). After the job reaches `acknowledged`, a host admin may verify catalog identity/digest and clear the immutable flag with `chattr -i -- <path>` if moves or retention must resume (never while queued, claimed, or pending acknowledgement). |
| **macOS & other platforms** | Both copies are retained; archive removal does not proceed.                                                                                                                                                                                                                                                                                                                                                                      |

## Catalog reconciliation (`/recordings/maintenance` → Reconciliation)

Choose **Inspect catalog** to run a bounded read-only inventory comparing the catalog against archive roots:

- **Scan bounds:** Reads at most **4,096 catalog rows** and **4,096 archive entries**, returning at most **128 findings**. Incomplete reports explicitly disable all remedies. Reports belong to the requesting Administrator, expire after **10 minutes**, and at most **16** live in server memory.
- **Findings reported:** Missing/unknown files, duplicate paths or catalog file identities, size/identity mismatch, rejected paths, temporary files, interrupted work, corrupt containers, and playback-index mismatch. Protected recordings are not mislabeled as interrupted; active recordings are never parsed.
- **Bounded fMP4 validation:** Finalized cataloged files verify box boundaries, decode times, track IDs, sample extents, and decoder metadata up to **4,096 fragments**, **8 MiB metadata**, **2 tracks**, and **65,536 samples/track fragment**. This is a structural check, not a full media decode or content checksum.
- **Reports & remedies:** Filter by category and save evidence via **Download reconciliation report**. Mutating remedies share storage coordination with deletion/restore and require a fresh scan afterward. Quarantine, content-checksum comparison, and previewed temp-file deletion are not implemented.

| Remedy               | Eligible finding                                                       | Effect & preconditions                                                                                                                                                                      |
| -------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Acknowledge**      | Any finding                                                            | Marks ignored in the in-memory report only; touches neither catalog nor disk.                                                                                                               |
| **Retain tombstone** | Missing, unprotected cataloged file                                    | Rechecks absence and catalog revision, removes stale row/playback indexes, preserves deletion-coverage tombstone; deletes **no** disk bytes. Rejected if file reappears or catalog changed. |
| **Rebuild index**    | Existing unprotected cataloged file with valid fMP4 and index mismatch | Rechecks revision, index fingerprint, and file identity, then rebuilds fragment/keyframe indexes and coverage in one transaction; media bytes untouched. Unavailable for unknown files.     |

## Preflight statuses and filesystem trust boundaries

Internal preflight checks run an actor-owned queued job by taking one catalog read transaction first, closing it, and performing at most **128 filesystem checks** under a shared **2-second cooperative deadline** (paths bounded to **4,096 bytes** and **16 components**; timeouts return no partial report, and malformed persisted fingerprints fail with redacted errors).

| Preflight status                                            | Meaning                                                                                                             |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `Present`                                                   | Preparation fingerprint, current catalog ID, observed file identity, and byte size all match (contents not hashed). |
| `IdentityChanged`                                           | Preparation fingerprint, current catalog ID, and observed file disagree.                                            |
| `IdentityUnavailable`                                       | Missing preparation or catalog identity evidence (path/size alone never match).                                     |
| `MissingFile` / `MissingCatalog`                            | File absent at resolved path, or catalog row absent (disk state unknown).                                           |
| `CatalogChanged`                                            | Source, stream, time bounds, or recorded byte count changed since preview.                                          |
| `ActiveRecording` / `ProtectedRecording` / `CleanupPending` | Catalog state blocks manual maintenance (reported before filesystem inspection).                                    |
| `SizeMismatch`                                              | Observed file length differs from catalog byte count.                                                               |
| `PathRejected` / `InspectionFailed`                         | Escapes, hidden directories (such as `.exports`), alternate streams, links, nonregular files, or OS I/O error.      |

### What filesystem checks prove—and remaining Alpha boundaries

- **Path & handle safety:** Inspection pins the archive root and traverses descendant directories without following links, rejecting hidden directories, traversal, alternate data streams, symlinks, nonregular files, and multi-hardlink files both before and on the opened non-writable handle (nonblocking open on Unix; attribute open on Windows; never reads media bytes or requests write/truncate access).
- **Staging & OS guarantees:** Deletion moves files into owner-only private staging, revalidates identity, and syncs directories before catalog completion. Unix enforces process-owned directories, atomic no-replace rename, and post-unlink inode link-count verification. Native Windows requires **NTFS** with persistent ACLs, checked directory flushes, and handle-based rename/disposition—**ReFS** (which needs 128-bit identity and a separate durability contract), FAT, and network shares are rejected before mutation.
- **Explicit limits:** A `Present` preflight is never standalone permission to delete (filesystem checks are not atomic with catalog reads or unlinks, and file IDs can be reused after handle drop). Checks do **not** isolate maintenance from another process running under the same OS user or `root`; do not treat retries after external staging tampering as qualified recovery.
- **Qualification status:** Implementation [issue #133](https://github.com/xnorpx/keeppeek/issues/133) passed native Windows/macOS/Linux synthetic checks, while deployment-specific qualification remains open under [Alpha gate #145](https://github.com/xnorpx/keeppeek/issues/145) (adversarial replacement at mutation boundaries, full NTFS/Linux runtime and unsupported-FS coverage, cross-feature coordination/audit, crash recovery across all file/catalog/tombstone states, remaining reconciliation remedies, browser retry/revocation flows, and final-commit performance evidence).

See [Diagnose incomplete recovery](./recording-archive-recovery.md#diagnose-incomplete-recovery) when troubleshooting missing bytes vs. catalog drift, [Backup and restore](./backup-and-restore.md) before changing storage, and [Release readiness and known limitations](./release-readiness.md).
