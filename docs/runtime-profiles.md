# Runtime profiles implementation contract

This document tracks [#202](https://github.com/xnorpx/keeppeek/issues/202).
Administrators can create, rename, edit, delete, and activate bounded persistent runtime profiles
through the existing WebRTC `StateStoreCommand` interface and the Settings UI. Selection is
revisioned and durable in `config.toml`; privacy overrides feed the server `PrivacyRegistry` media
gate; and recording, camera availability, and notification suppression converge asynchronously
through their runtime owners. The shell and Settings surfaces watch active-profile headers and
camera-scoped observed status across eight bounded parts. Profile targets and observations use
persistent camera UUIDs; the Administrator-only camera catalog maps them to current source IPs for
the editor. Reads return bounded profile parts, and writes stage bounded Administrator-owned parts
before one atomic configuration commit. Staging is an in-memory cache with a two-minute expiry; an
interrupted staging operation leaves the accepted registry unchanged. A degraded runtime result does
not roll back the durable desired profile.

The camera loop rejects commands older than its accepted registry or the pending desired status.
An identical revision can be retried; changed definitions at that revision are rejected. It checks
for supersession between camera operations and before publishing completion, so an older operation
cannot replace a newer pending status. Recording-policy admission and worker creation hold the
status fence; publishing a newer pending revision waits for those synchronous admissions.
Notification acknowledgement holds the fence for up to its existing five-second timeout. A queued
notification update is cancelled when its acknowledgement times out or a newer update is admitted.
The notification worker checks that request before applying suppression. Notifications already
in delivery continue under the delivery worker's existing timeout.

The camera loop gives all camera stops in one profile transition a shared six-second deadline.
Cancellation starts for every affected camera. A worker still running at the deadline reports a
Degraded camera. The recorder retains its thread handle, refuses a replacement at the same IP,
and joins it after it exits. Reapply the selected profile to refresh the per-camera result.
An event-only edit keeps media running if its previous event worker takes too long to stop.
The privacy gate continues to enforce the accepted policy throughout retirement.
New media starts pause if 127 cancelled groups are still running. Disabled definitions can
still be accepted. In the Windows debug fixture, four blocked camera threads shared one
6.012-second stop interval. Each camera reported Degraded. Releasing the four threads let
the recorder join them all. This fixture measures worker retirement, independently of the
release-build WebRTC acknowledgement and browser latency requirements.

The application publishes loop-owned startup and profile-application results through the existing
status watch. Startup and activation batches each produce one completion update, so 127 camera
results do not consume 127 watch acknowledgements. Publication releases the status write lock
before calling the server and holds a read fence through queue admission. Status snapshots hold
the same fence through watch registration to prevent a missed update. Existing session authorization
and camera visibility projections apply to these notifications.

A camera configured without credentials stays Degraded at startup and after a profile change.
Other configured cameras remain Pending until the recorder attempts to start them. During a
header interruption the shell and Settings show Pending until they receive fresh selection data.
Camera inventory changes, scoped watch registration and publication are serialized so a newly
visible camera reaches existing viewers and newly opened watches.

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

## Implementation readiness

| Boundary                  | Current behavior                                                                                                                                                                            | Remaining external verification                                                   |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Durable state and watches | Config-backed CAS, compact v3 registry, bounded definition parts, eight observed status parts, camera-scoped watches, and release-build 127-camera/32-profile benchmark.                    | None in software.                                                                 |
| Camera identity           | Per-camera UUIDs, atomic legacy migration, `identity_schema = 1` rollback guard, secret-IP snapshot consistency, explicit missing targets, and config backup/restore preservation.          | None in software.                                                                 |
| Recording                 | Admission, queue acceptance, writer acknowledgement, covered active input write failures, and same-policy recovery across all six recording modes (`off` through `event-only`).             | Physical hardware camera soak testing remains outside deterministic software CI.  |
| Camera availability       | Asynchronous durable acknowledgement, bounded six-second shared stop deadline, retained stalled worker handles, and Base restoration preserving accepted event edits.                       | Physical hardware camera disconnect/recovery remains outside deterministic CI.    |
| Privacy                   | Source-translated profile inputs share `PrivacyRegistry`, fail closed during transition/restart, guard in-flight snapshots by epoch, and cover live/recording/event/export/PTZ/group gates. | None in software.                                                                 |
| Notifications             | Candidate suppression and pending-outbox cancellation preserve rule definitions; timed-out or superseded queued updates are cancelled before applying.                                      | Already claimed external deliveries continue under the delivery worker's timeout. |
| Browser                   | Responsive desktop and mobile Settings CRUD, conflict-safe drafts, explicit reapply, shell active/degraded status, reconnect renegotiation, and keyboard/ARIA controls.                     | None in software.                                                                 |

The generic state store limits are 64 KiB per encoded value, 64 entries/64 KiB per snapshot, 1,024
entries per namespace, 64 MiB total generic values, and 64 watches per session. The control-message
ceiling remains 64 KiB including its envelope. Profiles retain the smaller 16 KiB document and
48 KiB snapshot budgets proposed in #202 so envelopes have headroom. Profile parts are bounded
projections of `config.toml`, and the in-memory staging cache is bounded independently (at most 4
drafts, 8 MiB total, two-minute TTL).

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
Names are limited to 80 UTF-8 bytes, profile IDs to 1–128 ASCII bytes (`A-Z`, `a-z`, `0-9`,
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

| Document             | Fields and limits                                                                                                                                                                                                                                  |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `registry`           | Up to 32 summaries: ID, name, definition revision, target count. No camera overrides.                                                                                                                                                              |
| Profile manifest     | ID, name, generation, expected prior definition revision, target count, and up to eight part hashes.                                                                                                                                               |
| Definition part      | Generation, part index, and at most 16 camera overrides. Each override contains camera ID and optional availability, recording mode, privacy, and notification suppression fields.                                                                 |
| Draft parts          | At most four Administrator-owned drafts and 8 MiB total staged data. Each draft holds at most 256 parts of at most 16 camera overrides each. Drafts expire after two minutes and are never persisted as accepted profiles.                         |
| `active`             | `profile_id` (empty for Base), `revision`, `definition_revision`, `observed_parts`, and optional scoped `degraded_cameras`. The entry revision is the compare-and-set value.                                                                       |
| Eight observed parts | Up to 48 visible cameras per part (383 total), desired revision, application state, effective policy, field provenance, and a reason of at most 96 bytes. Compact parts with 33–40 rows allow 64 reason bytes, and parts with 41–48 rows allow 24. |

Retain current and previous definition parts, one current manifest per profile, and the
bounded draft area. The upper bound is 544 definition entries + 72 draft entries + 10
registry/active/observed entries = 626 entries. Actual protobuf sizes, including entry
metadata and enclosing messages, must be measured before registering this schema.

The full view uses ten narrow watches: registry, active, and eight observed parts. The
shell shares one active-header watch with Settings and receives a camera-filtered degraded count. Definition reads use at most four concurrent Gets
and two retries if generation collection races a read. A client assembles at most two
observed generations and never presents mixed generations as applied. The keys `observed/0` through `observed/7` carry opaque generation identifiers which can
change as workers converge at the same desired revision. For Administrators, an empty watch
prefix selects only the `active` entry and is echoed as empty in the initial snapshot. Other
viewers must use an explicit permitted key and receive `NOT_AUTHORIZED` for an empty prefix.
Existing clients can read or watch the legacy `status` projection while it fits 16 KiB. A larger
legacy projection returns `VALUE_TOO_LARGE` on Get or initial Watch.

Up to 256 configured cameras and 127 retained missing targets use the same eight observed keys.
The recorder filters to the viewer's permitted cameras before hashing or assigning rows to a
part. The ordinary fleet uses 16 rows per part. A larger union uses up to 48. When a part's
full form exceeds 16 KiB, `keeppeek.runtime-profiles.observed.v2` encodes each camera as
`[state, recording_mode, flags, reason]` under its camera ID. In ascending bit order, flags
carry availability, privacy, notifications and the four profile-provenance flags for
availability, recording mode, privacy and notifications. A cleared provenance bit means Base.

A watch snapshot and its update envelopes use a namespace cursor that advances even for a
health-only change. The desired revision in the document and entry remains the CAS value.
A hidden-only change leaves the visible documents and subscriptions untouched. An unknown
visibility policy yields an unavailable degraded count. Settings shows Pending while parts
are changing and keeps profile management available during convergence.

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

## Verification status

The software acceptance suite for #202 covers:

- `tests/runtime_profiles_capacity.rs`: worst-case 32-profile / 127-camera wire framing budgets
  (<=16 KiB documents, <=48 KiB narrow watch snapshots, 626 entries, 10 narrow watches).
- `tests/runtime_profiles.rs`: runtime-safe field allow/deny matrix across all six recording modes,
  registry bounds and active-deletion denial, `identity_schema` unknown-version fail-closed check,
  config-only backup/restore preservation, and static non-WebRTC activation-surface isolation.
- `src/profiles/tests.rs`, `src/server/state_store_profiles.rs`,
  `src/server/state_store_profiles_application.rs`,
  `src/server/state_store_profiles_lifecycle_tests.rs`, `src/camera_events/snapshot/tests.rs`,
  `src/keeppeek/tests/profile_identity.rs`, `src/keeppeek/tests/profile_workers.rs`,
  `src/notifications/orchestrator.rs`: staged CAS commits, concurrent activation races, scoped
  viewer watches, structured audit redaction, bounded stop deadlines, writer failure/reapply,
  in-flight snapshot epoch guarding, and the full privacy media-leak matrix across live media,
  recording, snapshots, event publication, event subscriptions, exports, PTZ controls, groups, and
  restarts.
- `ui/src/lib/runtime-profiles.spec.ts`, `ui/src/lib/control-client-runtime-profiles.spec.ts`,
  `ui/src/lib/components/RuntimeProfilesSection.svelte.spec.ts`, `ui/e2e/runtime-profiles.e2e.ts`:
  desktop and 390x844 mobile CRUD, keyboard operation, two-client conflict and live updates,
  external WebRTC activation, reconnect renegotiation, and isolated recorder restart.
- `ui/e2e/runtime-profiles.performance.ts` (`bun run perf:runtime-profiles` under `ui/`): release
  benchmark over 30 sequential activations with 127 cameras and 32 sparse profiles, verifying
  durable acknowledgement + watch <=250 ms p95, connected UI desired revision <=500 ms p95, zero
  browser long tasks >50 ms, and separate recorder-observed camera convergence.

Physical hardware camera testing remains external to deterministic software verification.

### Selecting a displayed definition

An Administrator reads the registry manifest and the selected definition before activating
a named profile. A recorder advertising `keeppeek.runtime-profiles.select.v2` in the
manifest's `activation_schema` accepts that schema in a StateStore Put to the existing
`active` key. Supply the active entry's expected revision, the selected `profile_id`
and the displayed `definition_revision` as a decimal string, plus its SHA-256 `definition_digest`.
The recorder checks both against the current definition, including after deletion and recreation. A conflict requires
refreshing the selection. Base uses the active revision alone. Older recorders compare
the shared registry revision. Settings re-reads the selected digest before sending a legacy request
with the active revision the operator has observed.

### Audit evidence

The recorder's structured `access_audit` log includes the accepted registry revision for
profile changes. For each created, edited, deleted, or selected definition it also includes
the machine ID, definition revision and number of explicit camera overrides. A deletion
records the last accepted definition. Selecting Base records zero explicit overrides.
Rejected writes record the current registry revision and a bounded outcome. Display names,
request bodies and per-camera settings are omitted. The existing access-audit list retains
its standard action, target and result fields. The first part of a draft records its staging start.
The final commit records the accepted definitions. The list keeps up to 64 transient profile
outcomes and combines equivalent outcomes for an actor within one minute. Individual rejected
requests still appear in the structured trace.

### Registry capacity

A registry summary that would exceed 16 KiB uses the compact
`keeppeek.runtime-profiles.registry.v3` schema. Each map key supplies the profile ID once.
All names, definition revisions, part counts, target counts and digests are preserved.
For large staged commits, the Administrator uses the same compact metadata with the existing
StateStore Put. The server reconstructs each ID and validates all staged parts before
persisting the registry. Smaller registries continue to use the v2 representation.
Saving an edit preserves the definition revisions of profiles whose content is unchanged.
