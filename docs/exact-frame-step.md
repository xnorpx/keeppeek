# Recorded frame timing

On a paused stored-media cursor, exact stepping selects an encoded sample and decodes it in the
browser. The older-recorder fallback seeks by an estimated frame interval derived from camera
health. On the
variable-rate fixture below, picture 2 starts at 80 ms and picture 3 starts at 200 ms.
Advancing 40 ms from picture 2 lands at 120 ms, still inside picture 2. Two pictures in
the duplicate-timestamp fixture also share a start time. A timestamp alone cannot select
between them.

## Sample identity

`Mp4Track::sample_timing` takes a one-based sample ordinal and returns decode time,
presentation time and duration in track ticks. On a parsed track it reads the sample
tables without reading encoded media bytes. The presentation time is the decode time
plus the composition offset. Version 0 offsets are unsigned and version 1 offsets are
signed. Presentation time can precede zero. Within one track and timing epoch, the ordinal identifies a sample even when timestamps
coincide. A decoder supplies the presentation order of those pictures. In the duplicate fixture,
reference picture 11 is decoded first and displayed after B picture 10 at the same 640 ms. Exact timing lookup rejects hybrid tracks containing both initial
sample-table entries and later fragments. The MP4 reader reports an unsupported layout
when one track fragment contains multiple runs. Both cases require a complete ordinal index
before their timestamps can identify adjacent pictures.

The fixtures contain twelve independently identifiable H.264 pictures, two
random-access GOPs and both forward- and backward-referenced pictures. Each of four
luma stripes encodes one bit of the picture ID. The regression reads each numbered picture from FFmpeg's decoded pixels,
uses FFprobe's packet position to identify its encoded sample, and checks that sample's
track timestamp against the fixture schedule below. The values are relative to the
track's time origin. A demuxer can expose a different starting timestamp.

| Fixture       | Presentation times in milliseconds, in picture order   |
| ------------- | ------------------------------------------------------ |
| Constant rate | 0, 40, 80, 120, 160, 200, 240, 280, 320, 360, 400, 440 |
| Variable rate | 0, 40, 80, 200, 240, 280, 400, 440, 480, 600, 640, 680 |
| Duplicate PTS | 0, 40, 40, 200, 240, 280, 400, 440, 480, 600, 640, 640 |

Run `cargo test --locked -p mp4 --test exact_frame_timing` with FFmpeg and FFprobe on
the path. The binary fixtures total less than 7 KiB. Regenerate them with
`python tests/fixtures/exact_frame_step/generate.py` using FFmpeg with libx264.

## Recorder timing provenance

RTSP and native Reolink hand storage a single normalized camera timestamp per video
frame. The medium-term writer uses that timestamp or elapsed arrival time for decode
time, advances a duplicate or backward timestamp and writes a zero composition offset.
Those operations can discard the distinction between presentation and decode order.
A recording produced on these paths cannot establish original camera B-frame timing
through its MP4 timestamps alone.

Replaying the twelve VFR pictures through the medium-term recorder preserved all twelve decoded
picture IDs. Ten of the twelve stored presentation timestamps differed from the input. Reproduce
with `cargo test --locked -p keeppeek --test exact_frame_step -- --nocapture`.

Flat MP4 files written by older versions may carry unsigned version-0 composition offsets where
the writer was given a negative value. Both encodings can have the same bits. The reader follows
the file version. Qualifying original source timing for those files requires independent evidence.

## Paused playback

The initial request uses the displayed picture time. Further steps use the recorder's opaque
sample token, scoped to that paused cursor and generation. Reopening, seeking or changing the
source retires the selection. The recorder checks source timing and codec order across adjacent
fragments and returns an explicit status when the next picture cannot be identified. Keep keeps
the last picture visible in that case. The decoder starts from a verified random-access sample.

The lookup admits at most 4,096 samples, ten seconds of media and 32 MiB of encoded pictures.
Fragment timing and byte offsets use an index every 64 explicit entries. A random lookup sums
at most 63 preceding entries. The reader obtains a fragment or track default by multiplication.
The browser permits one active request and 32 queued button or keyboard repeats. The initial
response budget is two seconds and the decode budget is five seconds.
