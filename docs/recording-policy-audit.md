# Recording policy audit

This is the working evidence ledger for [#168](https://github.com/xnorpx/keeppeek/issues/168).
It records implemented behavior and gaps; it does not approve a new retention policy or certify
the Alpha candidate. Audit baseline: `872defab438a3d69c0c57fc3b48907fe818b8afd`, retrieved
2026-10-05. All source/test paths below refer to that KeepPeek commit unless noted otherwise.
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
and runtime acceptance remain pending.

## Outcome matrix

Evidence marked **historical pass** is from the linked merged PR/report, not a new test run.
**Source only** and **missing** do not establish acceptance. Supported behavior means the
audited KeepPeek commit, within the codec/filesystem/browser limits of the linked reports.

| ID  | Reference topic             | Class      | KeepPeek input/output and authoritative source                                                                                                        | Evidence and limitation                                                                                                                                                                                                                                           | Owner |
| --- | --------------------------- | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- |
| R01 | Recording enablement        | Equivalent | `off` admits no media; `sub`, `main`, and `both` select their configured streams. `CameraRecordingPolicy::decide`, `src/storage/recording_policy.rs`. | `storage::engine::tests::recording_admission_enforces_modes_and_keyframe_aligned_event_boost`; historical pass in #268. This is admission, not a retention schedule.                                                                                              | #168  |
| R02 | Encoded media               | Equivalent | Original H.264/H.265 samples become indexed MP4 media without video transcoding. `storage::engine`, `MediumTermWriter`.                               | `event_boost_round_trips_h264_h265_h264_with_audio_and_catalog` in `src/storage/engine.rs`; historical pass in #268. Unsupported browser decoding remains R20.                                                                                                    | #168  |
| R03 | UTC layout                  | Equivalent | A timestamp selects camera/date/hour storage paths rather than the reference's directory ordering. `layout::segment_path`.                            | `src/storage/layout.rs::tests::segment_path_format`; source fixture. Equivalent time addressing does not promise identical paths.                                                                                                                                 | #168  |
| R04 | Conservative example        | Partial    | `sub`/`main`/`both` preserve continuous admitted media; byte pressure can shorten coverage.                                                           | R01 plus `StorageSafetyPolicy::evaluate`; independent class deadlines are missing. No accepted exact retained-interval fixture yet.                                                                                                                               | #168  |
| R05 | Reduced-storage example     | Partial    | `event-only` selects an event stream; `event-boost` replaces sub GOPs with main in one logical recording.                                             | `event_only_idle_selects_nothing_and_repeated_events_extend_once`; #268 historical pass. Event admission is not motion-class retention.                                                                                                                           | #168  |
| R06 | Alerts-only example         | Partial    | Accepted events can open `event-only` recording; no dedicated alert classification/retention contract exists.                                         | `EventRecordings::note_event` in `src/storage/event_recording.rs`; missing class-predicate/deadline acceptance.                                                                                                                                                   | #168  |
| R07 | Class-specific capture      | Partial    | Camera-level pre-history and post-event duration apply to accepted events.                                                                            | [Pre-recording verification](pre-recording-verification.md), #268. Separate alert/detection windows are not implemented.                                                                                                                                          | #172  |
| R08 | Capture eligibility         | Partial    | Retained keyframes, media availability, deadline, decoder epoch and byte budget bound selected media.                                                 | `event_preroll_h264_is_independently_decodable`, `event_preroll_h265_is_independently_decodable`; historical pass. No `active_objects` retention eligibility model.                                                                                               | #168  |
| R09 | Capture display             | Partial    | Timeline coverage and health show available media/shortening reasons rather than implying requested footage exists.                                   | #268 desktop/mobile screenshots; `book/src/recording-and-evidence.md`. An event's displayed bounds do not certify all surrounding retained coverage.                                                                                                              | #172  |
| R10 | Continuous/motion durations | Gap        | Storage safety has byte/percentage/free-space thresholds, not independently evaluated class lifetimes.                                                | `StorageSafetyPolicy::evaluate`, `src/storage/safety.rs`; missing retention resolver and durable deadline metadata.                                                                                                                                               | #168  |
| R11 | Object/event durations      | Gap        | Canonical events exist; no accepted alert/detection predicate maps them to recording expiration.                                                      | `src/storage/events.rs`, `src/storage/catalog.rs`; source only. Class mapping needs an owner decision.                                                                                                                                                            | #168  |
| R12 | Maximum deadline            | Gap        | Catalog cleanup orders eligible files under pressure; no rule-overlap deadline resolver exists.                                                       | `RecordingCatalogHandle::claim_cleanup_candidate`; missing overlap/restart/policy-revision fixtures.                                                                                                                                                              | #168  |
| R13 | Fractional durations        | Gap        | Event durations are integer seconds, but no retained-media lifetime model implements precise sub-day class expiry.                                    | `src/cameras/mod.rs`, `src/config.rs`; missing checked-duration resolver.                                                                                                                                                                                         | #168  |
| R14 | Overlap deduplication       | Partial    | Repeated event admission writes each selected frame once to one recording.                                                                            | `overlapping_media_event_storm_writes_each_frame_once_to_one_catalog_recording`; #268 historical pass. Deduplicated class-based retention is not proven.                                                                                                          | #168  |
| R15 | Runtime schedules/control   | Partial    | Configured modes and server privacy schedules exist. Named profile activation remains open in #202.                                                   | #265/#125 and `src/storage/engine/event/tests.rs::worker_rejects_queued_media_after_a_complete_privacy_cycle`; historical pass. No approved arbitrary scheduler/control state table.                                                                              | #168  |
| R16 | Range/event exports         | Equivalent | Administrator export produces a durable searchable job with cancellation/failure handling.                                                            | #189/#113; `src/storage/playback.rs::tests::cancelled_export_removes_partial_file_and_can_retry`; historical CI. Container/codec limits remain documented.                                                                                                        | #113  |
| R17 | Export survival             | Partial    | Protected catalog media is excluded from ordinary cleanup; exports have a separate job/artifact lifecycle.                                            | `cleanup_candidates_exclude_active_and_protected_recordings`, `src/storage/catalog.rs`; source fixture. Do not infer indefinite exported-file preservation from source protection.                                                                                | #113  |
| R18 | Custom export/transcode     | Gap        | Indexed fragment assembly/remux exists; it does not expose arbitrary encoding arguments or a CPU transcode retry.                                     | `export_fragment_ranges_with_progress`, `browser_compatible_recording`; source only. Scope/divergence decision required; no new endpoint is authorized.                                                                                                           | #168  |
| R19 | Timelapse                   | Gap        | Bounded timelapse review/export remains open.                                                                                                         | #127 acceptance contract; ordinary playback speed/export is not a timelapse renderer.                                                                                                                                                                             | #127  |
| R20 | Apple/H.265 playback        | Partial    | Browser compatibility selection and audio-timescale repair exist; no universal H.265 transcode fallback.                                              | `compatibility_remux_repairs_audio_timescale_and_is_cached`, #161/#111; #131 remains open. Remux does not change video codec.                                                                                                                                     | #131  |
| R21 | Media reconciliation        | Partial    | Explicit owned reports/remedies and recoverable deletion jobs exist.                                                                                  | #232/#233/#133; `reconciliation_protocol_requires_owned_reports_and_explicit_remedies`. [Maintenance limits](../book/src/recording-maintenance.md) retain trust/filesystem/recovery qualification limits.                                                         | #133  |
| R22 | Recording accounting        | Equivalent | Catalog-attributed bytes and filesystem free capacity are distinct observations.                                                                      | #190/#122, `coverage_snapshot_retains_cleanup_evidence_after_restart`, `StorageSafetyPolicy::evaluate`; historical CI. Attribution is not a complete disk scan.                                                                                                   | #122  |
| R23 | Other disk consumers        | Equivalent | Non-KeepPeek usage reduces actual headroom without being attributed to recording bytes.                                                               | `src/storage/safety.rs::tests::reserve_accounts_for_non_keeppeek_disk_usage`; source fixture. Missing/stale capacity remains an explicit observation.                                                                                                             | #112  |
| R24 | Mount identity              | Partial    | Capacity queries use the nearest existing parent of the configured root. Named-volume ownership is in progress.                                       | `filesystem_capacity_queries_the_nearest_existing_parent`; #129/PR #269 owns placement/root recovery. Existing capacity does not prove the intended external volume is mounted.                                                                                   | #129  |
| R25 | Cache boundary              | Partial    | `ShortTermBuffer` and bounded encoded pre-history are separate from durable writer/catalog progress.                                                  | #268 budget/pressure tests; `src/storage/short_term.rs`, `src/storage/event_recording.rs`. No tmpfs layout is required or implemented as a parity feature.                                                                                                        | #168  |
| R26 | Stale disk metrics          | Partial    | Reconciliation is explicit; startup preserves interrupted evidence.                                                                                   | `startup_preserves_interrupted_recording_for_explicit_reconciliation`, #233. Arbitrary external deletion does not instantly update catalog attribution.                                                                                                           | #133  |
| R27 | Emergency cleanup           | Partial    | Oldest eligible finalized catalog files are removed to a recovery target; protected/active files remain excluded and recording can pause.             | `startup_cleanup_removes_only_oldest_catalog_media_to_recovery_target`, `cleanup_pauses_recording_when_no_eligible_media_remains`; source fixtures. The reference's unconditional cleanup outcome is not claimed; approval of a deliberate divergence is pending. | #112  |

No row is marked Intentional divergence solely because current code differs. The owner must
accept the consequence and workaround before that classification replaces Partial or Gap.

## Acceptance ledger

| Original criterion            | Current evidence                                                                       | Remaining closure evidence                                                                                          |
| ----------------------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| AC-1 reviewed complete matrix | R01–R27 cover the retrieved page and identify code/evidence/owner.                     | Maintainer review/date; reconcile any changed reference headings before closure.                                    |
| AC-2 accepted policy examples | Admission examples R04–R06 are explicitly Partial.                                     | Accepted class predicates and exact retained interval/byte fixtures.                                                |
| AC-3 deterministic expiry     | R10–R14 distinguish existing admission from missing lifetime rules.                    | Resolver, persistence, revision/late-event reevaluation, restart and migration tests.                               |
| AC-4 decodable event coverage | #268 real H.264/H.265, codec/audio transition, overlap, budget and privacy fixtures.   | Map revised-event semantics and every requested boundary to exact evidence; no assumption that all AC-4 cases pass. |
| AC-5 authoritative control    | Configured admission and #125 fail-closed fencing; #39 durable CAS/watch is available. | Accepted control/precedence state table and #202 profile-specific evidence.                                         |
| AC-6 related feature evidence | R16–R27 identify existing owners and unresolved limitations.                           | Final owner-specific evidence; #127/#131 remain open.                                                               |
| AC-7 scale bound              | #268 measures pre-roll ingest, not retention evaluation.                               | Accepted latency/query/memory/ingest budgets and 127-source/30-day retention harness.                               |

## Approved retention contract

The maintainer approved these product semantics on 2026-10-05. The resolver implementation
below does not yet activate them in the application. Existing recordings and size-pressure
behavior remain unchanged until settings and durable cleanup integration are qualified.
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
6. Decide the numeric AC-7 budget before runtime acceptance. Proposal for review: 127 sources,
   30 days, fixed event density/overlap, release build, 30 measured rounds after warmup; report
   p50/p95/max evaluation latency, queries per batch, peak memory and ingest delta. No invented
   pass/fail threshold or pre-roll benchmark substitutes for that decision.

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

This slice does **not** add active configuration fields, persist decisions in the recording
catalog, query event revisions, enforce deadlines during cleanup, report file-granularity byte
expansion or implement control profiles. Those integrations remain necessary before #168 can
close. In particular, pure resolver tests do not qualify restart, late-revision, disk-space or
real-media retention behavior. AC-7 still needs explicit numeric budgets and runtime measurement.

The PR also removes the obsolete Black 26.5.1 requirement from the Python example configuration.
The example intentionally installs current unpinned tools; CI's Black 26.10.0 otherwise rejects
the configuration before checking source formatting. Black, Ruff and mypy passed locally;
53 Python tests passed, with four existing platform or opt-in environment skips.
