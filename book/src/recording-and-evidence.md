# Recording and evidence

KeepPeek treats a recording as evidence only when the media is finalized, indexed, and playable.
A connected camera or an MP4 filename alone does not prove that the requested interval can be
reviewed later. The underlying live evidence dimensions are defined in
[Camera and stream health](./camera-health.md).

## Choose what each camera records

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

Timestamp burn-in requires a configured re-encoding worker and otherwise fails explicitly. The
normal export path preserves source frames and timestamps without re-encoding.

For the detailed coverage model and export lifecycle, see the
[recording integrity](https://github.com/xnorpx/keeppeek/blob/master/docs/recording-integrity.md),
[event presentation](https://github.com/xnorpx/keeppeek/blob/master/docs/event-presentation.md),
and [evidence export](https://github.com/xnorpx/keeppeek/blob/master/docs/evidence-exports.md)
references.
