# Named storage volumes (#129)

Base: `26ef522` on `main`. Branch: `feat/129-storage-volumes`.
The acceptance criteria and ordered slices in issue #129 remain authoritative.

## Invariants and limits

- Existing configurations keep their current roots and placement behavior.
- Configuration lives in `config.toml`; journals and object locations belong to the catalog.
- A policy change applies to new objects. Existing media keeps its recorded location.
- At most 32 volumes and 256 placement rules; at most 8 candidates per rule.
- Placement uses current capacity observations, explicit fallback, and stable ID tie-breaking.
- Missing capacity evidence cannot admit a write. A configured offline, draining, or read-only
  volume cannot receive new writes.
- Source data remains authoritative until a verified destination is published. Recovery must
  retain at least one verified copy. Unindexed files are never automatically removed.
- Metadata relocation is a confirmed restart operation, with the old catalog retained until
  the new authority opens and passes validation.
- Persist the filesystem/root generation with locations. An ID cannot be rebound while it
  owns objects. A missing mount is offline; never measure or write to its nearest ancestor.
- Observations have bounded age and generation. Admission subtracts outstanding reservations
  from both per-volume caps and shared physical-filesystem space. Growing writes must reserve
  more space before exceeding their allocation; restart recovers outstanding reservations.
- Moves, retention, and maintenance share a conflict domain. Publication is an authorized
  transition of the owning move, not a bypass of another job's claim. Unknown commit outcomes
  resolve through the same durable job ID before retry.
- New default placement bypasses legacy path migration. Offline roots retain catalog, export,
  and thumbnail references. Cleanup filters by volume and never treats offline as absent.
- Bootstrap fences the metadata handoff so the retained old catalog cannot open as a second
  writer. Rollback requires a stopped process and a verified authority transition.
- The first named binding or metadata handoff installs a generated-column format sentinel.
  These catalogs require the new binary, including for backup inspection and restoration.
  Turso 0.7.2 rejects their schema when generated columns are disabled. The builders in the
  checked repository revisions `103dbf7` and issue base `26ef522` leave that option disabled;
  this is a tested downgrade barrier for those builds, not a promise about arbitrary forks.

## Ordered implementation and evidence

- [ ] Config model, validation, legacy mapping, and pure placement decisions.
      Verify invalid roots/roles/limits/references, deterministic selection and rejection, and
      config round-trip/preservation with focused Rust tests.
- [ ] Durable object locations and placement reservations in the existing catalog actor.
      Verify old catalogs, reopen, failure injection, claims, and stable recording identity.
- [ ] Active/archive placement and isolated volume pressure.
      Verify offline/full destinations, allowed fallback, and independent camera continuity.
- [ ] Journaled copy/verify/publish/retire and restart recovery.
      Verify every durable boundary, cancellation, replacement races, and retained source bytes.
- [ ] Export and thumbnail owners use durable per-object locations.
      Verify old jobs/images after config changes, offline roots, and non-destructive reconciliation.
- [ ] Confirmed offline metadata relocation with one authoritative catalog.
      Verify WAL-bearing snapshots, restart interruption, conflicts, and bootstrap exclusion.
- [ ] Administrator protocol and UI, previews, draining, cancellation, and status.
      Verify authorization, revisions, draft preservation, keyboard/mobile flows, and real browser use.
- [ ] Benchmarks, operational docs, independent review, and canonical Windows validation.
      Record final SHA, commands, outcomes, performance distributions and CI links before completion.

## Approved API scope

Files: `api/webrtc.proto` and `api/webrtc.md`, plus generated bindings through the existing
generator. Preserve all existing fields and operations.

- Add an optional named-volume configuration message to `RuntimeStorageConfiguration` using
  the next unused field number. Omission preserves named-volume settings from older clients.
  The message contains bounded typed volumes (ID, root, roles, state, priority, byte thresholds,
  source/group allowlists) and placement rules (role, source/group selector, candidates,
  priority/free-space strategy, explicit fallback). Existing revision checks apply to updates.
- Add a dedicated storage-volume command family and response messages: list status, probe,
  preview placement, preview migration/drain, confirm migration/drain, cancel, inspect job,
  and preview/confirm metadata relocation. Mutation requires the preview revision and token;
  metadata changes explicitly report restart and downtime requirements.
- Add bounded per-volume health to storage health: stable ID, state, capacity, owned bytes,
  filesystem identity/capabilities, and pending job summaries. Paths are Administrator-only.
- Add a capability marker only after these operations are implemented and verified. No HTTP
  endpoint or unrelated state-store payload substitutes for these operations.

The owner approved extending the protobuf API on 2026-09-27 for this task. The scope above
includes the protocol documentation, generated bindings, and contract tests. Approval is recorded;
it does not imply that runtime operations or their acceptance evidence are implemented.

Captured legacy roots now suppress destructive startup reconciliation. Before exposing catalog
handles, startup may repair missing keyframes or a completed `.active` rename only when a pinned
file matches the recorded identity, size, initialization, fragments, and existing keyframes.
Finalization, coverage, and event links commit together. Offline files, changed evidence, and
maintenance/allocation conflicts retain their references. The pass attempts at most 64 candidates
within a shared five-second cooperative deadline; a durable cursor advances past unresolved files
on later starts. Filesystem calls can exceed the cooperative deadline. Disabled drafts retain the
original legacy startup behavior. The catalog regression selection passed 197 tests with one
existing ignored test (`target/129-captured-startup-regressions.log`); strict workspace/all-target
Clippy also passed (`target/129-captured-startup-clippy.log`). This remains a prerequisite for
application adoption, not completion of migration or activation.

## Durable location checkpoint

The next catalog slice needs these records and transitions before activating any destination:

Legacy bindings must snapshot the effective paths at migration time. Proposed reserved IDs are
`legacy-active` for the medium-term root, `legacy-archive` for the long-term root, `legacy-export`
for its `.exports` directory, `legacy-thumbnail` for the effective thumbnail root, and
`legacy-metadata` for the effective catalog directory. The catalog filename remains an object key;
custom catalog files are not assumed to be under a media root. Export history remains a
metadata-owned object at its original export location until a confirmed metadata relocation.
Overlapping legacy roots must share physical capacity accounting. They do not make overlapping
new named roots valid. Backfill must retain the original absolute path until its binding and
relative key are proven; ambiguous or outside-root rows block activation rather than being
silently reassigned. This mapping is proposed, not implemented in the draft-config increment.

| Record          | Identity and invariant                                                                                                                                            |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Volume binding  | Stable volume ID, generation, configured canonical root, opened filesystem/root identity. Rebinding requires no live objects or reservations.                     |
| Object location | Existing object ID and kind, volume ID/generation, confined relative key, location revision, expected byte count and verified digest.                             |
| Reservation     | Stable operation ID, object ID, volume generation, reserved bytes and phase. Shared filesystem reservations are accounted once.                                   |
| Move intent     | Stable job ID, expected source location/revision/identity, target reservation, private staging key, byte count/digest, cancellation flag, phase and failure code. |

| Durable phase      | Reader authority | Recovery rule                                                                                              |
| ------------------ | ---------------- | ---------------------------------------------------------------------------------------------------------- |
| Reserved           | Source           | Validate bindings and source identity; retain source on cancellation or failed destination admission.      |
| Copying            | Source           | Resume or restart only the job-owned staging object; never delete an unrelated file.                       |
| Verified           | Source           | Recheck expected revisions and identities; synchronize the destination and its parent as supported.        |
| Pending location   | Source           | Commit the non-readable destination location and move phase together before publishing the final filename. |
| Published file     | Source           | Final destination exists exclusively and is verified; resolve an uncertain catalog commit by job ID.       |
| Published location | Destination      | Readers resolve the new revision; preserve the source until existing readers release their leases.         |
| Retiring source    | Destination      | Retire only the captured source identity through confined, private staging.                                |
| Complete           | Destination      | Release reservations and retain bounded job evidence.                                                      |

The catalog actor must own transitions and conflicts with maintenance/retention. A failed or unknown
transition does not authorize source removal. Same-filesystem moves retain the same logical protocol;
optimization cannot remove recovery evidence. Metadata handoff uses a separate bootstrap fence and a
consistent catalog snapshot, not this per-object mover.

Use the existing SHA-256 dependency for streaming integrity evidence. Hash through the pinned
source and destination handles; the backup path-based hash helper is not a confinement primitive.
Length or container metadata alone does not prove that a copied file has the same bytes.

## Review findings addressed

- Root/filesystem generation, mount-loss behavior, reservations shared by physical filesystem,
  maintenance fencing, legacy migration bypass, artifact ownership, and exclusive metadata handoff
  are explicit above. These are requirements, not claims of implemented runtime behavior.
- The pure placement review requires Windows device-name rejection, boundary tests, source/group
  precedence and allowlists, cap-aware ranking, and explicit semantics for disabled fallback.
- Cross-model review was offered. No external CLI review has been authorized or run.

## Catalog ledger increment

`catalog::locations` now serializes immutable root bindings, bounded allocation reservations,
and initial authoritative object publication through the existing catalog actor. Migration 3 adds
the ledger without backfilling or changing legacy recording paths. A reservation is not permission
to open a file: the runtime owner still must pin and validate the configured root and enforce the
current policy before opening its destination.

Capacity evidence includes the root and filesystem identities, a five-second lifetime, and the
catalog revision read before the probe. Each mutation and catalog reopening invalidates earlier
observations. Durable reservations debit shared filesystem space, and per-volume counters keep
admission work bounded by 37 bindings rather than the number of recorded objects. Publication
requires the owner's verified file identity, size, and SHA-256 digest; pending allocations do not
appear as readable locations. Publication converts the reservation to owned bytes atomically.

Exact operation retries preserve intent across reopening. Changed intent, stale observations,
foreign root evidence, disabled bindings, unsafe relative keys, colliding destinations, and signed
integer overflow are rejected. Failed mutations roll back; if clean transaction state cannot be
established, the catalog writer stops instead of processing unrelated writes in an uncertain
transaction.

Fifteen focused `volume_ledger` tests cover these boundaries, competing callers, publication limits,
restart persistence, transaction cleanup, and maintenance conflicts by ID and destination path.
Published recordings reject legacy identity, size, path, and finalized-state changes. Legacy startup
backfill excludes volume-owned rows so replacing a file cannot silently replace captured evidence.
This is still an internal catalog increment: production registration/backfill, growing writers,
authorized move transitions, reader leases, artifact owners, metadata handoff, and Administrator
operations remain outstanding.

## Pinned root inspection increment

`volumes::root::Root` opens a validated absolute root one component at a time, without
creating missing directories or following links. Windows checks every opened handle for
reparse attributes and records the volume GUID, serial number, and full 128-bit directory ID.
Unix records the device and inode. Debug output redacts paths and identities.

Capacity observations carry the caller's preceding ledger revision. Unix queries the pinned
directory with `fstatvfs`; Windows queries the pinned volume GUID directly with
`GetDiskFreeSpaceExW`. Neither path substitutes an existing ancestor. Revalidation brackets
the probe. The two-second deadline rejects slow observations after system calls return; it
cannot interrupt a blocked filesystem call.

Tests cover missing roots without creation, identity stability, pinned-root capacity, and
final/intermediate links. Windows pins prevent directory renames; Unix tests rename and
replacement detection. This is an inspection primitive, not writer integration or permission
to enable a configured volume. Runtime ownership, recovery, and Administrator operations
listed above still must be implemented before activation.

## Writer and movement integration

The runtime now selects and reserves a root before file creation, grows its reservation before
each bounded write, and accounts for synchronized materialized bytes separately from outstanding
physical reservations. Recording finalization and authoritative location publication share one
catalog transaction. A failed writer cannot publish or accept a buffered write replay.

The move journal reserves a non-readable destination under the same stable object identity.
The worker copies in 64 KiB chunks, verifies SHA-256 through pinned handles, synchronizes and
renames without replacement, and switches the catalog location transactionally. Restart resumes
only the captured temporary identity and rechecks the full result. Source retirement waits for
reader leases and retains a verified destination handle while moving the old source through a
private quarantine. Durable receipts distinguish a completed removal from unexplained absence.
Cancellation uses the same removal receipts and releases destination capacity only after cleanup.

The background worker has a 64-item wakeup queue, 64-job scan pages, at most 4096 admitted moves,
one attempt per scan, and a 60-second rescan interval. Failed jobs wait for the next
scan without per-job retry sleeps that delay other volumes. Admission precedes the wakeup, so queue
overflow does not discard a committed job. The application joins the actual worker before closing
the catalog. A recording reservation stores its matching archive rule, candidate settings, source,
and groups in the same transaction, before creating the recording file. Finalization makes that
request runnable through its published allocation; the writer only wakes the existing scan.
Each recording can retain one archive request with at most 128 KiB of captured policy. Waiting
requests do not consume the 4096 admitted-move budget or prevent new recordings during an archive
outage. Each scan processes at most 4096 entries in 64-entry pages and resumes its cursor on the
next pass before wrapping to the beginning. Full or unavailable destinations leave the request and source
intact for another pass, including after restart. Later default-policy changes do not reinterpret
the captured request; current destination states, roles, roots, and write limits still constrain
admission. Move admission acknowledges the request in the same transaction as its destination
reservation. A rejected reservation rolls back both changes. If the sole permitted destination
already owns the recording, completing the request needs no additional capacity or media copy.

Event-image startup no longer deletes unindexed images or temporary files, and missing files keep
their catalog references. The legacy thumbnail quota considers only catalog-referenced filenames
in its configured root. This preserves unrelated files under a nonzero quota as well as during
startup. Named image placement now uses the ownership path described below; legacy image
backfill remains separate work.

The same scan retries up to 32 roots that were unavailable when the runtime started. It opens
and synchronizes roots outside the admission lock, then checks the durable binding before making
the root available. An existing binding must retain its filesystem and directory identity; a
different directory at the same path stays unavailable. Disabled volumes are never activated by
recovery. Once bound, roots remain pinned for this runtime; this increment does not replace a
lost filesystem handle or apply live configuration changes.

Copy recovery, publication, source retirement, cancellation cleanup, and receipt cleanup require
a volume that permits changes to existing objects. ReadOnly and Disabled states preserve files
and pending journal work for a later writable configuration. A read-only source may still supply
a copy, and an already published destination remains readable. The ledger persists draining
separately from permission to modify existing objects: new reservations are refused, including
requests from stale placement snapshots, while existing writers can grow within the same capacity
checks. Older binding rows migrate without changing their write permissions. This is admission
behavior; operator drain previews, queued migration, and live policy application remain outstanding.

Playback, scrub, event-search, and export workers retain reader leases for their actual file-use
lifetime. Leases also cover legacy aliases of named paths. After publication, new readers cannot
resolve an alias of the retired source. Existing readers finish before its retirement.
The library keyframe reader validates cached locations through the same catalog lease request
before opening a file. A stale path is refused even when the old file still exists; the lease
remains held until the file handle closes.

Catalog authority leases protect both database workers and surviving reader/move workers.
Validated backup imports, compaction, and legacy path migration use explicit authority transitions;
named-root restore remains rejected until root ownership transfer is implemented. Interrupted
operations preserve ambiguous files. These changes are internal integration, not activation:
configuration still accepts only Disabled named-volume drafts.

Remaining before activation: legacy bindings/backfill, incomplete-image recovery,
per-volume retention and drain, live policy changes and mounted-volume recovery, named metadata relocation,
Administrator operations and UI, and final performance/platform evidence. Focused test logs are
under `target/129-*-nextest.log`; failed intermediate runs must not be presented as final evidence.

## Named event-image integration

Snapshots, published event images, and native camera attachments reserve their matching thumbnail
volume before writing. The event revision and complete attachment-location set commit in one
catalog transaction. Full and offline matched volumes return an error without falling back to
the legacy directory. An immutable publication receipt permits the same commit to be retried
after a later event close, while rejecting changed evidence or a changed attachment set.

Current attachment mappings are explicit. Replacing an attachment with a legacy image or detaching
it deactivates the old mapping. Media delivery and native image reads hold the existing reader
lease through the read. Image moves use the existing copy, verification, publication, and
retirement workflow. A default-policy change does not redirect a published named image.

The existing worker retires superseded images after their readers finish. Removal uses the
captured identity and digest, and releases quota only after a durable removal receipt. Rejected
commits also journal cleanup for their sealed files; an already committed allocation is preserved
when the publication reply was lost. No directory scan grants ownership of unrelated images.

This increment does not enable named volumes. Recovery of crashes or failures before image
sealing, legacy root backfill, notification image-location refresh, per-volume retention,
migration management, and Administrator UI remain outstanding. The PR stays draft.

## Export writer prerequisite

The MP4 exporter now shares one remux implementation between legacy files and an empty
`Write + Seek` sink. A reserved volume file can receive the output directly, including reservation
growth and capacity refusal. The file adapter keeps its temporary-file publication behavior and
discards buffered bytes after failure. Sink callers own synchronization, location publication,
and cleanup. Tests cover byte-equivalent output, cancellation, invalid sinks, final-flush failure,
and a real capacity-limited reserved file. Export job placement, history recovery, downloads,
and expiry are not connected to named volumes by this prerequisite alone.

## Export retirement and readers

Export retirement is now journaled before cleanup. Its artifact ID fences late reservation,
growth, and publication. A worker that was creating a file may still record its initial identity
before cleanup captures evidence. The existing storage worker removes only that owned file,
using the existing durable removal receipts, and releases capacity after confirmed removal.
Absent, empty, and partial pending files follow the same path; ambiguous files remain untouched.

Export readers resolve the current location and acquire a lease in one catalog operation.
Retirement refuses new readers and waits for existing readers and admitted moves. Read-only or
offline roots keep their cleanup intent for a later pass. Existing compact artifact UUIDs retain
their catalog spelling, while file-removal receipts use the allocation operation ID.

Server export jobs now reserve the matching named volume before opening a writer. Matched
capacity failures do not create a legacy export. The actual worker holds its artifact lease
through publication and closes its file before releasing that lease. Verification reports
progress and observes cancellation through the existing monitor.

Downloads resolve the current catalog location and read through its pinned file handle, with
a reader lease and the existing browser size bound. A stale checksum failure cannot mutate or
retire a newer retry. Restart recovery runs after catalog attachment, preserves Ready jobs on
unavailable volumes, and admits interrupted-attempt cleanup before changing history. Expiry,
retry, and history pruning admit cleanup before forgetting an attempt. Invalid history remains
untouched and disables export creation until recovery succeeds on restart.

## Export integration and operator move API

Server exports now use named placement end to end. Seven server regressions cover verified MP4
publication/download, quota refusal, offline recovery, interrupted cleanup, malformed history,
retry fencing, and downloads after the old moved file is removed.

The Administrator-only storage command family exposes status, a non-mutating configured-root
probe, placement preview, bounded object/job pages, and preview/confirm/cancel for individual moves.
Confirmations reuse the existing durable journal and worker. Previews bind the requesting actor,
configuration revision, authoritative object location, and server-derived camera groups. Stale
configuration or a pending runtime restart rejects admission. Secret-based volume IDs retain their
references on the wire. The browser control client preserves named settings and exact byte limits.

Validation for this increment: 147 focused export, image, runtime, and protocol tests passed;
strict workspace/all-target Clippy passed; five system-client tests passed; Svelte check reported
zero errors and warnings. Full canonical Windows validation remains a final acceptance gate.
Activation remains disabled. Bulk drain, legacy adoption, named recording retention, incomplete
image recovery, metadata relocation, management screens, and final benchmarks remain outstanding.

## Recording retention and interrupted image recovery

Named-volume pressure now admits the oldest eligible finalized recording in that volume through
the existing worker. Active and protected recordings, maintenance claims, and unfinished moves
remain excluded. Existing readers finish before removal; new readers and protection changes are
fenced after admission. Durable root receipts retain ownership across offline/read-only restarts,
and the catalog keeps deletion coverage after removal. Legacy cleanup excludes named allocations
and counts only legacy bytes against the legacy recording cap.

Thumbnail reservations hold a writer lease from admission through file close. The worker recovers
unopened, empty, and partial abandoned images, using captured identity and existing removal
receipts. Empty-file evidence is separate from the positive reservation; quota is released only
after confirmed removal. Unrelated files and unavailable roots remain untouched.

Notification delivery resolves the current event attachment location at send time and holds its
reader lease through the provider read. A moved image remains available after its old copy is
removed; replaced event revisions cannot reuse a cached image path.

The focused retention/recovery regression run passed 132 tests, including six recording-retention,
four interrupted-image, and two notification-location tests. Evidence is in
`target/129-retention-recovery-regressions.log`. The two recovery tests first failed with retained
quota in `target/129-image-recovery-red.log`. Named activation remains disabled. Image-volume
pressure, legacy adoption, drain/live management, metadata relocation, UI and final qualification
remain open.

## Image pressure, failure isolation, and management UI

Image-volume pressure reuses the existing retirement worker after recording retention finds no
eligible recording. One pending retirement per volume prevents repeated refused allocations from
queuing unnecessary deletions. Event metadata remains intact; retained ownership evidence prevents
an unavailable named image from resolving an unrelated legacy file. Named recording cameras no
longer inherit an unrelated legacy-volume pause. Existing writer destinations remain authoritative
across camera-group changes, and rotation cannot silently enter paused legacy storage.

The Administrator settings screen now edits disabled volume and placement drafts, probes roots,
and exposes individual move preview/confirmation and job inspection/cancellation. Exact integers,
secret references, and dirty inputs survive the round trip. Bulk drain and metadata relocation
remain explicitly unavailable. Focused UI build, type checks, and Chromium tests passed; the
canonical Windows gate is still required at the final head.

Legacy file inspection and retirement now support bounded nested catalog-known paths through
pinned roots and parents. Retirement reuses existing receipts and binds the original root as well
as the nested parent. This is a confinement prerequisite, not completed legacy adoption.

The expanded storage/event/engine/reader/notification run passed 194 tests in 17.428 seconds
(`target/129-storage-owner-regressions.log`). The image-pressure test first failed because no image
was reclaimed (`target/129-image-pressure-red.log`). Protection-race qualification, strict linting,
legacy adoption, bulk drain/live management, metadata relocation, activation, and final
performance/platform evidence remain open.

The next checkpoint closes the image protection race: catalog triggers reject both a protection
upgrade and a late protected recording association once image retirement is admitted. The late
association regression failed before the fix (`target/129-pressure-protection-red.log`). The
expanded selection then passed 196 tests in 17.544 seconds
(`target/129-storage-checkpoint-tests.log`), and strict workspace/all-target Clippy passed
(`target/129-storage-checkpoint-clippy.log`). Independent review found no further issue in these
pressure transitions or nested legacy retirement receipts.

The catalog now stores one immutable snapshot of effective legacy media and metadata paths.
Registering changed defaults returns the original snapshot; loading disabled drafts does not
register it. This remains a migration prerequisite, not completed file adoption.

Persisted volume definitions now require deletion confirmation, including after editing their
IDs. Cancel preserves the draft; new unsaved rows remove directly. Four Chromium tests, eight
focused unit tests, clean Svelte checks, and the production UI build passed
(`target/129-volume-ui-*.log`). Canonical Windows validation and the remaining acceptance work
above are still required before activation or marking the PR ready.

Full qualification exposed one older catalog assertion that expected legacy cleanup to error on
named ownership. The assertion now expects no legacy candidate and retains the path/identity
mutation fences. The same full run stopped after 1,237 passes and this failure; it is not a
passing canonical gate (`target/129-checkpoint-check.log`).

Startup adoption also needs a catalog open mode that defers both legacy file-size inspection and
the legacy backfill worker. A regression demonstrated that normal backfill removes an unavailable
legacy row before adoption can inspect it (`target/129-adoption-open-red.log`). The new explicit
open mode preserves that row and its byte accounting. Normal legacy startup remains covered.
All 42 selected catalog tests passed (`target/129-adoption-catalog-green.log`). The application
adoption flow still needs to call this mode before media workers begin.

The complete UI quality command passed 409 Bun tests, 250 browser tests, and 57 compatibility
tests (`target/129-ui-quality.log`). CI exposed missing volume-status support in the shared
Settings E2E control-peer fixture; its list response now represents an empty disabled runtime.
All 14 affected logging/dashboard E2E tests then passed with browser-error assertions intact
(`target/129-settings-e2e-green.log`). Independent review found no startup-mode regression.

The checkpoint at `6729cc7` passed the canonical Windows script with Rust 1.99.0,
incremental compilation disabled, and slow tests enabled: 3,089 Rust tests, strict Clippy,
formatting/dependency checks, the full UI quality suite, and 280 E2E tests passed (two E2E
tests were skipped by their existing conditions). Evidence: `target/129-6729cc7-check.log`.
This qualifies that checkpoint, not the later recording-recovery changes or the remaining
migration, drain, metadata relocation, and activation acceptance work.

Interrupted named recordings now recover through the existing catalog actor and worker.
Recovery verifies a complete indexed fragment prefix, records its identity and digest before
truncation, and publishes the surviving coverage and byte ownership together. Empty or
initialization-only recordings use the existing retirement receipts. Unopened reservations
release capacity only after confirming that no file exists. Live writers, readers, conflicting
catalog paths, replaced files, and corrupt index evidence defer recovery without deleting data.
Durable plans resume after truncation or retirement and before catalog acknowledgement.
All 191 selected storage, catalog, reader, and container tests passed in 26.120 seconds
(`target/129-recording-recovery-regressions-green.log`). This is focused evidence; full acceptance
still requires the migration, management, metadata, activation, and final qualification work above.

Export-history recovery now receives the artifact root separately from the history filename.
Moving only the history file no longer marks ready legacy exports missing or redirects cleanup
to the metadata directory. The regression failed before the change; all nine selected export
history and named-export integration tests passed afterward (`target/129-export-metadata-root-green.log`).
This separates the two paths for metadata handoff; it does not implement the handoff itself.

Legacy recording inventory now records bounded pages of finalized catalog references separately
from named allocations. Verification preserves the exact owner revision and rejects admitted
cleanup, maintenance, named path aliases, replacement evidence, and delete/recreate reuse.
Offline paths remain unresolved without filesystem creation or deletion. Physical verification
uses captured roots and the existing pinned legacy reader. It checks existing catalog identity
and size; Windows legacy and named identity formats come from the same file handle.
Verified inventory continues to count toward legacy quota and remains eligible for legacy cleanup.
The 63 selected legacy, location, and export-history regressions passed in 17.038 seconds
(`target/129-legacy-inventory-regressions.log`). Root binding, startup integration, other legacy
owners, and confirmed transfer into named ownership still remain before activation.

Application startup now captures effective legacy roots before reconciliation when a named
volume is active. Disabled drafts retain ordinary legacy behavior. Once captured, later media
root edits are rejected even if named volumes are disabled or removed. The old directory
migration path refuses captured or named ownership while holding the catalog lease, before
creating destination directories or fencing the source. Pending migrations without catalog
metadata are refused before filesystem movement; operators must restore the original settings
and reschedule them. These checks preserve the migration boundary; they do not complete
confirmed adoption or enable named volumes.
All 29 migration regressions and three application startup regressions passed
(`target/129-migration-complete-green.log`, `target/129-application-capture-green.log`).
Independent review found no remaining blocker in this capture and migration guard.

Operational drain now has a separate persisted catalog flag. Configuration rebinding and root
recovery preserve it. New allocations refuse either configured or operational drain, while
already admitted writers may open, grow, publish, and recover. Placement observations include
effective drain so an explicitly allowed fallback can select another volume. Repeating the
same operation does not invalidate otherwise current capacity observations. Six new behavior
tests cover these cases, including catalog restart and clearing an operator drain while the
configured drain remains. This is the admission boundary for bulk drain; administrator
commands, UI, and bulk migration are not yet complete.
The broader storage regression run passed 540 tests with one existing ignored test
(`target/129-operational-drain-regressions.log`).
Strict workspace/all-target Clippy also passed (`target/129-operational-drain-clippy.log`).

The administrator API and UI now expose confirmed operator drain and clearing. Commands require
the current configuration revision and return refreshed status. Configured and operator drain
remain separate, including after clearing; root health remains independent. Secret-reference
volume IDs retain the existing resolution and redaction behavior. All nine volume API tests
and three focused browser tests passed (`target/129-drain-api-green.log`,
`target/129-drain-ui-green.log`). Bulk evacuation and activation remain unfinished.

The canonical Windows gate passed this control checkpoint: 3,146 Rust tests, strict Clippy,
409 Bun tests, 251 browser tests, 57 compatibility tests, and 280 E2E tests with two existing
skips (`target/129-drain-controls-check.log`). A subsequent 320-pixel regression exposed long
volume IDs overflowing action buttons. Short visible labels retain full accessible names;
all three focused browser tests passed (`target/129-drain-mobile-green.log`). Final acceptance
still requires the remaining migration, removal, metadata, and activation work and a final gate.

Captured thumbnail startup now leaves a missing media directory absent and preserves image
references and byte metadata, even under a pruning quota. The original directory can return
without recreating the event store; thumbnail and native attachment reads recover, including
Windows ASCII case variants. Online-root containment and uncaptured creation/retention remain
unchanged. Both directory recreation and case-variant recovery had failing regressions before
their fixes. All 30 event-storage tests passed (`target/129-thumbnail-recovery-green.log`).
This handles missing-directory startup; durable legacy root identity binding and migration
remain required before activation.
Strict workspace/all-target Clippy passed (`target/129-thumbnail-recovery-clippy.log`).

Ordinary settings now check captured paths under configuration coordination before migration
preparation, directory probes, or persistence. Media-root changes and catalog-path changes require
the separate confirmed workflow; unrelated settings changes still succeed. The regression
first reproduced an accepted changed path, then passed all eight path/migration-flag variants
without creating directories or changing saved settings. All ten volume API tests and three
application startup tests passed (`target/129-captured-settings-green.log`,
`target/129-captured-settings-startup.log`).

Settings removal now requires a drained, empty binding with no pending cleanup receipts. Checks
run under configuration coordination before probes or persistence and resolve secret-reference
IDs. Runtime-enabled volumes require operator drain; unbound offline roots must be disabled and
restarted first. Running archive rules require a policy edit and restart before removal, and
captured pending archive intent blocks removal even before destination allocation. Inspection
streams at most 1,024 pending policies and rejects a larger backlog. Removal preserves immutable
bindings, acknowledged history, and files. All 25 selected volume API, removal, receipt-restart,
and startup tests passed (`target/129-removal-complete-green.log`); the two archive regressions
first reproduced unsafe acceptance. Strict workspace/all-target Clippy passed
(`target/129-removal-clippy.log`). Independent review found no further blocker in this guard.
Bulk evacuation, confirmed legacy migration, metadata relocation, and activation remain open.
