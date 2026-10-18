# Runtime profiles implementation contract

This document tracks [#202](https://github.com/xnorpx/keeppeek/issues/202).
Runtime profiles are partially implemented. Administrators can create, rename, edit, delete, and
activate bounded definitions through the existing WebRTC state-store command and the settings UI.
Selection is revisioned and durable; privacy overrides feed the existing media privacy gate; and
recording, camera availability, and notification suppression reach their current runtime owners.
The shell and settings surfaces watch active-profile and per-camera application state. Profile
targets and observations use persistent camera UUIDs; the Administrator-only camera catalog maps
them to current source IPs for the editor. The full 127-camera multipart document layout,
asynchronous convergence, complete privacy leak matrix, camera-filtered observations and
capacity/performance evidence remain required for feature acceptance. Reads return bounded
profile parts, and writes can
stage bounded Administrator-owned parts before one atomic configuration commit. Staging is an
in-memory cache with a two-minute expiry; an interrupted staging operation leaves the accepted
registry unchanged. A degraded runtime result does not roll back the durable desired profile. The
baseline for this implementation is current `main` plus the branch commits listed in the PR.

The camera loop rejects commands older than its accepted registry or the pending desired status.
An identical revision can be retried; changed definitions at that revision are rejected. It checks
for supersession between camera operations and before publishing completion, so an older operation
cannot replace a newer pending status. Recording-policy admission and worker creation hold the
status fence; publishing a newer pending revision waits for those synchronous admissions.
Notification acknowledgement holds the fence for up to its existing five-second timeout. A queued
notification update is cancelled when its acknowledgement times out or a newer update is admitted.
The notification worker checks that request before applying suppression. An already executing
notification update, claimed external delivery or camera worker stop is not preempted. These
bounds do not establish the complete asynchronous convergence or privacy acceptance matrix.

The application publishes loop-owned startup and profile-application results through the existing
status watch. Startup and activation batches each produce one completion update, so 127 camera
results do not consume 127 watch acknowledgements. Publication releases the status write lock
before calling the server and holds a read fence through queue admission. Status snapshots hold
the same fence through watch registration to prevent a missed update. Existing session authorization
and camera visibility projections apply to these notifications.

Saved targets absent from the configured cameras remain degraded with `configured camera is missing`
after startup, profile completion and live removal. Restoring the same saved identity replaces this
placeholder with its application result; a newly added camera has a different identity. Activating
Base clears inactive-profile placeholders.
The server uses persisted camera settings rather than its retained discovery catalog when projecting
completion or runtime failure. If settings cannot be read, it logs a warning and uses the catalog;
that fallback cannot establish which cameras were deleted. Older reconciliation cannot replace a
newer pending revision. The identity migration and IP reuse rules are described below.

Events-only camera edits refresh the same accepted camera definition that availability transitions
reuse. Returning to Base therefore retains edited event settings and generic-motion retention.
The events-only path keeps its existing media workers; it replaces event workers as before.

Recording configuration checks storage admission and command-queue acceptance. Invalid event
settings, event-camera capacity exhaustion, a full queue or a disconnected writer produce a bounded
per-camera Degraded reason. Other cameras still apply their availability changes, and recording
failure does not stop live camera startup. Each camera configures storage once per transition.
The loop reports Pending until the writer accepts the configuration. It enqueues recording changes
and applies availability before waiting against one 200 ms deadline for the whole batch. Startup
batches share that bound. No profile-status or admission lock is held while waiting. Rejection,
timeout or a disconnected acknowledgement turns new recording input Off only if that request is
still current. A newer request invalidates older tokens across every recording mode, so an old
timeout cannot disable a newer policy and a timed-out queued command cannot re-enable a source.
An identical accepted profile can be retried after recovery. The writer clears partial rejected
state and reclaims disabled event sources before configuration and on its existing 25 ms tick.
Already admitted output may remain until the writer processes the change. Applied acknowledges
writer configuration acceptance, not successful later disk writes or the privacy no-more-bytes
barrier. Existing revision fences protect newer Pending status during acknowledgement waits.

After acceptance, active recording input write failures are latched against the current
acknowledged configuration. The loop observes them at most every 100 ms when it can run, changes
only matching Applied recording-enabled cameras to Degraded, and publishes one status update for
the batch. Each poll examines at most 127 ordered status entries and resumes from its cursor on
the next poll, so larger fleets and retained missing targets do not create an unbounded scan.
Busy event delivery does not reset that interval. The reason is fixed and contains no
storage path, label or raw error. A newer Pending revision and other Degraded reasons are preserved.
An explicit successful reapply replaces the failure latch; it acknowledges configuration without
proving later media recovery. Ordinary write progress does not automatically promote the camera.
After storage repair, an Administrator can select **Reapply active policy** for the current Pending
or Degraded selection. Retry uses the same CAS activation, keeps the current selection, and requires
current identity bindings and a saved or discarded draft. A failed retry retains its error; a stale
edit retains the draft and its original revision until explicit discard/reload. An authorized
WebRTC client can also reapply the current policy, and restart restores the durable selection.

These observations cover direct input writes, buffered flushes triggered by input, named-storage
routing errors during input, and event-frame writes. Legacy buffers can include earlier media, so
the observation describes a write after acceptance rather than per-frame generation ownership.
Replacement cleanup/finalization, periodic FlushAll, shutdown, producer queues and global storage
pauses retain their existing diagnostics and are not covered by this profile-status path.

## Scope

An Administrator selects Base or exactly one named profile. Each profile contains sparse
overrides for at most 127 configured cameras. Omitted fields inherit the current Base value.
There are at most 32 profiles. Profile activation is available only through the existing
WebRTC `StateStoreCommand` path used by the UI. No new HTTP, MQTT, Home Assistant, CLI,
webhook, scheduler, or location activation path is permitted. The protected `api/` files
remain unchanged.

Profile definitions, their revisions, and the active desired generation must have one
durable authority in `config.toml`. Runtime observations are reconstructed after restart.
Provider credentials, addresses, URLs, secrets, detector settings, storage settings, and
arbitrary user metadata are not valid profile fields.

## Baseline readiness

| Boundary                                                                                | Current behavior                                                                                                                                                     | Remaining acceptance                                                                                                                |
| --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Durable state and watches                                                               | Config-backed CAS, bounded definition parts/staged writes, loop-owned coalesced status publication and registration fencing.                                         | Combined quota, multipart observed layout, retained memory and final transport performance.                                         |
| Camera identity                                                                         | Per-camera UUIDs, atomic legacy migration, explicit missing targets and source mapping at runtime boundaries.                                                        | Full cross-platform upgrade/restore qualification; ambiguous active source routing fails closed.                                    |
| Recording                                                                               | Admission, queue acceptance, writer acknowledgement and covered later input write failures report per-camera status. Same-policy recovery retains desired selection. | Cleanup/finalization, periodic FlushAll, shutdown, producer queues, storage-pressure failures and complete media recovery evidence. |
| Camera availability                                                                     | Loop-owned worker start/stop preserves Base definitions and accepted event edits.                                                                                    | Worker stop has no deadline; caller timeout does not preempt in-progress work.                                                      |
| Privacy                                                                                 | Source-translated profile inputs share the existing schedule/media gate; restrictive transitions retain prior denies on failure.                                     | Complete tagged-byte leak matrix across all media operations and failure paths.                                                     |
| Notifications                                                                           | Candidate suppression and pending-outbox cancellation preserve rule definitions. Queued updates are cancelled after timeout or supersession.                         | Already executing updates and claimed external deliveries are not preempted; complete delivery-race qualification remains.          |
| Browser                                                                                 | Settings manages profiles through the same WebRTC CAS operations; explicit retry and discard/reload protect current selection and drafts.                            | Final-build browser/restart and complete accessibility/performance evidence.                                                        |
| The issue's source plan predates the merged state store. At this baseline the generic   |
| limits are 64 KiB per encoded value, 64 entries/64 KiB per snapshot, 1,024 entries per  |
| namespace, 64 MiB total generic values, and 64 watches per session. The control-message |
| ceiling remains 64 KiB including its envelope. Profiles retain the smaller 16 KiB       |
| document and 48 KiB snapshot budgets proposed in #202 so envelopes have headroom.       |
| Settings-backed values must participate in an explicit combined quota accounting model; |
| the test adapter must not be assumed to consume the generic database quota.             |

## Camera identity

Each camera table stores a server-managed canonical UUID v4 in `profile_identity`. Camera edits
preserve it, including an IP change in the same saved entry. Deleting and re-adding a camera
creates a new UUID, so the replacement inherits Base and the old profile target remains missing.
Profiles do not lock camera deletion or ordinary editing. Existing camera controls, storage source
IDs and access checks retain their current identities; the profile resolver maps UUIDs to those
owners at their boundaries. No protected protobuf contract changes.

Startup assigns missing UUIDs and migrates legacy profile IP targets in one atomic configuration
write before media starts. A matching unambiguous current camera supplies its UUID. An absent
target becomes `missing-ip:<address>` and never binds to a future camera. Each changed definition
and the registry revision advance once. Invalid or duplicate UUIDs, exhausted revisions, target
collisions and ambiguous active routing fail without a partial write. Duplicate-IP entries not
used by the active profile remain outside the profile catalog.

Camera `ip` values can use a supported reference such as `{secret:CAMERA_IP}`. Migration,
the profile identity map and the actual camera definitions resolve against the same loaded
disk `secrets.toml` snapshot. The original reference remains in raw `config.toml`; migration
does not replace it with the resolved address. A changed secrets file does not alter an already
loaded configuration snapshot. The next configuration load uses the new address with the same
saved camera UUID.

Migrated `[runtime_profiles]` tables carry the persistence-only `identity_schema = 1` marker.
Earlier binaries reject this field before startup, rather than treating UUID targets as missing
IP overrides and running Base. Activation, registry edits and ordinary configuration rewrites
preserve the marker even after the last camera is removed. Unknown versions fail validation.
Rollback requires stopping the recorder and restoring a verified compatible configuration backup.
The marker is not part of the WebRTC profile documents.

The Administrator-only `cameras` Get uses `keeppeek.runtime-profiles.cameras.v1` with
`{ "cameras": { "<uuid>": "<current-ip>" } }`. It has a 16 KiB encoded-value ceiling and reports a
resource error rather than truncating a larger catalog. The editor refreshes bindings when its
camera list changes and preserves unsaved drafts. An omitted camera's override controls stay
disabled; Base and profiles for other cameras remain available. Registry/status camera keys are UUIDs; only
previously committed missing targets can survive later edits. No automatic replacement retargeting
or name, MAC, serial or UID inference occurs.

Before live camera changes, the server installs the restrictive union of previous and next privacy
inputs, bounded to 254 sources. It releases old inputs only after the camera start/restart or removal
has acknowledged the transition. Failure retains the restrictive state. Startup restores translated
profile privacy before media workers start; the schedule boundary still wins over profile `false`.

## Proposed document layout

All persisted revision and generation fields are decimal strings in `0..=i64::MAX` to
preserve exact values through protobuf `Struct`, JavaScript, and the existing TOML adapter.
Names are limited to 80 UTF-8 bytes, profile IDs to 1â€“128 ASCII bytes (`A-Z`, `a-z`, `0-9`,
`._-`), and camera IDs to 256 bytes. Profile IDs `.` and `..` are invalid. Use the valid
namespace `service/runtime-profiles/`; an unqualified namespace is rejected by the existing
state store. Reject control characters, unknown fields, unknown write schemas, duplicate cameras,
and newly introduced unresolved targets before committing. Preserve previously committed missing
targets so camera deletion or identity changes do not block later profile edits.

The server override schema accepts only availability, recording mode, privacy enforcement, and
notification suppression. A Rust boundary test exercises each accepted field and rejects
representative identity, credential, detector, storage, notification-rule, and arbitrary-metadata
fields. Current recording modes also include `event-only`, added after the issue's five-mode plan.
The complete field classification must still resolve each candidate explicitly; omitting it or
routing event modes through the legacy admission map would misrepresent current recording behavior.

| Document             | Fields and limits                                                                                                                                                                                                          |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `registry`           | Up to 32 summaries: ID, name, definition revision, target count. No camera overrides.                                                                                                                                      |
| Profile manifest     | ID, name, generation, expected prior definition revision, target count, and up to eight part hashes.                                                                                                                       |
| Definition part      | Generation, part index, and at most 16 camera overrides. Each override contains camera ID and optional availability, recording mode, privacy, and notification suppression fields.                                         |
| Draft parts          | At most four Administrator-owned drafts and 8 MiB total staged data. Each draft holds at most 256 parts of at most 16 camera overrides each. Drafts expire after two minutes and are never persisted as accepted profiles. |
| `active`             | Base or profile ID, definition generation, desired generation, and expected observed-part count.                                                                                                                           |
| Eight observed parts | Up to 16 cameras each, desired generation, pending/applied/degraded state, effective values, Base/profile provenance flags, bounded retry count, and error code of at most 96 bytes.                                       |

Retain current and previous definition parts, one current manifest per profile, and the
bounded draft area. The upper bound is 544 definition entries + 72 draft entries + 10
registry/active/observed entries = 626 entries. Actual protobuf sizes, including entry
metadata and enclosing messages, must be measured before registering this schema.

The full view uses ten narrow watches: registry, active, and eight observed parts. The
shell uses only the two header watches. Definition reads use at most four concurrent Gets
and two retries if generation collection races a read. A client assembles at most two
observed generations and never presents mixed generations as applied. A broad snapshot
must fail explicitly when it exceeds transport bounds; it is not paginated implicitly.

## Commit and runtime ordering

1. Authorize the Administrator, require an expected revision, validate the whole candidate,
   and resolve every target against current camera identity under the configuration owner.
2. Commit accepted definitions and active desired generation together through the existing
   full-candidate atomic configuration writer. Preserve unrelated tables and secret references.
   Staged uploads never change accepted definitions or runtime policy.
3. Publish all changed projections in revision order under the same serialization boundary
   used by watch registration. Failed persistence publishes no successful transition.
4. Apply privacy denial before camera or recording changes. Keep one latest desired
   generation per camera. A superseded completion cannot publish current observed state.
5. Report pending until every applicable domain acknowledges its generation. Failure is
   degraded with a bounded non-secret code; it never silently selects Base.
6. Restore the desired overlay before starting affected media workers. Establish the new
   permitted state before removing a previous privacy denial.

Users may read the active header and camera-visible observations, but cannot mutate profiles.
Watch delivery must enforce camera filtering and current authorization, not only registration
authorization. Definitions and draft content are Administrator-only.

## Verification and remaining work

The numbered implementation slices in #202 remain the task list. This document rebases
slice 1 against current code; it does not mark any acceptance criterion complete.

The synthetic [capacity fixture](../tests/runtime_profiles_capacity.rs) executes the proposed
shapes through the generated protobuf types. It measures 32 full profiles, 127 maximum-length
camera IDs, current/previous parts, eight staged drafts, and ten narrow watch snapshots.
It round-trips full Get/snapshot response envelopes, Put request envelopes, and watch-update
envelopes. It does not execute a profile parser, a state-store mutation, or a live watch.

Observed on Windows x64, debug build, baseline `26ef522` plus this test, on 2026-09-27:

| Serialized shape | Value bytes | Get envelope bytes | Single-entry snapshot envelope bytes |
| ---------------- | ----------: | -----------------: | -----------------------------------: |
| Registry         |       9,875 |             10,501 |                               10,931 |
| Manifest         |         903 |              1,529 |                                1,959 |
| Definition part  |       6,404 |              7,030 |                                7,460 |
| Observed part    |      10,692 |             11,318 |                               11,748 |
| Active header    |         265 |                891 |                                1,321 |

All 626 projected values occupy 3,791,324 encoded bytes. Their broad snapshot occupies
4,097,516 bytes and exceeds both snapshot limits. Each of the ten actual narrow prefixes
selects exactly one entry and fits the proposed snapshot budget. These results establish
standalone serialization headroom, not combined generic/settings quota admission or latency.
The in-memory staging cache is bounded independently of the generic StateStore quota. TOML file
size, live StateStore capacity, and staged-commit latency still require measurement.

Reproduce with `cargo test --locked -p keeppeek --test runtime_profiles_capacity -- --nocapture`.
Both tests passed; none were skipped. No production behavior or dependency changed.

The loop-completion regression decodes one status notification on each of two staged WebRTC API
sessions. On Windows x64 with Rust 1.99.0 in a debug build (2026-10-17), one empty-registry
completion took 458 microseconds and queued two frames totaling 418 bytes. These are local queue
and framing measurements, not encrypted transport or browser latency. A separate 127-disabled-camera
test verifies one publication per startup/activation batch and publisher restoration after a rejected
revision. A deterministic registration test verifies that status writes cannot pass between snapshot
creation and watch registration. Full live-camera and release-build latency qualification remains open.

Focused Clippy with `-D warnings`, repository Rust formatting, Markdown formatting, and
`mdbook build book` also passed. The protected `api/` tree has no diff; UI changes include runtime
profile client tests and the real-server two-client activation test. The full
runtime/browser gate and activation performance benchmark remain required for the feature;
the current server slice does not claim those results.

The new `secret_backed_camera_ip_uses_one_identity_and_camera_snapshot` regression covers
secret-backed IP migration, raw-reference preservation and consistent camera/identity resolution
before and after a companion secrets-file change. Execution is pending; the previously completed
test runs do not qualify this final compatibility change. Final canonical validation and PR
publication remain pending in [the lifecycle task](../tasks/runtime-profile-lifecycle.md).

Remaining work:

- Complete cross-platform identity upgrade/restore qualification, runtime-safe policy-input classification, combined settings quota and 16 KiB multipart registry/status projections. Missing active targets remain degraded through startup, completion and removal. The real 127-target degraded status fits the 64 KiB transport ceiling; this does not establish the smaller proposed multipart layout. Profile parts remain bounded projections of durable configuration, not independent durable StateStore entries.
- Extend staged-write qualification with complete supersession/failure/restart coverage. Real-owner lifecycle tests exercise persisted CAS activation, storage I/O failure, same-profile retry, Base restoration, IP edit and delete/re-add across fresh owners. They inject encoded media at storage ingress and check writer progress; they do not establish wire decoding or every media-operation privacy barrier. The draft cache is process-local and the final CAS commit remains configuration-file atomic.
- Verify the notification delivery race including already-claimed sends, privacy byte barriers across every media operation, Base changes, storage-worker failure, and Base-change reapplication.
- Finish final-build desktop/mobile browser qualification for visible-UI activation, recovery, multi-client conflicts, reconnect/restart, accessibility and ordered desired/applied state; verify camera-filtered watch updates.
- Run canonical Windows validation and the full release-build 30-activation benchmark.

Server acknowledgement plus watch publication must be at most 250 ms p95. Connected
browser desired state must appear within 500 ms p95 with no main-thread task over 50 ms.
Measure physical convergence separately. Keep #202 open until all nine original criteria
and their final-build evidence are complete.
