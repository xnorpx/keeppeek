# Recording and evidence

KeepPeek treats a recording as evidence only when the media is finalized, indexed, and playable.
A connected camera or an MP4 filename alone does not prove that the requested interval can be
reviewed later. The underlying live evidence dimensions are defined in
[Camera and stream health](./camera-health.md).

## Find and play a recording

Open **Keep**, choose the camera and recorded day, then select footage or an event on the timeline.
The four modes serve different investigations:

| Mode      | Use it for                                                                          |
| --------- | ----------------------------------------------------------------------------------- |
| Timeline  | Follow footage and event intervals around the selected playback time.               |
| Stories   | Review available event stories and their associated media.                          |
| Swimlanes | Compare the available camera histories around a common time.                        |
| Export    | Prepare and inspect an evidence-export job; creation requires Administrator access. |

Use play/pause, skip, the recording-position slider, volume/mute, speed and fullscreen controls
below the image. Available stream quality depends on retained media and browser codec support.
Changing the quality preference is separate from [digital zoom](./digital-zoom.md). Stored
playback does not provide automatic transcoding for an unsupported codec; retain a compatible
H.264 stream when your viewing devices need one.

With the recorded player focused, Left and Right step frames. Space toggles playback; J, K and L
provide shuttle/pause controls. `[` and `]` set export boundaries. Form fields keep their normal
keyboard behavior, and arrow keys pan a focused digitally zoomed image instead of stepping frames.

### Use Keep on a phone

In portrait, Keep keeps a compact camera/date row, the player and a horizontal history timeline
in the main view. Tap the camera name or date to open **Camera and date**. Search for a camera or
use previous/next camera; camera navigation wraps at the ends. The recorded-day selector and its
arrows choose available days, with unavailable directions disabled.

Tap **Playback options** for volume, playback speed, quality, **Copy link to this moment**, and
**Refresh recordings**. Changes apply immediately. **Done**, the close button, Escape or browser
Back closes the sheet and returns focus to a visible control. Opening a sheet keeps the player
open. In recording fullscreen, rate and volume remain available beside the player controls.

The compact controls do not remove Timeline, Stories, Swimlanes or Export. Rotate the device or
close the sheet to return to the history view. The timeline changes recording time; the separate
Digital controls above the image change only the inspection crop.

### Keep the requested UTC time explicit

Recorded days and event time filters use UTC. Switching cameras preserves the selected absolute
moment, including when the destination camera has no footage there. Choosing another recorded day
keeps the UTC clock time on that chosen day. At midnight this means midnight of the selected day,
not the following day.

A gap stays visible until you explicitly choose available footage. Use **Previous recording** or
**Next recording** when offered, or select a recorded interval on the timeline. KeepPeek does not
silently move an unavailable requested moment to the latest clip. Loading, no recorded days,
unavailable footage and decode failure are distinct states; use the displayed action or refresh
after checking the camera's recording policy and retained coverage.

## Choose what each camera records

For a dated comparison of implemented recording outcomes, test evidence, and remaining work,
see the [recording capability audit](https://github.com/xnorpx/keeppeek/blob/main/docs/recording-policy-audit.md).
Recording modes choose streams; they do not provide independent continuous, motion, alert, or
detection retention periods. EventBoost does not provide pre-event replay, and export context can
only include footage that was already retained.

Each camera has one recording policy:

| Policy        | Recorded media                                                                                            |
| ------------- | --------------------------------------------------------------------------------------------------------- |
| `off`         | Records neither video stream. This is intentional and is not reported as a gap.                           |
| `sub`         | Records the substream continuously.                                                                       |
| `main`        | Records the main stream continuously.                                                                     |
| `both`        | Records main and sub independently.                                                                       |
| `event-boost` | Records the substream normally, switches to main at a keyframe after an event, then returns to substream. |

`event-boost` is the default. It writes one logical recording rather than recording main and sub
at the same time. Transitions happen only at keyframes, so the stored file remains seekable. A new
event extends the configured main-stream interval.

Choose a browser-compatible H.264 substream even when the main evidence stream is H.265. KeepPeek
stores the camera's encoded media without re-encoding it, so browser support still determines which
recordings can play directly.

## Prove recording integrity

Open **Recording integrity** to inspect the fleet before footage is needed. The workspace reports:

- whether recording is requested by policy;
- current writer state and the last frame, write, finalization, and catalog commit;
- oldest and newest retained media;
- effective retained duration, attributed bytes, and estimated daily growth;
- playable coverage for the selected 24-hour, 7-day, or 30-day interval;
- exact recent gaps and the evidence that explains each one.

Fleet states have narrow meanings:

| State              | Meaning                                                                  |
| ------------------ | ------------------------------------------------------------------------ |
| Recording healthy  | Every requested stream has current writer progress.                      |
| Recording degraded | A requested writer is stalled or failed.                                 |
| Paused by policy   | The effective policy intentionally requests no recording.                |
| Not configured     | No usable recording stream is configured.                                |
| Unknown            | KeepPeek lacks enough current evidence to make a truthful determination. |

The **Not configured** summary appears only when its count is nonzero. Policy-disabled streams and
unconfigured cameras remain distinct from unexpected recording loss.

Coverage comes from keyframe-indexed fragments, not from file presence. Current gaps remain open
and never claim an end time. Causes include source silence, transport outage, stale frames, decode
failure, writer failure, disk pressure, retention deletion, storage migration, catalog mismatch,
and an explicit unknown state. Gap actions open nearby footage, camera health, or relevant logs.

Long histories remain bounded: fleet pages contain 25 cameras by default, recent exact detail keeps
at most 256 ranges per stream, and longer periods use deterministic time buckets while retaining
exact totals.

## Review events consistently

Open **Events** to search across authorized cameras. Choose the UTC date and optional start/end
times, then narrow by camera, event kind, origin, zone, confidence, image availability or text.
Review and bookmark filters compose with the same search. If a filter reports unavailable
support, check the server capability instead of assuming that an empty result means no events.

Select a card to inspect its canonical image, description, timestamps and revision. Use **Open at
this moment** to investigate recorded footage or the export action to prepare a clip. The return
link restores the selected event and supported filter context. For an empty result, check the
effective filters and UTC date before broadening the search.

Mark events reviewed or dismissed without deleting evidence. Those flags belong to your authenticated
identity, or to a persistent **Local workspace** in trusted-LAN use. Event updates do not reset them.
Use the visible or selected count on bulk actions; undo applies only to the acknowledged targets.
Review and bookmark filters use server-computed counts, not the current page length.

**Reviewed** and **Dismissed** are independent flags. **Unreviewed** means neither flag is set;
dismissal removes an event from that review queue without deleting its attachments or recording.
**Mark N visible reviewed** affects the rendered page, while **Mark N selected reviewed** sends
the explicit selection, which may span pages. The selection limit is 128 events. A stale or
unconfirmed write requires **Reload and retry**; note drafts and action intent remain available.

Named credentials retain their review identity across credential rotation. A trusted-local
browser uses a persistent Local workspace identity rather than attributing work to a person.
Clearing that site's browser storage creates a new workspace; unavailable persistent storage
disables its mutations instead of silently using a temporary identity.

Bookmarks are shared with authorized camera viewers. Their creator or an Administrator can edit
the bounded note or remove the bookmark. **Saved bookmarks** retains honest metadata-only references
when media, events, or sources are unavailable. A bookmark does not pin video through retention.
Exports keep their source event/bookmark revision relationship without creating a retention hold.

Bookmark notes are plain text, limited to 1,024 UTF-8 bytes. The bookmark library has its own
source, date and creator scope, independent of the main Events filters. Recording retention can
remove footage without removing the bookmark. References for deleted events are retained for
90 days and explicitly identify missing event, source or media.

Events, Keep, notifications, MQTT, and export entry points use the same event revision and canonical
preview. An authorized producer may name the canonical attachment. Otherwise KeepPeek chooses a
supported snapshot, then a story frame, then a retained thumbnail using stable ordinal, capture
time, and attachment ID ordering.

If canonical image bytes are unavailable, KeepPeek shows that state instead of silently choosing a
different image. Bounding boxes appear only when their coordinate-space attachment matches the
canonical image. Event type remains authoritative for filtering and automation; semantic icons are
presentation-only and come from a fixed allowlist.

[Notifications and integrations](./notifications-and-integrations.md) explains how those same
event and operational identities remain stable across delivery, retry, and broker recovery.

## Copy a recording moment

Use [digital zoom and pan](./digital-zoom.md) to inspect recorded frames and canonical event images
without changing their source resolution or the evidence. Inspection crops are not included in
copied moment links or exports.

Use the link icon in Keep's command bar to copy the current recording moment. The command reads
the playback clock when invoked; it does not pause, seek, reload the video, or change browser
history. Event detail has the same command beside **Open at this moment**, using the event's
start time. Both event actions retain the selected event and active filters. **Back to event**
returns to that context from any Keep mode.

These are authenticated navigation links, not public shares or exports. Recipients need access to
the same KeepPeek server and camera, and remote recipients must sign in. A link does not contain an
access key, session ID, temporary media URL, recording filename, or filesystem path. It does include
the server-provided camera identifier and, for event links, event and filter context. Treat those
details as private operational information. Changing camera identity or deleting footage can make
an old link unavailable.

Links use the existing `/keep` route, including any configured application base path:

| Parameter  | Meaning                                                                         |
| ---------- | ------------------------------------------------------------------------------- |
| `camera`   | Server-provided source identifier, encoded without changing its value.          |
| `at`       | Absolute Unix timestamp in integer milliseconds.                                |
| `date`     | UTC date derived from `at`; the timestamp wins if an incoming date conflicts.   |
| `stream`   | Requested `auto`, `high`, `low`, `main`, or `sub` preference.                   |
| `mode`     | Keep view; omitted for Timeline, otherwise `stories`, `swimlanes`, or `export`. |
| `event`    | Optional event identifier.                                                      |
| `returnTo` | Optional local Events route with supported filters and selected event identity. |

The timestamp remains the same across browser timezones. For retained, supported media, playback
restoration is verified within one second or one source-frame duration, whichever is larger.
Browser codec support still applies. KeepPeek explains a compatible stream fallback and preserves
the requested preference in copied links.

An exact link into a gap does not open nearby footage automatically. It keeps the requested UTC
time visible and offers previous or next recordings found in the bounded five-minute window on
each side, within that UTC day. Selecting one explicitly moves the playhead. If no retained
footage is found, the view explains that it may have expired or never been recorded. Missing and
inaccessible cameras share an unavailable-or-not-authorized message to avoid disclosing hidden
sources. Malformed links fail visibly instead of selecting another camera.

Successful copying shows a checkmark and announces confirmation. If browser clipboard access is
denied, unsupported, or takes more than 2.5 seconds, a dialog presents the same link in a selected,
read-only field for manual copying. Escape closes that dialog and returns focus to the command.

## Export evidence

An Administrator can select up to two minutes in Keep and create a standalone MP4. Event export
opens the same editor with 15 seconds of context before and after the event while preserving the
camera, stream, timestamp, event revision, filters, and return route.

Export jobs can be running, ready, partial, failed, cancelled, or expired. KeepPeek:

- returns an identical running job instead of starting hidden duplicate work;
- offers an existing ready artifact before creating a fresh one;
- names every missing interval before a partial export proceeds;
- stops jobs after 30 seconds without progress or five minutes total runtime;
- removes partial output after failure or cancellation;
- marks interrupted jobs failed and retryable after restart;
- verifies SHA-256 again immediately before download.

Ready files remain available for 24 hours. Bounded job history remains for 30 days, up to 500 jobs,
so an expired or missing artifact can still explain what happened and support an explicit retry.
History and files remain scoped to the Administrator identity that created them.

Check the completed job's interval, stream and missing-range explanation before downloading. A
partial artifact contains only its available intervals and must not be described as continuous
coverage of the whole selection. Cancellation and retry are explicit actions; opening another
page does not turn a running job into a completed download. Download integrity failure or an
expired artifact requires a fresh job if the original recordings are still available.

Timestamp burn-in requires a configured re-encoding worker and otherwise fails explicitly. The
normal export path preserves source frames and timestamps without re-encoding.

For the availability and safety limits of manual deletion and catalog drift checks,
see [Recording maintenance](./recording-maintenance.md). The Administrator workflow
is under qualification; confirmed jobs can remove selected recordings permanently.

For the detailed coverage model and export lifecycle, see the
[recording integrity](https://github.com/xnorpx/keeppeek/blob/main/docs/recording-integrity.md),
[event presentation](https://github.com/xnorpx/keeppeek/blob/main/docs/event-presentation.md),
and [evidence export](https://github.com/xnorpx/keeppeek/blob/main/docs/evidence-exports.md)
references.
