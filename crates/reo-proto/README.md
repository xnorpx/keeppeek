# Baichuan Protocol

Baichuan is the proprietary binary protocol used by Reolink cameras for device
communication. The protocol runs over TCP (port 9000) and optionally UDP. This
crate implements both transports. Baichuan is used for authentication,
configuration, PTZ control, and -- most importantly -- video and audio streaming.
Unlike RTSP/RTP, Baichuan delivers media as complete, self-contained frames,
avoiding the fragmentation and reassembly issues inherent to RTP.

This document is rewritten based on reading the reverse engineering documentation from
[neolink](https://github.com/QuantumEntangledAndy/neolink/dissector) project and [blog](https://www.thirtythreeforty.net/posts/2020/05/hacking-reolink-cameras-for-fun-and-profit/)

---

## Table of Contents

1. [TCP Packet Structure](#tcp-packet-structure)
2. [Encryption](#encryption)
3. [Authentication](#authentication)
4. [Message IDs](#message-ids)
5. [Media Streaming](#media-streaming)
6. [Media Packet Format](#media-packet-format)
7. [Recording Playback](#recording-playback)
8. [Two-Way Audio](#two-way-audio)
9. [Device Discovery](#device-discovery)
10. [UDP Transport](#udp-transport)

---

## TCP Packet Structure

Every Baichuan message over TCP follows the same binary envelope. All
multi-byte integers are **little-endian**.

### Header (20 bytes, or 24 bytes with extension)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | `f0 de bc 0a` (0x0ABCDEF0 LE) |
| 4 | 4 | Message ID | Command identifier (see Message IDs) |
| 8 | 4 | Body Length | Size of payload in bytes |
| 12 | 4 | Encryption Offset | Byte offset where encryption begins within the body. If equal to body length, body is unencrypted. |
| 16 | 4 | Status / Class | Response status code, or message class flags |
| 20 | 4 | Extension (optional) | Present in modern messages. The nodelink-js implementation reads this as `payloadOffset` and uses it to separate extension XML from binary payload within the body. |

The magic bytes `f0 de bc 0a` identify every Baichuan packet. Receivers scan
for this sequence to find packet boundaries. A reversed-endian variant
`a0 cb ed 0f` (0x0FEDCBA0) has been observed in JPEG snapshot payloads.

### Status / Class Field

This 4-byte field serves double duty:

- **In responses**: contains a status code. `0x00` = success.
- **In requests**: contains class bits that describe the message type:
  - Bit 0 (`0x01`): message carries a binary (non-XML) payload
  - Bit 1 (`0x02`): message is part of the modern (XML-based) protocol
  - Bit 4 (`0x10`): message contains a 24-byte header (extension field present)

Common observed values:
- `0x0000` -- legacy message, XML body, 20-byte header
- `0x0001` -- binary payload (e.g. media stream data)
- `0x6414` -- modern message, 24-byte header
- `0x6482` -- file download variant
- `0x6514` -- legacy message class
- `0x6614` -- modern message class (alternate)

### Body

The body immediately follows the header. Its format depends on the message
class:

- **XML messages**: UTF-8 XML document (may be encrypted from `encryption_offset`
  onward).
- **Binary messages**: raw binary data (video/audio frames, firmware chunks,
  etc.).

---

## Encryption

Baichuan uses three levels of encryption, negotiated during login. All
encryption applies only to message bodies, never headers.

### Level 1: BCEncrypt (XOR)

The simplest scheme. Each byte of the body (starting at `encryption_offset`)
is XORed with a repeating 8-byte key:

```
KEY = [0x1F, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0xFF]
```

The XOR index cycles: `body[i] ^= KEY[i % 8]`.

This provides only trivial obfuscation.

### Level 2: AES (Configuration Only)

Uses AES-128-CFB to encrypt XML configuration payloads. Media streams remain
unencrypted.

Key derivation:
1. Compute `MD5("{nonce}-{password}")` where `{nonce}` is the server-provided
   nonce from the login handshake and `{password}` is the plaintext password.
2. Convert the MD5 digest to uppercase hexadecimal.
3. Take the first 16 characters as the AES key.

The initialization vector is always the fixed string `b"0123456789abcdef"` (16
ASCII bytes).

### Level 3: Full AES (Configuration + Media)

Same key derivation and cipher as Level 2, but applied to **all** message
bodies including media stream data. This is the most secure mode but carries
a CPU cost for real-time video decryption.

### Encryption Negotiation

During the login handshake, the client advertises its desired encryption level
by encoding a mode byte in the legacy login message. The nodelink-js
implementation uses these class values:

- `0xDC00` -- request no encryption
- `0xDC01` -- request BCEncrypt (XOR)
- `0xDC02` -- request AES (config only)
- `0xDC12` -- request Full AES (config + media)

Some firmware versions will reset the TCP connection if an unsupported mode is
requested. Implementations should be prepared to downgrade and retry.

### AES Stream State

When using AES encryption on media streams (Full AES mode), the AES-128-CFB
cipher is **stateful** across TCP chunks. The decipher object must persist
between consecutive `push()` calls on the same stream, since a Baichuan message
may be fragmented across multiple TCP reads. The cipher state should only be
reset when a new Baichuan message header (magic bytes) is detected.

### Encryption Offset

The `encryption_offset` field in the header tells the receiver where encryption
begins within the body. Bytes before this offset are plaintext; bytes from
this offset onward are encrypted. If `encryption_offset == body_length`, the
entire body is plaintext.

---

## Authentication

Authentication is a multi-step handshake over message ID 1 (Login).

### Step 1: Legacy Login Request (Client -> Camera)

The client sends a legacy-format login with message ID 1. The body is a
fixed-size binary struct:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 32 | Username (null-padded ASCII) |
| 32 | 32 | Password (null-padded ASCII) |

The password may be sent as an MD5 hash depending on the camera firmware.
Older cameras expect plaintext; newer cameras expect the hash.

### Step 2: Nonce Response (Camera -> Client)

The camera responds with an XML body containing a nonce value:

```xml
<body>
  <Encryption version="2">
    <type>aes</type>
    <nonce>HEXSTRING</nonce>
  </Encryption>
</body>
```

The `version` attribute indicates the encryption level the camera supports.
The `nonce` is a random hex string used in key derivation.

### Step 3: Modern Login Request (Client -> Camera)

The client sends a modern login (message ID 1, class `0x12`) with an XML body:

```xml
<body>
  <LoginUser version="2">
    <userName>admin</userName>
    <password>HASHED_PASSWORD</password>
    <userVer>1</userVer>
  </LoginUser>
  <LoginNet version="2">
    <type>LAN</type>
    <udpPort>0</udpPort>
  </LoginNet>
</body>
```

The password hash is computed as:
1. `MD5("{nonce}-{password}")` -- concatenate nonce, dash, plaintext password.
2. Convert to uppercase hexadecimal.
3. Take the first **31** characters (not 32).

The username is hashed the same way: `MD5("{nonce}-{username}")`, uppercase hex,
truncated to 31 characters. Both hashed values are sent in the XML body.

### Step 4: Login Confirmation (Camera -> Client)

The camera responds with device information and session details:

```xml
<body>
  <LoginUser version="2">
    <userName>admin</userName>
    <result>ok</result>
    <userId>123</userId>
  </LoginUser>
  <DeviceInfo version="2">
    <firmVer>...</firmVer>
    <workerVer>...</workerVer>
    <model>RLC-811A</model>
    <serialNumber>...</serialNumber>
    <channelNum>1</channelNum>
  </DeviceInfo>
</body>
```

After this, the session is established and subsequent messages use the
negotiated encryption level.

### Logout

Message ID 2 with an empty or minimal body terminates the session.

---

## Message IDs

Each Baichuan message is identified by a numeric message ID in the header. The
same ID is used for both request and response; the direction is inferred from
context (client-sent vs camera-sent).

### Device & System

| ID | Name | Description |
|----|------|-------------|
| 1 | Login | Authentication handshake (legacy and modern) |
| 2 | Logout | End session |
| 23 | Reboot | Restart the camera |
| 58 | AbilitySupport | Query supported features and user permissions |
| 59 | UserList | Manage user accounts |
| 67 | ConfigFileInfo | Firmware upgrade file transfer |
| 78 | VideoInput (push) | Camera-pushed video input status |
| 80 | VersionInfo | Query firmware version and device identifiers |
| 104 | SystemGeneral | Query system time, timezone, language |
| 151 | AbilityInfo | Detailed capability matrix |
| 199 | Support | Extensive device capability flags |
| 287 | TimeCfg | Synchronize system clock |
| 464 | NetworkInfo (push) | Camera-pushed network status |
| 623 | SleepStatus (push) | Battery camera sleep/wake events |

### Video & Streaming

| ID | Name | Description |
|----|------|-------------|
| 3 | Stream | Request/receive video stream (binary payload) |
| 4 | PreviewStop | Stop stream transmission |
| 25 | VideoInput Write | Set brightness, contrast, saturation, hue |
| 26 | VideoInput Read | Query video input settings |
| 56 | Compression Read | Query encoding parameters (resolution, bitrate) |
| 57 | Compression Write | Modify encoding settings |
| 78 | VideoInput | Query basic video settings |
| 109 | Snap | Request a snapshot capture |
| 132 | VideoInput Advanced | Query ISP and exposure settings |
| 146 | StreamInfoList | Query available stream types |

### Audio

| ID | Name | Description |
|----|------|-------------|
| 10 | TalkAbility | Query audio duplex and codec capabilities |
| 11 | TalkAbility (alt) | Alternative talk ability query |
| 201 | TalkConfig | Configure two-way audio parameters |
| 202 | Talk | Transmit audio data to camera speaker |
| 263 | AudioCfg | Configure audio output volume and settings |
| 264 | AudioPlayInfo | Play siren or alarm sound |
| 547 | SirenControl | Direct siren on/off control |

Supported audio codecs: PCM (8000 Hz, 16-bit), G.711 A-law, G.711 u-law, AAC,
and ADPCM.

### PTZ

| ID | Name | Description |
|----|------|-------------|
| 18 | PtzControl | Pan/tilt/zoom movement commands |
| 19 | PtzPreset | Manage PTZ preset positions |
| 190 | PtzPreset Read | Query saved PTZ positions |
| 294 | PtzZoomFocus Read | Query zoom and focus position |
| 295 | StartZoomFocus | Move to absolute zoom/focus position |
| 433 | PtzGuard | PTZ guard/patrol configuration |

PTZ movement commands: `Left`, `Right`, `Up`, `Down`, `ZoomIn`, `ZoomOut`,
`FocusNear`, `FocusFar`. Speed range 1-64 (default 32). Preset IDs range 1-64.

### Detection & Alarms

| ID | Name | Description |
|----|------|-------------|
| 31 | StartMotionAlarm | Enable motion detection event reporting |
| 33 | AlarmEventList | Motion detection event notifications |
| 46 | MotionDetect Read | Query motion detection configuration |
| 47 | MotionDetect Write | Set motion detection sensitivity/zones |
| 133 | RfAlarm | Query RF alarm sensor configuration |
| 204 | RfAlarmCfg Write | Configure RF sensor sensitivity |
| 212 | PirInfo Read | Query PIR (passive infrared) sensor config |
| 213 | PirInfo Write | Configure PIR sensor sensitivity |
| 232 | AudioTask Read | Query audio alarm schedule |
| 299 | AiCfg Read | Query AI detection/smart tracking settings |
| 342 | AiAlarm Read | Query AI alarm configuration |
| 343 | AiAlarm Write | Configure AI detection types (person, vehicle, pet, face, package) |
| 723 | CoordinateInfo | Auto-tracking coordinate data (camera-pushed) |

AI detection supports five classification types: `people`, `vehicle`,
`dog_cat`, `face`, and `package`.

### Network & Connectivity

| ID | Name | Description |
|----|------|-------------|
| 76 | Ip Read | Query network configuration |
| 77 | Ip Write | Modify network settings |
| 93 | LinkType | Network connectivity query |
| 115 | WifiSignal | Query wireless signal strength |
| 116 | Wifi | List available wireless networks |
| 255 | Net3g4gInfo | Cellular connectivity info |
| 268 | CloudBindInfo | Cloud service binding status |
| 282 | CloudLoginKey | Cloud authentication settings |

### Recording & Storage

| ID | Name | Description |
|----|------|-------------|
| 5 | FileOpen | Open a recording file for download |
| 6 | FileRead | Read recording file data (binary) |
| 7 | FileClose | Close an open recording file |
| 54 | RecordCfg Read | Query recording parameters |
| 55 | RecordCfg Write | Modify recording settings |
| 81 | Record Schedule Read | Query recording schedule |
| 82 | Record Schedule Write | Modify recording schedule |
| 102 | HDDInfoList | Query storage device information |
| 138 | CoverPreview | Request recording cover thumbnail |
| 272 | RecordingSearch | Search recordings by date/time |
| 273 | RecordingSearchMonth | Search recordings by month |
| 274 | RecordingCalendar | Query which days have recordings |
| 298 | RecordThumbnail | Request recording thumbnail image |
| 458-462 | Cover/Thumb variants | Additional cover/thumbnail requests |

### Notifications & Display

| ID | Name | Description |
|----|------|-------------|
| 42 | Email Read | Query email configuration |
| 43 | Email Write | Configure SMTP/email |
| 44 | OsdChannelName Read | Query on-screen display settings |
| 45 | OsdChannelName Write | Modify OSD text |
| 52 | Shelter Read | Query privacy mask configuration |
| 53 | Shelter Write | Configure privacy mask regions |
| 124 | PushInfo | Register push notification tokens |
| 141 | Email Test | Validate email settings |
| 208 | LedState Read | Query indicator light status |
| 209 | LedState Write | Control indicator light |
| 216 | EmailTask Write | Enable/disable motion email alerts |
| 217 | EmailTask Read | Query email alert schedule |
| 219 | PushTask Read | Query push notification schedule |

### Lighting & Battery

| ID | Name | Description |
|----|------|-------------|
| 252 | BatteryList | Battery status, charge level, voltage |
| 253 | BatteryInfo | Detailed battery information |
| 288 | FloodlightManual Write | Control floodlight on/off |
| 290 | FloodlightTask Write | Configure floodlight schedule |
| 291 | FloodlightStatusList Read | Query floodlight state |
| 438 | FloodlightTask Read | Query floodlight configuration |

---

## Media Streaming

To start a video stream, the client sends message ID 3 (Stream) with an XML
body specifying the desired channel and stream type:

```xml
<body>
  <Preview version="1.1">
    <channelId>0</channelId>
    <handle>0</handle>
    <streamType>mainStream</streamType>
  </Preview>
</body>
```

The `streamType` can be `mainStream`, `subStream`, or `externStream`.

After the camera acknowledges, it begins sending message ID 3 responses with
binary payloads containing media frame data. These are marked with class bit
`0x01` (binary payload).

To stop streaming, send message ID 4 (PreviewStop) with the same handle and
channel.

### Keepalive

The nodelink-js implementation sends a ping every 30 seconds to keep the
session alive, and performs a full re-login every 5 minutes when no active
streams are running. Without keepalives, the camera will close the TCP
connection after an idle timeout.

### Stream Watchdog

If no media frames are received within 30 seconds (TCP) or 60 seconds (UDP),
the stream should be considered stale and restarted. The camera may silently
stop sending data after network glitches or internal errors.

---

## Media Packet Format

The binary payload of a Stream message (ID 3) contains one or more media
frames. Each frame has its own header identified by a magic number.

### Frame Types

| Magic (LE) | Type | Description |
|------------|------|-------------|
| `0x31303031` | InfoV1 | Stream metadata: resolution, FPS, timestamps |
| `0x32303031` | InfoV2 | Stream metadata v2 (same structure as InfoV1) |
| `0x63643030`-`0x63643039` | I-Frame | Keyframe (per-channel: last digit = channel) |
| `0x63643130`-`0x63643139` | P-Frame | Inter-frame (per-channel: last digit = channel) |
| `0x62773530` | AAC Audio | Audio frame encoded as AAC |
| `0x62773130` | ADPCM Audio | Audio frame encoded as ADPCM |

The I-frame and P-frame magic values encode channel number in the last digit.
Channel 0 uses `0x63643030`/`0x63643130`, channel 1 uses `0x63643031`/
`0x63643131`, and so on up to channel 9.

### Stream Info Header (InfoV1/InfoV2)

Before video frames begin, the camera sends a stream info packet describing
the media parameters. This appears at the start of each stream and after
configuration changes.

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | `0x31303031` (InfoV1) or `0x32303031` (InfoV2) |
| 4 | 4 | Header Size | Total header size in bytes |
| 8 | 4 | Video Width | Horizontal resolution (e.g. 2304) |
| 12 | 4 | Video Height | Vertical resolution (e.g. 1296) |
| 17 | 1 | FPS | Configured frame rate |
| 18 | 1 | Start Year | Recording start time (year offset) |
| 19 | 1 | Start Month | |
| 20 | 1 | Start Day | |
| 21 | 1 | Start Hour | |
| 22 | 1 | Start Minute | |
| 23 | 1 | Start Second | |
| 24 | 1 | End Year | Recording end time (year offset) |
| 25 | 1 | End Month | |
| 26 | 1 | End Day | |
| 27 | 1 | End Hour | |
| 28 | 1 | End Minute | |
| 29 | 1 | End Second | |

The start/end timestamps are primarily relevant for recording playback. For
live streams, these may be zero or reflect the current time.

### Video Frame Header

Video frames (both I-frame and P-frame) share this header structure:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | Frame type identifier |
| 4 | 4 | Video Type | ASCII codec identifier: `"H264"` or `"H265"` |
| 8 | 4 | Data Length | Size of the frame payload in bytes |
| 12 | 4 | Additional Header Size | Size of extra header data (0 if none) |
| 16 | 4 | Microseconds | Sub-second timestamp component |
| 20 | 4 | Unknown | Observed as zeros or small values |
| 24 | var | Additional Header | Optional extra data (size from offset 12) |
| 24+AH | var | Data | Raw video bitstream (Annex B H.264 or H.265) |

After the data payload, frames are **8-byte aligned** -- padding bytes are
appended so the next frame starts at a multiple of 8 bytes from the frame
start.

The `Video Type` field at offset 4 is a 4-byte ASCII string (`"H264"` or
`"H265"`) that identifies the codec, replacing the need to inspect NAL unit
headers.

The `data` field contains a raw Annex B bitstream. For I-frames, this includes
SPS and PPS NAL units (and VPS for H.265) followed by the IDR slice. For
P-frames, it is just the slice data.

### Audio Frame Header

**AAC:**

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | `0x62773530` |
| 4 | 2 | Data Length | Size of audio payload (u16 LE) |
| 6 | 2 | Data Length (verify) | Duplicate of data length for validation |
| 8 | var | Data | Raw AAC frame (no ADTS header) |

**ADPCM:**

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | `0x62773130` |
| 4 | 2 | Size field 1 | |
| 6 | 2 | Size field 2 | |
| 8 | 2 | Magic data | Always `0x0100` |
| 10 | 2 | Half-block size | |
| 12 | var | Data | DVI/IMA ADPCM samples |

### Key Properties

- **Frames may span messages**: A message can carry several complete frames,
  but a large frame (a 4K H.265 keyframe is commonly 500 KB or more) is split
  across consecutive messages. The first message carries the header plus a
  prefix of the payload; the following messages carry raw continuation bytes
  with no header of their own. A parser must accumulate until it has the
  `Data Length` announced in the header.

  This applies to audio as well. An AAC frame is only ~520 bytes, so it looks
  like it should always fit, but the camera splits on message boundaries and
  not on frame boundaries: a short message can end part-way through an audio
  frame. Discarding that partial frame instead of accumulating it leaves its
  tail at the start of the next message, where it matches no magic value. The
  parser then discards that entire message — typically 40 KB carrying video —
  and the GOP is destroyed. Audio needs the same accumulator as video.

- **Headers may span messages too**: the split can land inside a frame header,
  not just its payload. A video header is 24 bytes plus a variable extension
  (80 or 112 bytes on the cameras seen so far), so the first 24 bytes can parse
  successfully while the extension still lies in the next message. Treating the
  announced header length as consumed would splice the remaining header bytes
  into the payload and shift every subsequent frame boundary. A partial header
  must be carried over and completed, never consumed early.
- **Annex B format**: Video data uses H.264/H.265 Annex B byte stream format
  with start codes (`00 00 00 01`), ready for direct consumption by decoders.
- **Timestamps**: Embedded wall-clock timestamps in each frame header provide
  absolute timing without relying on RTP timestamp arithmetic.
- **8-byte alignment**: All frames are padded to 8-byte boundaries. Parsers
  must account for padding bytes between frames. The padding belongs to the
  frame that precedes it, so it can spill into the next message and push the
  following frame's magic up to seven bytes into that message's body. Such a
  body is still media: it must not be decrypted, and it must not be appended
  to a frame that is still accumulating.
- **Media is not encrypted**: Media bodies are sent in the clear even when the
  session negotiated BCEncrypt or AES for XML (except under Full AES).
  Decrypting a media body destroys the frame, so the media check has to run
  before the decryption decision.
- **Corruption recovery**: If the parser encounters an unknown magic value, it
  should scan forward looking for a known magic. The nodelink-js implementation
  checks fast-path offsets at 528, 1056, and 1584 bytes before falling back to
  a linear byte-by-byte scan.

### Diagnosing Frame Loss

Losing or mis-assembling a single frame is not a local defect: every later
frame references it, so one bad frame invalidates the rest of the GOP. A
decoder reports this as `Error constructing the frame RPS` and
`First slice in a frame missing`, and a WebRTC receiver shows complete frames
arriving while `framesDecoded` stops advancing.

`SessionStats` exposes counters for each way media can be dropped:

| Counter | Meaning |
|---------|---------|
| `video_accum_started` / `video_accum_completed` | Chunked frames begun and finished; a growing gap means frames never complete |
| `video_accum_abandoned` | A new frame header arrived while the previous frame was still accumulating |
| `audio_accum_started` / `audio_accum_completed` | Chunked audio frames begun and finished; a gap means an audio tail was lost and the next message will not parse |
| `split_headers` | Frame headers that straddled a message boundary and were carried over |
| `pending_drops` | The event queue was full when a frame was ready |
| `resync_skipped_bytes` | Bytes discarded while resynchronising to a BC header |
| `stream_bodies_unrecognized` | `COMMAND_STREAM` bodies matching no frame, accumulator, or XML |
| `trailing_bytes_unrecognized` | Bytes after a frame that did not begin another frame |
| `continuation_ambiguous` | Continuation data that matched no single accumulator |
| `padded_media_bodies` | Frames found behind the previous frame's alignment padding |

Any non-zero value in the last five counters means media bytes were dropped.

### NAL Unit Format Handling

Camera firmware versions are inconsistent about the format of video data in
the `data` field. The payload may arrive as:

- Standard Annex B with 4-byte start codes (`00 00 00 01`)
- 4-byte length-prefixed NAL units (big-endian or little-endian)
- 3-byte or 2-byte length-prefixed NAL units
- RTP aggregation payloads (STAP-A, STAP-B, MTAP16, MTAP24)
- Raw single NAL units without any framing

A robust implementation should attempt detection in the order above, falling
back through each format. The nodelink-js H264Converter implements this
cascading detection strategy.

---

## Recording Playback

Baichuan supports searching and downloading recordings stored on the camera's
SD card or NVR hard drive.

### Searching Recordings

Use message ID 272 (RecordingSearch) with an XML body specifying date range
and channel:

```xml
<body>
  <RecordSearch version="1.1">
    <channelId>0</channelId>
    <startTime>
      <year>2024</year>
      <mon>1</mon>
      <day>15</day>
      <hour>0</hour>
      <min>0</min>
      <sec>0</sec>
    </startTime>
    <endTime>
      <year>2024</year>
      <mon>1</mon>
      <day>15</day>
      <hour>23</hour>
      <min>59</min>
      <sec>59</sec>
    </endTime>
  </RecordSearch>
</body>
```

The camera responds with a list of recording clips including filenames, start/
end times, and file sizes.

### Downloading Recordings

Recording files are downloaded using a file transfer protocol:

1. **FileOpen** (ID 5): Open the recording by filename, receive a file handle
2. **FileRead** (ID 6): Read file data in chunks (binary payload)
3. **FileClose** (ID 7): Close the file handle

The file data arrives as binary Baichuan messages. The nodelink-js
implementation also supports demuxed downloads where the raw BcMedia frames
are parsed and converted to MP4 on the fly.

### Recording Calendar

Message ID 274 (RecordingCalendar) queries which days in a given month have
recordings, returning a bitmask of days with available footage.

---

## Two-Way Audio

Two-way audio (intercom) allows sending audio from the client to the camera's
speaker while receiving the camera's microphone audio in the video stream.

### Querying Audio Capabilities

Message ID 10 (TalkAbility) returns the camera's supported audio codecs and
duplex mode. Common response fields include supported codecs and whether
full-duplex is available.

### Starting a Talk Session

1. Send message ID 201 (TalkConfig) to configure audio parameters (codec,
   sample rate, channel count)
2. Send audio frames via message ID 202 (Talk) as binary payloads

Audio format requirements:
- Sample rate: 8000 Hz
- Bit depth: 16-bit
- Channels: mono
- Supported codecs: PCM, G.711 A-law, G.711 u-law, AAC, ADPCM

The camera's audio stream arrives interleaved with video in the message ID 3
binary payloads (AAC or ADPCM frames identified by their magic values).

---

## Device Discovery

Reolink cameras can be discovered on the local network via UDP broadcast
on port 2000.

### Discovery Protocol

The discovery client sends a UDP broadcast packet and listens for camera
responses. Each response contains:

- **IP address**: Camera's current network address
- **MAC address**: Hardware identifier
- **Hostname**: Camera's configured name
- **Model**: Hardware model (e.g. RLC-811A, RLC-520A)
- **Device type**: camera, NVR, or hub
- **Serial number / UID**: Unique device identifier
- **Firmware and hardware versions**
- **Protocol ports**: Baichuan (9000), HTTP (80), RTSP (554), RTMP (1935),
  ONVIF (8000)
- **Channel count**: Number of channels (relevant for NVRs)

This discovery mechanism is separate from the BCUDP discovery described in
the UDP Transport section. It provides a quick way to enumerate all Reolink
devices on the network without establishing Baichuan sessions.

---

## UDP Transport

The crate provides SANS-I/O packet encoding, discovery, acknowledgements,
reordering, and retransmission through `BcUdpTransport`. Socket ownership and
timer scheduling remain the application's responsibility. UDP streaming is
experimental: H.264 sub-stream recording has been validated against an RLC-820A,
but high-bitrate H.265 main-stream delivery is not yet complete on that camera.

Baichuan also supports a UDP transport mode for cameras that don't expose TCP
port 9000 (common on battery-powered devices). The UDP protocol adds its own
framing on top of Baichuan messages to handle packet loss and reordering.

### Packet Types

UDP uses three packet types, each with its own magic:

| Magic | Type | Purpose |
|-------|------|---------|
| `3a cf 87 2a` | Discovery | Connection setup, encrypted XML payloads |
| `20 cf 87 2a` | Ack | Acknowledge received data packets |
| `10 cf 87 2a` | Data | Carries Baichuan message fragments |

### UDP Discovery

Discovery establishes the connection. The client broadcasts on port 2015
(general discovery) and/or 2018 (UID-targeted discovery) to find cameras. The
exchange negotiates connection IDs and MTU size (default 1350 bytes).

The nodelink-js implementation wraps all UDP discovery XML in `<P2P>` tags
and supports additional message types for relay connections:

- **C2M_Q**: Query relay/map server for camera availability
- **C2R_C**: Connect to camera through relay server
- **C2R_CFM**: Confirm relay connection

**Discovery Header (20 bytes):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Magic (`3a cf 87 2a`) |
| 4 | 4 | Payload size |
| 8 | 4 | Unknown (always `01 00 00 00`) |
| 12 | 4 | Transmission ID (increments per round-trip, also used for encryption) |
| 16 | 4 | CRC32 checksum of encrypted payload |

The XML payloads are encrypted with a XOR cipher distinct from BCEncrypt.
The key is derived from an 8-element 32-bit array:

```
XML_KEY = [0x1F2D3C4B, 0x5A6C7F8D, 0x38172E4B, 0x8271635A,
           0x863F1A2B, 0xA5C6F7D8, 0x8371E1B4, 0x17F2D3A5]
```

The keystream is generated by combining array elements with the transmission
ID (from the discovery header) and extracting little-endian bytes. Each
plaintext byte is XORed with the corresponding keystream byte.

The handshake sequence:

1. **C2D_S** (Client broadcast): announces client port
2. **Binary reply** (Camera): provides camera name, IP, TCP port, UID
3. **C2D_C** (Client): specifies camera UID, client port, MTU, connection ID
4. **D2C_C_R** (Camera): confirms connection, provides camera's connection ID
   and timer parameters
5. **C2D_T** (Client): session setup with SID
6. **D2C_T** (Camera): session confirmation

After discovery, the client can proceed with standard Baichuan login over UDP.

If no reply is received within 500ms, the last message is resent.

### UDP Data

Data packets carry fragments of Baichuan messages.

**Data Header (20 bytes):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Magic (`10 cf 87 2a`) |
| 4 | 4 | Connection ID (signed i32) |
| 8 | 4 | Reserved (always `00 00 00 00`) |
| 12 | 4 | Packet ID (monotonically increasing) |
| 16 | 4 | Payload size |

The payload is a standard Baichuan message (or fragment thereof). Since UDP
packets have a size limit (negotiated MTU, typically 1350 bytes), a single
Baichuan message may span multiple UDP data packets.

### UDP Ack

Ack packets confirm receipt of data packets. If the sender doesn't receive an
ack within 1000ms, it resends the data packet.

**Ack Header (28 bytes):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Magic (`20 cf 87 2a`) |
| 4 | 4 | Connection ID (signed i32) |
| 8 | 4 | Reserved (always `00 00 00 00`) |
| 12 | 4 | Group ID |
| 16 | 4 | Last received packet ID |
| 20 | 4 | Latency (possibly RTT measurement) |
| 24 | 4 | Payload size |

The ack payload is a bitmap/truth table. Byte 0 corresponds to
`last_packet_id + 1`, byte 1 to `last_packet_id + 2`, and so on. A value of
`0x01` means the packet was received; `0x00` means it was not received and
should be resent.

If the ack payload grows beyond ~205 bytes (meaning ~205 packets are
outstanding), the camera considers the connection dead and disconnects.

### UDP Disconnect

After logout (message ID 2), the client sends a discovery-type packet with
a `C2D_DISC` XML payload containing the client and camera connection IDs. The
camera replies with `D2C_DISC`.

---
