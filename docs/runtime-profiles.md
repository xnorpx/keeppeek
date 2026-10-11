# Runtime profiles implementation contract

This document tracks [#202](https://github.com/xnorpx/keeppeek/issues/202).
Runtime profiles are partially implemented. The server can persist and validate bounded
definitions and administrators can select a preconfigured profile or Base through the existing
WebRTC state-store command. Selection is revisioned and durable; profile privacy overrides feed
the existing media privacy gate. Profile CRUD, UI, recording and notification application,
per-camera convergence status, and full client watch projections remain incomplete. The baseline
for this implementation is current `main` plus the branch commits listed in the PR.

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

| Boundary                                       | Existing owner                                                                 | Required integration                                                                                                                                                                                                                                                                                            |
| ---------------------------------------------- | ------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Durable generic state and acknowledged watches | `src/server/state_store.rs`, `state_store_durable.rs`, `state_store_watch.rs`  | Reuse watch response barriers, acknowledgements, revocation, and reconnect behavior.                                                                                                                                                                                                                            |
| Settings-backed state                          | `src/server/state_store_settings.rs`                                           | The existing adapter implements only `service/state-store-test/` and its two-field test schema. It is a worked example, not a pluggable profile registry. Profile commits must atomically update multiple projected entries and publish them under the same configuration lock.                                 |
| Stable camera identity                         | `src/server/configuration.rs`, `src/config.rs`, `src/keeppeek.rs`              | Current configured IDs and runtime ownership use camera IP addresses. Resolve the identity/migration checkpoint before persisting long-lived profile references; #95 owns same-IP child/channel support.                                                                                                        |
| Recording mode                                 | `src/storage/engine/event.rs::StorageHandle::configure_camera_event_recording` | Apply through the current event-recording owner, preserving Base stream and pre/post settings. The older `configure_camera_recording` only updates the legacy path; active event recording can bypass an Off override sent there. Add acknowledged failure reporting and reapply the overlay when Base changes. |
| Camera availability                            | `src/server.rs::start_runtime_camera`, `src/keeppeek.rs`                       | Establish an acknowledged, bounded start/stop operation. Existing stop waits for workers without a deadline; caller timeout does not cancel queued work. Configuration-plan activation restarts cameras and is not a profile adapter.                                                                           |
| Privacy                                        | `src/privacy.rs::PrivacyRegistry`                                              | Add an internal profile input to the existing decision owner. Calling `PrivacyGate::activate` alone is insufficient: schedule evaluation can replace that decision.                                                                                                                                             |
| Notifications                                  | `src/notifications/orchestrator.rs`, `src/notifications/delivery.rs`           | Enforce the profile input at candidate admission and queued delivery without editing notification rules.                                                                                                                                                                                                        |

The issue's source plan predates the merged state store. At this baseline the generic
limits are 64 KiB per encoded value, 64 entries/64 KiB per snapshot, 1,024 entries per
namespace, 64 MiB total generic values, and 64 watches per session. The control-message
ceiling remains 64 KiB including its envelope. Profiles retain the smaller 16 KiB
document and 48 KiB snapshot budgets proposed in #202 so envelopes have headroom.
Settings-backed values must participate in an explicit combined quota accounting model;
the test adapter must not be assumed to consume the generic database quota.

## Camera-reference decision

#202 requires resolution of camera-ID migration before persisted profile mutations are enabled.
Completing all of #95 is not inherently necessary. A compatible current-ID design can bind each
accepted target to both its advertised ID and its existing canonical configuration/storage key.
Store that binding once as internal integrity metadata, not as another editable identity registry.

That design must reject deletion, rename, IP reassignment, or ambiguous replacement while an
accepted profile references the camera. It must validate bindings before startup and restore
start media workers, and fail closed on manual configuration mismatches. Normal Base edits that
preserve identity remain valid. #95 must explicitly migrate these references when its stable
logical-ID migration lands. Binding metadata needs a separate measured persistence budget.

The owner rejected this camera-editing restriction on 2026-09-27. Profiles must not prevent
deletion or identity changes for referenced cameras. Runtime implementation waits for the
camera-identity migration decision in #95; the serialization fixture below is independent of it.

## Proposed document layout

All persisted revision and generation fields are decimal strings in `0..=i64::MAX` to
preserve exact values through protobuf `Struct`, JavaScript, and the existing TOML adapter.
Names are limited to 80 UTF-8 bytes, profile IDs to 1–128 ASCII bytes (`A-Z`, `a-z`, `0-9`,
`._-`), and camera IDs to 256 bytes. Profile IDs `.` and `..` are invalid. Use the valid
namespace `service/runtime-profiles/`; an unqualified namespace is rejected by the existing
state store. Reject control characters, unknown fields,
unknown write schemas, duplicate cameras, and unresolved targets before committing.

Current recording modes also include `event-only`, added after the issue's five-mode plan.
The final field matrix must resolve its eligibility explicitly; omitting it or routing event
modes through the legacy admission map would misrepresent current recording behavior.

| Document             | Fields and limits                                                                                                                                                                    |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `registry`           | Up to 32 summaries: ID, name, definition revision, target count. No camera overrides.                                                                                                |
| Profile manifest     | ID, name, generation, expected prior definition revision, target count, and up to eight part hashes.                                                                                 |
| Definition part      | Generation, part index, and at most 16 camera overrides. Each override contains camera ID and optional availability, recording mode, privacy, and notification suppression fields.   |
| Draft manifest/parts | At most eight concurrent drafts, each with one manifest and eight parts. Expire drafts after a fixed bounded interval without removing accepted generations.                         |
| `active`             | Base or profile ID, definition generation, desired generation, and expected observed-part count.                                                                                     |
| Eight observed parts | Up to 16 cameras each, desired generation, pending/applied/degraded state, effective values, Base/profile provenance flags, bounded retry count, and error code of at most 96 bytes. |

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
Internal identity bindings, staged-expiry bookkeeping, and TOML file overhead still require
measurement when their implementation is defined.

Reproduce with `cargo test --locked -p keeppeek --test runtime_profiles_capacity -- --nocapture`.
Both tests passed; none were skipped. No production behavior or dependency changed.

Focused Clippy with `-D warnings`, repository Rust formatting, Markdown formatting, and
`mdbook build book` also passed. The protected API and UI trees have no diff. The full
runtime/browser gate and activation performance benchmark remain required for the feature;
this serialization-only slice does not claim those results.

Remaining work:

- Resolve identity, runtime-safe field classification, settings quota, and watch filtering.
- Implement and test staged CAS commits, restart recovery, and unchanged state after failure.
- Verify all runtime domains, supersession, privacy byte barriers, and Base changes.
- Add UI CRUD, shell status, provenance, external-client updates, and reconnect tests.
- Run canonical Windows validation and the release-build 30-activation benchmark.

Server acknowledgement plus watch publication must be at most 250 ms p95. Connected
browser desired state must appear within 500 ms p95 with no main-thread task over 50 ms.
Measure physical convergence separately. Keep #202 open until all nine original criteria
and their final-build evidence are complete.
