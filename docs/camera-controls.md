# Camera controls and vendor alignment

KeepPeek shares motion detection and PTZ commands between Hikvision/Annke and
Reolink. Camera media selection does not select the control protocol: Hikvision
uses ISAPI HTTP, and the current Reolink server adapter uses Reolink HTTP even
when media uses `reo-proto`. No Hikvision SDK is required.

## Shared controls

| Meaning                 | Hikvision/Annke ISAPI                                               | Reolink HTTP in KeepPeek                         | `reo-proto` library                                                 |
| ----------------------- | ------------------------------------------------------------------- | ------------------------------------------------ | ------------------------------------------------------------------- |
| Detector enabled        | `MotionDetection/enabled`                                           | `GetAlarm/Alarm/enable`                          | `GetMotionDetect` / `SetMotionDetect`, `MotionDetectConfig.enabled` |
| Current motion activity | Active/inactive VMD notifications                                   | `GetMdState`                                     | `AlarmEventList`                                                    |
| Event subscription      | Persistent alert stream or configured callback                      | Separate from detector configuration             | `StartMotionAlarm`                                                  |
| Continuous PTZ          | Independent signed pan/tilt/zoom speeds, bounded by reported spaces | Direction plus speed 1..64                       | `PtzCommand::Move` with direction and speed 1..64                   |
| Stop                    | Zero-speed continuous command                                       | `PtzCtrl` Stop                                   | `PtzCommand::Stop`                                                  |
| Preset list/recall      | Typed preset list and `goto`                                        | Existing preset list/recall                      | `PresetList` / `PresetGoto`                                         |
| Preset save/delete      | Available in the ISAPI library                                      | Not connected to shared commands                 | `PresetSave` / `PresetDelete` in the library                        |
| Device evidence         | Device, PTZ, motion and two-way audio queries                       | Existing ability permissions                     | `GetAbilitySupport` and `GetCapabilityDetails`                      |
| Speaker audio           | Raw G.711 A-law or mu-law over an owned ISAPI session               | Not connected to the shared HTTP control adapter | Negotiated IMA ADPCM talkback connection                            |

The motion switch controls detector configuration, not current activity. An idle
camera can have detection enabled. Both server adapters read the full alarm
configuration, change the enabled field, preserve unrelated fields, and read back
the result. A failed or mismatched read-back is reported as an error. KeepPeek does
not automatically reboot a camera to apply the setting.

`record_generic_motion_events` is a separate KeepPeek retention policy. It does
not disable camera detection or event subscriptions. Smart classifications can
depend on those subscriptions even when generic motion is not retained.

The shared PTZ axes are finite values in -1..1. Hikvision scales each axis against
its advertised signed range, up to -100..100, and rejects unavailable axes or
directions. Reolink exposes directional movement rather than independently scaled
axes; its adapter retains that behavior and rejects unsupported combinations.
Equal slider values therefore mean relative device speed, not equal physical
degrees per second. Preset identifiers are device identifiers, not list indexes.
Relative movement and preset save/delete remain unavailable in the shared handler;
their presence in a protocol library does not advertise application support.

Continuous movement belongs to one WebRTC connection. Stop and disconnect cleanup
use the original camera endpoint, channel and credentials even after a settings
replacement. An uncertain movement or preset acknowledgement triggers a safety
stop. Ownership remains reserved when that stop cannot be confirmed; failures are
logged without camera response bodies. A reboot-required status is not an
immediate PTZ success. PTZ transitions are serialized, and concurrent commands are
rejected while another command is in progress. These controls do not replace a
device-side movement timeout or an operator's physical safety precautions.

## Capability evidence

For eligible Hikvision cameras, startup and successful live camera activation
queue read-only metadata discovery. One shared pool runs at most four workers
with 128 pending entries; queued updates for a camera are coalesced. No probe opens
an audio session, moves PTZ, or changes camera settings. Results from a replaced
camera generation are discarded for both ONVIF identity and ISAPI capabilities.

The selected camera channel is queried for PTZ spaces and detector configuration;
audio discovery returns separate microphone and speaker evidence. A successful
negative response updates the corresponding flags. Failed queries are logged as
unverified and do not erase previous successful evidence for that configuration.
The existing Boolean API cannot express unknown or probe freshness: initial false
does not prove hardware absence, and cached true does not prove current reachability.
Command responses remain authoritative for whether an operation succeeded.

Hikvision PTZ support is not assumed from a brand or RTSP path. Continuous zoom,
pan/tilt and preset capacity are read from `PTZChanelCap` (also accepting the
`PTZChannelCap` spelling). The usable shared PTZ flags are derived independently.
An audio microphone alone never sets `two_way_audio`; an explicit speaker channel
does. Stream/ONVIF audio evidence remains separate from ISAPI microphone evidence.
The audio channel's enabled flag is configuration, not proof of current ownership.

For multi-channel devices, IDs are not interchangeable. ISAPI `101/102` RTSP stream
IDs identify video input 1; `201/202` identify input 2. Video input, PTZ and audio
channel IDs are queried in their respective namespaces. Explicit audio/video
associations take precedence; without one, only an equal channel ID is used for
the camera's capability projection. `reo-proto` channels are zero-based. The
current Reolink HTTP adapter controls channel 0; generalized NVR channel mapping
is not implemented by this change.

## Two-way audio transport

`isapi::blocking::Client::open_audio(channel, lifetime)` consumes one client and
returns an owned `Talk` session. It reads the channel, requires it to be enabled,
checks codecs, then opens it. It never resets another busy owner or enables audio
automatically. `AudioChannel::list/query`, `AudioChannel::open/close`,
`AudioSession::send_request/receive_request` and `Endpoint::Audio` also expose the
Sans-I/O request and parsing layer.

`Talk::speaker()` sends the empty binary PUT handshake, validates the response,
and retains that exact connection for raw audio. It does not send a separate HTTP
request per audio block. `Talk::microphone()` opens an independent HTTP receive
stream. The handles can run concurrently on different threads. The camera's
`audioCompressionType` is its speaker output codec; `audioInboundCompressionType`
is its microphone codec, falling back to the common codec when absent.

Only 8 kHz mono G.711 A-law and mu-law are accepted by these media handles. Other
codec names and advertised options remain available for discovery but are not
silently converted or treated as G.711. Callers supply already encoded samples:
this transport does not capture a microphone, resample PCM, encode codecs, or add
WAV/RTP framing. It does not alter volume or noise reduction. The camera may impose
additional model-specific duplex restrictions despite the separate transport paths.

Bounds per session:

- Lifetime: greater than zero and at most 300 seconds, including setup.
- Encoded audio: at most the remaining lifetime times 8,000 bytes per second in
  each direction; at most 1,600 bytes in a speaker write or microphone read.
- Speaker pacing follows the sample clock. A delay over 200 ms resets the pacing
  reference instead of causing an unbounded catch-up burst.
- Each speaker write has a 100 ms absolute budget, shortened by the session
  deadline. Partial writes check cancellation and remaining time before continuing.
- Microphone reads have a two-second idle deadline and the session deadline.
  Unsupported media types, sample rates and transfer codings are rejected.
- XML/JSON setup/status responses retain the normal 256 KiB bound and verified
  device-status parsing. TLS stays verified; proxies and redirects are disabled.

Call `Talk::close()` to observe the bounded close result. Dropping the session
cancels local handles and attempts one close within two seconds; drop cannot report
an error. An uncertain write or close is not automatically replayed. Already
transmitted audio cannot be retracted by cancellation.

ISAPI closes an entire channel without a session-scoped conditional close. The
caller must coordinate exclusive use with other applications. If another client
has externally closed/replaced the session, use `Talk::abandon()` to cancel local
handles without closing that replacement. The library cannot detect or fence an
unreported external takeover. A lifetime deadline stops further I/O; retain a
responsible owner that calls close or drops the session when finished.

## Reolink options

The existing `reo-proto` library already exposes these related controls:

- Device-level PTZ, talk, recording and alarm support, plus per-channel
  main/sub-stream, audio and PTZ details.
- Detector enabled/sensitivity, RF alarm, PIR, tracking configuration, and AI
  flags for person, vehicle, dog/cat, face and package where the camera supports them.
- Directional movement, preset CRUD, zoom/focus operations and guard position.
- `OpenTalkback` / `CloseTalkback`, talk-ability query, configuration, ADPCM block
  send and reset. `TalkAudioProfile` carries sample rate, precision, samples per
  encoder block and channel layout. `select_adpcm` selects a reported complete
  profile and prefers full-duplex speaker mode; `ImaAdpcmEncoder` supplies its
  block encoder. This is not interchangeable with Hikvision's raw G.711 stream.

Alignment belongs at the shared meaning level: detector enabled, normalized
movement, device preset ID, speaker/microphone availability and negotiated audio
format. Keep vendor-specific sensitivity scales, AI categories, focus/guard
settings and codec framing explicit. Do not invent support or overwrite a richer
vendor configuration to fit a common Boolean. Moving server controls to native
Baichuan requires a separate adapter using these existing commands; it is not
implied by choosing `backend = "reo-proto"` for media.

## Application and test coverage

The protected WebRTC contract exposes a two-way-audio device flag but has no
camera talk-session command or camera speaker destination. Browser Talk remains
disabled for both brands. Microphone permission, browser codec conversion,
publication-to-camera routing and application talk-session lifecycle are not
implemented. No protected API schema was changed to hide this dependency.

The independent `test-hikvision` fixture verifies Digest, motion read-back,
capability changes, PTZ ownership, original-target cleanup, audio busy/open/close,
raw speaker bytes, gated full-duplex progress, cancellation, expiry and malformed
responses. Reolink tests distinguish idle activity from enabled configuration and
preserve its existing PTZ behavior. These are synthetic integration tests, not
physical-camera talkback or PTZ certification. No physical camera setting or
speaker output was changed by this work.

Source references: [ISAPI guide](isapi.pdf), sections 8.3, 15.10.185-190,
16.2.245 and 16.2.301-304; [ISAPI crate](../crates/isapi/README.md);
[Reolink alarm](../crates/reo-proto/src/alarm.rs),
[device](../crates/reo-proto/src/device.rs), [PTZ](../crates/reo-proto/src/ptz.rs)
and [talk](../crates/reo-proto/src/talk.rs) modules.
