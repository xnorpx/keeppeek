# Bounded Timelapse Review and Export

KeepPeek supports sparse timelapse planning and silent timelapse MP4 generation as a time-compressed derivative workflow.
A timelapse output is not continuous source evidence; every generated file is paired with a versioned JSON provenance manifest that maps each output frame back to its exact recorded source frame and UTC timestamp.

## Sampling Policy

- Cadence accepts either an explicit millisecond interval (`IntervalMs`) or a target sample count (`TargetCount`) using checked integer arithmetic over absolute UTC epoch milliseconds.
- Presets (`Last 15 minutes`, `Last hour`, and `Custom`) resolve in absolute UTC milliseconds so day and DST transitions never skip or duplicate intervals.
- For each target timestamp, KeepPeek selects the nearest decodable video picture at or after the target within the configured tolerance (`default 10 s`, `max 300 s`).
- Samples are identified by `(recording_id, fragment_sequence, sample_ordinal)` and deduplicated when multiple targets resolve to the same picture. Source timestamps stay strictly monotonic.
- Missing, retained/deleted, privacy-disabled, unsupported-codec, corrupt, and open/unfinalized intervals produce explicit gap entries (`Missing`, `Retained`, `Privacy`, `UnsupportedCodec`, `Corrupt`, `OpenRecording`). KeepPeek never repeats a stale frame across a gap.

## Explicit Limits

| Limit                      | Default / Cap                                    |
| -------------------------- | ------------------------------------------------ |
| Requested range            | `<= 31 days` (`2,678,400,000 ms`)                |
| Output samples             | `<= 3,600`                                       |
| Output dimensions          | `64x64` to `1920x1080` (even dimensions)         |
| Output frame rate          | `1` to `30` FPS                                  |
| Output file size           | `<= 512 MiB`                                     |
| Source read budget         | `<= 4 GiB`                                       |
| Concurrent generating jobs | `1` globally, `1` per requester                  |
| Visible thumbnail fetches  | `<= 2` concurrent, `64 MiB` cache                |
| Confirmation threshold     | `>= 600` samples or `>= 64 MiB` estimated output |

## Silent MP4 Generation and Provenance Manifest

- Timelapse exports are silent by default (`-an`) and never carry private source audio.
- Generation runs incrementally through a capability-gated local FFmpeg worker (`libx264` / `libopenh264`) using fixed safe arguments with no user-supplied FFmpeg flags. Normal camera recording never requires FFmpeg availability.
- Optional timestamp overlays render in the configured site timezone while the companion manifest (`schema_version = 1`) preserves exact UTC source timestamps, source recording/fragment/ordinal provenance, codec and dimensions, explicit gap intervals, sampling settings, and SHA-256 checksums.
- Partial results are created only when explicitly allowed (`allow_partial = true`).
- Source recordings are opened read-only and never modified. Cancellation, timeout, storage safety-reserve pressure, output-size overflow, and decode/encode failures terminate child processes promptly and remove partial staging artifacts inside the confined export root.
