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
