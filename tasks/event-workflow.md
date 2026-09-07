# Issue #121: Durable Event Review Workflow

## Contract

Reviewed and dismissed are independent per-principal flags. Unreviewed means neither flag is set.
Authenticated users use the server credential ID. Trusted-LAN clients use a persistent browser
workspace UUID; it is not an authentication or privacy boundary. Event revisions preserve review.
One shared bookmark per stable source/event pair has an independent CAS revision, creator,
timestamps, bounded plain-text note, and bounded audit history. Only its creator or an administrator
can edit or remove it. Bookmarking never changes recording protection or deletes evidence.

The user authorized additive changes to `api/webrtc.proto` for this issue. All existing field
numbers and meanings remain unchanged. Other protected API files remain unchanged.

## Bounds and Risks

- Bulk actions contain 1 to 128 unique explicit event targets and expected revisions. A conflict
  rolls back the whole batch. Visible means the current rendered page, never hidden pages.
- IDs contain at most 256 UTF-8 bytes. Notes contain at most 1,024 UTF-8 bytes. One event has at
  most 64 principal review records and one shared bookmark.
- The catalog bounds total workflow rows and retained audit/tombstone data independently of
  recording retention. Missing media is reported as metadata-only, not as playable evidence.
- Queries remain server-filtered and bounded to 31 days and 128 hits. Counts cover the effective
  authorized query, not the visible page. Cursor identity includes the principal and predicates.
- The database worker owns mutations. Search uses its existing separate connection. No per-event
  network requests or global client-side event scans are introduced.
- Measure a fixed mixed-principal catalog workload before and after workflow query integration;
  report p50/p95 latency, sample count, payload size, environment, and command. The initial
  local query budget is 100 ms p95 and a 128-target mutation budget is 250 ms p95.

## Ordered Tasks

1. Durable review slice: catalog CAS batches and restart/revision/principal tests, authenticated
   control command, trusted-LAN identity test. Dependencies: existing catalog and access policy.
2. Shared bookmark slice: create/edit/remove, bounded audit, conflict/ownership tests, retained
   references, deleted-event and tombstone cleanup tests. Depends on task 1.
3. Query slice: composable workflow filters and authoritative counts, stable pagination, authorized
   mixed-principal tests and performance measurements. Depends on tasks 1 and 2.
4. Events slice: card/detail actions, selection, explicit visible/selected bulk and undo, filters,
   optimistic rollback and retained drafts. Depends on task 3.
5. Related surfaces: Keep stories/timeline/detail and export handoff show bookmark state and retain
   source relationships without implying a hold. Depends on task 4.
6. Verification: focused Rust/UI tests, production-process Playwright desktop/mobile and keyboard,
   back navigation and conflict tests, adversarial review, operator docs, and `./check.sh`.

## Progress and Evidence

- [x] Catalog review isolation, conflict, event-revision preservation, and restart regression passes.
- [x] Authorized workflow control commands and stable local identity pass.
- [x] Shared bookmark ownership, audit, conflict, and bounds pass.
- [x] Retained media, deleted events, and tombstone cleanup pass; library reports missing sources.
- [x] Server filters/counts and bounded pagination pass with mixed principals and permissions.
- [x] Events card/detail, visible/selected actions, undo, and optimistic rollback pass.
- [x] Keep/timeline/detail/export integration typechecks and focused timeline tests pass.
- [x] Browser keyboard/mobile/back-navigation/filter-race/export tests pass.
- [x] Before/after performance evidence meets the stated budgets.
- [x] Adversarial review findings were reconciled and actionable defects have regression tests.
- [ ] Canonical `./check.sh` passes on the final tree.

The issue stays open until every requirement has observed evidence. The user authorized a feature
branch, commit, push, and PR on 2026-09-06. Merge and issue closure are not authorized.

## Initial Performance Baseline

`cargo test --locked --lib storage::catalog::tests::event_workflow_query_latency_measurement -- --exact --nocapture`
measured the pre-workflow-query implementation on macOS arm64 in the debug test profile: 1,024
events, eight sources, two selected sources, 18-hit pages, 30 runs. p50: 10.568 ms; p95: 12.903 ms.
The same harness and profile must measure the final ordinary-query path and the added workflow path.

## Performance Investigation

The first workflow-enabled run measured 466.402 ms query p95 and 251.594 ms mutation p95, exceeding
the budgets. A joined aggregate reduced query p95 to 144.467 ms. Aligning the review index with
principal/source/event identity reduced workflow query p50/p95 to 23.927/24.374 ms and 128-target
mutation p50/p95 to 193.861/198.388 ms. Ordinary query p50/p95 was 10.295/10.898 ms. Final-tree
measurements follow the remaining correctness checks; these intermediate results are not final evidence.

## Review Record

A fresh-context reviewer examined the workflow storage/server/UI contract. Its proposed deleted-event
search mutation would update a nonexistent event; bookmark-library revisions already change through
triggers. Its race proposals omitted the surrounding immediate transaction. Audit history is bounded
to 16 entries per bookmark and cascades with reference cleanup. A session-only UUID fallback would
violate durable local identity. These findings did not warrant those proposed changes.

Local adversarial tests exposed and fixed retry identity crossover, stale file-availability claims,
misclassified storage errors, and lost selection/filters on Back navigation. External cross-model
review was offered; the user was unavailable and directed autonomous continuation, so no external
CLI or credential flow was invoked.

A second bounded review identified the need for explicit control-response sizing. Responses now
reserve envelope headroom below 64 KiB, and retained bookmark event types are validated before
insertion. Local follow-up tests also protect concurrent dismissal during a review retry and
bookmarked representatives inside dense timeline clusters.

## Acceptance Criteria Verification

| Criterion                                                                          | Observable outcome                                                                                                                      | Verification                                                                                                                                                        | Observed result               |
| ---------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------- |
| Durable principal review/dismissal; authorized shared bookmarks                    | Independent credentials and LAN workspaces retain their own flags; shared notes enforce creator/admin writes                            | Rust `event_workflow_review_is_principal_scoped_revisioned_and_durable`, `event_workflow_server_enforces_principal_and_camera_boundaries`; real workflow Playwright | Pass                          |
| Single, selected, and explicit visible scope                                       | An 18-card bulk action changes 18 of 24 records; selected actions preserve hidden-page state; undo uses acknowledged revisions          | Rust `event_workflow_bulk_conflict_rolls_back_and_explicit_undo_preserves_other_events`; desktop workflow Playwright                                                | Pass                          |
| Authoritative state, filters, and counts                                           | Mixed-principal counts precede pagination and honor source/creator predicates across metadata, text, and semantic search                | Rust `event_workflow_query_filters_and_counts_precede_pagination`, `event_workflow_text_and_semantic_filters_keep_authoritative_counts`, bookmark-library test      | Pass                          |
| Conflicts preserve intent                                                          | Whole batch rollback, current CAS state, retained note draft, explicit retry, and unrelated newer flags preserved                       | Rust conflict tests; Svelte workflow/controls tests; two-tab mobile Playwright                                                                                      | Pass                          |
| Bookmark, media, export, and hold remain distinct                                  | Missing files report metadata-only; bookmarks do not protect recordings; exports keep historical bookmark revision after removal        | Rust `event_workflow_bookmark_never_pins_recording_or_claims_a_missing_file`, `export_job_runs_reports_gaps_and_downloads_verified_file`; export-handoff Playwright | Pass                          |
| Restart/deletion remain understandable                                             | Reopen retains flags and bookmark audit; deleted references remain queryable and expire after 90 days without resetting live tombstones | Rust durable-review/bookmark, deleted-reference cleanup, and bookmark-library tests                                                                                 | Pass                          |
| Multi-principal, bulk, conflict, retention, accessibility, and end-to-end coverage | Keyboard actions, 44px touch controls, 320/390/768/1440 layouts, browser Back, filter changes, selection restoration, and safe notes    | 14 focused Rust tests; Svelte browser tests; 17 Events/Keep Playwright cases                                                                                        | Pass; canonical suite pending |

`bun x buf breaking ../api --against '../.git#branch=HEAD,subdir=api'` passes. The WebRTC changes
are additive. No public field numbers or existing meanings changed.

## Pre-Batch Performance Evidence

Environment: macOS 26.6.2 (25G83), arm64 Apple M5 Max, Rust 1.97.1
(`a7727ddedd`, ms-prod build), Bun 1.4.0. Debug test profile; 30 samples per metric. The workload
contains 1,024 events across eight sources, two authorized source filters, two review principals,
18-hit query pages, and repeated atomic 128-target mutations. No media bytes are fetched.

Command: `cargo test --locked --lib storage::catalog::tests::event_workflow_query_latency_measurement -- --exact --nocapture`.
The same test is also included in the `event_workflow` focused test run and canonical Nextest suite.

| Metric                                       |               Baseline p50 / p95 (ms) | Result p50 / p95 (ms) |          p95 delta | Budget |
| -------------------------------------------- | ------------------------------------: | --------------------: | -----------------: | -----: |
| Ordinary metadata query                      |                       10.568 / 12.903 |       10.089 / 10.352 | -2.551 ms (-19.8%) | 100 ms |
| Workflow state and authoritative-count query | 10.568 / 12.903 without workflow work |       20.708 / 21.179 | +8.276 ms (+64.1%) | 100 ms |
| Atomic 128-event review mutation             |   No previous mutation implementation |     106.465 / 111.005 |      New operation | 250 ms |

The workflow query's additional work costs about 8.3 ms p95 on this fixture. Query and mutation
tests keep their original budgets. Maximum control responses are checked below 64 KiB; batches
also bound aggregate identity bytes to 16 KiB.

## Canonical Gate Follow-up

The first completed canonical run stopped after 959 of 2,273 Rust tests: 958 passed and the
workflow mutation benchmark failed at 259.312 ms p95 against its unchanged 250 ms budget. The
implementation then removed redundant event-existence and post-write hydration queries from each
review mutation. A fresh database read independently verifies the acknowledged state.

The corrected four-worker Nextest workflow slice passed all 14 tests, with query p95 23.781 ms and
mutation p95 124.536 ms. The same-command serial measurements are in the table above. A subsequent
eight-worker canonical run passed the mutation budget at 187.085 ms p95 but exceeded the query
budget at 117.826 ms p95. Search hydration now batches review and bookmark rows for the page,
omits unused audit reads, and retains bounded filesystem checks for linked media. An independent
point read verifies the batched state before and after media removal.

The final eight-worker workflow slice passes all 14 tests. The neighboring storage and WebRTC
workload passes all 169 tests. Both commands use `cargo nextest run --locked -p keeppeek --lib
--features macos-test-aws-crypto --test-threads 8 --success-output immediate`, with filter
`-E 'test(event_workflow)'` or `-E 'test(storage::) or test(webrtc::)'`, respectively. The fixture,
sample count, hardware, profile, and budgets match the earlier measurements; the runner and
concurrent workload differ from the serial baseline and are reported separately.

| Metric                          | Workflow slice p50 / p95 (ms) | Storage/WebRTC workload p50 / p95 (ms) | p95 budget |
| ------------------------------- | ----------------------------: | -------------------------------------: | ---------: |
| Ordinary metadata query         |               10.173 / 10.657 |                        10.427 / 10.966 |     100 ms |
| Workflow state and counts query |               16.885 / 17.551 |                        18.256 / 23.427 |     100 ms |
| Atomic 128-event review         |             106.890 / 111.637 |                      118.556 / 129.021 |     250 ms |

The latest canonical run has passed all 2,273 Rust tests, with 20 existing skips, including the
unchanged workflow performance budgets. Its remaining Clippy and UI phases are still pending at
PR preparation. Strict package Clippy and the prior complete UI quality phase pass: 297 Bun tests,
141 browser/visual tests, 57 compatibility tests, formatting, lint, Svelte/E2E typechecks, Paper,
and harness checks. No threshold or test has been disabled. Keep the PR in draft until the full
canonical gate and the remaining template requirements have passing evidence.

Builds use the supported `KEEPPEEK_CAMERA_DATABASE_ARCHIVE` override when public network access is
unavailable. The local v2.8.0 archive SHA-256 is
`9b86ff8d4afa8721ab115e3fd0b04ca33a4b28e5b519d2283ea2ef68a0c8f009`, matching the build's pinned digest.
Dependency manifests, registries, and quality thresholds remain unchanged.
