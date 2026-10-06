# Recording policy audit

This is the working evidence ledger for [#168](https://github.com/xnorpx/keeppeek/issues/168).
It records implemented behavior and gaps; it does not approve a new retention policy or certify
the Alpha candidate. The initial audit used `872defab438a3d69c0c57fc3b48907fe818b8afd` on
2026-10-05. The outcome matrix below includes the runtime implementation through `25201d9`
and its subsequent CI-only upstream merge. Historical reports retain their recorded source
versions; current qualification and its exact provenance are recorded below.
The four classifications are **Equivalent**, **Partial**, **Gap**, and **Intentional divergence**.
An equivalent outcome requires behavior evidence; a source symbol alone is insufficient.

## Reference and coverage

Reviewed the live [Frigate recording page](https://docs.frigate.video/configuration/record/)
on 2026-10-05. Its source file is `docs/docs/configuration/record.md`; the latest file-specific
commit returned by GitHub was
[`82be9fff5e80ddc9f48a7117a9fb69ae6633943a`](https://github.com/blakeblackshear/frigate/blob/82be9fff5e80ddc9f48a7117a9fb69ae6633943a/docs/docs/configuration/record.md).
That source revision identifies the reference, not a tested Frigate executable version or proof
that the deployment serves exactly those bytes. No Frigate runtime comparison was performed.

The inventory covers the introduction, three example configurations, capture windows and their
retention interaction/display, duration rules, recording control, export options/fallback,
codec compatibility, reconciliation, accounting, mounts/cache, and emergency cleanup.
Each row has one implementation/evidence owner. Related features may contribute tests without
becoming a second owner. The retention contract below is approved; final classification review
remains pending. Runtime qualification and the completed canonical gate are recorded
below; maintainer review must be complete before issue closure.

## Outcome matrix

Evidence marked **historical pass** is from the linked merged PR/report, not a new test run.
**Source only** and **missing** do not establish acceptance. Supported behavior means the
audited KeepPeek commit, within the codec/filesystem/browser limits of the linked reports.

| ID  | Reference topic             | Class                  | KeepPeek input/output and authoritative source                                                                                                        | Evidence and limitation                                                                                                                                                                                                                             | Owner |
| --- | --------------------------- | ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- |
| R01 | Recording enablement        | Equivalent             | `off` admits no media; `sub`, `main`, and `both` select their configured streams. `CameraRecordingPolicy::decide`, `src/storage/recording_policy.rs`. | `storage::engine::tests::recording_admission_enforces_modes_and_keyframe_aligned_event_boost`; historical pass in #268. This is admission, not a retention schedule.                                                                                | #168  |
| R02 | Encoded media               | Equivalent             | Original H.264/H.265 samples become indexed MP4 media without video transcoding. `storage::engine`, `MediumTermWriter`.                               | `event_boost_round_trips_h264_h265_h264_with_audio_and_catalog` in `src/storage/engine.rs`; historical pass in #268. Unsupported browser decoding remains R20.                                                                                      | #168  |
| R03 | UTC layout                  | Equivalent             | A timestamp selects camera/date/hour storage paths rather than the reference's directory ordering. `layout::segment_path`.                            | `src/storage/layout.rs::tests::segment_path_format`; source fixture. Equivalent time addressing does not promise identical paths.                                                                                                                   | #168  |
| R04 | Conservative example        | Equivalent             | Continuous rules retain admitted whole files until their committed deadline; holds and prior floors dominate shortening.                              | Accepted interval/byte fixtures in `tests/recording_retention_catalog` and decoded H.264/H.265 fixtures in `tests/recording_retention_media`; whole-file over-retention is owner-approved.                                                          | #168  |
| R05 | Reduced-storage example     | Equivalent             | Continuous and canonical motion evidence select independent configured lifetimes for the same finalized file.                                         | Accepted policy examples and exact UTC/deadline/byte fixtures; real-media missing-evidence cases retain only actual canonical evidence. This does not certify detector-service availability.                                                        | #168  |
| R06 | Alerts-only example         | Equivalent             | Exact canonical event-type selectors retain matching files; zero disables a rule and absent settings inherit.                                         | Approved canonical selector contract and catalog/media fixtures. This equivalence covers the accepted lifetime outcome, not Frigate alert classification or separate capture windows (R07).                                                         | #168  |
| R07 | Class-specific capture      | Partial                | Camera-level pre-history and post-event duration apply to accepted events.                                                                            | [Pre-recording verification](pre-recording-verification.md), #268. Separate alert/detection windows are not implemented.                                                                                                                            | #172  |
| R08 | Capture eligibility         | Partial                | Retained keyframes, media availability, deadline, decoder epoch and byte budget bound selected media.                                                 | `event_preroll_h264_is_independently_decodable`, `event_preroll_h265_is_independently_decodable`; historical pass. No `active_objects` retention eligibility model.                                                                                 | #168  |
| R09 | Capture display             | Partial                | Timeline coverage and health show available media/shortening reasons rather than implying requested footage exists.                                   | #268 desktop/mobile screenshots; `book/src/recording-and-evidence.md`. An event's displayed bounds do not certify all surrounding retained coverage.                                                                                                | #172  |
| R10 | Continuous/motion durations | Equivalent             | Checked global/camera durations resolve continuous and canonical motion deadlines independently. `src/storage/retention`.                             | Catalog and settings tests cover inheritance, zero, sub-day durations and committed floors; actual encoded files remain independently decodable. Whole-file granularity is approved.                                                                | #168  |
| R11 | Object/event durations      | Equivalent             | Accepted exact canonical event-type selectors map matching evidence to durable file deadlines.                                                        | Catalog/media tests cover person-only, motion-only, absent, late and revised evidence. No fabricated detector evidence or unsupported Frigate classification is implied.                                                                            | #168  |
| R12 | Maximum deadline            | Equivalent             | Latest matching deadline wins; holds and previously committed floors survive shortening, zero-day activation and restart.                             | Overlap/revision/restart catalog fixtures and genuine pre-runtime cold-schema migration preserve deadlines, identities and media bytes.                                                                                                             | #168  |
| R13 | Fractional durations        | Equivalent             | Checked durations support precise sub-day expiry over half-open UTC intervals.                                                                        | Resolver/settings/catalog tests cover fractional, zero and invalid durations and shared boundaries. Whole-file over-retention remains explicit.                                                                                                     | #168  |
| R14 | Overlap deduplication       | Equivalent             | Overlapping evidence updates one recording obligation without duplicating encoded media.                                                              | Catalog overlap fixtures and real H.264/H.265 retained-byte/decoded-coverage checks supplement #268 admission evidence.                                                                                                                             | #168  |
| R15 | Runtime schedules/control   | Partial                | Configured modes and server privacy schedules exist. Named profile activation remains open in #202.                                                   | #265/#125 and `src/storage/engine/event/tests.rs::worker_rejects_queued_media_after_a_complete_privacy_cycle`; historical pass. No approved arbitrary scheduler/control state table.                                                                | #168  |
| R16 | Range/event exports         | Equivalent             | Administrator export produces a durable searchable job with cancellation/failure handling.                                                            | #189/#113; `src/storage/playback.rs::tests::cancelled_export_removes_partial_file_and_can_retry`; historical CI. Container/codec limits remain documented.                                                                                          | #113  |
| R17 | Export survival             | Partial                | Protected catalog media is excluded from ordinary cleanup; exports have a separate job/artifact lifecycle.                                            | `cleanup_candidates_exclude_active_and_protected_recordings`, `src/storage/catalog.rs`; source fixture. Do not infer indefinite exported-file preservation from source protection.                                                                  | #113  |
| R18 | Custom export/transcode     | Gap                    | Indexed fragment assembly/remux exists; it does not expose arbitrary encoding arguments or a CPU transcode retry.                                     | `export_fragment_ranges_with_progress`, `browser_compatible_recording`; source only. Scope/divergence decision required; no new endpoint is authorized.                                                                                             | #168  |
| R19 | Timelapse                   | Gap                    | Bounded timelapse review/export remains open.                                                                                                         | #127 acceptance contract; ordinary playback speed/export is not a timelapse renderer.                                                                                                                                                               | #127  |
| R20 | Apple/H.265 playback        | Partial                | Browser compatibility selection and audio-timescale repair exist; no universal H.265 transcode fallback.                                              | `compatibility_remux_repairs_audio_timescale_and_is_cached`, #161/#111; #131 remains open. Remux does not change video codec.                                                                                                                       | #131  |
| R21 | Media reconciliation        | Partial                | Explicit owned reports/remedies and recoverable deletion jobs exist.                                                                                  | #232/#233/#133; `reconciliation_protocol_requires_owned_reports_and_explicit_remedies`. [Maintenance limits](../book/src/recording-maintenance.md) retain trust/filesystem/recovery qualification limits.                                           | #133  |
| R22 | Recording accounting        | Equivalent             | Catalog-attributed bytes and filesystem free capacity are distinct observations.                                                                      | #190/#122, `coverage_snapshot_retains_cleanup_evidence_after_restart`, `StorageSafetyPolicy::evaluate`; historical CI. Attribution is not a complete disk scan.                                                                                     | #122  |
| R23 | Other disk consumers        | Equivalent             | Non-KeepPeek usage reduces actual headroom without being attributed to recording bytes.                                                               | `src/storage/safety.rs::tests::reserve_accounts_for_non_keeppeek_disk_usage`; source fixture. Missing/stale capacity remains an explicit observation.                                                                                               | #112  |
| R24 | Mount identity              | Partial                | Named-volume placement/ownership from merged PR #269 supplements nearest-existing-parent capacity queries.                                            | #129 owns remaining platform/root recovery qualification. Capacity and catalog identity do not by themselves prove the intended external volume is mounted.                                                                                         | #129  |
| R25 | Cache boundary              | Partial                | `ShortTermBuffer` and bounded encoded pre-history are separate from durable writer/catalog progress.                                                  | #268 budget/pressure tests; `src/storage/short_term.rs`, `src/storage/event_recording.rs`. No tmpfs layout is required or implemented as a parity feature.                                                                                          | #168  |
| R26 | Stale disk metrics          | Partial                | Reconciliation is explicit; startup preserves interrupted evidence.                                                                                   | `startup_preserves_interrupted_recording_for_explicit_reconciliation`, #233. Arbitrary external deletion does not instantly update catalog attribution.                                                                                             | #133  |
| R27 | Emergency cleanup           | Intentional divergence | Oldest eligible finalized catalog files are removed to a recovery target; protected/active files remain excluded and recording can pause.             | `startup_cleanup_removes_only_oldest_catalog_media_to_recovery_target`, `cleanup_pauses_recording_when_no_eligible_media_remains`; source fixtures. The maintainer approved preserving protected evidence and pausing under unrecoverable pressure. | #112  |

No row is marked Intentional divergence solely because current code differs. The owner must
accept the consequence and workaround before that classification replaces Partial or Gap.

## Acceptance ledger

| Original criterion            | Current evidence                                                                                                                                         | Remaining closure evidence                                                                                                                           |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| AC-1 reviewed complete matrix | R01?R27 now reflect the runtime implementation, with scope, evidence and one owner per row.                                                              | Maintainer review/date and final reference-heading review.                                                                                           |
| AC-2 accepted policy examples | Conservative, reduced-storage and exact-event examples have UTC/deadline/byte catalog fixtures and real-media evidence.                                  | Maintainer review of the approved canonical selector and whole-file contract mapping.                                                                |
| AC-3 deterministic expiry     | Overlap, fractional/zero durations, revisions, late events, restart and genuine old-schema migration preserve committed floors.                          | Maintainer acceptance review.                                                                                                                        |
| AC-4 decodable event coverage | Exact case mapping below combines #268 pre/post evidence with current decoded H.264/H.265, missing-media/evidence and late/revised/overlap fixtures.     | Maintainer review; detector-service availability is not claimed.                                                                                     |
| AC-5 authoritative control    | Configured-disabled/privacy fencing and retained-file obligations have separate authorities; effective-control boundary below records precedence.        | #202 owns persistent profile activation and source/reason/expiry/state API/UI qualification. No generic external control interface is approved here. |
| AC-6 related feature evidence | Linked-owner table records evidence and remaining limitations; #127/#131 remain open with their original Alpha scope.                                    | Maintainer review of the linked-owner evidence and limitations.                                                                                      |
| AC-7 scale bound              | Release 127-source/30-day samples meet approved latency/RSS/ingest limits; paired native SQL profiles report compilations and program starts separately. | Maintainer review of the recorded environments/scope; SQL diagnostic timings are not latency acceptance evidence.                                    |

### Linked-owner review on 2026-10-05

The original owners retain their acceptance scope. Closed tracker state does not
replace the qualification evidence or imply support outside documented limits.

| Owner            | Observed evidence                                                                                                                                                                                                                                          | Remaining limitation and owner                                                                                                                                                                                                                             |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #112 safety      | Merged [#163](https://github.com/xnorpx/keeppeek/pull/163), merge `318467fa94f7b87d38b6034057ad19d750522614`; its eight-row acceptance table and final `e9a7749` gate report cover thresholds, editing, protected cleanup, recovery and health.            | Its issue still has unchecked historical checklist items. Current-candidate disk/filesystem and recovery qualification remains #145.                                                                                                                       |
| #113 exports     | Merged [#189](https://github.com/xnorpx/keeppeek/pull/189), merge `2c96b9672679c9f267fce1113cf076a1f8e1d597`; #113's acceptance and completion checklist is checked.                                                                                       | Export history does not establish #127 timelapse or #131 adaptive playback.                                                                                                                                                                                |
| #127 timelapse   | Open; its sampling, provenance, cancellation and export acceptance is unchecked.                                                                                                                                                                           | #127 owns this gap in Alpha; no implementation is duplicated here.                                                                                                                                                                                         |
| #131 adaptation  | Open; its aligned-variant, hysteresis, decoder and resource acceptance is unchecked.                                                                                                                                                                       | #131 owns this gap in Alpha; browser compatibility alone does not establish adaptation.                                                                                                                                                                    |
| #133 maintenance | Merged #232/#233 and later [PR automation](https://github.com/xnorpx/keeppeek/actions/runs/34722005032) and [main automation](https://github.com/xnorpx/keeppeek/actions/runs/34722240086), both successful at `4df9a94e2a09fbc02346af363ea79f6b8027d8a2`. | The issue's historical checklist and older draft narrative are not a completed qualification ledger. The maintenance book records scope, relationships, native/filesystem trust boundaries and recovery limits; #145 owns current-candidate qualification. |

### Effective-control boundary

| Precedence                      | Existing authority                                       | Accepted result and outstanding work                                                                                                                                                                             |
| ------------------------------- | -------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Configured recording permission | `CameraRecordingPolicy::decide`                          | Configured Off denies media admission. A later runtime request cannot grant permission.                                                                                                                          |
| Privacy                         | `PrivacyRegistry` and storage admission fencing          | Active privacy denies admission and clears reusable pre-history. #125 supplies the existing fail-closed schedule foundation.                                                                                     |
| Permitted mode selection        | Configured camera mode and #202's profile owner          | Current configured selection works. Persistent profile activation, source/reason/expiry/current-state exposure and its API/UI qualification remain #202; this PR adds no generic scheduler or external override. |
| Retained-file eligibility       | Current holds, retention commitments and deletion owners | Holds dominate expiry. Policy changes preserve prior committed floors, and automatic cleanup must pass current authority and owner checks. This governs finalized media, not permission to record new media.     |

## Approved retention contract

The maintainer approved these product semantics on 2026-10-05. The draft runtime implementation
activates optional persisted retention settings through the existing restart workflow.
Size-pressure cleanup preserves committed deadlines; cameras without an applicable policy
retain the existing cleanup behavior.
Protected `api/` changes require separate current-task approval.

1. Keep #168 as the retention-policy owner. Use bounded camera-local rules in `config.toml`
   with checked integer durations and stable canonical-event predicates. Preserve old configs
   by leaving the new policy disabled by default. The resolver enforces a ceiling of 16 rules
   per camera and 128 ASCII identifier bytes per rule ID or event-type selector.
2. Use half-open UTC intervals. A shared boundary alone is not an overlap. The latest matching
   deadline wins; evidence holds dominate policy expiry. An unknown/unavailable event class
   cannot fabricate motion/object evidence. Zero disables a rule; absent configuration inherits.
3. Preserve whole independently decodable files when different retained intervals share an MP4.
   Report file-granularity over-retention. Exact retained bytes require separately approved
   immutable compaction; do not delete ranges from the middle of existing files.
4. Record policy/event revisions and deadlines durably. Preserve a previously committed deadline
   on policy shortening until an explicit migration/remedy decision authorizes shortening.
   Bound reevaluation batches and keep ingest independent of cleanup.
5. Control precedence: configured-disabled and privacy deny admission first; a permitted
   runtime request then selects its mode. #202 owns profile activation. A generic scheduler or
   external recording-toggle feature has no accepted interface here; choose its owner/scope
   before implementing one. Do not overload state-store documents to bypass API approval.
6. The owner accepted the proposed Windows limits on 2026-10-05: p95 at most
   250 ms per eight-item reconciliation call, 1 second per complete single-camera
   late-event reconciliation, sampled evaluation RSS at most 256 MiB, and
   at most 130 indexed event seeks and 257 candidates per file. A 240-frame,
   16-file ingest-and-flush sample must have p95 at most 800 ms and at most
   30% increase over its matching baseline. Measure the 127-source/30-day
   release fixture and retain raw samples. Indexed seeks are not total SQL
   statements; total query-count qualification remains separate.

### Interval fixtures and remaining integration

These are KeepPeek fixtures, not copies of external configuration.
All times are UTC offsets from a fixed epoch and all intervals are half-open.

| Case          | Input                                                                    | Accepted behavior                                                                                              |
| ------------- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| Overlap       | File coverage `[0s, 10s)`, two matching rules with deadlines 12h and 48h | One media identity, latest deadline 48h; no duplicate file.                                                    |
| Boundary      | File `[0s, 10s)`, event `[10s, 20s)`                                     | No event overlap; continuous rule alone may retain the file.                                                   |
| Disabled rule | Continuous duration zero, no matching event predicate                    | No policy retention; protect/hold precedence still applies.                                                    |
| Mixed file    | File `[0s, 20s)`, selected interval `[5s, 10s)`                          | Keep complete file and report expansion; no arbitrary byte deletion.                                           |
| Late revision | Previously matched event is revised after recording finalization         | Reevaluate bounded indexed candidates with revision fencing; do not shorten prior committed deadline silently. |
| Restart       | Persisted decision/hold exists before restart                            | Restore the same decision before expiration becomes eligible.                                                  |

## Reproduction and validation

Focused commands for the existing behavior, with no new policy implementation:

```powershell
$env:RUSTUP_TOOLCHAIN = '1.99.0'
$env:CARGO_INCREMENTAL = '0'
cargo test --locked -p keeppeek --lib storage::engine::tests::recording_admission_enforces_modes_and_keyframe_aligned_event_boost
cargo test --locked -p keeppeek --lib storage::safety::tests
cargo test --locked -p keeppeek --lib storage::event_recording
cargo test --locked -p keeppeek --lib storage::playback::tests
```

Real-media tests require the repository FFmpeg prerequisites. Slow pipeline verification is
explicitly enabled with `KEEPPEEK_RUN_SLOW_TESTS=1`, as documented in
[pre-recording verification](pre-recording-verification.md). Never count a zero-test invocation.
Record observed results and the exact build identity in the implementing PR.

Observed on 2026-10-05, Windows, Rust 1.99.0, `CARGO_INCREMENTAL=0`: **38 tests passed**
against the audit baseline's unchanged runtime sources. Cargo built
`target/debug/deps/keeppeek-1e6a276cc181a3ce.exe`; safety and admission used Cargo,
and the remaining filters ran that same executable directly to avoid redundant rebuilds.

| Filter                                                                | Passed | Observed outcome                                                                               |
| --------------------------------------------------------------------- | -----: | ---------------------------------------------------------------------------------------------- |
| `storage::safety::tests`                                              |      8 | Capacity, foreign disk usage, hysteresis and cleanup targets.                                  |
| `recording_admission_enforces_modes_and_keyframe_aligned_event_boost` |      1 | Mode selection and keyframe transitions.                                                       |
| `storage::layout::tests`                                              |      2 | UTC paths and active-file paths.                                                               |
| `storage::event_recording`                                            |     18 | Four real-media tests plus overlap, deadlines, decoder epochs, byte budgets and history reset. |
| `storage::playback::tests`                                            |      8 | Codec/audio remux, aligned export, gaps, overlap and cancellation.                             |
| `cleanup_candidates_exclude_active_and_protected_recordings`          |      1 | Active/protected media excluded from pressure cleanup.                                         |

The full repository Markdown check and `mdbook build book` passed. The book used the CI versions
mdbook 0.5.4 and mdbook-mermaid 0.17.1; the existing preprocessor compatibility warning was
non-fatal. This focused run does not certify the full platform gate, a release candidate,
revised-event retention, or the missing runtime retention/control criteria.

### First implementation slice

`src/storage/retention.rs` implements a validated policy model and a deterministic resolver.
`tests/recording_policy_acceptance.rs` exercises the maximum matching deadline, fractional-day
durations, half-open boundaries, camera/stream identity, exact event types, protected media,
disabled rules, committed deadlines, invalid revisions, checked arithmetic and deserialization.
The overlap fixture failed against the initial implementation before the resolver was added.
All eight acceptance tests passed on Windows with Rust 1.99.0 and incremental compilation disabled.

This model accepts at most 256 canonical event revisions per decision and rejects oversized
snapshots. It does not truncate observations or infer motion from detection metadata. The caller
must supply current canonical revisions; repeated event IDs fail rather than selecting a revision.
An open event extends to the recording's end; a finite pulse covers one millisecond.

The resolver alone does not qualify restart, late-revision, disk-space or real-media retention
behavior. The catalog increment below adds persistence; active configuration, automatic
reevaluation, automatic expiry, file-granularity byte reporting and control integration remain
necessary before #168 can close. AC-7 still needs explicit numeric budgets and runtime measurement.

### Durable catalog commitments

`src/storage/catalog/retention.rs` stores one retention decision per stable recording ID in the
existing recording database. A transaction reads the finalized recording and current canonical
events, resolves the policy, and commits the deadline, matching rule IDs, policy revision and
fingerprint, and event snapshot revision. Recording path changes preserve that identity.
Canonical event writes advance the snapshot revision in the same database transaction.

Reusing a policy revision with different rules or supplying an older revision fails. Newer rules
and corrected events may extend a deadline but never shorten an existing commitment. Active files,
pending cleanup and active maintenance claims cannot acquire a new commitment. Protected state
comes from the current recording row; a stored reason is an audit record, not hold authorization.
Invalid stored metadata and oversized event snapshots fail without replacing a prior decision.

The ledger uses one additional connection to the same database, a nonblocking connection lock,
the catalog's two-second database busy timeout, and at most 256 event rows plus one overflow row.
Metadata reads are bounded. A transaction that exceeds the elapsed-time budget rolls back before
commit. This elapsed-time check does not interrupt a running SQL statement. The temporal index
below bounds candidate traversal; archive-scale runtime qualification remains necessary.
No new settings, background expiry job, cleanup authorization or public protocol fields ship in
this increment. Global policy activation fencing and bounded late-event reevaluation remain open.

`tests/recording_retention_catalog.rs` covers restart, shortened/stale/reused policy revisions,
canonical event corrections, half-open and camera/stream query boundaries, file moves, protected
and active files, shutdown, pending cleanup, event overflow, injected write failure and corrupted
metadata. These fixtures use the real catalog database; they do not certify physical media expiry
or the 127-source/30-day acceptance workload.

Observed on 2026-10-05, Windows, Rust 1.99.0, `CARGO_INCREMENTAL=0`: all eight catalog
acceptance tests and the library shutdown ownership regression passed, alongside the eight
resolver acceptance tests. The initial restart test failed before the catalog implementation.

The Python example tracks current unpinned tools. Its obsolete Black 26.5.1 requirement is absent
on current main. Black, Ruff and mypy passed locally;
53 Python tests passed, with four existing platform or opt-in environment skips.

### Storage authority integration

PR #269 merged on 2026-10-05. This branch incorporates main commit
`5e782456e2dda9a1b45022c1362c0b52476d8769`, preserving its catalog authority,
reader leases and named-volume location initialization. Each in-flight retention connection
holds the same catalog authority lease. Shutdown rejects new retention operations; an operation
that already owns its connection keeps the catalog locked until that connection is released.
Retention reads and commits verify the existing authority. Commit verification runs inside the
write transaction, so an offline handoff fence rejects the operation without leaving a transaction
open or changing its prior commitment. These guards do not activate retention cleanup.

### Automatic cleanup admission

Legacy archive cleanup and named-volume capacity or disk-pressure retirement now exclude files
with a committed future retention deadline. Admission validates stored decision metadata before
it changes cleanup ownership. Invalid metadata blocks admission without deleting the file.
The existing write transactions serialize admission with retention commitments. An admitted
named-volume retirement rejects a later commitment, preserving the previous ledger entry.

Named-volume restart recovery also checks the deadline before resuming an incomplete retirement.
Legacy pending-claim inspection remains available so the owner can cancel an interrupted claim;
new admission still validates the deadline. Protection and maintenance ownership retain their
existing behavior. These checks preserve committed obligations but do not activate configuration
or introduce an expiry job. Archive-scale candidate-selection performance remains unqualified.

Regression fixtures cover physical file preservation across catalog restart, independent capacity
and disk-pressure admissions, both admission/commit orderings, an older interrupted retirement,
and corrupt-metadata rollback followed by recovery after repair.

Observed on 2026-10-05, Windows, Rust 1.99.0, incremental compilation disabled: all 25
`cargo test --lib retention` matches, all eight resolver acceptance tests, and all eleven catalog
acceptance tests pass. `cargo clippy --lib --tests -- -D warnings` passes. These results do not
replace the full repository gate or the outstanding #168 runtime acceptance criteria.

### Runtime settings, reevaluation and expiry

The runtime increment at `0f75eb6` passed the complete Windows `check.bat` on 2026-10-05
with Rust 1.99.0, incremental compilation disabled and slow tests enabled:
3,246 Rust tests (26 configured skips), workspace Clippy and dependency checks,
Rust/TOML/Python formatting, UI static checks, 409 Bun tests, 259 browser
component/visual tests, 57 compatibility tests and 284 Playwright tests
(two existing capability skips). The gate used the prebuilt auth fixture with
SHA-256 `9c583b5a9574154bee795b26c0120c5bb9196a938915b4b69d9e531886ad82c6`.
The unchanged runtime sources and benchmark sources also passed the expanded
one-camera/one-day archive smoke run. The 127-camera/30-day release measurement
failed qualification as recorded below; these passing checks do not establish AC-7
or full issue completion. The subsequent accounting fix needs its own full gate.

The draft runtime implementation adds optional global and camera-specific
`storage.retention` settings to the existing configuration. The configuration
reference describes inheritance, explicit zeros, exact event selectors and limits.
Existing settings commands preserve this unexposed section from the current file;
retention activation uses the existing configuration activation/restart workflow.
No protected API contract changes are included.

Activation persists the accepted settings transition before replacing the active
policy map. Old-generation pending files and event windows finish first. Later
events use a separate queued generation, so continuous publication cannot keep
extending the old work window. Each global sweep has a fixed high-water ID;
finalizations behind its cursor retain their pending flags. Overflow schedules
another bounded sweep without resetting the current cursor. Work survives restart.

The background worker evaluates at most eight records per call in separate
transactions and obtains at most eight indexed expiry candidates. Invalid or
oversized evidence quarantines the affected file while other work progresses.
Canonical repairs enqueue reevaluation. Cleanup admission checks current policy
generation, pending event work, protection, ownership and committed deadlines.
Expiry removes whole files through legacy cleanup or the named-volume retirement
journal. Admitted legacy expiry recovery runs before a later policy activation.

Focused tests cover activation fencing, accepted-transition ordering, live events,
restart, shorter policies, finalization behind the cursor, protection release,
native expiry query plans, evidence overflow and repair, configuration round trips,
and physical MP4 expiry without capacity pressure. Named-volume reader leases
remain enforced by their owner. Legacy reader access retains the existing legacy
contract; this increment does not establish named-owner reader parity there.

These implementation details alone do not close the issue. The later qualification
sections below supply archive measurements, cold migration and event-coverage
mapping. Effective runtime-control ownership and final maintainer review remain
explicit in the current acceptance ledger.

`tests/recording_retention_media.rs` writes the repository's H.264 and H.265 camera
fixtures through `MediumTermWriter`, maps the relative media timeline to a fixed
UTC epoch after stopping its catalog owner, and retains the same complete bytes.
Continuous, motion and selected-person cases retain one two-second file with the
latest expected one-, seven- or thirty-day deadline. A matching 100-ms event expands
to that complete two-second file; the test does not claim exact event-only bytes.
FFmpeg must decode all 30 frames and treat decoder errors as failures. Events are
published after the file finalizes. The person case then publishes revision two
with its kind changed to motion; bounded runtime reevaluation must advance the
event revision, preserve the prior thirty-day commitment and identical media,
keep one catalog identity, and decode all thirty frames again.

The companion selection fixture compares conservative, reduced-storage and
alerts-only policies over two separately indexed, decodable files. Its fixed
future UTC epoch lets the production clock preserve matching lifetimes while
disabled nonmatching rules expire the other file. It checks physical removal,
unchanged selected bytes, exact retained intervals and distinct catalog identities
without capacity pressure. This fixture does not add a production clock override.

`examples/recording_retention_runtime.rs` defines a metadata-only scale harness:
127 sources, two streams, 30 days of 30-minute files, one canonical event per hour,
and the first segment of each day protected. It measures initial activation,
30 commitment evaluations and 30 late-event reconciliations after five warmups,
a restarted full-policy extension, validated disabling and sampled process RSS.
Full sweeps report per-batch histograms and one observed total per phase; they are
not 30 independent full-sweep measurements. The histogram measures catalog reconciliation. The same harness also runs real
H.264 ingest against that historical catalog with retention enabled and disabled,
reusing `cam-000/sub` and preserving its catalog authority. Each phase adds sixteen
verified records per sample; before/after counters verify historical record-count
preservation and disclose pending reconciliation work. Ingest timers exclude these
counter reads and startup. Historical timestamps are fixed in the future so the
production clock cannot expire synthetic rows. Actual recording counts and backlog
are reported; this is an accelerated workload with synthetic historical metadata.
Physical deletion and live pacing remain separate qualification workloads. No numeric acceptance budget is implied.

`examples/recording_retention_ingest.rs` separately measures accelerated real H.264
input and engine shutdown flush into fresh catalogs. The existing frame generator
supplies a 15-frame GOP repeated sixteen times. Each of five warmups and thirty
measured runs must retain all 240 MP4 samples. Runtime builds verify policy
activation before timing; a baseline without the runtime schema reports null
activation. Frame generation, startup, activation and verification are untimed.
This workload does not qualify live pacing or contention against the full archive.

Release measurements compare production base `b877bdd` with this runtime build
using the identical harness and encoded input. Each sample ingests 240 H.264
frames into sixteen independently decodable files, then flushes shutdown. All
35 runs per mode retain 458,320 bytes. Independent decoding of each mode's
representative output yields 240 frames; every file's encoded `mdat` payload
has the same SHA-256 across modes. The source, environment and raw reports are
in `docs/verification/recording-retention/runtime-*.json`.

| Fresh-catalog workload          | Median (ms) | P95 (ms) | P95 change from b877 |
| ------------------------------- | ----------: | -------: | -------------------: |
| b877 without the runtime schema |     246.399 |  269.055 |             Baseline |
| Current runtime, unconfigured   |     304.639 |  313.855 |              +16.65% |
| Current runtime, enabled        |     311.551 |  334.335 |              +24.26% |

These fresh-catalog costs meet the subsequently approved 800-ms and 30% p95
limits. They are not a full-archive ingest claim.
The benchmark distinguishes baseline schema absence from actual enabled/disabled
runtime state. It performs startup and verified activation before timing.

To reproduce the baseline, copy the archived `runtime-ingest-harness.rs` as the ingest example to a detached b877
worktree and build with Rust 1.99.0, `CARGO_INCREMENTAL=0`, four build jobs and
`cargo build --release --example recording_retention_ingest`. Run the resulting
binary with `enabled`; its activation entries must be null. Current `enabled`
and `disabled` runs report true and false, respectively. Each run retains its
reports and synthetic media under the printed temporary artifact directory.

The metadata harness seeds a fresh fixture before installing four new recording
indexes and the runtime file hooks. Its complete initial catalog reopen includes
native index construction and is measured separately from reconciliation. Columns
already exist in the fixture; this does not certify full old-schema migration.
Native schema/index operations are not interrupted by the runtime's per-transaction
elapsed checks. The runtime report retains raw histogram bins and thirty steady
and late-event timings; empty post-warmup histograms report null latency values.

Event reevaluation hooks exist only while an active policy uses event evidence.
Activation changes the hooks in the same transaction as the policies; restart
restores them from persisted policies. Unconfigured event writes avoid these
hooks. The existing 256-event shutdown regression failed with unconditional hooks
and passed unchanged after this correction; retention activation, restart and
hook removal also have a catalog regression.

### Full archive failure and accounting repair

The first release run at equivalent runtime source `0f75eb6` seeded 127 cameras,
30 days, 365,760 historical recording rows and 91,440 events. Initial activation,
steady/late-event assertions, restart during the thirty-to-thirty-one-day extension,
the exact committed floor and policy disablement passed. All 35 enabled ingest
samples retained 240 frames and sixteen files. Enabled p95 was **15.114 seconds**,
which fails the approved 800-ms limit. Disabled sample 25 failed the sixteen-record
assertion. No complete disabled distribution or successful combined report exists.
The [failure manifest](./verification/recording-retention/runtime-archive-failure-before.json)
and [completed enabled samples](./verification/recording-retention/runtime-archive-ingest-enabled-before.json)
preserve this result. The original harness wrote metadata timings only at the end;
the failed run therefore lost those distributions. The revised harness checkpoints
each completed phase and every ingest sample and captures warning/error diagnostics.

A [native query probe](./verification/recording-retention/legacy-byte-query-before.json)
localized repeated work in legacy byte accounting. The original ownership-filtered
sum measured 438–1,686 ms in five localization runs. It runs on segment finalization.
A covering-index sum still took 80–103 ms per call, repeated across sixteen files;
that alone cannot establish the ingest limit. These are localization measurements,
not a qualified thirty-sample before/after benchmark. The cause of the no-media
sample remains provisional until direct diagnostics or repeated validation resolve it.

The repair maintains an exact all-file byte total in the catalog transaction.
Legacy-only accounting uses it when no active named recording allocation exists;
named ownership retains the original ID/path exclusion query. Overflow or malformed
input makes the total unavailable without rejecting otherwise valid named writes.
Bootstrap scans at most one million rows with a two-second elapsed check and constant
application memory; an incomplete total remains unavailable and uses the original
query. The check does not interrupt a native row read. Schema/bootstrap/hooks install
atomically, update arithmetic subtracts the old value before adding the new one,
and rollback covers failed commits. Native backups include the total and hooks.
Cleanup cadence, quotas and safety deadlines are preserved.

Six native regressions pass: bootstrap and byte changes, rollback/overflow, normalized
path and ID ownership, cancelled allocations, reopen, representable named accounting
with an overflowing global total, near-limit replacement and failed initialization.
Workspace/all-target/all-feature Clippy passes. The archive harness requires an
available accounting total and verifies its growth against actual MP4 bytes.
The repaired accounting query measured p95 3.283 ms over thirty samples. Archive
ingest still failed the 800-ms limit: p95 807.423 ms with the probe index and
1,136.639 ms after its removal. Both distributions are retained in
[the first repaired run](./verification/recording-retention/runtime-archive-ingest-disabled-after.json)
and [the production-index run](./verification/recording-retention/runtime-archive-ingest-disabled-production-index.json).
All samples preserved sixteen files, 240 frames and exact accounting growth.

A [bounded diagnostic trace](./verification/recording-retention/runtime-archive-stage-before.json)
then observed 128 path-update calls during one sixteen-file ingest window.
Startup maintenance repeated public updates for already-complete finalized rows,
after synchronous size/identity refresh. The repair skips these actor transactions
when both finalization timestamps exist, while preserving missing-timestamp repair,
interrupted rename recovery, missing/offline-file handling and keyframe backfill.
A regression reproduced the redundant call before the fix and passed afterward,
including restored timestamps, identity, bytes and coverage. All 22 startup/recovery
tests and strict workspace/all-target/all-feature Clippy pass. The
[repaired diagnostic trace](./verification/recording-retention/runtime-archive-stage-after.json)
observes 32 path updates for sixteen files, including each writer finalization and
the existing same-root publication update. Diagnostic traces are single samples,
not acceptance distributions.

The [uninstrumented archive rerun](./verification/recording-retention/runtime-archive-ingest-disabled-startup-fixed.json)
passes the 800-ms ingest limit: thirty measured samples after five warm-ups give
median 414.719 ms, p95 471.039 ms and maximum 476.927 ms. Every sample retains
sixteen files, 240 frames and 458,320 bytes, with exact accounting growth and no
historical-row loss. The earlier missing-media failure did not recur; its original
cause remains unproven.

Fresh-catalog remeasurement remains unresolved against the older 269.055-ms
baseline: [disabled p95 417.791 ms](./verification/recording-retention/runtime-ingest-disabled-accounting-fixed.json)
is +55.28%, and [enabled p95 487.167 ms](./verification/recording-retention/runtime-ingest-enabled-accounting-fixed.json)
is +81.07%. Both pass 800 ms but fail the separate 30% regression limit in that
comparison. Other-agent Cargo work was observed on this shared host just after
these runs; its contribution is not established. The first matching controls
measured baseline p95 338.687/362.495 ms, disabled 515.071 ms (+52.08%) and enabled
396.287 ms (+17.01%). Disabled mode still failed the regression limit.

Successful writer finalization already commits the catalog path, identity, size,
timestamps and coverage. Same-root publication now skips its duplicate update;
distinct-root relocation keeps the update. Existing named-publication handling and
safety-limit enforcement are unchanged. The new regression observed two updates
before the fix and one afterward, for both direct and buffered writes; relocation
retains two updates with exact metadata and coverage. Pinned-evidence finalization
and retention fencing also pass, alongside strict Clippy. The
[new trace](./verification/recording-retention/runtime-archive-stage-same-root.json)
observes sixteen path updates for sixteen files.

At equivalent source `6435559`, matching baseline controls on both sides of the
current runs give p95 [344.063 ms](./verification/recording-retention/runtime-same-root-baseline-before.json)
and [395.775 ms](./verification/recording-retention/runtime-same-root-baseline-after.json).
Against the lower control, [disabled p95 327.423 ms](./verification/recording-retention/runtime-same-root-disabled.json)
is -4.84%, and [enabled p95 327.167 ms](./verification/recording-retention/runtime-same-root-enabled.json)
is -4.91%. Both pass 800 ms and the 30% regression limit; they also pass relative
to the original 269.055-ms baseline. All four blocks use five warm-ups and thirty
measured samples, preserving identical file/frame counts and byte totals.

The [same-root archive rerun](./verification/recording-retention/runtime-archive-ingest-same-root.json)
reports median 372.479 ms, p95 765.439 ms and maximum 845.823 ms; p95 passes 800 ms.
The new full gate began just before this process exited, so possible end-of-run
overlap is recorded rather than claiming complete isolation. A fresh full archive
run with this agent's builds stopped remains the final qualification gate.

The complete Windows gate at prior source `dc472d05` passed 3,253 Rust tests
(26 configured skips, three slow), 409 Bun tests, 259 component/visual tests,
57 compatibility tests and 284 Playwright tests (two capability skips), plus
Clippy/dependency/format/static checks. The complete Windows gate for the same-root
fix at equivalent source `6435559` also passes: 3,255 Rust tests (26 configured
skips, three slow; 612.673 seconds), the same 409/259/57 UI unit counts, 284
Playwright tests with two capability skips, and the required static checks. Its
prebuilt authentication fixture SHA-256 is
`27bcc5f1ca3b36bb84f36444f64601573dd8b7e77a1e0595df28e6002e8f50db`.
The fresh [full-scale report](./verification/recording-retention/runtime-full-report.json)
completed on equivalent source `6435559`: 127 cameras, main/sub streams,
365,760 historical recordings and 91,440 hourly canonical events over 30 days.
Initial eight-item reconciliation p95 is 78.207 ms (46,430 measured calls),
restart/31-day extension p95 is 62.943 ms (45,715 measured calls), and complete
late-event publication/reconciliation p95 is 70.399 ms (30 samples after five
warm-ups). Sampled evaluation peak RSS is 103,653,376 bytes initially and
95,641,600 bytes after restart. All meet the approved limits. Initial full
sweep takes 2,654,172 ms; the restarted sweep takes 2,167,843 ms. These are
bounded-call latency results, not a claim that full archive activation is instant.
Catalog startup with index rebuild takes 95.280 seconds; restart open takes
11.613 seconds. Columns already exist: full old-schema migration remains open.

Archive ingest p95 is 350.719 ms enabled and 395.263 ms disabled, with exact
240 frames, sixteen files and 458,320 written bytes per sample. Both pass the
800-ms limit. Enabled is 11.27% below disabled at current source, establishing
only enablement overhead. Disabled samples follow enabled samples and begin
with 560 additional recording rows. The fresh-catalog `b877` controls above
establish fresh-catalog regression only.

The separate [pre-feature archive control](./verification/recording-retention/runtime-archive-baseline.json)
at `b877bdd674c32dc1f84e92356f9c35d67197ea14` uses the same 365,760 main/sub
historical rows, 91,440 hourly events and 35 late events, real H.264 GOP,
240 frames, sixteen files and 458,320 bytes. Five warm-ups and thirty measured
samples give median 7,245.823 ms, p95 7,942.143 ms and maximum 8,081.407 ms.
Current enabled p95 is 95.58% lower; current disabled p95 is 95.02% lower.
Both meet the matching-archive 30% regression limit and absolute 800-ms limit.
Schema differences belong to the feature; the disabled current run follows the
additional 560 recordings from enabled ingest. Startup and verification remain
outside the timer in both implementations. No whole-host isolation is claimed.

The [control provenance](./verification/recording-retention/runtime-archive-baseline-environment.json)
records its binary, lockfile, source hashes and the wrapper-status discrepancy:
PowerShell returned 1 although the executable produced the final report and all
35 verified states, with no benchmark error. Completion is established by these
artifacts; no explicit native exit-code claim is made. Earlier control attempts
were interrupted to match the late-event population or stopped before samples
by a corrected verification-only column-name error; their timings are unused.
The exact [harness](./verification/recording-retention/runtime-archive-baseline-harness.rs)
and [support](./verification/recording-retention/runtime-archive-baseline-support.rs)
are retained. To reproduce, copy them into a `b877` checkout's `examples/` as
`recording_retention_archive_baseline.rs` and `recording_retention_archive_support.rs`,
then build/run `recording_retention_archive_baseline` in release with Rust 1.99.0
and incremental compilation disabled. Production source was unchanged; the
existing regression module is `cfg(test)` and excluded from release.

Policy disable drains successfully, and the exact committed 31-day floor
survives restart, disable and both ingest modes. Historical metadata is
synthetic and future-dated to exclude physical expiry; real H.264 is used for
ingest. This particular archive timing run does not qualify cold migration or
total SQL work; the separate diagnostics and media cases below provide that
evidence within their stated scope. Named-volume parity is not claimed.

### Cold migration, missing evidence and complete SQL work qualification

The 2026-10-06 closeout adds a transaction-level startup reconciliation guard:
missing media cannot discard a protected catalog row or an unexpired committed
deadline. The deterministic pre-fix regression at `f3200b0` waits for startup
maintenance and loses the stored decision; the repaired regression preserves
both missing references, protection, exact byte accounting and the committed
floor through index migration, restart and zero-day runtime policy activation.
Unprotected stale rows without a retained obligation still follow the existing
startup reconciliation contract. Explicit maintenance remedies are unchanged.

`cold_pre_runtime_schema_preserves_offline_references_holds_and_committed_floor_across_restart`
starts from the pinned pre-runtime definitions in
`tests/fixtures/recording_retention_b877.sql`, including an existing decision.
It does not simulate a cold upgrade by dropping an index from current tables.
The [full cold archive report](./verification/recording-retention/runtime-cold-archive.json)
upgrades an actual generated `b877` catalog: 365,760 historical rows plus 560
real H.264 recordings, 91,475 canonical events and a known non-NULL 31-day floor.
All legacy recording fields (including identity/finalization), fragments,
keyframes, canonical event fields, protections and committed decisions have
identical ordered SHA-256 digests before upgrade, after startup maintenance and
after restart. All 560 encoded media files also have identical combined hashes
and exact catalog/file lengths. Catalog open takes 10.583343 seconds; open plus
maintenance takes 12.700873 seconds. The pre-feature index was already prepared:
this report verifies index readiness across restart, and does not claim a cold
index rebuild. The smaller committed regression independently verifies bounded
index migration interrupted by restart.

`missing_source_and_absent_motion_or_object_producers_do_not_fabricate_media_or_evidence`
passes six real-media cases: H.264/H.265 with neither evidence producer, motion
only or person only. Three independently decoded clips occupy adjacent fixed
UTC intervals. A committed floor precedes removal of the middle clip; startup
maintenance completes before assertions. Matching late evidence does not
recreate missing source bytes. Surviving clips keep exact bytes and two-second
intervals, evidence selects 1/7/30-day lifetimes correctly, the missing recording
identity remains, and export rejects the unavailable file. These fixtures cover
absent canonical evidence and missing media; they do not assert detector-service
availability or fabricate unavailable lead-in. The focused suite passes all
12 catalog and three media tests, plus the SQL-counter calibration.

The SQL diagnostic counts native compilations separately from VM program starts
at instruction zero, including trigger/internal programs. It reads no SQL text,
instructions or parameter values. Calibration observes six caller statements
plus Turso's internal schema read, verifies one compilation/three executions of
a reused statement, and excludes work outside the measurement scope.
[Before](./verification/recording-retention/runtime-sql-before.json) and
[after](./verification/recording-retention/runtime-sql-after.json) use the same
archive and commitment: five warm-ups and thirty samples each. Native
compilations are 11 in both; program starts are 140 before and 141 after.
Canonical index preparation occurs outside the measured calls. Counts include
all native work within the stated operation, rather than equating the 130 indexed
candidate seeks with total SQL work.

The [runtime call profile](./verification/recording-retention/runtime-sql-batches.json)
measures thirty complete eight-record calls after five warm-ups over that
archive. Each evaluates eight records with zero quarantines, compiles 209
statements and starts 1,265 native programs, including traversal, transactions,
authority, evidence, commitments and bookkeeping. This profiles the first 35
activation calls; it does not claim completion of the full sweep or query totals
for every archive phase. Trace timings include instrumentation overhead and do
not replace the release latency/ingest measurements above. The event-candidate
bound remains 130 indexed seeks and 257 candidates per file, with separately
approved latency/RSS/ingest limits unchanged.

The [build manifest](./verification/recording-retention/runtime-cold-sql-environment.json)
records exact source/binary/report hashes and exit codes. The
[direct harness](./verification/recording-retention/runtime-sql-direct-harness.rs)
and [counter](./verification/recording-retention/runtime-sql-counter.rs) used for
the paired data are retained. Reproduce with Rust 1.99.0, incremental compilation
disabled, release `recording_retention_sql <closed generated catalog>` on `b877`
then current source; run `recording_retention_cold <catalog>` between them.
The `runtime` second argument selects complete current runtime calls. Diagnostics
accept only generated temporary archive paths. The final full Windows gate and
new-head CI remain pending until their results are recorded.

### Final canonical verification

The unchanged Windows `check.bat` completed with exit 0 at `cb4342a` on
2026-10-06: 3,257 Rust tests (26 configured skips), 409 Bun, 259 component/visual,
57 compatibility and 284 Playwright tests (two capability skips), plus strict
Clippy, dependency, format, Python and UI static checks. Rust 1.99.0,
`CARGO_INCREMENTAL=0`, slow tests and four Rust workers were used. The verified
library authentication fixture is identified in the build manifest; it contains
`server::authentication_browser_fixture::issue123_browser_fixture`.

The preceding run passed Rust/UI quality but failed three browser cases: two
missing-control timeouts and one recorded-frame timeout. All three passed
unchanged in isolation and then in the unchanged complete gate. Their cause is
not established; failed artifacts and logs remain preserved. A separate earlier
fixture-selection mistake used a bin test executable without the authentication
test; Cargo rebuilt the correct library target, and all eight authentication
browser cases passed before the complete rerun. No assertions, timeouts or
configuration thresholds were weakened.

Published implementation head `cb4342a` has 39 successful CI checks, three
configured skips and one neutral result, including the Book, Security and Visual
Regression jobs. This documentation-only closeout identifies that equivalent
implementation build; current PR status records checks for the final published
head. Automated reviews found no unresolved concrete defects. Human maintainer
matrix review and #202 control evidence remain outstanding; #168 is not closed.

### Event-coverage acceptance mapping

| AC-4 case                | Verification and observed scope                                                                                                                                                                                                                                                                                                                                        |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Keyframe-limited lead-in | `event_preroll_h264_is_independently_decodable` and its H.265 counterpart request 1.3 seconds before a 2.5-second trigger. The retained clip starts at the 2-second keyframe; independent decoding, audio bounds and catalog seek pass. See `docs/pre-recording-verification.md`.                                                                                      |
| Post-capture boundary    | `deadline_finalization_clamps_the_last_mp4_video_sample` verifies the exclusive deadline; `selected_replay_bypasses_short_term_aging_and_silent_deadline_finalizes` verifies finalization after silent input for both selected streams.                                                                                                                                |
| Overlapping events       | `overlapping_media_event_storm_writes_each_frame_once_to_one_catalog_recording` verifies 1,001 triggers, one finalized recording, ordered unique samples and decoding.                                                                                                                                                                                                 |
| Late/revised evidence    | `retention_examples_expire_only_unmatched_decodable_media_without_pressure` verifies late matching evidence and person-to-motion revision, advancing the event revision while preserving the earlier thirty-day floor, exact bytes, two-second coverage, recording identity and 30 decoded frames for each codec. This does not backfill media absent at capture time. |
| Unavailable lead-in      | `snapshots_distinguish_history_from_pending_replay_and_expire_silent_streams` verifies startup, missing-keyframe and duration-eviction reasons with zero available coverage. `snapshot_reasons_recover_when_history_refills` verifies recovery. These are metadata fixtures, not a complete missing-source real-media scenario.                                        |

The named tests above passed in the runtime increment's full Windows gate.
The combined real-media missing-source/absent-evidence fixture above completes
this coverage mapping. Existing server reason codes and browser rendering remain
covered by their canonical tests. No fixture claims to create source media or
detection evidence when unavailable.

### Bounded canonical event traversal

`src/storage/catalog/retention/event_index.rs` derives one temporal bucket per canonical event.
The bucket is the smallest aligned binary interval containing the event's inclusive timestamp
range. A finite event uses its exclusive end minus one; a pulse uses its timestamp; an open event
extends to the largest timestamp. Events crossing the signed timestamp boundary use the root
bucket. Every actual overlap falls in a queried bucket at its stored level.

A lookup performs at most 130 composite-index seeks: 65 levels for each of the camera-wide and
logical-stream scopes. It examines at most 256 candidates plus one overflow row, then checks
canonical geometry and half-open overlap. Coarse buckets can include nonmatching candidates.
The derived scope key is non-null and distinguishes camera-wide events from every stream name.
The native plan test requires equality and bucket-range constraints with no sorter. A nullable
`IS` parameter failed that test because Turso only sought camera and level.
An oversized candidate set rejects the decision without truncating evidence or changing an
existing deadline. Elapsed checks reject work exceeding two seconds; they do not interrupt OS I/O.

Canonical insertions and replacement revisions update the index within their transaction.
Database triggers mark other insertions and temporal changes pending, including the existing
native-event close path; pending changes for the camera fence commitments until reconciliation.
An existing database starts with migration incomplete. `reconcile_retention_events` accepts
1–256 entries per transaction, persists a keyset cursor, resumes after restart and repairs pending
entries. Index maintenance does not change canonical event revisions. An incomplete migration
fences all new commitments while leaving prior decisions readable. Index initialization does
not walk the archive or build an index over its existing canonical rows.

`examples/recording_retention_query.rs` measures metadata-only debug lookup latency with five
warmup decisions and 30 samples. It uses a 2 MiB thread stack, matching Rust test threads; the
Windows main-thread stack overflows during debug catalog startup before seeding. Synthetic bulk
seeding and bounded index preparation are outside the timed decisions. Run with Rust 1.99.0,
incremental compilation disabled, and `cargo run --example recording_retention_query -- 50000`.
The unchanged query at `cd870b2` measured median 966,655 µs and p95 1,183,743 µs over 50,000
historical events. Its native query plan used the camera/start-time index and an order-by sorter.
The indexed query measured median 19,279 µs and p95 22,031 µs on the same archive workload,
about 54 times faster at p95. Synthetic seeding took 144,131 ms and bounded index preparation
took 95,492 ms; neither is part of lookup timing or a live-ingest measurement. Three index tests
cover signed and half-open interval geometry and the native composite query plan. Catalog tests
also cover a pending canonical change, bounded migration with restart and a write failure that
rolls back the canonical event revision along with its derived index.
This harness does not establish RSS, live ingest cost or the 127-source/30-day AC-7 budgets.

The archived [baseline harness](./verification/recording-retention/baseline.rs) excludes only
the indexed implementation's untimed reconciliation step. [Baseline measurements](./verification/recording-retention/baseline.json)
and [indexed measurements](./verification/recording-retention/indexed.json) record the environment
and histogram results. To reproduce the baseline on Windows from this branch, keep the same
Rust and incremental-compilation settings and use a fresh worktree:

```powershell
git worktree add --detach ../keeppeek-retention-baseline cd870b2
Copy-Item docs/verification/recording-retention/baseline.rs ../keeppeek-retention-baseline/examples/recording_retention_query.rs
cargo run --manifest-path ../keeppeek-retention-baseline/Cargo.toml --example recording_retention_query -- 50000
```

Observed on 2026-10-05, Windows, Rust 1.99.0, `CARGO_INCREMENTAL=0`, slow tests enabled:
all 25 retention tests and the full `check.bat` passed. The full gate passed 3,220 Rust tests
(26 configured skips), workspace Clippy and formatting, zero-error/zero-warning Svelte checks,
409 Bun tests, 259 browser component/visual tests, 57 compatibility tests and 284 Playwright
tests (two existing capability skips). Markdown formatting and mdbook 0.5.4 passed separately.
An initial full browser run had one Chromium `ERR_NO_BUFFER_SPACE` CSS-load failure; its
artifacts were preserved. The unchanged case passed alone and the unchanged full gate passed
on rerun. The resource observations did not establish the transport failure's cause.
These checks qualify the index increment, not settings activation or physical expiry.
