# RepoWise Decision and Health Ledger

This tracked file is KeepPeek's durable record of architecture and tooling decisions plus
longitudinal RepoWise health snapshots. The generated `.repowise/` directory is local and ignored;
it is a derived index, not institutional memory.

## Ledger Rules

- Product feature ranking and owner approval remain in [features.md](features.md). Do not duplicate
  proposals from that file here as active architecture decisions.
- RepoWise findings are advisory evidence, not decisions. A score or hotspot never changes product
  or architecture policy by itself.
- Add a decision as `Proposed` until the owner explicitly approves it. Change its status to `Active`
  only after approval.
- Never delete an old decision. Mark it `Deprecated` or `Superseded` and link the replacement so the
  reason for the transition remains visible.
- Keep detailed behavior in the linked source document. This ledger records the choice, rationale,
  consequences, affected surface, and evidence.
- Replace the current snapshot after a fresh index, but append one score-history row. Never rewrite
  earlier history.
- Record the RepoWise version and indexed commit with every score. Treat a tool-version change as a
  methodology change, not automatically as a code-quality change.

## Current Snapshot

### Provenance

| Field                       | Value                                          |
| --------------------------- | ---------------------------------------------- |
| Recorded                    | 2026-08-27                                     |
| Health analyzed             | `2026-08-26T15:47:40.482583`                   |
| Indexed commit              | `caf3545c827547e8a537c444dc8d6cf139f948fc`     |
| Index behind committed HEAD | No                                             |
| Working tree represented    | No; this snapshot describes the indexed commit |
| RepoWise CLI                | 0.44.0                                         |
| Repository pin              | 0.45.0 from `.github/workflows/repowise.yml`   |
| Index mode                  | Local, no provider, no model, 1,719 pages      |

The CLI was older than the repository pin when this baseline was captured. The first 0.45.0
snapshot should be recorded as a new baseline rather than interpreted as a pure code delta.

### Health

RepoWise scores are on a 0-10 scale where higher is healthier.

| Metric                   | Value | Interpretation                                                       |
| ------------------------ | ----: | -------------------------------------------------------------------- |
| Average health           |  6.87 | NLOC-weighted headline score; `warning` band                         |
| Code-only average health |  6.54 | Excludes 208 non-code files                                          |
| Hotspot health           |  3.73 | Health of historically high-change files                             |
| Maintainability average  |  6.94 | Separate maintainability dimension                                   |
| Performance average      |  9.70 | Separate performance dimension                                       |
| Healthy files            |   913 | 50.5% of analyzed NLOC                                               |
| Warning files            |   107 | 27.1% of analyzed NLOC                                               |
| Alert files              |    22 | 22.3% of analyzed NLOC                                               |
| Open findings            | 3,235 | Diagnostic findings, not accepted work items                         |
| Files below target 8     |   129 | Weighted gap: 254,167 score-points times NLOC                        |
| Worst performer          |  1.00 | [ui/src/lib/stream-peer.svelte.ts](ui/src/lib/stream-peer.svelte.ts) |

RepoWise's 180-day defect-ranking self-check found 18 of the top 20 ranked files in the recent
defect set: precision `0.90`, base rate `0.0653`, and lift `13.79`. This validates prioritization on
the indexed history; it is not a guarantee that a particular file contains a defect.

### Governance

| Metric                    | Value |
| ------------------------- | ----: |
| Active RepoWise decisions |     0 |
| Proposed decisions        |     0 |
| Stale decisions           |     0 |
| Ungoverned hotspots       |    87 |

The local RepoWise decision store was empty when this ledger was introduced. The decision register
below is the durable baseline to synchronize into local RepoWise stores.

Highest-priority ungoverned areas reported by the baseline:

1. [src/server.rs](src/server.rs)
2. [ui/src/lib/proto/webrtc_pb.ts](ui/src/lib/proto/webrtc_pb.ts)
3. [src/webrtc.rs](src/webrtc.rs)
4. [ui/src/lib/control-client.spec.ts](ui/src/lib/control-client.spec.ts)
5. [ui/src/lib/control-client.ts](ui/src/lib/control-client.ts)
6. [ui/src/routes/keep/+page.svelte](ui/src/routes/keep/+page.svelte)
7. [ui/e2e/fixtures/control-peer.ts](ui/e2e/fixtures/control-peer.ts)
8. [src/storage/catalog.rs](src/storage/catalog.rs)

RepoWise recommends reviewing [src/server.rs](src/server.rs) first because its change entropy
accounts for 36.3% of the repository's weighted health gap, followed by
[src/webrtc.rs](src/webrtc.rs) and
[ui/src/lib/control-client.ts](ui/src/lib/control-client.ts). This is a review priority, not approval
for a refactor.

## Health Improvement Program

The program favors regression coverage and smaller ownership boundaries over mechanical score
improvement. RepoWise scores prioritize the work; observable behavior and executable tests decide
whether a change is acceptable.

| Milestone                         | Status                                                   | Scope                                                                                                                                                   | Completion gate                                                                                               |
| --------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| M0: Comparable baseline           | Partial; network blocked                                 | Upgrade to the pinned RepoWise version, rebuild the index, ingest available coverage, and record a methodology-reset snapshot                           | Version, indexed commit, index scope, and coverage provenance are recorded                                    |
| M1: Stored-media control boundary | Implemented and validated                                | Characterize cursor ownership and move command dispatch plus disconnect cleanup out of `ServerControlHandler`                                           | Stored-media cursor, codec-period, mixed-codec, and timeline tests pass; full Rust and repository checks pass |
| M2: WebRTC lifecycle boundary     | Implemented and validated                                | Characterize session startup, background draining, thread reaping, failure cleanup, and shutdown, then centralize lifecycle ownership                   | Lifecycle and all WebRTC tests pass; session and thread synchronization have one owner                        |
| M3: Live and Keep safety net      | Characterized and validated                              | Characterize `LivePeer` connection/reconnect/close and Keep cold-seek, segment, keyframe, error, and return-to-live behavior                            | Unit/browser/E2E tests and required visual checks pass on desktop and mobile                                  |
| M4: Server domain handlers        | Discovery, runtime, event, health, and logging validated | Extract stored media, camera/runtime configuration, health, logging, and events behind stable internal dispatch APIs, one domain per change             | No protobuf change; focused domain tests and full server suite pass after every extraction                    |
| M5: Browser control domains       | System and notification slices validated                 | Split transport/request lifecycle from media, configuration, event, health, logging, and notification operations while retaining a compatibility facade | Existing callers migrate incrementally and the canonical UI check passes after every slice                    |
| M6: Storage policy boundaries     | Admission slice validated; catalog planned               | Isolate pure recording policy and catalog path/migration behavior before changing orchestration                                                         | Table-driven policy, catalog, storage pipeline, and safety tests pass                                         |
| M7: Large runtime classes         | Deferred                                                 | Reassess `KeepPeekLoop` and `MediumTermWriter` only after earlier boundaries reduce their responsibilities                                              | A separate approved decision and measured blast radius justify each large extraction                          |

### Non-Regression Gates

1. Add or strengthen behavior-level tests before moving production ownership.
2. Keep protobuf, HTTP, configuration, filesystem, and data-channel contracts unchanged during a
   pure extraction. Contract changes require a separate decision and acceptance criteria.
   The `api/` directory is read-only and may not be modified.
3. After the first production edit, run the narrowest test that can falsify the boundary. Repair
   that slice before opening another one.
4. Run all tests for the touched domain, then formatting, Clippy or frontend checks as applicable.
5. Run `./check.sh` on macOS/Linux or `.\check.bat` on Windows before completing any milestone that
   changes code. UI milestones also require the repository's browser and visual validation.
6. Compare RepoWise health only after a clean, pinned-version index. Never weaken behavior or tests
   merely to improve a score.

### M1 Evidence

- Added dispatch-level regression assertions for duplicate cursor rejection, cross-session close
  isolation, recording-demand preservation, disconnect cleanup, and post-disconnect close behavior.
- Moved stored-media command dispatch and session-owned cursor cleanup into
  `src/server/stored_media.rs`; catalog lookup, cursor transitions, and media encoding remain in
  `src/server.rs` for a later tested slice.
- Focused validation passed for `stored_media_cursor_opens_seeks_updates_and_releases_demand`, all
  three `stored_media` tests, and
  `stored_timeline_query_emits_indexed_availability_events_and_end_marker`.

### M0 Blocker

- The active RepoWise 0.44.0 executable is owned by one pipx environment and remained intact after
  both failed upgrade attempts.
- The machine-configured package-feed proxy does not yet expose RepoWise 0.45.0. Public PyPI does,
  but `files.pythonhosted.org` timed out and then failed its TLS read even with a bounded 120-second
  retry.
- Workspace Rust LCOV was generated with `cargo llvm-cov` and ingested into RepoWise: 329 indexed
  files and 73.8% measured line coverage. Modified and newly added Rust files cannot map until the
  working tree is committed and the index is refreshed.
- UI LCOV remains unavailable because public npm connectivity closed while resolving the exact
  `@vitest/coverage-v8` provider. The existing UI environment remained intact, and no manifest or
  lockfile changed.
- Do not bypass TLS or use an unapproved mirror. Retry public PyPI after network access is restored,
  then record a methodology-reset baseline before comparing health scores.

### M2 Evidence

- Strengthened the existing API-session test to require session-close callback delivery, removal
  from the active session map, idempotent second-close behavior, and finished-thread reaping.
- Existing focused tests already cover cancellation of background data, outbound-byte backpressure,
  terminal DTLS/ICE events, and nonblocking camera operations.
- Moved the API-session map and all WebRTC join handles into the private
  `webrtc::session_registry` module. `WebRtc` now uses narrow lookup, registration, removal, reaping,
  targeted-join, and shutdown-join operations instead of locking those collections directly.
- All 29 WebRTC tests pass with the new lifecycle owner, including
  `api_session_accepts_the_documented_data_channel_offer`.

### M3 Evidence

- Added a focused Chromium suite for `LivePeer` negotiated channel topology, multi-camera
  subscription, explicit teardown, attachment/hold lifetime, and cleanup when setup fails after a
  server session is created.
- All three new browser tests pass, and Svelte/TypeScript validation reports zero errors and zero
  warnings.
- The existing Keep timeline E2E suite already covers startup fallback, route-abort cleanup,
  exact-gap seeking, cold-seek frame preservation and failure, rapid drag coalescing, return to
  live, and mobile containment. No duplicate scenario was added.

### M6 Evidence

- Moved `CameraRecordingPolicy`, event-boost state, and admission decisions into the private
  `storage::recording_policy` module while retaining policy locking and writer command dispatch in
  `storage::engine`.
- The full mode/keyframe transition test and cross-source admission/enqueue atomicity test pass.

### M4 Evidence

- Moved runtime configuration command dispatch into the private
  `server::runtime_configuration` module. Persistence, path normalization, write probes, protobuf
  conversion, and configuration transactions remain in their existing owners.
- All eight direct and indirect runtime configuration regressions pass: get/update, required
  storage, omitted-versus-zero safety fields, secret references, validation rollback, pending
  configuration, and both storage migration paths.
- Moved event-search command dispatch and disconnect cancellation into the private
  `server::event_search` module. Search workers, task registration and limits, catalog queries,
  media reads, and result encoding remain with their existing owners.
- Query/media behavior, task limits, adjacent event behavior, and cancellation through
  `session_closed` pass their focused regressions.
- Centralized discovery task creation, progress, cancellation, completion, and disconnect cleanup
  in `server::camera_discovery`. Two-session regressions prove cancel and disconnect remain scoped
  to the owning session.
- Moved discovery command validation and response assembly behind the same boundary. Extracted
  configured-network preference as a pure function covering local preference override, missing
  subnet insertion, stable ordering, and public-address exclusion.
- Moved health command dispatch, ingress aggregation and issue thresholds, and all health protobuf
  serialization into `server::health_snapshot`. The focused aggregation regression pins counters,
  jitter warnings, and maximum-gap warnings; all 20 health-related tests pass.
- Moved logging command dispatch into `server::logging` while retaining settings persistence and
  stream behavior with their existing owners. All 26 command, filter, snapshot, and stream tests
  pass.

### M5 Evidence

- Added a typed `SystemControlClient` behind the existing `ControlClient` facade. Health, logging,
  server administration, and runtime configuration now own their command construction, response
  validation, and wire-to-domain mapping without changing public callers or transport lifecycle.
- Replaced high-complexity health state/reason conditionals with typed membership sets. The new
  paired mapper tests cover known and future enum values plus safe bigint clamping.
- All 15 focused system and `ControlClient` regressions pass, including the canonical negotiated
  channel scenario and every stored-media, event-search, notification, and teardown scenario.
- Added a typed `NotificationControlClient` behind the same facade. Rule lifecycle, inbox, history,
  receipt mutations, clear scopes, command validation, and wire mapping now have one owner while
  transport-level revision-conflict decoding remains in `ControlClient`.
- Expanded the facade regression to cover all notification commands and result variants. The
  paired notification tests pin all three clear-scope oneofs, bigint precision, unsupported value
  rejection, and incomplete-history rejection; all 17 system, notification, and facade tests pass.

### Structural Deltas

These compare file-local structure under RepoWise 0.44.0. They are not health-score history because
coverage ingestion changed the scoring methodology and the working tree is not yet indexed at the
pinned version.

| File                                         | Baseline                       | Current                        | Structural change                                                                                  |
| -------------------------------------------- | ------------------------------ | ------------------------------ | -------------------------------------------------------------------------------------------------- |
| `src/server.rs`                              | 15,385 NLOC                    | 14,590 NLOC                    | 795 NLOC moved behind six domain boundaries; maximum CCN remains 30                                |
| `src/server/runtime_configuration.rs`        | New boundary                   | 138 NLOC; maximum CCN 8        | Runtime configuration validation and dispatch are cohesive without owning persistence transactions |
| `src/server/event_search.rs`                 | New boundary                   | 104 NLOC; maximum CCN 10       | Event-search routing and session cancellation are isolated from worker and catalog mechanics       |
| `src/server/camera_discovery.rs`             | New boundary                   | 340 NLOC; maximum CCN 5        | Discovery task lifecycle, command routing, and configured-network preference have one owner        |
| `src/server/health_snapshot.rs`              | New boundary                   | 550 NLOC; maximum CCN 8        | Health dispatch, ingress aggregation, issue thresholds, and protobuf serialization have one owner  |
| `src/server/logging.rs`                      | New boundary                   | 26 NLOC; maximum CCN 2         | Logging command routing is isolated from persistence and stream mechanics                          |
| `ui/src/lib/control-client.ts`               | 4,036 NLOC; 29.38% duplication | 3,136 NLOC; 20.85% duplication | 900 NLOC moved behind typed system and notification domains; maximum CCN remains 30                |
| `ui/src/lib/control-client-system.ts`        | New boundary                   | 692 NLOC; maximum CCN 13       | System commands and wire mapping are isolated behind the existing compatibility facade             |
| `ui/src/lib/control-client-notifications.ts` | New boundary                   | 316 NLOC; maximum CCN 5        | Notification commands, result validation, and wire mapping are isolated behind the same facade     |
| `src/webrtc.rs`                              | 4,905 NLOC; 14.66% duplication | 4,810 NLOC; 13.69% duplication | 95 NLOC removed and duplication reduced by 0.97 percentage points; maximum CCN remains 35          |
| `src/webrtc/session_registry.rs`             | New boundary                   | 131 NLOC; maximum CCN 5        | API-session and join-handle synchronization now have one owner                                     |
| `src/storage/engine.rs`                      | 1,732 NLOC; maximum CCN 20     | 1,619 NLOC; maximum CCN 14     | 113 NLOC removed and maximum CCN reduced 30%                                                       |

The unchanged maximum complexity in `server.rs` and `webrtc.rs` means later milestones still need
to split their largest behavior owners. The current gains are narrower synchronization and command
ownership, not completion of those decompositions.

### Validation Evidence

- Workspace Rust: 1,387 passed, 16 skipped; Clippy with warnings denied passed; cargo-machete found
  no unused dependencies.
- UI unit and browser: 301 passed across Bun and Vitest, including all three new `LivePeer` tests,
  both direct system-mapper tests, and both direct notification-domain tests;
  Svelte/TypeScript reported zero errors and zero warnings.
- Playwright: 146 passed and 2 intentionally skipped across the full 148-test E2E suite. The Keep
  timeline subset passed all 17 scenarios.
- Rust, TOML, Python, and Markdown formatting checks passed.

## Score History

| Recorded   | Analyzed         | Commit         | RepoWise | Average | Hotspot | Maintainability | Performance | Alert files | Active decisions | Ungoverned hotspots | Note                                                                          |
| ---------- | ---------------- | -------------- | -------: | ------: | ------: | --------------: | ----------: | ----------: | ---------------: | ------------------: | ----------------------------------------------------------------------------- |
| 2026-08-27 | 2026-08-26 15:47 | `caf3545c8275` |   0.44.0 |    6.87 |    3.73 |            6.94 |        9.70 |          22 |                0 |                  87 | Initial committed baseline; next pinned-version result is a methodology reset |

Change-risk scores do not belong in this table. RepoWise change risk measures the size and spread
of one exact diff, so record it in a pull request with its revision range instead of comparing it to
repository health over time.

## Decision Register

### KP-ADR-001: Keep Core Operation Local-First

- Status: Active
- Recorded: 2026-08-27 as a pre-existing contract
- Context: Camera media and administration are privacy-sensitive and must remain useful without a
  vendor service.
- Decision: Recording, playback, detection, and administration work without a vendor cloud. Open
  camera protocols and documented integration APIs are architectural requirements; cloud services
  may be optional extensions only.
- Rationale: Local operation preserves privacy, predictable availability, and user control of
  camera evidence.
- Consequences: Core workflows cannot depend on a cloud relay. Remote access must remain compatible
  with user-managed encrypted networking.
- Affects: Product architecture, camera ingest, storage, integrations, remote access.
- Evidence: [README.md](README.md), [features.md](features.md)
- Supersedes: None

### KP-ADR-002: Use Rust for the NVR and Media Gateway Core

- Status: Active
- Recorded: 2026-08-27 as a pre-existing contract
- Context: The media core needs predictable low-overhead concurrency and memory safety.
- Decision: Keep the NVR and Media Gateway core in Rust using the repository's stable Rust 2024
  toolchain contract.
- Rationale: Rust supports memory-safe, explicit ownership across concurrent media, storage, and
  networking paths without a garbage-collected runtime.
- Consequences: Rust changes follow the pragmatic Rust guidelines and must pass formatting, Clippy,
  build, and nextest checks across supported platforms.
- Affects: `src/`, Rust workspace crates, build and CI tooling.
- Evidence: [README.md](README.md), [Cargo.toml](Cargo.toml), [AGENTS.md](AGENTS.md)
- Supersedes: None

### KP-ADR-003: Support Linux, macOS, and Windows Natively

- Status: Active
- Recorded: 2026-08-27 as a pre-existing contract
- Context: KeepPeek targets self-hosted deployments that do not share one operating system.
- Decision: Support Linux, macOS, and Windows with equivalent runtime behavior and native CI rather
  than inferring support from cross-compilation alone. Containers supplement native operation; they
  do not replace it.
- Rationale: Native validation catches differences in networking, filesystem paths, timers,
  services, packaging, and browser tooling.
- Consequences: Shared behavior needs platform-neutral abstractions, while unavoidable differences
  stay behind target-specific boundaries and tests.
- Affects: Runtime, CI, installers, configuration paths, release artifacts.
- Evidence: [README.md](README.md), [docs/event-loop-runtime.md](docs/event-loop-runtime.md),
  [features.md](features.md)
- Supersedes: None

### KP-ADR-004: Use an Explicit Threaded Event-Loop Runtime

- Status: Active; migration remains incomplete
- Recorded: 2026-08-27 as a pre-existing design
- Context: Socket readiness, protocol state, recording, HTTP work, and storage maintenance need
  clear ownership without pushing high-rate media through a global router.
- Decision: Use one process-level router and one worker thread per physical camera. Protocols use a
  SANS-I/O state-machine contract, standard channels carry control events, and media frames go
  directly from camera protocol workers to their recorders. Do not introduce an async runtime into
  this design by default.
- Rationale: Explicit ownership keeps protocol state testable, isolates camera failures, and makes
  shutdown and cross-platform wakeups deterministic.
- Consequences: Camera workers own their sockets and recorders; the router owns authoritative status
  and join handles. Migration checklist items in the runtime document remain implementation work.
- Affects: Camera runtime, RTSP and Reolink transports, recording, shutdown, HTTP query routing.
- Evidence: [docs/event-loop-runtime.md](docs/event-loop-runtime.md)
- Supersedes: None

### KP-ADR-005: Keep Shared State Declarative and Separate from Media Truth

- Status: Active design; verify implementation against its acceptance scenarios
- Recorded: 2026-08-27 as a pre-existing design
- Context: Services and clients need durable coordination without turning coordination state into a
  media bus, authorization grant, or second event catalog.
- Decision: The shared state store contains bounded complete desired-state documents. It uses
  namespace ACLs, compare-and-set revisions, TTL leases, and snapshot-plus-ordered-update watches.
  Capabilities and ordinary media commands remain authoritative for actual media availability and
  ownership.
- Rationale: Separating intent from observed capability prevents stale or malicious state from
  granting access or asserting media that does not exist.
- Consequences: State values cannot contain media bytes, credentials, raw SDP, or arbitrary logs.
  Clients recover watch gaps by replacing state from a fresh snapshot.
- Affects: Shared state API, service coordination, group preferences, media intents.
- Evidence: [docs/state-store.md](docs/state-store.md)
- Supersedes: None

### KP-ADR-006: Store Reusable Secrets Outside Tracked Configuration

- Status: Active
- Recorded: 2026-08-27 as a pre-existing contract
- Context: Camera credentials, access keys, tokens, and private hostnames must be reusable without
  leaking through source control, logs, support bundles, or configuration APIs.
- Decision: Store reusable private strings in owner-only `secrets.toml` beside `config.toml` and
  reference them with `{secret:KEY}` placeholders. Preserve references through API and editor round
  trips, and redact resolved values from diagnostics.
- Rationale: Separating secret values from ordinary configuration reduces accidental disclosure
  while keeping configuration portable and reviewable.
- Consequences: Missing or malformed references fail safely, environment overrides take precedence,
  and real values are entered only on the KeepPeek host.
- Affects: Configuration loading, camera setup, access control, diagnostics, UI configuration.
- Evidence: [docs/secrets.md](docs/secrets.md)
- Supersedes: None

### KP-ADR-007: Use Svelte 5 and Bun for the UI Toolchain

- Status: Active
- Recorded: 2026-08-27 as a pre-existing contract
- Context: The frontend needs one reproducible package and script runner plus a consistent modern
  Svelte model.
- Decision: Use Svelte 5 patterns under `ui/` and Bun as the only package manager and script runner.
  Resolve packages from the public npm registry and do not create JavaScript lockfiles.
- Rationale: One toolchain avoids divergent dependency resolution and legacy Svelte patterns.
- Consequences: Do not use npm, pnpm, or Yarn for repository UI work. UI changes must pass the root
  platform check script.
- Affects: `ui/`, frontend CI, formatting, tests, browser tooling.
- Evidence: [AGENTS.md](AGENTS.md), [ui/package.json](ui/package.json)
- Supersedes: None

### KP-ADR-008: Keep RepoWise Local, Deterministic, and Advisory

- Status: Active
- Recorded: 2026-08-27
- Context: Agents and maintainers need durable architecture context, risk signals, and health trends
  without sending private source to a model or treating a heuristic as a merge verdict.
- Decision: Use the repository-pinned RepoWise CLI and local no-prose index for code intelligence.
  Keep this tracked ledger as durable memory, use the local decision store as a derived cache, and
  keep pull-request change-risk results advisory.
- Rationale: A local deterministic index improves navigation and review while source, tests, and
  explicit human decisions remain authoritative.
- Consequences: Refresh the index after significant changes, record score provenance, verify stale
  findings against the working tree, and never fail a change solely because of a RepoWise score.
- Affects: Agent workflow, repository setup, pull-request review, architecture governance.
- Evidence: [README.md](README.md), [AGENTS.md](AGENTS.md),
  [.github/workflows/repowise.yml](.github/workflows/repowise.yml)
- Supersedes: None

## Update Workflow

### Decisions

1. Read this ledger and run `repowise decision health` plus the relevant RepoWise `get_why` query
   before a significant architecture or tooling change.
2. Add a new entry with the next `KP-ADR-NNN` identifier. Include concrete affected files or
   modules so staleness can be measured.
3. Keep the entry `Proposed` until the owner explicitly accepts it. Record rejected alternatives
   and meaningful tradeoffs rather than only the final choice.
4. After approval, mark the entry `Active`. If it replaces an older decision, mark the old entry
   `Superseded` and cross-link both entries.
5. Synchronize the local derived store without duplicating titles:

```sh
repowise decision list --format json
repowise decision add \
  --title "<title>" \
  --context "<context>" \
  --decision "<decision>" \
  --rationale "<rationale>" \
  --affects "<path-or-module>" \
  --format json
```

Flag-driven additions are `Proposed`. Run `repowise decision confirm <id>` only after explicit
approval. The committed ledger wins if a local store is missing or disagrees.

### Scores

1. Prefer a clean committed revision. Record dirty-tree state explicitly when a working-tree
   snapshot is intentional.
2. Refresh deterministic data and collect structured output:

```sh
repowise update --index-only
repowise status --format json
repowise health --format json
repowise health --trend
repowise decision health --format json
```

3. Replace **Current Snapshot** with the new values and append one **Score History** row. Include the
   analysis timestamp, full indexed commit, RepoWise version, score band, and decision-health
   counts.
4. Explain discontinuities caused by RepoWise upgrades, parser coverage changes, generated files,
   or a materially different index scope. Do not attribute a score delta to one code change without
   evidence.
5. Keep issue and pull-request acceptance criteria tied to observable behavior and executable tests,
   not to reaching a RepoWise score.

## Decision Template

```markdown
### KP-ADR-NNN: Short Decision Title

- Status: Proposed
- Recorded: YYYY-MM-DD
- Context: What forced this decision?
- Decision: What was chosen?
- Rationale: Why was it chosen?
- Alternatives: What meaningful alternatives were rejected?
- Consequences: What tradeoffs are accepted?
- Affects: Concrete files or modules governed by this decision.
- Evidence: Links to source, issue, pull request, benchmark, or test.
- Supersedes: None, or a prior decision ID.
```
