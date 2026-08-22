# Testing hap-video against the Apple Home app on macOS

`hap-video` is sans-I/O: it has no sockets, no Bonjour publication, and no
clock. The runnable accessory lives in the root crate, so local Home app
testing goes through the `homekit-file-stream` binary, which loops H.264 frames
read from an MP4 file into a real HAP accessory.

```
homekit-file-stream
  ├── crates/mp4          reads AVCC samples + SPS/PPS from the fixture
  ├── webrtc::Publisher   publish(Source { camera_ip, Main }, ...)
  └── homekit::HomeKitService
        ├── mdns-sd        _hap._tcp.local. advertisement
        ├── hap-video      pairing, TLV8, record layer, WebRTC signaling
        └── Str0mSession   ICE + DTLS-SRTP + RTP packetization
```

## macOS prerequisites

**Local Network permission is the single most common failure.** The responsible
process is the terminal or editor that launches the binary, not the binary
itself. Without it `mdns-sd` fails silently and the accessory never appears in
Home.

1. System Settings → Privacy & Security → **Local Network** → enable your
   terminal (Terminal, iTerm, or VS Code). If the entry is missing or stuck,
   reset it with `tccutil reset LocalNetwork` and relaunch the terminal.
2. Sign in to iCloud with Home enabled. A home hub (Apple TV / HomePod) is
   **not** required for live view; it is only required for Secure Video
   recording.
3. Allow incoming connections for the binary if the macOS firewall is on.

## Running

```sh
cargo run --bin homekit-file-stream
```

Useful flags:

| Flag       | Default                                                | Purpose                                            |
| ---------- | ------------------------------------------------------ | -------------------------------------------------- |
| `--file`   | `crates/test-camera/testdata/cc-4k-1920x1080-h264.mp4` | MP4 whose H.264 track is looped                    |
| `--name`   | `KeepPeek File Camera`                                 | Accessory name shown in Home                       |
| `--config` | platform config path                                   | Supplies the `[homekit]` block and state directory |
| `--bind`   | `[homekit] bind`                                       | Override the bind address                          |
| `--port`   | `[homekit] port`                                       | Override the port; `0` picks an ephemeral one      |

## Pairing must happen on an iPhone or iPad

The macOS Home app has **no Add Accessory command**. The File menu offers only
Add Scene, Add Automation, Add Room, and Add People, and neither toolbar menu
exposes accessory pairing. `sdef /System/Applications/Home.app` also fails with
error -192, so there is no scripting dictionary either. Pairing therefore cannot
be automated on the Mac.

On an iPhone or iPad joined to the same subnet:

1. Home app → **+** → **Add Accessory**
2. Scan `<config-dir>/homekit/<sha1>-setup.svg`, or tap **More options…**, pick
   the accessory, and enter the code from `<sha1>-setup.txt`

Pairing persists in `<config-dir>/homekit/<sha1>.json`, so subsequent runs skip
this step entirely and the accessory then also appears on the Mac. Reset with
`DELETE /api/cameras/{id}/homekit/pairings` or by deleting the state file.

## Driving live view from the Mac

Once paired, everything after pairing is automatable:

```sh
crates/hap-video/scripts/home-live-view.sh \
  --camera "KeepPeek File Camera" --log /tmp/hkfs.log --wait 20
```

It dismisses any open window, finds and clicks the camera tile through the
Accessibility API, screenshots, then reports which characteristics the
controller touched and exits with a distinct code:

| Exit | Meaning                                                    |
| ---- | ---------------------------------------------------------- |
| 0    | `SolicitOffer` written — the 2026 WebRTC path              |
| 3    | `SetupEndpoints` written — legacy RTP path                 |
| 4    | Controller connected but never asked for media             |
| 5    | Controller read the legacy capability characteristics only |

This needs Accessibility permission for the terminal, plus Automation access to
"System Events" and "Home".

## Verifying

```sh
dns-sd -B _hap._tcp local.                 # accessory is advertised
dns-sd -L "<name>" _hap._tcp local.        # TXT record: c# ff id md pv s# sf ci sh
RUST_LOG=info,keeppeek::homekit=debug,hap_video=debug cargo run --bin homekit-file-stream
```

Expected log sequence on a successful live view:

1. `SolicitOffer` characteristic write received
2. `Action::CreateOffer` → offer returned in the **207 Multi-Status** response
3. `ProvideAnswer` write → `Action::ApplyAnswer`
4. `TransportConnected` → `ActiveSessionsChanged(1)`
5. `write_homekit_frame` returning `true` repeatedly

## Working legacy configuration, macOS 26 / Home 10.0 (2026-08-21)

Live video renders in the Home app over the legacy
`CameraRTPStreamManagement` path. The controller negotiated 1280x720 at 30 fps,
Main profile level 4.0, 299 kbps, payload type 99, and the accessory delivered
1592 RTP plus 6 RTCP packets with no errors.

Every one of the following had to be correct at the same time. Any single one
missing produces the same two symptoms — "No Response", or an endless spinner —
which is what makes this failure mode so hard to bisect.

| Requirement                                             | Why it matters                                                                                                              |
| ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Serve the **legacy** accessory profile                  | Home ignores an accessory whose `primary` service is the 2026 WebRTC one. The legacy database is 2851 bytes against 5281    |
| Use the SSRC from **Selected RTP Stream Configuration** | It feeds the SRTP initialisation vector; the Setup Endpoints value is a different number, so every authentication tag fails |
| Generate **accessory-side SRTP keys**                   | The request key is controller-to-accessory. Echoing it back reuses one key across both directions                           |
| `-bsf:v dump_extra=freq=keyframe`                       | `-f rtp` sets a global header and moves SPS/PPS into an SDP that HAP never transports                                       |
| `-x264-params sliced-threads=0:slices=1`                | x264 otherwise emits multiple slices per frame, which the HomeKit decoder will not reassemble from RTP                      |
| Bind the media socket to the **routed** local address   | The relay must both receive from FFmpeg and send from the advertised port                                                   |

Verify the SRTP transform itself with
`cargo test -p hap-video libsrtp`, which reproduces the libsrtp
`AES_CM_128_HMAC_SHA1_80` reference vector byte for byte.

## Controller support for the 2026 WebRTC service

No controller has yet written `SolicitOffer` (iid 10). Attributing each HAP
connection to its peer:

| Controller                    | Behaviour                                                             |
| ----------------------------- | --------------------------------------------------------------------- |
| Mac, Home 10.0 / macOS 26.6.1 | reads legacy `1.38`, `1.39`, then stops                               |
| iPhone, iOS 27.0              | reads legacy `1.38`, `1.39` only                                      |
| Apple TV, tvOS 27 beta (hub)  | blanket-subscribes to every stateful characteristic; serves snapshots |

The Apple TV subscribes to iids 9, 13, 15, 16, 17 in the WebRTC service, which
looks like recognition but **is not evidence of it**. Those are simply the
characteristics in that service carrying the `ev` permission, and the hub also
subscribed to all twelve `ev` characteristics elsewhere in the accessory plus
iids 23–25, which have no `ev` at all. That is a home hub caching accessory
state, and requires no knowledge of the specification.

`SolicitOffer` has `Read, Write, WriteResponse` and no `ev`, so it can only be
exercised deliberately — which is why a write to it, and nothing else, is the
decisive signal. The spec is a Developer Preview dated June 3, 2026, and
nothing observed here demonstrates that any shipping or beta Apple OS
implements it yet.

When analysing the log yourself, strip ANSI first — `tracing` colours its
`key=value` separators, so `grep 'iid=42'` silently matches nothing:

```sh
sed $'s/\033\\[[0-9;]*m//g' /tmp/hkfs.log | grep 'iid='
```

Also confirm the accessory is still alive before each attempt
(`pgrep -fl homekit-file-stream`); a plain background job is killed when its
terminal is cleaned up, which silently invalidates a test run.

## Triage

| Symptom                  | Check                                                                                                            |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| Accessory never appears  | Local Network permission; `dns-sd -B _hap._tcp local.`                                                           |
| Pairing fails            | Setup code matches `-setup.txt`; delete state file and retry                                                     |
| Paired but no live view  | Was `SolicitOffer` received? If Home wrote `SetupEndpoints` instead, it chose the legacy RTP path — see below    |
| Offer sent, no answer    | SDP exceeds 255 bytes and must be TLV8-fragmented; `hap-video` does this, but confirm the controller reassembles |
| Answer applied, no video | ICE state; `h264_profiles_match` against the fixture's `profile-level-id`; a keyframe must be delivered first    |
| Video stutters           | Media timestamps must increase monotonically across loop wraps                                                   |

### If Home selects the legacy path

`homekit-offer-probe` distinguishes the two paths without streaming anything:

```sh
cargo run --bin homekit-offer-probe -- --camera <name> --profile webrtc
cargo run --bin homekit-offer-probe -- --camera <name> --profile legacy
```

`WebRtcSolicitOffer` means the 2026 WebRTC path is in use and everything here
applies. `LegacySetupEndpoints` means the controller wants
`CameraRTPStreamManagement`, which is **advertised but not implemented**: there
is no `SetupEndpoints` decoder, no `SelectedRTPStreamConfiguration` decoder, and
no SRTP anywhere in the repository. That path needs
`AES_CM_128_HMAC_SHA1_80` plus an RFC 6184 packetizer before it can work.

## Fixture notes

`cc-4k-1920x1080-h264.mp4` is used by default because 1920x1080 is the only
resolution present in every HAP camera resolution list and is one of the two
required by HomeKit Secure Video. It is constrained baseline, level 4.0, 30 fps,
no B-frames.

The accessory advertises video tiers derived from the file's own dimensions and
frame rate, so the advertised High/Medium/Low tiers always describe something
the file can actually supply. Substituting a different `--file` automatically
re-derives them.

Two known cosmetic gaps: the snapshot endpoint returns a black placeholder JPEG,
so the camera tile thumbnail looks broken even when live view works, and there
is no Opus audio write path despite audio tiers being advertised.

## Desktop automation

The Home app has no AppleScript dictionary and is not scriptable. The only route
is System Events UI scripting through the Accessibility API, which needs both
Accessibility and Automation TCC grants. The `shortcuts` CLI exposes Home
actions for accessory on/off but cannot add an accessory or open live view.

Automating the Add Accessory flow is not worth it: pairing is a one-time action
that persists to disk, and the signal that matters comes from the logs above
rather than from the screen. For evidence capture use `screencapture -x`, and
for controller-side detail use
`log stream --predicate 'process == "Home"' --level debug`.
