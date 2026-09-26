# Event pre-recording contract proposal

Status: proposal for issue #172. The protected API has not been edited.
Base: `7ab761b`. Existing field numbers remain unchanged.

## Protected files and generated output

The requested API scope is `api/webrtc.proto` and `api/webrtc.md`, plus canonical
regeneration of `ui/src/lib/proto/webrtc_pb.ts` and normal Rust build generation.
No new HTTP endpoint, arbitrary command payload, or unrelated schema is proposed.

## Additive field map

`CameraRecordingMode` gains `EVENT_ONLY = 6`. A new `EventRecordingStream` enum
has `UNSPECIFIED = 0`, `SUB = 1`, and `MAIN = 2`. The effective default is main.
Unknown enum values fail validation rather than falling through to EventBoost.

| Message                        | New fields and tags                                                                  |
| ------------------------------ | ------------------------------------------------------------------------------------ |
| `UpdateCameraConfiguration`    | optional pre-roll seconds 17; optional event stream 18                               |
| `CameraConfigurationPatch`     | optional-update pre-roll seconds 16; optional-update event stream 17                 |
| `CameraDefaultPatch`           | optional-update pre-roll seconds 8; optional-update event stream 9                   |
| `CameraTemplateValues`         | optional pre-roll seconds 10; optional event stream 11                               |
| `CameraDefaultValues`          | configured/effective pre-roll seconds 14/15; configured/effective event stream 16/17 |
| `CameraEffectiveConfiguration` | effective pre-roll seconds 12; effective event stream 13                             |
| `CameraSettings`               | effective pre-roll seconds 19; effective event stream 20                             |
| `RuntimeStorageConfiguration`  | optional per-stream pre-roll byte limit 16; optional global byte limit 17            |
| `CameraHealthSnapshot`         | pre-roll diagnostics 19                                                              |

Use the existing optional-update and effective-value conventions. Add matching
typed event-stream wrappers with set/clear and configured/inherited/effective
values. Omitted new fields preserve existing configuration during updates.
Defaults are zero seconds, 64 MiB per stream, and 256 MiB globally. Validate
duration as 0–30 seconds, enabled budgets as nonzero, and every conversion before
mutating configuration. Store settings in the existing configuration file.

## Diagnostics

A typed pre-roll diagnostic reports enabled/active state, selected stream,
requested milliseconds, available milliseconds, retained bytes, and one bounded
reason enum. Required reasons are startup, missing keyframe, duration eviction,
per-stream pressure, and global pressure. Additional explicit states cover
ready history, pending replay, disabled policy, malformed order, decoder/session discontinuity, privacy,
storage pause, and writer failure. A queued replay is pending, not committed
coverage; writer failure must not report a successful recording interval.

Diagnostics contain no encoded media, private URLs, credentials, or host paths.
Use existing Administrator write authorization and camera read filtering.

## Recording semantics requiring the owner decision

The owner approved the opt-in delay below. Protected API changes remain pending
explicit approval for this issue.

EventBoost with nonzero pre-roll holds recent candidate sub/main GOPs before
committing one monotonic recording. The opt-in delay is bounded by the selected
history duration; memory pressure shortens optional history and releases
continuous sub coverage. A crash can lose the uncommitted horizon. Zero pre-roll
preserves the existing immediate admission path.

The hard history ceiling is 30 seconds. A GOP beginning before that ceiling is
unavailable even if it would otherwise cover the requested cutoff. Report the
actual shortened result rather than retaining extra history.

Event-only uses an exclusive post-event endpoint. No video sample at or after
that endpoint is persisted. Audio packets must fit wholly inside the selected
video interval; drop a packet crossing a boundary rather than transcode it.
Replay starts at a validated independently decodable keyframe, including a
keyframe-starting open prefix when the event arrives before the next keyframe.
Subsequent dependent frames continue that same recording exactly once.

## Compatibility and verification

Old configurations allocate no pre-roll buffer and preserve all five modes.
Old clients can read older fields and omit new settings. Capability-gate the
new mode and controls; do not advertise support until the complete server path
is available. Verify canonical schema generation, old/new roundtrips, invalid
enums, stale revisions, defaults/inheritance, restart, and unchanged unrelated
contracts. Real H.264/H.265 decode tests and the issue's complete acceptance
matrix remain required before a completion PR.
