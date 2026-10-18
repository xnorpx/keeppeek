# Local archive UI design handoff

Status: Paper Boards 47–50 were authored and visually reviewed in the shared Paper file. Their
JSX sources and eleven lossless reference frames are checked into the design source tree.
The isolated fresh-backend lifecycle and focused UI checks pass. Running-page visual comparison,
accessibility qualification and the canonical repository gate remain pending. This mapping does not
claim pixel parity or design approval.

Scope: one local filesystem archive destination, Administrator configuration and operations, and
an archive playback notice for authorized viewers. The broader offsite provider capability remains
planned. Keep the existing settings navigation and recorded-player controls.

## Existing design references

The archive manifest records Paper file `01M0B0VBH78TMTX40GCYYQ37SG`, token hash `b35ec365`.
Board 47 setup is `BAX-0`, Board 48 jobs/recovery is `BET-0`, Board 49 playback/mobile is
`BHT-0`, and Board 50 mobile administration is `BMN-0`. Checked-in sources are
[setup](../ui/design/paper/keeppeek-nvr-alpha/boards/47-archive-setup.jsx.txt),
[jobs and recovery](../ui/design/paper/keeppeek-nvr-alpha/boards/48-archive-jobs-recovery.jsx.txt),
[playback](../ui/design/paper/keeppeek-nvr-alpha/boards/49-archive-playback.jsx.txt), and
[mobile administration](../ui/design/paper/keeppeek-nvr-alpha/boards/50-archive-mobile-administration.jsx.txt).
The [manifest](../ui/design/paper/keeppeek-nvr-alpha/manifest.json) records exact source and image
hashes and dimensions. Editable implementation exports also remain in
`target/agent-artifacts/paper-130/`.
The earlier shell references below retain their historical `cf3b1cd7` token record.

| Surface               | Reference                                                                                                                                       | Placement for archive work                                                                                                           |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Desktop Storage       | [Board 13](../ui/visual-harness/stories/Board13StorageRetention.stories.ts), board `233-0`, frame `23D-0`, `settings.desktop.storage-retention` | Extend Storage below measured retention and named volumes. Preserve the distinction between primary placement and retained replicas. |
| Desktop Keep          | [Board 04](../ui/visual-harness/stories/Board04KeepTimeline.stories.ts), `keep.desktop.timeline-anatomy`                                        | Place the archive notice in playback messages beside existing waiting and error evidence.                                            |
| Mobile administration | [Board 27](../ui/visual-harness/stories/Board27MobileAdministration.stories.ts)                                                                 | Keep the Storage section within the existing mobile Settings navigation; stack the same actions.                                     |
| Mobile Keep           | [Board 22](../ui/visual-harness/stories/Board22MobileMedia.stories.ts), board `3FF-0`, frame `3HN-0`, `keep.mobile.timeline`                    | Keep the archive notice with the player; retain camera/date navigation and playback-options sheet.                                   |
| Waiting and failure   | [Board 33](../ui/visual-harness/stories/Board33States.stories.ts)                                                                               | Separate pending, unavailable, failed, empty, and confirmed success.                                                                 |

Use the existing [Paper theme tokens](../ui/src/styles/paper-theme.css) and
[light/dark aliases](../ui/src/app.css). Archivo is the interface face; IBM Plex Mono is for
identities, exact byte counts, and playback time. Existing sizes are 11px secondary evidence,
13px body, and 15px section titles. Use ground/surface/raised, hairline, text/text-muted, primary,
healthy, activity, and destructive semantic colors. Do not add a parallel palette. Keep the
4/8/12/16/24px spacing rhythm and current focus-ring and button primitives.

## Paper screens and states

The boards cover desktop and 390px mobile versions of these states. Also inspect 320px behavior. State,
identity, and permitted actions must remain legible without relying on color or horizontal scroll.

| State                                | Content and actions                                                                                                                                                                                                                                                                                  | Mobile arrangement                                                                                                            |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| First destination                    | Local archive heading; explanation that this is a verified replica; runtime unavailable until enabled settings are applied; **Edit archive settings**. No destination-success badge.                                                                                                                 | Heading, explanation, then full-width edit action.                                                                            |
| Setup/edit                           | Target ID and root; enabled and automatic switches; explicit eviction opt-in default off; source selection; minimum age; bounded object, queue, deadline, and attempt settings; **Save archive settings**, **Reload archive settings**, cancel. Explain existing directory and restart requirements. | One column. Keep labels above inputs and bounds beside their fields. Separate eviction consequences from ordinary enablement. |
| Saved, restart pending               | Saved acknowledgment and restart requirement. Preserve the distinction between saved configuration and the active worker.                                                                                                                                                                            | Keep acknowledgment adjacent to settings actions and use the existing application restart affordance.                         |
| Configured and healthy               | Latest destination availability; **Test archive destination**; **Refresh archive jobs**; catalog source/object selectors; **Queue archive copy**; current job cards and bounded pagination.                                                                                                          | Stack controls and job cards. Wrap object and target identities and preserve exact byte counts.                               |
| Probe pending or offline             | Explicit pending probe or unavailable destination, with recording continuing. Refresh remains available. Offline must not look like a successful test.                                                                                                                                               | One concise status block above operations. Keep recovery and error messages next to their actions.                            |
| Copy queued/copying/failed/cancelled | Object identity, job ID, target, state, bytes, attempts, primary state, and safe failure evidence. Cancel eligible transfers. Retry failed work with the same job ID. **Retry / finish cleanup** for cancelled work; explain that cleanup precedes copying.                                          | Use labeled evidence lines instead of a wide table. Keep job actions with their job, never at the page footer.                |
| Verified replica, primary retained   | Ready replica and retained primary shown independently. **Evict primary** requires explicit confirmation.                                                                                                                                                                                            | Keep the state distinction above the action so it is visible before confirmation.                                             |
| Eviction confirmation                | Object, target, exact bytes, and consequence: remove the primary; archive playback remains available and may be slower. Deliberate confirm and cancel; no preselected destructive action. The server rechecks current evidence and readers.                                                          | A focused decision surface with wrapped identities, visible consequences, and clear cancel/confirm actions.                   |
| Eviction reconciling                 | Primary retirement intent is durable. Explain why cancellation is unavailable; retain refresh and failure/retry evidence.                                                                                                                                                                            | State and explanation remain on the job card. Do not replace them with a spinner alone.                                       |
| Primary evicted / restore            | Archive retained; primary evicted. **Restore primary** shows identity, bytes, and destination consequence before queuing background work. Show pending, failed/retry, cancellable restore cleanup, then primary retained.                                                                            | Same job card, with action progress and cancellation next to restore evidence.                                                |
| Keep archive playback                | **Archive · Access and seeking may be slower.** Retain normal timeline, seek, play/pause, waiting, failure, and cancellation behavior. No artificial delay and no mandatory restore step.                                                                                                            | Notice stays with the recorded player and wraps onto two lines if needed. Playback options remain reachable.                  |

## Interaction requirements for comparison

- Administrator and exact capability gates hide configuration and job mutations from Users. An
  authorized viewer can still receive the archive playback notice.
- The editable configuration retains its accepted revision while dirty. Status refresh must not
  replace the draft. A revision conflict keeps inputs and error evidence. Reload explicitly
  confirms discarding dirty inputs before fetching a new baseline.
- Secret references stay visible as references; root paths and reusable private strings must not
  be replaced with resolved secret values in the draft.
- Copy uses an existing catalog object and a durable UUID. A lost reply is reconciled by that job
  ID. Transfer state is independent of local-primary state; ready does not imply primary eviction.
- Probe acknowledgment means queued work. Only a completed destination result can report
  availability. Background copy, restore, and probe work must not lock the whole Settings page.
- Announce asynchronous status and errors through existing status/alert semantics. Keep keyboard
  focus visible. Use the existing dialog/sheet interaction rules for focus, Escape, and return
  focus when Paper defines a custom confirmation surface.

## Current implementation and comparison debt

The current controls are in
[ArchiveSection](../ui/src/lib/components/storage-volumes/ArchiveSection.svelte),
[ArchiveDraftEditor](../ui/src/lib/components/storage-volumes/ArchiveDraftEditor.svelte), and
[ArchiveCopy](../ui/src/lib/components/storage-volumes/ArchiveCopy.svelte). Settings gates the
section, and Keep renders the archive notice from accepted stored-media state.

Setup keeps the folder, target ID, enablement and automatic/eviction opt-ins visible, with source
IDs and numeric transfer bounds in native Advanced selection & transfer limits disclosure.
Eviction and restore use an in-page native dialog with object, target, exact bytes and consequences.
Cancel receives initial focus; Escape cancels and restores focus. Job actions have concise visible
labels, with unique identities in accessible names and expandable job details. Responsive job cards
adapt the desktop table so exact evidence and actions fit the same mobile flow.

Initial playback preparation and failure remain generic until accepted server state identifies
archive media. After that classification, the notice uses Paper's archive preparing, unavailable,
retry and close states. This avoids labelling an ordinary local failure as an archive failure.
Close cancels the current playback and leaves the timeline available; Retry opens the same selected
moment again. Existing player options, access checks and timeline controls remain available.

Eight real Chromium workflows passed after implementation alignment: conflict/draft preservation and
explicit reload, manual selection with lost-copy acknowledgment and eviction/restore, pending
probe and eviction cancellation guard, cancellation/retry with original job IDs, generic pending
playback with close, known archive failure with retry/close, real Keep-route classification after a
cold seek with stale replaced-cursor rejection, and visible missing-randomness recovery without a
copy command. They cover advanced disclosure, 44px mobile targets, 320px dialog width, Cancel focus,
Escape and focus return. Five Bun helper cases pass, including exact private references and byte
budgets, bounded transfer policy, canonical v4 IDs on trusted LAN HTTP without `randomUUID`, missing
cryptographic randomness, and reconciliation with the same generated job ID after a lost reply.

The isolated [archive E2E](../ui/e2e/local-archive.e2e.ts) passed against the fresh release backend and
final Vite UI: **1/1, 34.4s test time (43.1s including startup)**. It saves the destination, restarts
the actual recorder, tests the destination, copies a finalized recording, confirms primary
eviction, restarts, opens and cold-seeks the archived recording twice, restores the primary, and
restarts again to verify the retained-primary state. Actual `requestVideoFrameCallback`
presentation after a changed media source proves each cold seek. A second loopback camera records
and presents live frames concurrently. The fixture uses its exact configured recording source/key
and an existing private NTFS archive root; an earlier invalid inherited Windows ACL was diagnosed
and corrected without weakening production ownership checks.

Evidence is preserved in
[`target/agent-artifacts/archive-lifecycle-focused-final/`](../target/agent-artifacts/archive-lifecycle-focused-final/):

- `archive-setup-{dark,light}-{desktop,mobile}.png`: four bounded viewport captures of the actual
  setup, with settled 44px mobile button dimensions asserted before capture.
- `archive-jobs-{desktop,mobile}.png`, `archive-eviction-confirmation-{desktop,mobile}.png`, and
  `archive-playback-{desktop,mobile}.png`: six real workflow captures, including decoded footage.
- `archive-playback-observations.json`: Chromium **156.0.8078.4**, original 1280×720 viewport,
  desktop captures at 1280×900 and mobile captures at 390×844, and the observations below.
- `isolated-archive-server.log` and `isolated-fixture-metadata/`: stopped fixture catalog, allocation
  metadata and exact directory ACL evidence. Fixture secrets and media payloads are excluded.

| Playback source               | Navigation to first presented frame | Video load to first presented frame | Cold seek to new-source presented frame |
| ----------------------------- | ----------------------------------: | ----------------------------------: | --------------------------------------: |
| Normal local                  |                          3938.966ms |                            17.766ms |                                  60.9ms |
| Initial archive after restart |                          1223.866ms |                             4.266ms |                                 435.3ms |
| Repeat archive open           |                            1131.5ms |                               8.5ms |                                 442.9ms |

Copy and restore each transferred a **1,456,425-byte** object. UI submission to the first observed
completed state took **922.652ms** for copy and **928.687ms** for restore, including control and
refresh-polling overhead. Live presented frames advanced **8→36** during the copy interval and
**1→12** during restore; actual writer last-write timestamps advanced in both intervals. The
recording byte counter did not advance during these short intervals, so it is not evidence of byte
throughput. Navigation timings include Vite/page startup and cache effects; they do not show that
archive storage is faster than primary storage. This single small-object loopback run supplies
observations with finite deadlines, not latency, throughput or fleet nonstarvation guarantees.

Desktop/mobile/light/dark comparison, visual/accessibility approval and the full canonical gate
remain pending. These checks do not qualify physical remote storage, long outages, a large fleet,
or every archive error-state pixel. The dark setup mobile screenshot is scrolled within the form
and omits the archive heading; use the other setup captures for heading context. This screenshot
limit does not establish exact viewport parity with Paper. The broader offsite-provider capability
remains planned.
