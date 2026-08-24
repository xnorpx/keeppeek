# Storage Module

Three-tier recording pipeline that ingests frames from any camera source
(RTSP via retina, Reolink Baichuan via reo-proto) through a unified format
and writes them to disk in time-partitioned segments.

## Data Flow

```
  ┌──────────────┐     ┌──────────────┐
  │  reo-proto   │     │   retina     │
  │  (Baichuan)  │     │   (RTSP)     │
  └──────┬───────┘     └──────┬───────┘
         │                    │
         │  convert to        │  convert to
         │  MediaFrame        │  MediaFrame
         ▼                    ▼
  ┌────────────────────────────────────┐
  │        KeepPeekEvent::Frame        │
  │   { camera_ip, stream, frame }     │
  └──────────────────┬─────────────────┘
                     │
                     ▼
  ┌────────────────────────────────────┐
  │          StorageEngine             │
  │      (per-camera pipeline)         │
  └──────────────────┬─────────────────┘
                     │
         ┌───────────┴───────────┐
         ▼                       ▼
  ┌──────────────┐       ┌──────────────┐
  │  Short-Term  │──────▶│ Medium-Term  │
  │  (in-memory) │ drain │  (on-disk)   │
  └──────────────┘       └──────┬───────┘
                                │ finalize
                                ▼
                         ┌──────────────┐
                         │  Long-Term   │
                         │  (on-disk)   │
                         └──────────────┘
```

## Unified Frame Format

Both camera sources convert their native types into `MediaFrame` before
entering the pipeline. This decouples the storage layer from any
protocol-specific details.

```
MediaFrame
├── Video(VideoFrame)
│     codec:       H264 | H265
│     is_keyframe: bool
│     width:       u32
│     height:      u32
│     data:        Vec<u8>    (AVCC / length-prefixed NALUs)
│
└── Audio(AudioFrame)
      codec:       Aac | G711Alaw | G711Ulaw | Adpcm
      sample_rate: u32
      data:        Vec<u8>
```

`RecordingFrame` wraps a `MediaFrame` with a `received_at: Instant`
timestamp used for time-based eviction and segment rotation.

### NAL Unit Format

Video data in `VideoFrame.data` is always AVCC format (4-byte
big-endian length prefix per NALU), which is what the MP4 container
expects.

```
┌──────────────────┬────────────────────────────┐
│ length (4 B BE)  │ NALU payload               │
├──────────────────┼────────────────────────────┤
│ length (4 B BE)  │ NALU payload               │
└──────────────────┴────────────────────────────┘
```

| Source    | Native format          | Conversion            |
| --------- | ---------------------- | --------------------- |
| retina    | AVCC (length-prefixed) | None needed           |
| reo-proto | Annex B (start codes)  | `nal::annexb_to_avcc` |

## Three Tiers

### Short-Term (in-memory)

`ShortTermBuffer` — a `VecDeque<RecordingFrame>` ring buffer.

- Holds the most recent N seconds of frames while a stream is idle (default 120 s).
- Automatically evicts frames older than `max_duration` on every push.
- Drains completed segments on keyframe boundaries via
  `drain_up_to_last_keyframe_before(cutoff)`.
- A live WebRTC guard or renewable recording-review lease marks one stream active. Active streams
  drain queued frames immediately and remain active for a short grace period after the last viewer
  leaves; unrelated streams keep their idle batching window.

### Medium-Term (on-disk, active segment)

`MediumTermWriter` — MP4 muxer using the `mp4` crate.

- Receives batches of `RecordingFrame`s drained from `ShortTermBuffer`.
- Buffers the first GOP to discover the video and optional AAC track set, then writes a flushed
  `ftyp`/`moov` initialization range with `mvex` track defaults and bounded metadata padding.
- Registers video sample descriptions from keyframes. Different GOPs in one track may use
  different codecs, decoder parameter sets, and dimensions; each `tfhd` selects the matching
  one-based `stsd` entry.
- Serializes event-boost admission and writer enqueue under one policy lock so concurrent main and
  sub producers cannot reorder frames across a handoff keyframe.
- Writes H.264/H.265 video track (timescale 90 000) and optional AAC
  audio track (timescale = sample rate). Non-AAC audio is skipped.
- Derives video sample durations from adjacent decode timestamps. It does not assume a frame rate.
- Stored browser delivery emits one sample description per GOP period and normalizes that
  fragment's `tfhd` index to 1. This keeps the archive flexible while remaining compatible with
  MSE `SourceBuffer.changeType` and updated initialization segments.
- Writes and flushes one `moof`/`mdat` fragment per video GOP. Every fragment begins with a video
  keyframe and has an exact byte range suitable for the recording catalog and HTTP range serving.
- Active segments use a `.mp4.active` suffix. Finalization flushes the last fragment and removes
  the suffix; it does not remux media or move the `moov` box.
- Segment rotation is driven by `StorageEngine` when
  `writer.elapsed() >= medium_term_duration`.

### Long-Term (on-disk, finalized segments)

`LongTermStore` — manages finalized segment files.

- Finalized segments are simply the renamed medium-term files (`.active`
  suffix removed). No copy or re-encoding.
- Provides queries: `finalized_segments()`, `total_bytes()`.
- Supports date-based purging via `purge_before()` to enforce retention.

## Disk Layout

Camera-first structure inspired by Frigate, optimized for per-camera
retention policies:

```
<root>/
  <camera_id>/
    YYYY-MM-DD/
      HH/
        MMSS.mp4            ← finalized segment
        MMSS.mp4.active     ← segment currently being written
```

Example:

```
recordings/
  front_door/
    2026-02-17/
      14/
        3509.mp4
        3842.mp4.active
  backyard/
    2026-02-17/
      14/
        3510.mp4
```

## StorageEngine

Central orchestrator that owns one pipeline per camera. All timing
decisions live here so that limits are configurable and testable with
short durations (1–5 s for unit tests, minutes/hours for production).

```
StorageConfig {
  medium_term_path:          PathBuf,
  long_term_path:            PathBuf,
  recording_catalog_path:    PathBuf,
  event_thumbnail_path:      PathBuf,
  event_thumbnail_max_bytes: u64,
  short_term_duration:       Duration,   // default 120 s
  medium_term_duration:      Duration,   // default 1800 s (30 min)
  flush_interval:            Duration,   // default 60 s
}
```

On each `ingest()` call:

1. Frame is pushed into `ShortTermBuffer`.
2. If the stream has viewer demand, queued frames are appended to `MediumTermWriter` immediately.
   Otherwise, `flush_interval` batches frames older than `short_term_duration`.
3. If the active writer has run for `medium_term_duration`, it is
   finalized and a new segment is started.

Video and audio remain in ordinary `.mp4` files on disk. The embedded Turso catalog stores
recording paths, timestamps, initialization and fragment byte ranges, plus normalized camera and
KeepPeek events. It does not store encoded media blobs.

The recording catalog defaults to `recordings.db` under long-term storage, and camera-native event
snapshots default to `.event-thumbnails` there. Both paths are configurable. Snapshots are resized
once to 384×216 JPEG thumbnails and written atomically. The default aggregate thumbnail limit is
1,024 MiB; when exceeded, the oldest JPEGs and their catalog references are removed. A zero limit
leaves thumbnail storage unbounded. HTTP thumbnail lookup verifies the event's camera ownership and
canonical path before opening the file.

## Module Map

| File             | Type               | Role                                         |
| ---------------- | ------------------ | -------------------------------------------- |
| `catalog.rs`     | `RecordingCatalog` | Turso-backed recording and fragment metadata |
| `demand.rs`      | `RecordingDemand`  | Per-stream viewer guards and review leases   |
| `frame.rs`       | `MediaFrame`       | Unified codec-agnostic frame types           |
| `segment.rs`     | `RecordingFrame`   | Timestamped wrapper around `MediaFrame`      |
| `short_term.rs`  | `ShortTermBuffer`  | In-memory ring buffer with time eviction     |
| `medium_term.rs` | `MediumTermWriter` | Fragmented MP4 muxer (H.264/H.265 + AAC)     |
| `long_term.rs`   | `LongTermStore`    | Finalized file management and purging        |
| `engine.rs`      | `StorageEngine`    | Per-camera pipeline orchestrator             |
| `events.rs`      | `EventStore`       | Event lifecycle and secure thumbnail storage |
| `layout.rs`      | (functions)        | Path generation for the on-disk hierarchy    |
| `nal.rs`         | (functions)        | Annex B ↔ AVCC conversion, SPS/PPS extract   |
| `metadata.rs`    | `CameraMetadata`   | Per-camera metadata types (zones, events)    |
