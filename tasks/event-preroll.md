# Issue 172: bounded event pre-recording

Issue: https://github.com/xnorpx/keeppeek/issues/172

Implementation base: `7ab761b`, branch `feat/172-event-preroll`.
The issue's AC-1 through AC-10 remain the completion contract. This plan records
work and decisions; it does not claim acceptance evidence.

## Current source findings

- `CameraRecordingPolicy` selects live frames before storage enqueue. EventBoost
  changes main/sub quality within one logical sub recording.
- `RecordingAdmission` already serializes policy and enqueue decisions. Extend
  that owner and preserve its privacy and disabled-camera checks.
- The existing short-term buffer does not supply byte-bounded complete GOP
  history. The writer and catalog already support media configuration changes.
- Camera configuration supports five recording modes and post-event duration.
  Pre-roll needs additive configuration, wire mappings, UI, and diagnostics.
- The earlier state-store and privacy implementation PRs have merged. Doorbell
  work has an existing PR; do not create a second PR for it.

## Decisions before integration

1. EventBoost with pre-roll enabled needs a bounded delay before committing sub
   frames. Otherwise previously committed sub frames overlap retrospective main
   frames. Proposed behavior: hold candidate sub/main GOPs for the requested
   horizon, select one monotonic output, and release continuous sub coverage
   early when pressure requires it. The uncommitted horizon can be lost on crash.
   Default zero pre-roll preserves current persistence behavior. Owner decision
   is required by the issue before connecting this to live admission.
2. The hard history ceiling is 30 seconds. A preceding keyframe older than that
   ceiling is unavailable, even when it would cover the requested cutoff. Report
   shortened coverage. Event-only writes no samples at/after the exclusive
   post-event deadline; finalize the retained GOP prefix without extending the
   accepted recording window.
3. Protected API scope must be approved for this task before edits: additive
   EventOnly mode, event stream selection, pre-roll duration, storage memory
   limits, and requested/available/reason diagnostics. Keep existing tags and
   behavior. Prepare the concrete field map before asking for authorization.

## Ordered work and evidence

- [ ] Record owner decisions and the exact additive API proposal.
- [ ] Establish regression fixtures for all five legacy recording modes.
- [ ] Implement a pure GOP history primitive with duration, stream-byte, global-
      byte, and metadata bounds. Cover open GOP accounting, malformed order,
      audio trimming, configuration epochs, and deterministic whole-GOP eviction.
- [ ] Checkpoint: primitive invariants and independent design review pass.
- [ ] Integrate event-only with the existing admission and writer owners. Prove
      idle creates no files/rows and overlapping events write each frame once.
- [ ] Integrate the approved EventBoost commit horizon. Prove one recording,
      monotonic frame order, continuous sub coverage, and pressure fallback.
- [ ] Checkpoint: real H.264/H.265 output decodes; privacy, pause, reconnect,
      shutdown, writer failure, and event storms preserve safety.
- [ ] Extend configuration/default/template/backup validation and the approved
      API. Prove old configuration compatibility and atomic invalid-write failure.
- [ ] Add accessible editor/setup controls and server-owned diagnostics, with
      desktop/mobile browser, keyboard, and save/reload/restart evidence.
- [ ] Measure disabled/enabled ingest and flush behavior over at least 30 release
      runs at 1080p/4K and 127-source pressure. Report median/p95, bytes, and the
      issue's 5% enabled ingest budget against the same-host baseline.
- [ ] Synchronize operator/configuration documentation and rollback guidance.
- [ ] Run the canonical Windows check, relevant slow real-media tests, review,
      and final acceptance table before opening the sole implementing PR.

## Work checkpoint

The isolated `storage::pre_record` foundation has 13 passing tests for whole-GOP
eviction, per-stream/global byte limits, metadata limits, replay reservations,
drop ordering, stale history, malformed order, and bounded registration. Its
initial three tests failed before implementation; the stale-arrival and excess
eviction regressions also have observed red/green results. Independent review
has no remaining findings for that scoped foundation.

Canonical Cargo verification passed all 13 foundation tests, the existing
five-mode admission regression, and the existing H.264/H.265/H.264 EventBoost
roundtrip with audio and catalog assertions. Logs are in the ignored target
directory (`preroll-foundation-final.log`, `preroll-legacy-baseline.log`). The
standalone harness compiled these same source files for rapid red/green cycles;
it does not replace the canonical Cargo results.
`cargo clippy --locked --lib --tests -- -D warnings` also passed. Formatting and
diff checks passed. These are checkpoint results, not full-issue qualification.

The module remains test-only until integration is authorized and complete. Audio
window trimming, decoder/session epochs, diagnostic reason codes, bounded replay
scheduling, runtime integration, configuration, API, UI, and performance evidence
remain unfinished. No issue acceptance criterion is marked complete. No PR has
been opened, and no protected API source has changed.

The EventBoost persistence-delay decision is pending. The additive API proposal
is in `docs/pre-recording-contract-proposal.md`; its protected edits also require
current-task approval. Existing generic task plans were preserved.

## Constraints

Independent design review identified the following integration requirements:

- Charge replay memory until frames leave every pending pre-roll stage; draining
  into a detached vector cannot release the reservation while bytes remain live.
- Reolink currently selects one audio stream before storage. Preserve candidate
  audio before selection, and ensure the writer's initial track includes audio.
- Compare audio sample intervals with video coverage. Drop whole encoded audio
  packets that cross an exclusive boundary; do not imply transcoding or exact
  sample trimming.
- Distinguish closed GOPs from a replayable keyframe-starting open prefix. Moving
  that prefix must preserve dependent-frame continuation and single ownership.
- Carry a decoder/session epoch, not only codec and resolution. Validate starting
  decoder parameters using the existing writer's media parsing rules.
- Expire against caller-supplied current time before replay and diagnostics,
  including silent streams; clear history on privacy/reconnect/reconfiguration.
- Bound replay work per scheduler turn and prioritize continuous recording over
  optional history. Include concurrent replay in performance measurements.

Reuse encoded payloads and the existing writer, queue, catalog, and event path.
No detector, transcoder, second recording writer, or persistent pre-roll cache.
Settings stay in `config.toml`; secret references stay intact. Disabled pre-roll
does not allocate a history buffer. Privacy activation must release and fence
history as well as live output. Optional history may be shortened or discarded;
it cannot block camera workers or continuous recording.

Performance and memory assertions need measured evidence, not inferred results.
Do not check acceptance items or open a completion PR while any required result
is failed, blocked, or unverified.
