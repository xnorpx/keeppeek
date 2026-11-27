# Recording and evidence

This page covers recorded investigation and exports in Keep (`/keep`), event search and triage in Events (`/events`), bounded temporal analytics in Activity (`/activity`), fleet recording integrity (`/recordings`), recording policies, the local fourth-tier archive, RTSP source-clock inspection, and live audio calibration. For full TOML schemas see [Configuration reference](./configuration-reference.md); for manual deletion and catalog drift repair see [Recording maintenance](./recording-maintenance.md).

KeepPeek treats footage as evidence only when media is finalized, keyframe-indexed, and playable. A connected camera or MP4 filename alone does not prove an interval can be reviewed later; see [Camera and stream health](./camera-health.md) for live evidence dimensions.

## Keep workspaces (`/keep`)

Open **Keep**, choose a camera and UTC recorded day, and select one of five investigation modes:

| Mode          | Purpose                                                                                                                          |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| **Timeline**  | Inspect footage and event intervals around one UTC playhead, with exact frame stepping and digital zoom.                         |
| **Stories**   | Review exact `story` events published by event sources alongside the **Footage calendar** (only days with a dot are selectable). |
| **Swimlanes** | Compare indexed recording availability across authorized cameras around a shared UTC window.                                     |
| **Timelapse** | Plan sparse sampled frames across a range and export a silent MP4 plus SHA-256 JSON provenance manifest.                         |
| **Export**    | Prepare, track, and download standalone evidence MP4s (job creation requires Administrator role).                                |

**Stories note:** Story conclusions and wording belong to the publishing event source; KeepPeek never rewrites them.

### Keep timeline, export, and timelapse walkthrough

Watch navigating **Timeline**, **Stories** and the **Footage calendar**, **Swimlanes**, **Export**, and **Timelapse** in Keep (`/keep`):

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/keep.desktop.timeline-export.mp4">Download the Keep timeline, export, and timelapse demo</a>.
</video>

### Playback, exact frame stepping, and shortcuts

Transport controls (play/pause, skip, position slider, volume/mute, speed, quality, fullscreen) sit below the video and stay separate from [digital zoom](./digital-zoom.md). Stored playback does not transcode unsupported codecs—retain a browser-compatible H.264 stream when browsers must play recordings directly.

| Input            | Action                                                                                          |
| ---------------- | ----------------------------------------------------------------------------------------------- |
| `Space`          | Toggle play/pause.                                                                              |
| `J` / `K` / `L`  | Reverse shuttle / pause / forward shuttle.                                                      |
| `Left` / `Right` | Step previous/next recorded frame when paused at `1.0x` (pans when digitally zoomed & focused). |
| `[` / `]`        | Set export range start / end at the current playhead.                                           |

To inspect adjacent pictures in **Timeline**, pause playback and use **Previous recorded frame**, **Next recorded frame**, or `Left`/`Right`. KeepPeek verifies sample order (including duplicate-timestamp pictures) from a random-access GOP bounded to `4,096` samples, `10 s`, and `32 MiB`. **Recorded frame** displays the UTC recording position (syncing timeline and export handles); **Source presentation** also appears when original camera timing differs. Use **Read selected frame time** for screen-reader announcements and **Play recording** to resume. Gaps, recording changes, or unresolvable source timing keep the last displayed picture and explain why. Older recorders or browsers without exact decoding fall back to estimated frame-rate stepping (variable-rate media may skip or repeat pictures).

### Synchronized multi-camera comparison

In **Keep**, open **Compare with** and use **Add camera** or **Group** to compare up to `4` cameras on one UTC playhead (`2` active decoders on compact screens; groups larger than four load the first four authorized cameras with a notice). **Remove** drops a camera (dropping to one returns to single-camera view at the shared time).

- **Navigation:** **Play**, **Pause**, **Speed**, 5-second seek buttons, **Seek to UTC** → **Go to time**, and **Previous event** / **Next event** (current UTC day, up to `256` events/stream with truncation notice) drive one shared UTC clock. When paused on a focused camera supporting exact stepping, **Prev frame** / **Next frame** steps that camera's verified picture while sibling cameras seek to the stepped UTC timestamp without claiming identical frame boundaries.
- **Drift and codec diagnostics:** Each tile displays latest presented UTC frame time, stream/codec, and **Diagnostics** (drift, forward buffer, audio state, correction count). Drift of `100`–`250 ms` uses a bounded `3%` rate slew; drift `> 250 ms` seeks once buffered and shows **Synchronizing** with measured offset until back within tolerance (`750 ms` minimum between corrections per player). After two failed paused corrections, **Retry recording** reopens that camera. Gaps show explicit gap status and rejoin when playable footage resumes; unsupported codecs name the stream and codec instead of showing a gap.
- **Audio, focus, and export:** **Audio** selects the single audible camera (**Mute** / **Unmute**; tiles show active, muted, unavailable, or configured audio-offset state). **Focus** sets decoder priority, paused frame-step target, and export camera. Mark up to `2 min` with **Set range start** / **Set range end**, then choose **Export focused camera** (**Back to comparison** restores cameras, focus, audio, range, and playhead). **Single camera** exits to the focused camera; **Back to event** returns to Events.
- **Session behavior:** Browser storage remembers playback speed and mute. Pausing, seeking, or changing focus/audio updates the URL; **Copy link to this moment** captures the live playhead while playing. Hidden tabs release players and reopen on return; disconnects clear views and reopen paused at the last shared UTC time. Unavailable cameras in a link still open remaining authorized cameras.

### Keep on phones

In portrait, Keep shows a compact camera/date row, player, and horizontal timeline. Tap the camera or date to open **Camera and date** (search or wrap-around previous/next camera; available days only). Tap **Playback options** for volume, speed, quality, **Copy link to this moment**, and **Refresh recordings** (**Done**, close, `Escape`, or browser Back returns focus without resetting playback). In multi-camera comparison on phones, **Comparison options** groups camera, audio, speed, UTC seek, and range controls. All five modes remain accessible.

### Bounded timelapse review and silent MP4 export

Open **Timelapse** in **Keep** to inspect quiet periods via sparse sampled frames without loading continuous video. Choose **Last 15 min**, **Last hour**, or **Custom** (absolute UTC milliseconds, unaffected by DST transitions), set cadence by target count or second interval, and review lazily loaded thumbnails. Selecting any sampled frame opens **Timeline** at its exact UTC source timestamp.

Unavailable ranges (`Missing`, `Retained`, `Privacy`, `UnsupportedCodec`, `Corrupt`, `OpenRecording`) render as explicit gap cards—KeepPeek never repeats a stale frame across a gap.

Generating a timelapse MP4 (**Generate Silent MP4**) is a separate Administrator action using a capability-gated local FFmpeg worker (`libx264` / `libopenh264`, fixed safe arguments). Output is always **silent** (`-an`, never carries source audio) and is a time-compressed derivative rather than continuous source evidence. Optional timestamp overlays render in the configured site timezone, while the companion SHA-256-verified JSON provenance manifest (`schema_version = 1`, **Manifest JSON**) records exact UTC source timestamps, `(recording_id, fragment_sequence, sample_ordinal)` identities, sampling settings, and omitted gap intervals. Source recordings remain read-only and immutable.

| Timelapse bound           | Limit / default                                                                              |
| ------------------------- | -------------------------------------------------------------------------------------------- |
| Requested range           | `<= 31 days` (`2,678,400,000 ms`)                                                            |
| Sampled frames            | `<= 3,600` (deduplicated; tolerance default `10 s`, max `300 s`)                             |
| Output dimensions & FPS   | `64×64` to `1920×1080` (even dimensions), `1`–`30 FPS`                                       |
| Output size / source read | `<= 512 MiB` MP4 / `<= 4 GiB` source read budget                                             |
| Concurrency & thumbnails  | `1` generating job globally and per requester; `<= 2` thumbnail fetches (`64 MiB` cache)     |
| Explicit confirmation     | Required at `>= 600` samples or `>= 64 MiB` estimated output; gaps require **Allow partial** |

See [Bounded Timelapse Review and Export](https://github.com/xnorpx/keeppeek/blob/main/docs/timelapse.md).

### UTC rules and explicit gaps

Recorded days, timelines, and event time filters use **UTC**. Switching cameras keeps the exact UTC instant even if the destination camera has a gap; switching recorded days preserves the UTC clock time on the chosen day (midnight stays `00:00:00 UTC` of that day).

KeepPeek never silently jumps an unavailable timestamp to the latest clip. Gaps stay visible with **Previous recording** and **Next recording** actions (searching a bounded `±5 min` window within that UTC day) or direct timeline selection. Loading, no recorded days, unavailable footage, and decode failures are distinct states.

## Copy a recording moment

Use the link icon in Keep's command bar (or beside **Open at this moment** in Events) to copy an authenticated `/keep` deep link. Copying reads the current clock without pausing, seeking, or modifying history. If clipboard access is denied, unsupported, or exceeds `2.5 s`, a dialog selects the URL in a read-only field for manual copy (`Escape` closes and restores focus).

These links require server authentication and camera authorization—they are not public shares and contain no access keys, session IDs, media URLs, or file paths (treat camera IDs and event filter context as private operational data). Inspection zoom crops are never included.

| `/keep` query parameter                      | Meaning                                                                                          |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `camera`                                     | Server-provided camera source identifier.                                                        |
| `at`                                         | Absolute Unix timestamp in integer milliseconds (wins over conflicting `date`).                  |
| `date`                                       | UTC `YYYY-MM-DD` derived from `at`.                                                              |
| `stream`                                     | Requested preference: `auto`, `high`, `low`, `main`, or `sub`.                                   |
| `mode`                                       | Omitted for Timeline; otherwise `stories`, `swimlanes`, `timelapse`, or `export`.                |
| `event`                                      | Optional event identifier.                                                                       |
| `returnTo`                                   | Optional validated local `/events` route preserving supported filters and selected event.        |
| `compare`, `audio`, `rangeStart`, `rangeEnd` | Optional synchronized comparison cameras (`2`–`4`), audio camera, and marked range (`<= 2 min`). |

Playback restoration for retained, supported media is verified within `max(1 s, 1 source frame)`. Missing and unauthorized cameras share one unavailable-or-not-authorized message; malformed links fail visibly instead of substituting another camera.

## Export evidence

Administrators can export up to `2 minutes` of continuous or explicitly partial retained footage from **Keep → Export** as a standalone MP4 (no re-encoding on the normal path; timestamp burn-in requires a configured re-encoding worker and otherwise fails explicitly). Opening export from an event seeds `±15 s` around the event while preserving camera, stream, event revision, filters, and **Back to event**.

- **Lifecycle states:** `running`, `ready`, `partial`, `failed`, `cancelled`, `expired`.
- **Job guarantees:** Deduplicates identical running jobs; offers an existing matching `ready` file before creating a new one; names every missing interval before allowing a `partial` export; stops after `30 s` without progress or `5 min` total runtime; deletes partial files on failure/cancel; marks interrupted jobs `failed` and retryable after restart; re-verifies SHA-256 immediately before download.
- **Retention and scope:** Ready files expire after `24 hours`; bounded job history retains up to `500` jobs for `30 days`, scoped to the creating Administrator identity. A `partial` file covers only its available intervals—never describe it as continuous coverage.

## Events search, boolean queries, and triage (`/events`)

Open **Events** to search authorized cameras by UTC date/time range, camera, event kind, origin, zone, confidence, image availability, text prefix, review state, and bookmarks. Select a card to inspect its canonical preview, metadata, revision, **Open at this moment**, or export (**Back to event** restores filters and selection). Check server capability notices before assuming an empty result means no events exist.

### Events search, review, and Activity walkthrough

Watch **Advanced search** boolean filtering, inspecting the event detail drawer, managing bookmarks, and opening **Activity summary** (`/activity`):

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/events.desktop.search-review.mp4">Download the Events search, review, and Activity demo</a>.
</video>

### Advanced boolean and hybrid semantic search

Select **Advanced search** (requires `keeppeek.event-search.advanced.v1`) to build nested `AND`, `OR`, and `NOT` trees over `14` indexed predicates (`camera_ids`, `group_ids`, `event_types`, `origins`, `zones`, `missing_zone`, `missing_confidence`, `has_image`, `confidence`, `time_range`, `text`, `workflow`, `producer_ids`, `model_ids`).

- **Strict two-valued null and negation rules:** Every leaf evaluates to `true` or `false` inside the caller's authorized cameras and UTC window—never SQL `UNKNOWN`. Missing `zone` or `confidence` makes positive zone/threshold predicates `false`, so `NOT (zone in ['porch'])` includes events with missing zones, and `NOT (...)` never widens camera authorization or UTC bounds.
- **Hybrid semantic ranking (`keeppeek.event-search.semantic.v1`):** Ranks structured-predicate matches by exact cosine similarity against embeddings supplied by authorized external producers (`Administrator` role). KeepPeek never generates embeddings implicitly and never returns raw embedding vectors in hits, summaries, logs, errors, exports, or planner diagnostics. Embeddings are keyed by `(event_id, model_id, model_version)` and bound to the event's current `revision` (invalidated/cascaded in the same transaction on update or delete). Queries compare only within exact `(model_id, model_version, dimensions)`, filter `minimum_score` before top-K, and tie-break deterministically by `(score DESC, start_time_ms DESC, event_id ASC)`. If semantic storage is unavailable, structured boolean and common search continue normally.
- **Saved searches:** Owner-scoped saved searches persist under `[saved_event_searches]` in `config.toml` (not copied with catalog DB embeddings; see [Saved event searches](./configuration-reference.md#saved-event-searches)). Each entry tracks a revision, flags missing/tombstoned camera or group references, and preserves newer schema versions read-only.

| Advanced search bound          | Limit                                                                                                     |
| ------------------------------ | --------------------------------------------------------------------------------------------------------- |
| Expression depth / AST nodes   | `<= 8` depth / `<= 64` nodes                                                                              |
| Predicate list values / text   | `<= 64` total list values; `<= 256 B` value (`128 B` model/producer); `<= 16 KiB` expression              |
| Cameras / UTC window           | `<= 127` authorized cameras / `<= 31 days`                                                                |
| Candidates / page / URL `expr` | `<= 10,000` candidates / `<= 128` hits per page / `<= 2,048 B` URL param (use saved search ID above that) |

### Review workflow, bookmarks, and canonical previews

- **Review states:** **Reviewed** and **Dismissed** are independent per-identity flags (**Unreviewed** = neither set). Dismissal removes an item from the queue without deleting recordings or attachments, and event updates never reset workflow flags. Counts are server-computed. Bulk actions (**Mark N visible reviewed** / **Mark N selected reviewed**) support up to `128` selected events across pages; undo applies only to acknowledged targets, and stale writes prompt **Reload and retry** while keeping drafts.
- **Identity:** Named credentials retain review state across credential rotation. Trusted-local browsers use a persistent **Local workspace** identity (clearing site storage starts a new workspace; unavailable storage disables mutations instead of silently using an ephemeral ID).
- **Saved bookmarks:** Shared with authorized camera viewers; creator or Administrator can edit plain-text notes (up to `1,024` UTF-8 bytes) or delete. The bookmark library has its own source/date/creator filters. Bookmarks and exports do not pin recordings through retention; deleted-event references persist for `90 days` as honest metadata-only entries naming missing events, sources, or media.
- **Canonical preview:** Events, Keep, notifications, MQTT, and exports share the same event revision and canonical preview (producer-named attachment, else snapshot → story frame → retained thumbnail by deterministic ordinal/time/ID order). Missing canonical bytes report unavailable instead of silently substituting another image; bounding boxes render only when their coordinate space matches the canonical image. Event `kind` is authoritative; semantic icons are presentation-only.

## Activity summary and temporal analytics (`/activity`)

Open **Activity summary** from Events (or `/activity`, requires `keeppeek.activity-analytics.v1`) for bounded temporal summaries of producer-reported `recording_events` metadata. Activity never reprocesses video, runs object detection, deduplicates real-world objects across cameras, or infers real-world identity, occupancy, or threat totals.

- **Scope and privacy:** Camera authorization is enforced before counting, comparing, or ranking. `reviewed` and `dismissed` workflow filters stay strictly principal-local (global reviewed totals are never computed).
- **Windows and civil timezones:** Choose **Last 24 hours**, **Last 7 days**, or **Last 30 days** (underlying storage always queries UTC half-open `[start, end)`), granularities (`Hour`, `Day`, `Week`, `Month`, `Hour of day (0-23)`, `Weekday vs weekend`), an IANA display timezone, week start (`Monday`/`Sunday`), and **Comparison** (**Previous equal duration**, **Previous calendar window**, or **No comparison**). Civil day/week/month boundaries respect 23/25-hour DST days and leap days; hour buckets record `utc_offset_seconds` and `dst_fold` (spring-forward missing hours are never fabricated); non-hour-offset zones (such as `Asia/Kolkata`) sum interior UTC-hour rollups and scan exact partial edge slices.
- **Measures and confidence bands:** Reports `event_count` (`== distinct_event_count`), clipped interval durations (`total_duration_ms`, `average_duration_ms` excluding zero-length point events, plus `open_interval_event_count`), completeness percentages (`image`, `confidence`, `zone`), and fixed confidence bands: `unknown`, `low [0.0, 0.5)`, `medium [0.5, 0.8)`, `high [0.8, 1.0]`.
- **Completeness and source health:** Retention deletions mark overlapping buckets `partial_retention`. Historical event-source health is never extrapolated backward from present status—cameras are classified per window as `reporting`, `healthy_no_reports`, `missing_health`, `no_expected_source`, or `unknown_history`.
- **Exports and rollup recovery:** Use **Export CSV** or **Export JSON** (capped at `25,000` aggregate rows). Incremental double-buffered rollups degrade safely without blocking canonical event writes if quotas (`500,000` default rows, range `1,000`–`5,000,000`) or integrity checks trip; Administrators can run **Rebuild rollups** for a bounded shadow rebuild (`<= 512` events/tx, `<= 65,536` dirty catch-up IDs) without modifying canonical events.

| Activity bound                | Limit                                                                         |
| ----------------------------- | ----------------------------------------------------------------------------- |
| Time buckets / grouped series | `<= 366` buckets / `<= 64` grouped series / `<= 64` ranked rows per dimension |
| Cameras / `group_by` / range  | `<= 127` authorized cameras / `<= 8` dimensions / `<= 10 years` UTC           |
| Canonical fallback / workflow | `<= 7 days` short-range fallback / `<= 250,000` events workflow scan          |
| Response payload / export     | `<= 1 MiB` serialized response / `<= 25,000` aggregate export rows            |

See [Event Activity Summaries and Temporal Analytics](https://github.com/xnorpx/keeppeek/blob/main/docs/activity-analytics.md).

## Camera recording policies and pre-recording

Each camera uses one recording policy in its camera settings (see [Camera defaults and camera entries](./configuration-reference.md#camera-defaults-and-camera-entries)):

| Policy        | Behavior                                                                                                  |
| ------------- | --------------------------------------------------------------------------------------------------------- |
| `off`         | Records no video; intentional and never reported as a gap.                                                |
| `sub`         | Records substream continuously.                                                                           |
| `main`        | Records main stream continuously.                                                                         |
| `both`        | Records main and substream independently.                                                                 |
| `event-boost` | Default. Records substream continuously, switches to main at a keyframe on events, then returns to sub.   |
| `event-only`  | Records selected `main` or `sub` stream only for accepted events plus available pre-event memory history. |

- **Pre-recording memory buffer:** `event-only` and `event-boost` accept `0`–`30 s` of in-memory pre-event history (default `0 s`, default RAM ceilings `64 MiB`/stream and `256 MiB` globally in advanced storage settings). In `event-boost`, KeepPeek delays continuous sub writes by at most the configured pre-roll so main-stream history can replace it in one recording (uncommitted delay can be lost on crash; memory pressure shortens history). Recordings always begin at a retained decodable keyframe; startup, long GOPs, or byte limits can shorten the interval. Audio packets crossing the video boundary are omitted without re-encoding. History does not survive restart, privacy activation, reconnect, or storage pause. Before downgrading to a version without `event-only` support, switch policies, set pre-recording to `0`, and back up configuration (committed media needs no conversion).
- **Runtime profiles and write-failure recovery:** When a runtime profile reports a camera **Degraded** (storage rejected settings, queue/acknowledgement failed, or an active input write failed after acceptance), new recording input stops while live viewing can continue. **Pending** awaits writer acceptance; **Applied** confirms configuration acceptance only (verify retained footage in **Recording integrity**). After repairing settings or storage, save/discard profile edits and select **Reapply active policy** in **Settings → Runtime profiles** (idle event recording and ordinary writes do not clear a latched write failure automatically).
- **Admission vs. retention:** Recording policies control admission, not independent per-event-class retention deadlines; storage pressure can shorten coverage. See [recording policy audit](https://github.com/xnorpx/keeppeek/blob/main/docs/recording-policy-audit.md).

## Local fourth-tier archive (`ARCHIVE`)

KeepPeek separates RAM buffering, active recording files, finalized primary media, and the optional fourth **ARCHIVE** tier (a verified replica on a mounted local filesystem—distinct from the `archive` named-volume placement role, which relocates primary files without creating a second replica). Offsite providers are not yet supported.

1. As Administrator, create a private directory owned by the recorder account on a mounted local disk that does not overlap primary or metadata roots.
2. Configure target ID, root, limits, and optional source filter in **Settings → Storage → Local archive** (see [Fourth-tier local archive](./configuration-reference.md#fourth-tier-local-archive)), save with revision check (**Reload archive settings** discards drafts), restart KeepPeek, test the destination, and refresh jobs (a pending probe is not a passing test).
3. Queue manual copies of finalized recordings or completed exports, or enable automatic copying after minimum age (empty source list selects all). Copies verify the replica independently; refresh before retrying uncertain results (duplicate replies reconcile by durable job ID).

- **Playback and primary eviction:** Primary eviction defaults **off**. Enable automatic eviction deliberately or confirm **Evict primary** per job (checked against verified archive, active reader leases, and retention holds). Evicted recordings remain indexed in Keep and stream needed byte ranges directly from archive with the notice **ARCHIVE — Playing from archive. Startup and seeking may take longer.** Use **Restore primary** to transfer a verified copy back.
- **Archived exports:** After local export expiry/pruning, **Keep → Export → Find completed exports** (same account, no segment selected) lists up to `64` additional verified archived exports newest-first (older exports remain accessible by job ID; active camera privacy hides and blocks downloads; download verifies the mounted target).
- **Verified archive deletion:** **Preview archive deletion** or **Preview expired archive copies** (`16` jobs/page when retention days are set; blank retains indefinitely; expiry alone never deletes) previews targets/IDs/bytes; **Confirm archive deletion** authorizes only that page. Deletion verifies both copies within one operation timeout, then records retirement intent, quarantines, stages, and re-verifies before unlink (resuming from the last durable step on restart). Windows holds an exclusive read handle through removal; Linux requires `fs-verity` plus immutable inode flag (`chattr -i -- /abs/path/to/verified-primary.mp4` only after `acknowledged` if normal moves/retention must resume; `fs-verity` persists); mutable Linux/macOS retain both copies. Acknowledged deletions exclude that object from future automatic/manual archiving in the current catalog. Evicted recordings require **Restore primary** first; holds, active readers, changed media, or unavailable primary block deletion.
- **Cancellation and offline mounts:** Cancel stops cancellable copy/restore jobs and cleans partial staging (**Retry / finish cleanup** keeps job identity and waits for acknowledged cleanup). Durable primary eviction intents cannot be cancelled and must reconcile. An offline archive mount marks archive health unavailable without blocking live recording ingest; never substitute a missing mount. See [Recording archive recovery](./recording-archive-recovery.md).

## Prove recording integrity (`/recordings`)

Open **Recording integrity** to verify retained footage before an incident occurs. It reports policy demand, writer state and latest frame/write/finalization/catalog timestamps, oldest/newest media, effective duration, attributed bytes, estimated daily growth, keyframe-indexed playable coverage (`24 h`, `7 d`, `30 d`), and exact open or closed gaps with causes (`source silence`, `transport outage`, `stale frames`, `decode failure`, `writer failure`, `disk pressure`, `retention deletion`, `storage migration`, `catalog mismatch`, `unknown`) plus links to nearby footage, health, and logs.

| Fleet state            | Meaning                                                               |
| ---------------------- | --------------------------------------------------------------------- |
| **Recording healthy**  | Every requested stream has current writer progress.                   |
| **Recording degraded** | A requested writer is stalled or failed.                              |
| **Paused by policy**   | Effective policy intentionally requests no recording.                 |
| **Not configured**     | No usable recording stream is configured (shown only when count > 0). |
| **Unknown**            | Insufficient current evidence for a truthful determination.           |

Bounds: `25` cameras/page default, at most `256` exact recent ranges per stream, and deterministic time buckets with exact totals over longer windows.

### Interrupted legacy recording initialization

On Unix, if a failed legacy header write encounters two files at the same active name, the current occupant stays in place and the displaced entry moves to `.keeppeek-header-recovery-v1/` in that recording hour. Its private `<active name>.pins-v2/` directory holds a hard link encoding the writer's device and inode across restarts. Listings, integrity scans, and pruning skip the recovery directory; the next request for that active name verifies the pin before reclaiming a failed partial file or restoring the displaced file. For older recordings, stop directory writers and inspect both files before changing either (entries without a valid recovery record or in untrusted/inaccessible directories are left untouched). Named storage volumes use their allocation ledger instead.

## Compare RTSP source clocks

RTSP streams log `camera.rtsp.source_timing` / `RTSP source timing` at info level on completed sessions and failed `PLAY` attempts with accepted RTP. To collect periodic samples, enable `keeppeek::rtsp::timing=trace`—the first enabled collection starts one `5-minute` window per recorder process (reconnects/toggles do not extend it). Each single-track media stream with at least one accepted RTP packet emits its own structured entry, keyed by four network-order 32-bit UUID words (`session_uuid_0..3`, and `camera_uuid_0..3` for registered cameras; unregistered cameras log `camera_registered=false`).

| Log field                                                       | Meaning                                                                                                         |
| --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `first_rtp_ticks`, `latest_rtp_ticks`, `clock_rate_hz`          | Raw 32-bit RTP timestamps (`elapsed_ticks * 1000 / clock_rate_hz = ms`; use extended fields across wrap).       |
| `missing_sequence_numbers`, `backward_timestamp_steps`          | Sequence gaps and backward PTS steps (reordering can cause backward steps; neither alone proves a clock reset). |
| `sender_rtp_ticks`, `sender_ntp_seconds`, `sender_ntp_fraction` | Latest matching RTCP SR RTP/NTP clock (camera-supplied; may be absent or inaccurate).                           |
| `latest_audio_minus_video_play_us`                              | Audio minus video position on camera PLAY timeline (positive = audio ahead; interleaving affects readings).     |

Readings precede transcoding, timeline normalization, and presentation offsets; measure audible sync with a flash/tone preview or physical clap. Newly recorded RTSP files retain per-sample source timing for the file's lifetime; older files retain stored PTS only. When long GOPs or repeated clock discontinuities exhaust a file's metadata budget, KeepPeek logs `recording recovery budget exhausted`, preserves the valid prefix, and rotates at the next keyframe (leaving a coverage gap until that keyframe).

## Calibrating live audio

Open a camera's **Audio synchronization** settings and select **Live audio** to align live audio and video on the Main or Sub stream:

1. Produce a clearly visible sound (such as a clap) on the target stream and listen across several repetitions to separate constant offset from network/browser jitter.
2. Enter a whole-millisecond offset from `-200` through `+200`: use a **positive** offset to delay audio when sound arrives early, or a **negative** offset to delay video when sound arrives late (preview displays the added video latency).
3. Select **Apply preview** to test in the current viewer, **Reset preview** to restore saved timing, then save as the camera default or as a per-stream override (`0` disables correction; **Inherit** uses the camera default).

Live timing is independent of recorded-audio sync and never modifies stored recordings or exports. Live diagnostics show saved vs. applied timing separately from browser jitter-buffer delay, and `HealthCommand` / camera health exposes `live_audio` held bytes, held encoded video frames, and capacity/expiry drops (rising drops indicate the server's time or memory hold budget was exceeded). Measurements end at the server listener handoff; camera capture, external transcoders, speakers, and physical distance require separate measurement.

Further references: [recording integrity](https://github.com/xnorpx/keeppeek/blob/main/docs/recording-integrity.md), [event presentation](https://github.com/xnorpx/keeppeek/blob/main/docs/event-presentation.md), and [evidence exports](https://github.com/xnorpx/keeppeek/blob/main/docs/evidence-exports.md).
