# Recording capability audit

This is the evidence and decision ledger for [#168](https://github.com/xnorpx/keeppeek/issues/168).
It records the baseline audit and the approved retention/control implementation slices below.
The issue remains open: runtime integration, dependency evidence, remaining owner decisions,
performance qualification, and final maintainer review are pending.

## Baseline and classification

- Reference: [Frigate Recording](https://docs.frigate.video/configuration/record/), retrieved
  2026-09-20. This is the live page, not a version-pinned release; no page revision was exposed.
- KeepPeek: `0.1.0` source at
  [`d56e145bfae56daac6e7c900bb08d725d904806a`](https://github.com/xnorpx/keeppeek/tree/d56e145bfae56daac6e7c900bb08d725d904806a).
  The matrix describes that baseline; relative links locate the owning files. Follow-up commit
  evidence below records subsequent changes and supersedes the corresponding baseline gaps.
  This does not certify every released `0.1.0` binary.
- Audit author: Codex, 2026-09-20. Approved policy decisions are recorded below. Final maintainer
  review against the completed implementation remains pending for every row.
- `Equivalent` means the narrowly stated outcome has an executed automated assertion and a
  reproducible procedure below. It is not a camera/browser qualification claim.
- `Partial` means a foundation exists, but a required outcome or its qualification is missing.
  `Gap` means the inspected runtime/configuration cannot express the outcome.
- `Intentional divergence` requires an existing explicit safety requirement or an approved
  decision. An unapproved difference stays `Partial` or `Gap`.
- Every row names one accountable issue. Where #168 is the owner, it owns the unresolved decision
  and evidence gap; that is **not** approval to implement the feature or open another issue.

## Source coverage

The following inventory covers every heading on the retrieved page, including its introduction.
Names in this table are navigation labels, not claims about KeepPeek.

| Source section                                                                       | Rows                   |
| ------------------------------------------------------------------------------------ | ---------------------- |
| Recording introduction                                                               | R01–R05, R12, R16, R20 |
| Common recording configurations; conservative, reduced-storage, alerts-only examples | R06–R08                |
| Pre/post capture; interaction with retention mode; where to view footage             | R13–R15                |
| Configuring retention; continuous/motion; object recording                           | R05, R09–R12           |
| Recording at certain times                                                           | R17–R18                |
| Export; custom FFmpeg arguments; CPU fallback                                        | R19–R23                |
| Apple H.265 compatibility                                                            | R24                    |
| Syncing media files with disk                                                        | R25–R26                |
| Understanding usage; measurement; accounting differences                             | R27–R28                |
| Free space and mounts; separate cache; metrics/disk mismatch                         | R16, R28–R30           |
| Low-space deletion                                                                   | R31–R32                |

## Outcome matrix

Evidence IDs resolve to exact symbols, tests, and observations below. The baseline and review status
above apply to every row. Workflows W1–W6 are reproducible qualification procedures; only the
automated executions explicitly listed in the evidence ledger were run in this audit.

| ID  | Required outcome / concrete example                                      | Class                  | Implemented behavior, evidence, and limitation                                                                                                                                                                                                            | Owner       |
| --- | ------------------------------------------------------------------------ | ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| R01 | Disable one camera: admit neither main nor sub.                          | Equivalent             | `CameraRecordingPolicy::decide` rejects both for `Off`; T1, W1. Config editing can later change the mode; this is not an immutable automation permission.                                                                                                 | [#168][168] |
| R02 | Record main, sub, or both continuously.                                  | Equivalent             | Admission selects the configured stream(s); T1, W1. Continuity still depends on source, writer, and capacity. No age-based lifetime is promised.                                                                                                          | [#168][168] |
| R03 | Preserve encoded H.264/H.265 in segmented MP4.                           | Partial                | `MediumTermWriter::append_one` and `finalize`; T2 inspects codec descriptions, audio timing, and catalog entries. No independent H.265 decode/browser run was performed here; container assertions alone do not complete qualification.                   | [#168][168] |
| R04 | Locate recordings by UTC camera/time.                                    | Equivalent             | `layout::segment_path`; T3 maps `2026-02-17T14:35:09.123Z` to `front_door/2026-02-17/14/3509123.mp4`. W2. Path order differs; the outcome is time addressability.                                                                                         | [#168][168] |
| R05 | Global recording defaults with per-camera overrides.                     | Partial                | `CameraCredentialDefaults` and config inheritance cover mode and EventBoost duration; C1. Independent retention-rule defaults/overrides do not exist.                                                                                                     | [#168][168] |
| R06 | Conservative example: progressively retain fewer classes.                | Gap                    | C1 has storage capacities and buffer/segment durations, not continuous/motion/event lifetimes. A byte cap cannot reproduce the example. D1.                                                                                                               | [#168][168] |
| R07 | Reduced-storage example: keep motion-selected footage.                   | Gap                    | P1 selects streams, not motion intervals. EventBoost still records during quiet periods. D1/D2.                                                                                                                                                           | [#168][168] |
| R08 | Alerts-only example: discard unrelated footage.                          | Gap                    | Event records do not define admission or expiry classes. `Off` disables all recording; EventBoost is not alerts-only. D1/D2.                                                                                                                              | [#168][168] |
| R09 | Independent continuous, motion, alert, and detection lifetimes.          | Gap                    | C1/P1 contain no rule-class expiry fields or resolver. Event metadata and coverage statistics do not supply one.                                                                                                                                          | [#168][168] |
| R10 | Eligibility equivalent to all/motion/active objects.                     | Gap                    | `CameraRecordingPolicy::decide` consumes stream/keyframe/time, not segment motion/object provenance. Generic event ingestion is a dependency, not eligibility proof. D2.                                                                                  | [#168][168] |
| R11 | Overlapping matches keep one media object to the latest deadline.        | Gap                    | Catalog IDs and event relationships exist, but no maximum-deadline retention resolver exists in P1/C1. EventBoost's one-file test T2 proves a different property. D1/D3.                                                                                  | [#168][168] |
| R12 | Precise sub-day lifetimes; zero disables only one rule.                  | Gap                    | Seconds-valued buffering and EventBoost duration are not retention durations. No 12-hour rule or exact-expiry test exists for this policy model. D1.                                                                                                      | [#168][168] |
| R13 | Retain decodable pre-event coverage.                                     | Gap                    | B1 is a duration-evicted frame queue downstream of admission, not event-triggered GOP replay. Do not describe it as pre-roll.                                                                                                                             | [#172][172] |
| R14 | Configurable post-event coverage by event class/camera.                  | Partial                | EventBoost extends main selection from event arrival and returns on a sub keyframe; T1/T2. It has no independent alert/detection windows, durable event revisions, or post-event retention policy. D2/D3.                                                 | [#168][168] |
| R15 | Browse available lead-in/tail and explain unavailable coverage.          | Partial                | [Recording integrity](recording-integrity.md) and Keep expose coverage/gaps; W3. Export's 15-second context requests existing media and does not create pre-roll. R13/R14 remain prerequisites.                                                           | [#172][172] |
| R16 | Cache and validate before policy-selected persistence.                   | Partial                | B1 and writer admission/queue bounds exist; T4. No retention-class promotion stage exists. `short_term_secs` is not a disk-retention rule.                                                                                                                | [#168][168] |
| R17 | Manually change recording at runtime.                                    | Partial                | Typed configuration exposes mode/effective inherited value; C1, W1. No separate override with actor, reason, expiry, precedence, and durable restart semantics. D4.                                                                                       | [#168][168] |
| R18 | Scheduled/external control respects disabled/privacy bounds.             | Gap                    | Configuration editing, event forwarding, and rules capabilities do not establish a recording-control resolver. Privacy schedules remain open in #125; generic control ownership requires D4.                                                              | [#168][168] |
| R19 | Export a range/event and retrieve durable job history.                   | Equivalent             | E1/T6 and historical H2 cover bounded range assembly, lifecycle, event entry, and requester-scoped history. W4. Maximum range is two minutes; incomplete coverage is explicit.                                                                            | [#113][113] |
| R20 | Keep exported evidence beyond recording expiry.                          | Partial                | Exports are separate from automatic recording cleanup (T5), but server artifacts expire after 24 hours and metadata after 30 days/500 jobs (E1). Download and verify externally for durable custody; permanence divergence is not approved by this audit. | [#113][113] |
| R21 | Timelapse review/export.                                                 | Gap                    | Playback-rate controls and normal MP4 remux do not generate a timelapse artifact. P2/E1; no timelapse evidence claimed.                                                                                                                                   | [#127][127] |
| R22 | Custom export encoding/filter arguments.                                 | Gap                    | E1/P2 expose typed export options, not arbitrary FFmpeg arguments. Timestamp burn-in requires a worker; it does not establish a general export API. D5.                                                                                                   | [#168][168] |
| R23 | Export hardware selection, software retry, quality tuning.               | Gap                    | P2 remux is not an encoder/fallback service. No custom-export GPU failure/retry fixture is identified. D5; do not infer this from playback adaptation.                                                                                                    | [#168][168] |
| R24 | Browser-compatible playback including Apple H.265.                       | Partial                | P2 compatibility remux and UI variant ranking/fallback exist; T6/H3, W3. Remux cannot make an unsupported codec decodable; adaptive/transcoding and current Safari/device qualification remain outstanding.                                               | [#131][131] |
| R25 | Reconcile missing catalog media and report drift.                        | Partial                | M1 provides bounded inspection/reconciliation and recovery; T7/H4, W5. Local native removal tests fail filesystem qualification; general deployment recovery is not certified. Closed issue status is not proof of full parity.                           | [#133][133] |
| R26 | Reconciliation never silently deletes unowned evidence.                  | Intentional divergence | M1 and #168's explicit non-goal preserve unindexed media and interrupted `.active` files; T7. Consequence: orphan bytes can remain. Use bounded maintenance inspection and reviewed recovery, not unconditional orphan purge.                             | [#133][133] |
| R27 | Distinguish catalog-attributed bytes from filesystem capacity.           | Equivalent             | S1 evaluates OS available bytes separately from KeepPeek bytes; T5, W6. Other users/files can trigger pressure even below the archive cap.                                                                                                                | [#112][112] |
| R28 | Explain snapshots/exports/database/other space outside recording totals. | Partial                | S1 reports filesystem pressure; recording integrity reports attributed media. It is not a complete per-directory accounting tool. Compare OS capacity and known configured roots; exact cross-surface attribution needs further qualification.            | [#122][122] |
| R29 | Diagnose an absent or incorrect storage mount.                           | Partial                | `filesystem_capacity` queries the nearest existing parent; T5. Successful capacity probing does not prove the intended external volume is mounted. W6 checks volume identity; no mount-identity guard is claimed.                                         | [#168][168] |
| R30 | Separate cache pressure from archive pressure and stale usage.           | Partial                | B1 is memory buffering; medium/long roots can differ (C1). Archive safety does not prove independent medium-root headroom enforcement. M1 handles catalog drift; W5/W6. D6.                                                                               | [#168][168] |
| R31 | Reclaim oldest eligible recordings and recover safely.                   | Equivalent             | T5 removes 40-byte oldest catalog media while preserving newer, exported, and unindexed files; pause/recovery assertions pass. S1 uses explicit capacity/headroom/hysteresis, not an age guarantee. W6.                                                   | [#112][112] |
| R32 | Pressure cannot override protected evidence.                             | Intentional divergence | #168 explicitly requires the stricter #112 protection boundary. T5 preserves held media and pauses when no eligible candidate remains. Consequence: new footage can be lost; restore capacity or review holds explicitly.                                 | [#112][112] |

## Authoritative implementation and capability map

| ID  | Source symbols and documentation                                                                                                                                                                                                                                      | Contract/UI boundary                                                                                                                                                                                                                                                |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | [`CameraCredentialDefaults`, `StorageToml`](../src/config.rs), [`CameraRecordingMode`, `CameraConfig`](../src/cameras/mod.rs); [configuration reference](../book/src/configuration-reference.md)                                                                      | `keeppeek.configuration.v1`; typed editor and effective inheritance in [`server/configuration.rs`](../src/server/configuration.rs), [`CameraConfigurationEditor.svelte`](../ui/src/lib/components/CameraConfigurationEditor.svelte). No retention-class capability. |
| P1  | [`CameraRecordingPolicy::{note_event, decide}`](../src/storage/recording_policy.rs), [`RecordingAdmission`, `StorageHandle::configure_camera_recording`](../src/storage/engine.rs)                                                                                    | Admission and configuration are distinct from a future runtime permission/override model.                                                                                                                                                                           |
| B1  | [`ShortTermBuffer`](../src/storage/short_term.rs), [`StorageConfig`, writer queue](../src/storage/engine.rs); [storage architecture](../src/storage/README.md)                                                                                                        | Queue: 4,096 commands / 64 MiB media; short-term buffer counts bytes but evicts by duration. This is not the byte/GOP-bounded #172 design.                                                                                                                          |
| S1  | [`StorageSafetyPolicy::evaluate`, `filesystem_capacity`](../src/storage/safety.rs), [`WriterWorker`](../src/storage/engine.rs), [`RecordingCatalogHandle`](../src/storage/catalog.rs)                                                                                 | Shared health/metrics/storage editor; no independent age-retention capability. [Storage configuration](../book/src/configuration-reference.md).                                                                                                                     |
| E1  | [`ExportJobRecord` and export lifecycle](../src/server.rs), [evidence export lifecycle](evidence-exports.md), [operator export workflow](../book/src/recording-and-evidence.md#export-evidence)                                                                       | `keeppeek.media-export.v1`; Administrator and requester ownership still required. Capability presence does not promise permanent artifacts or transcoding.                                                                                                          |
| P2  | [`export_fragment_ranges_with_progress`, `browser_compatible_recording`](../src/storage/playback.rs), [`recorded-playback-policy.ts`](../ui/src/lib/recorded-playback-policy.ts)                                                                                      | Stored-media codec metadata and browser decode support determine selection; [recording workflow](../book/src/recording-and-evidence.md).                                                                                                                            |
| M1  | [`recording_reconciliation`, `apply_recording_reconciliation`](../src/storage/catalog/maintenance/reconciliation.rs), [`recover_recording_deletions`](../src/storage/catalog/maintenance/jobs/recovery.rs); [maintenance guide](../book/src/recording-maintenance.md) | `keeppeek.recording-maintenance.v1` requires catalog support and Unix/Windows; Administrator authorization and native filesystem checks still apply.                                                                                                                |

Capability identifiers above are read from [`server.rs`](../src/server.rs) and the existing
[`ServerCapabilities` contract](../api/webrtc.proto), not invented audit identifiers. The server
advertises no dedicated retention-rule, pre-roll, timelapse, or generic recording-override capability.
`keeppeek.rules.v1` and `keeppeek.mqtt-forwarder.v1` must not be used as substitutes. #136's completed
editor and #96's completed event ingestion are dependencies; neither implements the missing policy.

## Executed evidence

On 2026-09-20, Windows x86_64, Rust/Cargo 1.98.1, debug test profile, baseline above:

```text
cargo test --locked -p keeppeek --lib storage::
196 passed; 11 failed; 1 ignored; 0 measured; 921 filtered out
Test execution: 14.99 seconds. Exit status: 101 (not a passing suite).
```

The initial sandboxed build could not download the existing camera database. The network-enabled
retry built successfully. Compiler incremental-cache access warnings did not prevent execution.
No private camera configuration or media was used. The ignored case is the existing
`event_workflow_query_latency_measurement`, not a newly skipped test.

| ID  | Named tests (all under `storage::`, suffixes shown)                                                                                                                                                                                                                                                                                                                                                                | Observed assertions                                                                                                                                                                                                         |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T1  | [`engine::tests::recording_admission_enforces_modes_and_keyframe_aligned_event_boost`](../src/storage/engine.rs)                                                                                                                                                                                                                                                                                                   | Passed. Off rejects both; Sub/Main/Both select correctly. Event waits for main keyframe at +2 s; repeated events extend selection; return waits for sub keyframe at +152 s.                                                 |
| T2  | [`engine::tests::event_boost_round_trips_h264_h265_h264_with_audio_and_catalog`](../src/storage/engine.rs)                                                                                                                                                                                                                                                                                                         | Passed. Seven video samples with description indices `[1,1,2,2,2,1,1]`, three monotonic audio samples, seven fragments in one sub path, zero main fragments. Synthetic container/catalog evidence, not decoded-image proof. |
| T3  | [`layout::tests::segment_path_format`, `active_segment_path_format`](../src/storage/layout.rs)                                                                                                                                                                                                                                                                                                                     | Both passed; exact UTC fixture paths asserted.                                                                                                                                                                              |
| T4  | [`engine::tests::admission_and_enqueue_are_atomic_across_source_threads`, `full_writer_queue_drops_until_a_keyframe_can_be_enqueued`, `writer_queue_enforces_media_byte_capacity`](../src/storage/engine.rs)                                                                                                                                                                                                       | Passed; bounded admission and keyframe recovery.                                                                                                                                                                            |
| T5  | [`engine::tests::startup_cleanup_removes_only_oldest_catalog_media_to_recovery_target`, `cleanup_pauses_recording_when_no_eligible_media_remains`, `cleanup_delete_failure_is_actionable_and_capacity_recovery_resumes_recording`](../src/storage/engine.rs); [`safety::tests::reserve_accounts_for_non_keeppeek_disk_usage`, `filesystem_capacity_queries_the_nearest_existing_parent`](../src/storage/safety.rs) | All passed. Oldest eligible removal, export/unindexed preservation, protected-media pause, and capacity recovery asserted with synthetic capacity.                                                                          |
| T6  | [`playback::tests::export_preserves_timestamp_gap_between_indexed_recordings`, `export_preserves_mixed_codec_gop_descriptions`, `export_omits_overlapping_samples_across_recording_boundaries`, `compatibility_remux_repairs_audio_timescale_and_is_cached`](../src/storage/playback.rs)                                                                                                                           | Passed; timestamps, sample descriptions, deduplication, and compatibility container behavior. Lifecycle/UI evidence is historical H2/H3, not part of this filtered run.                                                     |
| T7  | [`engine::tests::startup_preserves_interrupted_recording_for_explicit_reconciliation`, `startup_preserves_unowned_active_files_in_both_media_roots`](../src/storage/engine.rs); [`catalog::maintenance::reconciliation::reindex::tests::queued_reindex_rejects_replaced_media_before_catalog_commit`](../src/storage/catalog/maintenance/reconciliation/reindex/tests.rs)                                          | Passed. Recovery preserves interrupted/unowned media; reindex rejects replaced identity. Native deletion qualification below failed.                                                                                        |

### Initial qualification gap: native Windows maintenance

Ten cases returned `PermissionDenied: recording path is not eligible for inspection`: nine
maintenance execution/recovery cases and the native NTFS primitive case. The checkout-archive case
returned `Unsupported: recording removal requires NTFS persistent ACLs and metadata flushing`.
The latter reproduces alone with:

```powershell
cargo test --locked -p keeppeek --lib storage::catalog::maintenance::jobs::execution::tests::checkout_archive_stages_and_removes_the_selected_recording -- --exact --nocapture
```

The equivalent already-built test binary was used for the isolated reproduction: one test executed,
one failed. `Get-Volume` identified the checkout volume as ReFS and the system volume as NTFS.
[`validate_directory`](../src/storage/long_term/inspection/removal/windows.rs) explicitly rejects
non-NTFS storage. The ten inspection failures require separate ACL/path qualification; the generic
error does not establish their precise cause. Do not describe all eleven as a ReFS diagnosis.
No filesystem checks were bypassed or permissions changed. #133 owns this qualification limitation;
re-run native maintenance on an approved NTFS test root before claiming this deployment supports it.

The later protected-NTFS canonical run recorded below passed these native tests. That resolves
the audit runner's inspection failure for that environment; it does not add ReFS removal support
or qualify every deployment's ACLs and directory ancestry.

### Historical owner evidence

These are identified earlier builds, not fresh final-baseline browser or device tests. The linked
PR bodies carry their acceptance tables; CI status was inspected on 2026-09-20.

| ID / owner                        | Exact evidence                                                                                                                                                                                                                                                                           | Result and qualification limit                                                                                                                                                                 |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H1 / #112, closed, POC            | [PR #163](https://github.com/xnorpx/keeppeek/pull/163), tested `e9a77493ab832e745e7a5e8169203a780f3148bb`; [Windows CI](https://github.com/xnorpx/keeppeek/actions/runs/32927695180/job/98053765677)                                                                                     | Success. PR documents eight threshold tests, cleanup/recovery tests, editor workflow, and a 64-segment benchmark. That workload does not satisfy #168's 127-source/30-day retention benchmark. |
| H2 / #113, closed, MVP            | [PR #189](https://github.com/xnorpx/keeppeek/pull/189), head `638e46e60eacaae1b1715ed2b75c10eb0333429e`; [Windows CI](https://github.com/xnorpx/keeppeek/actions/runs/33229773978/job/99040400013), [UI CI](https://github.com/xnorpx/keeppeek/actions/runs/33229773978/job/99040399905) | Success. PR body also names older local `ab4274e` evidence; do not mislabel it as the final head. Current lifecycle docs retain the 24-hour artifact limit.                                    |
| H3 / #111, closed, POC            | [PR #161](https://github.com/xnorpx/keeppeek/pull/161), local tests `284d0a555ee9c0f4ecc280ac500aa86fe5f3e927`, CI head `f587dd0aedd46a06908577048cba7f0dcbf08b23`; [UI CI](https://github.com/xnorpx/keeppeek/actions/runs/32917145263/job/98023120465)                                 | Success. Compatible variant selection and one visible fallback; two platform codec skips in the reported local E2E run. Not universal H.265/Safari qualification.                              |
| H4 / #133, closed, Alpha          | [PR #233](https://github.com/xnorpx/keeppeek/pull/233), tested `47c9e7ff1774982d9f5121e23b322110a19a3e57`; [Windows CI](https://github.com/xnorpx/keeppeek/actions/runs/34282344700/job/102253479337)                                                                                    | Success. PR explicitly leaves exhaustive parent crash/scope qualification incomplete. Local audit failures above remain visible despite historical success.                                    |
| Open implementation owners, Alpha | [#172][172] pre-roll; [#125](https://github.com/xnorpx/keeppeek/issues/125) privacy; [#127][127] timelapse; [#131][131] adaptation                                                                                                                                                       | No completion evidence claimed. Alpha membership is required, not automatic deferral.                                                                                                          |

### AC-6 owner review, 2026-09-21

AC-6 explicitly permits either linked completion evidence or a recorded limitation and owning
milestone. The audit therefore does not require implementing the open owners inside #168.
Issue state and checklist state are separate evidence: the counts below include both acceptance
and completion-contract checkboxes from each live issue body, not just its acceptance criteria.

| Owner                      | Live state / milestone | Checked / unchecked boxes | Evidence and remaining limitation                                                                                                                                                                                                                                                                                  |
| -------------------------- | ---------------------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [#112][112] safety         | Closed / POC           | 0 / 15                    | H1 and T5 verify the named threshold, cleanup, hold/pause and recovery behavior. The unchecked issue body does not establish complete qualification. Separate-volume protection remains D6; its existing safety benchmark is not AC-7 evidence.                                                                    |
| [#113][113] exports        | Closed / MVP           | 15 / 0                    | H2 supplies final-head Windows/UI CI; T6 and the qualified build below exercise export/container behavior. Artifacts expire after 24 hours; arbitrary FFmpeg options, encoder retry and permanent custody remain D5, not implied export capabilities.                                                              |
| [#127][127] timelapse      | Open / Alpha           | 0 / 14                    | No completed timelapse implementation/build is claimed. Bounded sparse review and generated timelapse remain this owner's Alpha work; normal-speed export does not satisfy them.                                                                                                                                   |
| [#131][131] adaptation     | Open / Alpha           | 0 / 14                    | H3 establishes earlier compatible-variant selection only. Adaptive switching/transcoding remains this owner's Alpha work. The qualified Chromium build still skips H.265 WebCodecs and mixed AVC/HEVC MSE, so neither it nor remux establishes universal codec support.                                            |
| [#133][133] reconciliation | Closed / Alpha         | 0 / 15                    | H4 explicitly leaves complete scope/relationship coverage, exhaustive crash cases, removed/gap UI qualification and some remedies incomplete. The current protected-NTFS gate passes existing regressions; ReFS removal remains unsupported. Closed issue state does not erase those documented Alpha limitations. |

Reproduce the owner-state review with `gh issue view NUMBER --json state,milestone,body,url` for
112, 113, 127, 131 and 133, and inspect the exact H1-H4 PR/build links above. The canonical
`3bf5ad5` build below is current regression evidence, not a replacement for missing owner tests.
This completes the owner-evidence review portion of AC-6 while keeping each unimplemented
capability and its milestone explicit. It does not complete AC-2/3/4/5/7 or approve D5/D6.

## Reproducible operator qualification

Use the identified build, synthetic/non-sensitive cameras, and an isolated archive. These procedures
are not reports of a manual run. Existing automated assertions above provide the narrow equivalence
evidence; deployment-specific qualification must record its build, camera/codec, OS/filesystem, and
browser versions. Never exercise low-space or deletion experiments against an evidence archive.

1. **W1 — admission:** In Camera configuration, apply `off`, then `sub`, `main`, and `both` to a
   dual-stream test camera. For each interval inspect Recording integrity and catalog coverage.
   Off admits no new main/sub frames; the others admit only selected streams. Already queued media
   may finalize after an edit. For EventBoost, emit a supported event and compare stream/GOP sequence
   with T1/T2; do not expect two simultaneous event files.
2. **W2 — time layout:** Run T3 with the exact UTC fixture. In an isolated recording root, compare
   finalized names with the source's UTC capture time. Do not infer local-time naming from the UI.
3. **W3 — playback:** Record a known H.264 sub and H.265 main. Open the same UTC moment in Keep on
   each target browser, record selected variant/fallback and decoded progress, then request a gap.
   The gap must stay explicit. No compatible retained stream means playback can remain unavailable.
4. **W4 — export:** As Administrator, export a retained interval shorter than two minutes, revisit
   history, and download/verify the MP4. Repeat the same request and inspect reuse, then select an
   interval with a known gap and review partial coverage. Follow E1's independent FFmpeg decode
   command on a host with the required tools. Download before 24-hour artifact expiry.
5. **W5 — recovery:** Follow the [maintenance guide](../book/src/recording-maintenance.md) on copied
   synthetic media. Preview catalog/file drift and preserve unknown files. Record filesystem
   qualification failures rather than forcing deletion. Restart must not authorize new removal.
6. **W6 — capacity:** Compare Recording integrity's attributed bytes with OS capacity for both
   configured media roots. Verify the mounted device, not only its directory name. Reproduce
   pressure using T5's injected capacities; expect oldest eligible cleanup, protected-media pause,
   and recovery after headroom returns. Recording bytes need not equal total disk usage.

The later implementation build `d7f209f` ran the real-media pipeline with
`KEEPPEEK_RUN_SLOW_TESTS=1` and
`cargo test --locked -p keeppeek --test storage_pipeline -- --nocapture`: all four tests passed
in 20.05 seconds. `three_tier_storage_pipeline` ingested 588 frames and produced six MP4 files;
`segment_moves_from_medium_to_long_term` observed zero medium and six long-term files;
`same_path_no_extra_copy` observed six files. `long_term_retention_limit` observed five files /
340,258 bytes before its legacy helper and zero afterward. This last test exercises the recursive
helper, not the catalog-driven pressure or new retention executor. None of these observations
proves independent decoding, pre-roll, or policy expiry; FFmpeg decoding remains outstanding.

## Decision ledger for Checkpoint A

Policy direction approved by the repository owner in the implementation session on 2026-09-20:

- Independent continuous, motion, and event rules inherit global defaults with camera overrides.
  Durations are precise integers; zero disables only the selected rule.
- Latest matching expiry wins. Keep each physical file once until all retained intervals expire.
  Accept file-boundary over-retention explicitly; do not introduce compaction or claim exact
  fragment-level physical-byte deletion.
- Preserve committed deadlines across policy edits. Later matching evidence may extend them;
  unavailable or already deleted media stays an explicit gap.
- Configured-off and privacy bound manual/external recording requests. Configuration editing
  remains distinct from temporary runtime overrides.
- Preserve evidence holds and pressure safety; new policy settings default to disabled.

The owner approved the concrete event/control semantics, additive API scope, and benchmark budgets
below with "ok please continue" in the implementation session on 2026-09-20. This approval allows
implementation; it does not constitute passing acceptance evidence.
Per the owner's instruction, do not open a PR until every issue step and acceptance criterion is
complete. Keep all #168 work in one PR.

The direction for D1/D3 and the configured-off/privacy bounds in D4 is approved above.
Detailed D2/D4/D7 semantics below are also approved. Export and mount decisions D5/D6 remain pending.
The baseline audit remains tied to the inspected commit. The subsequent implementation evidence
below is separate; it does not retroactively change a baseline classification.

### Approved implementation progress

The initial implementation added a bounded pure resolver and an additive `recording_retention`
catalog table. They do not activate retention or remove media. Later slices add validated,
default-disabled configuration and change recording admission as described below; cleanup
behavior is unchanged.

On 2026-09-20, the following command passed on Windows with Rust 1.98.1, using the executable
sources committed as `d7f209f` (resolver commit `7331173`):

```text
cargo test --locked -p keeppeek --test recording_policy_acceptance --test recording_retention_catalog
recording_policy_acceptance: 8 passed; 0 failed
recording_retention_catalog: 5 passed; 0 failed
```

- `recording_policy_examples_resolve_expected_deadlines` verifies the three example deadline
  oracles. It does not yet prove retained physical intervals or cleanup eligibility.
- The other resolver tests cover zero/sub-day duration, exact expiry, UTC boundaries, intersecting
  class/eligibility windows, ordering, duplicate evidence, checked arithmetic, and bounded inputs.
- `retention_deadline_survives_restart_and_zero_policy_without_losing_media` proves persistence
  and monotonic expiry with unchanged synthetic media bytes.
- `retention_conflict_incomplete_evidence_and_missing_media_leave_state_unchanged` checks missing
  catalog identities, incomplete watermarks, and competing compare-and-swap revisions.
- `shorter_deadlines_preserve_media_and_stale_policies_fail` verifies prospective policy safety.
- `incomplete_and_claimed_recordings_reject_retention_updates` rejects unfinished recordings,
  pending cleanup, and active maintenance claims without changing the prior snapshot or bytes.
- `legacy_catalog_migration_and_path_changes_preserve_retention_identity` verifies additive
  initialization and retention identity across a path change and restart.

These tests use synthetic catalog/media fixtures, not decoded event recordings. Catalog commits
preserve the maximum previous deadline and evidence watermark, reject stale policy revisions,
and check a two-second command deadline before transaction commit. A reply timeout requires
reloading state before retry. The stored watermark is a caller assertion; production evidence
collection, bounded reevaluation, and safe expiry execution remain to be implemented. No complete
acceptance criterion is inferred from these primitive tests.

Runtime-control commits `f403d2b` and `ee6a475` add the pure state resolver and apply it at the
same locked boundary as frame/event admission. Seven deterministic control tests, eight retention
resolver tests, five catalog tests, and 22 engine tests passed. These include exact TTL expiry,
clock invalidation, stale revisions, restart epochs, configured Off/privacy bounds, reconnect,
keyframe reacquisition, EventBoost remapping, and a full writer queue.

The subsequent additive API/UI slice provides `keeppeek.recording-control.v1`, administrator-only
get/set/clear, actor attribution, input validation, source-scoped revisions, and a camera-page pause
form. Generated TypeScript comes from the repository's Buf command. The capability does not
claim class retention or a production privacy provider. Chromium tests cover keyboard submission,
draft preservation, clear, capability/access loss, stale camera responses, and 320/768/1024/1440
pixel layouts. Final-commit command evidence remains required before closure.

The initial full `check.bat` run stopped after 666 passes at
`protocol_confirmation_rejects_wrong_text_and_reports_durable_deletion` (zero deletions instead
of one). The same executable passes that test and
`missing_staging_directory_is_not_proof_of_completed_removal` when `TEMP` and `TMP` point to a
dedicated NTFS directory protected with `.github/scripts/protect-test-directory.ps1`. The default
temporary directory's inherited permissions are insufficient for native removal. This does not
authorize weakening the removal boundary. The checkout-volume removal fixture additionally
requires NTFS; this checkout is on ReFS. Full qualification remains outstanding.

At `7467abc`, the protected NTFS worktree run of `check.bat` passed all 2,598 Rust tests
(21 skipped, 464.952 seconds). It then stopped at two `missing_const_for_fn` Clippy errors in
the new recording control code. This is partial verification, not a passing canonical check.
The subsequent bulk configuration regression passes with the seven recording-policy API tests:
all affected camera bounds are applied before attempting runtime activation, including offline cameras.

The next configuration slice adds default-disabled `[recording_retention]` settings with validated
global rules, sparse camera overrides, exact event source/kind mappings, and whole-candidate
validation. A regression reproduced direct-deserialization validation bypass before the validated
conversion fixed it. This slice does not activate expiration, prove producer completeness, or
complete the durable reevaluation worker. The configuration reference states that limitation.
At `e4dee83`, `cargo test --locked -p keeppeek --test recording_retention_configuration -- --nocapture`
passed all ten tests, and `cargo test --locked -p keeppeek --lib config::retention::tests -- --nocapture`
passed the atomic-write rejection test. Markdown validation and `mdbook build book` passed;
the book used the CI-pinned mdBook 0.5.4 and mdbook-mermaid 0.17.1 Windows release binaries,
verified against their release asset digests. The two Clippy corrections are committed at `1c751c8`;
the complete canonical check still needs to pass on the final build.

At `c9ca87a`, `cargo test --locked -p keeppeek --test recording_policy_acceptance --test recording_retention_configuration -- --nocapture`
passed nine resolver and ten configuration tests. Decisions include stable, validated rule IDs,
all current matches in deterministic order, and a reason distinguishing a current match from a
preserved committed deadline. Current matching IDs do not claim to explain the historical origin
of a preserved deadline. Persisting that attribution with durable reevaluation remains pending.

At `7df6b9e`, all eight `server::recording_policy::tests` passed. The new camera-removal regression
first failed with effective mode `Sub` after successful deletion. Removal now installs an `Off`
admission bound before requesting runtime shutdown; stale enable requests cannot revive it.

Producer completeness is still an explicit blocker for safe automatic event-based expiration.
`camera_events/lifecycle/history.rs` replay watermarks are in-memory deduplication state, not durable
coverage assertions. `catalog::insert_event` permits later revisions with changed intervals, and
the native event producer may end with an incomplete drain. Neither closed events, queue emptiness,
transport health, nor the latest observed timestamp proves that no backdated event remains.
The existing retention catalog primitive accepts a caller-supplied `evidence_through_ms`; that is
not trusted producer evidence and does not authorize deletion. The producer scope, durable ordering,
gap/restart invalidation, and handling of later revisions need a concrete completeness contract.
Automatic expiration remains inactive while that contract is unresolved.

The pure `Settings::normalize_event` adapter distinguishes unrelated canonical cameras/streams
from unavailable evidence. It accepts only an explicitly mapped source/kind with a closed,
nonempty interval; opaque camera IDs, malformed identity, unknown kinds, and open/invalid intervals
do not become retention facts. The result retains the event ID/revision even when a later revision
becomes unavailable. Camera-wide events can apply to both main and sub streams. This adapter does
not advance an ingestion watermark, activate a policy, or authorize deletion.
At `89e0d49`, the resolver/configuration/normalization suites passed 9/10/3 tests respectively;
the final normalization run also exercised empty event IDs and canonical IPv6 identities.
At `a374f3c`, the server policy suite passed all nine tests with deterministic UTC/monotonic
boundary observations, no revival after rollback, and rejection of a revision invalidated by expiry.
The injected clock is test-only; production admission still samples time inside its authority lock.
The canonical protected-NTFS run at `d985c5f` passed all 2,616 Rust tests (21 skipped) in
418.894 seconds, then stopped on two new Clippy style errors in normalization and the test clock
fallback. Commit `7b48784` applies the required `map_or` and lazy `or_else` forms. The complete
canonical check remains pending; the Rust pass alone does not complete the repository gates.

At `83cc65c`, the protected-NTFS canonical run passed all 2,616 Rust tests (21 skipped) in
373.219 seconds, Clippy with warnings denied, dependency checks, and formatting. UI validation
passed 389 Bun tests and 200 of 201 Chromium tests. The remaining storage migration story left
its radio unselected after a direct input click. Commit `3bf5ad5` clicks the visible native label
and explicitly asserts selection before retaining the existing review/save assertions; production
validation is unchanged. This verifies label activation, not the cause of the direct-click failure.
On that commit, `bun run quality:check` passed, including zero Svelte errors/warnings, 389 Bun
tests, all 201 Chromium tests, and 57 compatibility tests. The full canonical check is still required.

The first `3bf5ad5` canonical run passed the same Rust/UI gates and built the release E2E binaries,
then stopped because the Vite test server exceeded its 60-second startup timeout. No E2E case ran
in that attempt. After standalone Vite startup succeeded under the same environment,
`bun run test:e2e:run` passed 266 tests with two skips in 2.7 minutes, without a source change.
The skipped WebCodecs H.265 keyframe and mixed H.264/H.265 MSE cases remain unsupported-platform
qualification gaps; this result does not certify H.265 browser playback. A complete canonical
rerun remains required.

The subsequent uninterrupted `check.bat` run on the same `3bf5ad5d1d473456932349297ffc79f0954ca434`
source completed with exit code 0 on Windows 11 Pro 10.0.26200. It passed 2,616 Rust tests
(21 skipped, 426.950 seconds), Clippy with warnings denied, dependency/formatting checks,
389 Bun tests, 201 Chromium component/story tests, 57 compatibility tests, and 266 E2E tests
(two H.265 platform skips, 2.3 minutes). The worktree and protected temporary directory were on
NTFS; shared Cargo artifacts were on ReFS. `TEMP` and `TMP` selected the task's NTFS directory
protected using `.github/scripts/protect-test-directory.ps1`, and `CARGO_TARGET_DIR` selected the
shared build directory. Run `check.bat` from that worktree's root to reproduce the gate. Subsequent
audit-only edits do not change the qualified executable sources. This passing gate does not
complete the missing runtime retention, dependency, or benchmark acceptance evidence below.

### Owner clarification: indefinite recording and event preservation

On 2026-09-21, while resolving decision 1 about event completeness, the owner requested that a
recording or event can be marked to remain saved forever until the owner removes it. Record this
as acceptance of the conservative completeness direction plus a required operator-controlled
preservation outcome. It does not resolve the separate export and mount decisions D5/D6.

- Automatic event-based expiration requires durable producer evidence of completeness. Missing
  confirmation preserves footage rather than assuming no further event will arrive. Producer
  integration and its exact revision/gap contract remain implementation work.
- Provide an explicit **Keep forever** action for a recording or event. The protection has no TTL,
  survives restart, and prevents both ordinary age expiration and disk-pressure cleanup.
- Saving an event must protect its available associated recording media as well as the event
  information; saving only a bookmark or event label is insufficient. Already missing footage
  remains an explicit gap, and protection cannot authorize recording through privacy/Off bounds.
- Only an explicit authorized operator action can release this protection. Removing one hold must
  not remove another hold on shared media. Unprotecting media and confirming its deletion must
  remain distinct, visible actions so automatic retention consequences are clear.
- If protected media prevents capacity recovery, retain it and report the recording pause/storage
  pressure. Do not silently revoke protection to make space.

The catalog `set_recording_protected` primitive supplies the protection flag consulted by
cleanup/maintenance. The subsequent named-hold slice below adds durable recording-level
attribution, independent release and race/restart tests. It is not yet a complete operator feature:
no typed recording/event hold command or matching UI exists, and event/media association and
the complete operator workflow still need implementation.
The requested outcome is approved; any expanded protected API contract must follow the repository's
existing scoped approval requirement before those files are changed. No API file is changed here.

#### Proposed preservation contract extension

The earlier recording-control approval covers temporary per-camera recording requests. The
operator preservation workflow needs an additional, additive scope in `api/webrtc.proto` and
`api/webrtc.md`, with canonical regeneration of `ui/src/lib/proto/webrtc_pb.ts`:

- A typed preservation target selects exactly one stable recording ID or event ID; no host path
  or arbitrary SQL/filter is accepted.
- Typed read, save-forever and release commands use the existing authenticated control channel.
  Mutations require Administrator authority, a nonempty reason of at most 256 UTF-8 bytes and an
  expected revision; actor identity comes from the authenticated session.
- Responses distinguish unprotected, protected, pending and unavailable coverage, and expose
  revision, attribution, protected object/byte counts and gaps. A pending event projection cannot
  be presented as fully saved. Protecting one shared media object must preserve other active holds.
- A dedicated advertised capability gates the UI. Older servers show the action as unavailable;
  no unrelated command or metadata field is used as a fallback.
- Release removes only the selected preservation marker. It does not issue a media deletion
  command; the existing separately confirmed deletion workflow remains authoritative.

This is a reviewable proposed contract scope, not an implemented protocol or an approval record.
The storage primitive can be implemented and tested independently while this extension awaits
the repository-required explicit API approval. Export-copy lifetime remains the separate D5 decision.

#### Verified recording-hold storage slice

Commit `e752bf5` adds `catalog::holds` behind the existing serialized writer. Hold state has no
TTL and retains its revision after release. A recording can retain at most 256 named hold
identities, including released identities; existing identities can still be released/reactivated
at that limit. IDs and actors are bounded to 128 UTF-8 bytes, reasons to 256, and requests use
the existing bounded queue and two-second deadline. These are internal storage limits, not a
claim that the operator API exists.

The first active hold captures prior independent protection; the last release restores it.
Legacy flag changes are rejected while named holds are active. Finalized media with a known end
can acquire a hold only before cleanup or maintenance claims it. Existing cleanup selection
continues to consult the effective flag. Catalog deletion and clearing that flag are also fenced
while a named hold remains active. The new tables are additive and keyed by recording identity,
so moving the owned media path does not discard protection. Arbitrary direct database edits are
not a supported preservation interface.

On Windows, `cargo test --locked -p keeppeek --lib storage::catalog::holds::tests -- --nocapture`
passed all nine tests (0.66 seconds). They cover overlapping holds, restart, legacy protection,
stale/inactive release, cleanup-first ordering, active/unknown-end/maintenance-claimed rejection,
UTF-8 limits, the retained-identity cap, revision overflow, transaction failure rollback, additive
migration, media relocation and expired actor requests. The same executable passed these five
existing regressions with `TEMP`/`TMP` set to the protected NTFS test directory:

- `storage::engine::tests::cleanup_pauses_recording_when_no_eligible_media_remains`
- `storage::engine::tests::cleanup_delete_failure_is_actionable_and_capacity_recovery_resumes_recording`
- `storage::engine::tests::startup_cleanup_removes_only_oldest_catalog_media_to_recovery_target`
- `storage::catalog::tests::cleanup_candidates_exclude_active_and_protected_recordings`
- `storage::catalog::maintenance::jobs::tests::active_protected_pending_unknown_end_and_empty_scopes_cannot_be_prepared`

`cargo clippy --locked -p keeppeek --lib --tests -- -D warnings`, Rust formatting and Markdown
validation passed. These focused results do not extend the earlier canonical `3bf5ad5` gate to
the new implementation or complete event preservation, API/UI, retention evaluation or AC-7.

### Approved event and control semantics for the runtime checkpoint

This section is the approved implementation contract. The primitives described above have
execution evidence; production retention integration remains incomplete.

Event classification uses explicit administrator mappings from `TimelineEvent.source` and exact
`TimelineEvent.kind` to motion, alert, detection, or active-object evidence. Do not infer an alert
from a detection label, bounding box, confidence, or arbitrary payload. Match the stable camera
identity and, when present, the logical stream. A camera-wide event can apply to either configured
stream. Limit mappings to 16 per camera and each event kind to 128 UTF-8 bytes. Unknown kinds and
events without a closed, valid UTC interval report unavailable evidence. They cannot establish an
evidence-complete deletion decision. Producer health and a durable ingestion watermark must
establish evidence completeness separately from the absence of matching events.

Event revisions invalidate only the intersecting source/time range. Reevaluation preserves prior
committed deadlines; retraction cannot shorten them. Requested pre/post coverage is distinct from
available decodable coverage, with #172 supplying pre-roll and its gap reasons. Policy edits are
prospective unless bounded reevaluation is explicitly requested; disabling policy stops new age
expiration and preserves catalog rows and media.

| Input/state                                | Proposed effective result                              | Restart/expiry rule                                                            |
| ------------------------------------------ | ------------------------------------------------------ | ------------------------------------------------------------------------------ |
| Configured Off, any request or event       | Off; reason `configured_disabled`                      | Persist configured mode only                                                   |
| Privacy active, any permitted mode/request | Off; reason `privacy`                                  | #125 supplies authoritative privacy state; unknown required state fails closed |
| Enabled configuration, no request          | Configured mode; reason `configuration`                | Rebuild from validated config                                                  |
| Valid manual/external disable              | Off; expose actor, source, reason, expiry and revision | Temporary request is cleared at restart                                        |
| Valid manual/external enable               | Configured mode, within privacy bound                  | Temporary request is cleared at restart                                        |
| Expired request                            | Configured mode, within privacy bound; report expiry   | Expire at the earliest of UTC or monotonic deadline                            |
| Concurrent/stale request revision          | Reject without changing the applied state              | Client reloads before retry                                                    |
| Clock correction                           | Never extend a request's monotonic lifetime            | Invalid clock state suppresses the override                                    |

Accept only one current request per camera, replaced through an expected-revision operation.
Require a nonempty reason (at most 256 UTF-8 bytes), authenticated actor, and a positive TTL at most
24 hours. No general scheduler or Home Assistant/MQTT adapter is proposed: a future approved
adapter must use this same operation. Configuration edits remain the existing durable settings
operation. After permission resumes, admission must reacquire a video keyframe before writing
dependent frames. A new instance uses a new revision epoch so an old client request cannot match
a reset counter.

Approved additive contract scope: `api/webrtc.proto` and `api/webrtc.md`, with regenerated
`ui/src/lib/proto/webrtc_pb.ts`; typed get/update/set-override/clear-override commands, policy and
effective-state responses, stale-revision checks, Admin-only mutations, and a recording-policy
capability identifier. Existing fields remain unchanged. The approval covers only this additive
scope under `AGENTS.md`.

Approved AC-7 budgets: at most 256 rows and two seconds per batch;
one-event reevaluation p95 at most 250 ms; full reevaluation p95 at most 60 seconds; additional
peak memory at most 256 MiB; ingest p95 regression at most 5%; at most four SQL statements per
evaluated recording plus eight per batch. Use the 127-source, 30-day main/sub fixture, warm-up,
and at least 30 release runs. Report baseline, result, delta, query counts, peak RSS, environment,
and raw summaries; a failing measurement cannot silently change an approved budget.

| ID  | Decision status                                                                                                                                                                            | Consequence / current workaround                                                                                                                                                                      | Accountable owner |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------- |
| D1  | Approved: independent class lifetimes, integer durations, latest-match expiry, and global/camera overrides.                                                                                | Existing capacity limits cannot guarantee an age or the three examples. Export/download specific evidence; do not advertise class retention.                                                          | #168              |
| D2  | Approved: explicit source/kind mappings and durable producer completeness; preserve when confirmation is missing. Operator-controlled Keep forever for recordings/events is also required. | Completeness ingestion and the operator preservation workflow remain unimplemented. Event labels alone are insufficient; #96 supplies ingestion inputs and #172 owns decodable pre-roll.              | #168              |
| D3  | Approved: whole-file retention and no shortening of committed deadlines.                                                                                                                   | Shared MP4 files can require over-retention; exact retained bytes may require separately approved compaction. No duplicate-object or exact-expiry result is claimed.                                  | #168              |
| D4  | Approved: temporary manual/external authority, configured/privacy bounds, attribution, expiry and restart semantics.                                                                       | Existing config edits change mode. They are not expiring automation requests; #125 covers privacy only. Temporary get/set/clear protocol is approved; production privacy integration remains pending. | #168              |
| D5  | Accept typed export/transcoding outcomes or approve deliberate differences for arbitrary arguments, hardware retry, and permanent export custody.                                          | Download normal exports promptly. #127 owns timelapse; #131 owns playback adaptation, not an implied arbitrary export service.                                                                        | #168              |
| D6  | Decide independent capacity/mount qualification for separated medium/long storage.                                                                                                         | Check both mounted volumes operationally; do not infer protection of one from the other's capacity.                                                                                                   | #168              |
| D7  | Numeric latency, query-count, memory and ingest-impact budgets approved; measurement remains outstanding.                                                                                  | Existing safety/coverage benchmarks measure different paths. Numeric budgets are approved above; production evaluator and benchmark evidence remain pending.                                          | #168              |

Required control cases for D4 are: Off + event stays Off (current P1); configured enabled + privacy
must suppress recording (pending #125); manual/external enable cannot bypass a disabled/privacy
bound (verified by pure and admission tests); expired request/restart/conflicting revisions resolve
through the new authority. Production privacy integration remains incomplete. Configuration edits and runtime override permission
must be distinguished before treating `off` as a hard upper bound.

After D1–D3 approval, the acceptance fixtures must specify half-open UTC intervals, source/stream
identity, exact retained intervals and physical bytes, zero/sub-day boundaries, overlapping events,
late revisions, restart, clock corrections, keyframe expansion, and holds. No speculative fixture
is presented as passing production behavior.

## Acceptance status

| Criterion | Audit result                                                                                                      | Remaining closure evidence                                                                          |
| --------- | ----------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| AC-1      | All retrieved headings mapped to 32 classified rows with baseline, symbols, evidence/owner, and workflow.         | Final-build matrix review remains pending; implementation decisions are recorded above.             |
| AC-2      | Pure-resolver examples verified; production interval/physical-byte outcomes remain unverified.                    | Executed exact interval/physical-byte fixtures for the approved semantics.                          |
| AC-3      | Pure resolver and monotonic catalog primitive verified; production expiry/reevaluation remains incomplete.        | Production evidence ingestion, bounded reevaluation, safe expiry, and final migration tests.        |
| AC-4      | Keyframe EventBoost foundation passes T1/T2; pre-roll and independent event retention remain missing.             | #172 real-media decoded coverage plus delayed/revised-event evidence.                               |
| AC-5      | Temporary authority, admission, API and UI implemented; full integration is incomplete.                           | #125 integration and final-build deterministic-clock admission/API/UI qualification.                |
| AC-6      | Owner evidence reviewed on 2026-09-21; exact builds, checklist discrepancies and milestone-owned limits recorded. | Final matrix review; D5/D6 remain separate owner decisions, not implicit parity claims.             |
| AC-7      | Not measured or satisfied.                                                                                        | Repeated release-build results against approved D7 budgets: median/p95, queries, RSS, ingest delta. |

Performance for the baseline documentation commit is N/A. Subsequent executable changes require
AC-7 measurements; those measurements are outstanding. Keep all incomplete issue criteria unchecked.
Do not open the issue's single PR until the required implementation, dependencies, and verification
are complete.

[96]: https://github.com/xnorpx/keeppeek/issues/96
[112]: https://github.com/xnorpx/keeppeek/issues/112
[113]: https://github.com/xnorpx/keeppeek/issues/113
[122]: https://github.com/xnorpx/keeppeek/issues/122
[127]: https://github.com/xnorpx/keeppeek/issues/127
[131]: https://github.com/xnorpx/keeppeek/issues/131
[133]: https://github.com/xnorpx/keeppeek/issues/133
[168]: https://github.com/xnorpx/keeppeek/issues/168
[172]: https://github.com/xnorpx/keeppeek/issues/172
