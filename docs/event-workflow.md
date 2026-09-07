# Event Review and Shared Bookmarks

The `keeppeek.event-workflow.v1` capability enables durable review state and shared bookmarks.
The recording catalog owns this historical metadata. It is not configuration and does not use
browser storage or the generic StateStore as its durable domain store.

## Ownership and State

- **Reviewed** means the current reviewer has examined the event.
- **Dismissed** means the reviewer removed the event from their unreviewed queue without deleting
  the event, its attachments, or recordings.
- **Unreviewed** means neither reviewed nor dismissed is set. Reviewed and dismissed are independent
  flags, so both may be set.
- **Bookmarked** is one shared record per stable source ID and event ID. Authorized camera viewers
  can see it. Its creator and Administrators can edit or remove it.
- **Evidence hold** is a separate retention operation. This capability does not add a hold action
  or change recording protection. The planned retention-pin capability is not a workflow bookmark.

Authenticated review state belongs to the server-authenticated credential ID, not its display name,
credential revision, network address, or browser session. Credential rotation preserves the review
identity. Event revisions do not reset review or dismissal; there is no automatic material-change
re-review policy.

Trusted-LAN browsers keep a random workspace UUID in site storage under
`keeppeek.event-workflow.workspace.v1`. The server namespaces it as `workspace:<UUID>`. This is
labelled **Local workspace**, not a named person. It survives browser/server restarts but clearing
site storage creates a new workspace. It provides neither multi-user attribution nor privacy
between trusted local Administrators. If persistent site storage is unavailable, mutations remain
unavailable with an actionable error; there is no silent session-only fallback.

## Control Commands

`EventWorkflowCommand` runs over the existing authenticated WebRTC control channel:

| Action           | Input                                                                               | Result                                                       |
| ---------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| `get`            | Explicit source/event targets; optional single-event audit                          | Current caller review state and shared bookmarks             |
| `review`         | Explicit source/event targets, expected review revisions, reviewed/dismissed values | Atomically acknowledged review states                        |
| `bookmark`       | One target, expected bookmark revision, active flag, plain-text note                | Current shared bookmark and bounded audit                    |
| `list_bookmarks` | Authorized source scope, UTC interval, optional creator filter, page token          | Bounded bookmark references, authoritative total, next token |

Authenticated callers leave `local_workspace_id` empty. Trusted-LAN callers supply their stable
workspace UUID. Clients set `expected_actor_id` to the actor that originated the operation; a
different current actor is rejected. This is a precondition, not a way to select another user.

Revision zero means no record exists. Every accepted mutation increments its independent revision.
All targets in a review batch commit together. A stale target rejects the entire batch and returns
`EventWorkflowError` with the current state. Explicit undo writes the previous values with the
revision returned by the successful action. It does not bypass compare-and-set.

Clients preserve note drafts and action intent on conflict or failure. **Reload and retry** is an
explicit user action using newly read revisions. A timeout has an unconfirmed outcome: the write
may already have committed, so clients must reload before retrying. Changing reviewer identity
clears selection, cached review state, pending intent, and undo state.

## Filters and Counts

`QueryEvents.workflow` composes with metadata, structured text, and semantic searches. Its review,
bookmark, and creator predicates execute in the database before pagination. Camera, type, origin,
zone, confidence, image, text, and time filters keep their existing meanings.

Each search hit carries the caller's authoritative workflow state. `EventSearchQueryEnd` carries
counts for the effective authorized query before the page limit: total, unreviewed, reviewed,
dismissed, bookmarked, and bookmarked by the caller. Counts are not inferred from visible cards.
Reviewed and dismissed counts can overlap. Semantic counts describe eligible events; the existing
`candidates_truncated` field still reports bounded ranking over only the newest eligible candidates.

Queries read a database snapshot. Cursor fingerprints include the reviewer and workflow predicates.
Workflow changes invalidate affected event-search continuation tokens. Bookmark-library cursors
also bind the authorized source scope and bookmark revision. Server envelopes are signed and
expire under the existing event-search token policy.

## Retention and Limits

Workflow rows use stable source/event IDs, never card indexes or mutable camera labels. Recording
retention does not remove review state or bookmarks. A missing indexed file makes the bookmark
metadata-only. Media availability is a current indexed-file hint, not a guarantee that every frame
will decode or that the file cannot disappear after the response.

Deleting an event retains its review and bookmark references for 90 days. Saved bookmarks retain
the event's original type/time and explicitly report missing event, source, or media. Cleanup removes
at most 128 expired rows per workflow table at catalog startup and before workflow mutations.
Audit rows are removed with their bookmark. Inactive bookmarks for events that still exist retain
their revision so stale clients cannot recreate them using revision zero.

| Resource                                               | Bound                              |
| ------------------------------------------------------ | ---------------------------------- |
| Explicit review batch                                  | 1 to 128 unique targets            |
| Source, event, and actor IDs                           | 256 UTF-8 bytes each               |
| Aggregate explicit target IDs                          | 16 KiB                             |
| Review records per event                               | 64 principals                      |
| Total review records                                   | 250,000                            |
| Shared bookmark records, including inactive tombstones | 10,000                             |
| Bookmark note                                          | 1,024 UTF-8 bytes; plain text      |
| Audit history                                          | Latest 16 mutations per bookmark   |
| Workflow state read                                    | 16 events, or one event with audit |
| Bookmark library page                                  | 16 records                         |
| Search page / interval                                 | 128 hits / 31 days                 |
| Workflow response wait / database lock wait            | 5 seconds / 2 seconds              |
| Catalog command queue                                  | Existing 256-command bound         |
| Browser workflow cache / selection                     | 512 states / 128 targets           |

Capacity errors preserve existing records and never evict live review state silently. The installed
Turso API has no public execution-interrupt hook: search runs on the existing separate worker;
response timeouts bound caller waits, not an already-running database statement.

## User Workflow and Export

Events provides single-event controls, explicit **Mark N visible reviewed** and **Mark N selected
reviewed** actions, and revision-bound undo. Visible means the current rendered page. Selection may
span pages but always sends explicit IDs and shows its count. Filters, nested scroll position, and
reviewer-owned selection remain in browser navigation state. A changed filtered result may require
a new first-page cursor; no hidden page is mutated.

Bookmarks appear on event cards/detail, Keep stories, timeline entries, and export entry points.
**Saved bookmarks** has its own date/source/creator scope, including retained references whose
event or source is no longer available. Its count is independent of other Events filters.

Exports retain the source event revision and the server-observed active bookmark revision in
`EventExportSeed.bookmark_revision`. Later bookmark editing/removal does not rewrite that historical
export relationship. Export existence does not create a bookmark, and neither bookmarking nor
export handoff creates an evidence hold. The existing export authorization and media validation
remain in effect.
