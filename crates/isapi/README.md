# ISAPI

Plain-Rust Hikvision/Annke ISAPI requests, Digest authentication, and alert-stream
parsing. The protocol core is Sans-I/O. It takes request descriptions, challenge
strings, caller-provided nonces, and byte slices. It does not open sockets, read
files, generate entropy, sample clocks, or start background workers.

The optional `ureq` feature, enabled by default, exposes `isapi::blocking::Client`.
No Hikvision SDK binary, FFI binding, Python interpreter, or JavaScript runtime is
required. The HTTP adapter uses the platform TLS implementation through
`ureq`/`native-tls`; "plain Rust" here excludes vendor SDK dependencies, not the
operating system's TLS libraries.

## Protocol core

```rust
use isapi::{Credentials, Decoder, Ptz, Request, Session};

let request = Request::get("/ISAPI/System/deviceInfo?channel=1")?;
let movement = Ptz::new(-25, 40, 0)?.momentary(1, 500)?;
assert_eq!(movement.method().as_str(), "PUT");

let mut decoder = Decoder::new("multipart/mixed; boundary=camera")?;
decoder.push(b"--camera\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}")?;
let part = decoder.next_part()?.expect("complete part");
assert_eq!(part.body(), b"{}");
# Ok::<(), isapi::Error>(())
```

`Session` holds one camera's credentials and Digest challenge state. Supply
`handle_challenge` with the camera's `WWW-Authenticate` value, then call
`authorization(&request, client_nonce)` with fresh, unpredictable entropy from
the transport. The supplied nonce prevents the authentication library from
calling its RNG. Each signature advances the nonce count and covers the exact
method, path plus query, and request body where `auth-int` is used. Do not reuse
a session across origins or print `Authorization::as_str()`.

`Request::get` and `Request::put` accept only origin-relative `/ISAPI/` resources.
Bodies are owned byte sequences; constructing a request never contacts or changes
a camera. `Ptz` adapts the upstream momentary payload layout and signed -100..100
axis ranges. Its duration is bounded to 1..10,000 ms and its channel is nonzero.
Scheduling, permission to move a camera, and interpreting its response remain
the caller's responsibility.

## Blocking adapter

```no_run
# #[cfg(feature = "ureq")]
fn receive(origin: &str, username: String, password: String) -> Result<(), isapi::Error> {
    use std::time::Duration;
    use isapi::{Credentials, blocking::Client};

    let mut client = Client::new(origin, Credentials::new(username, password))?;
    let mut stream = client.alert_stream(Duration::from_secs(30))?;
    while let Some(part) = stream.next_part()? {
        if let Some(event) = part.event()? {
            let active = event.active();
            let camera_reported_target = event.detection_target();
        }
    }
    Ok(())
}
```

The caller loads credentials privately. The adapter rejects credentials in URLs,
all redirects, and proxy environment variables. HTTPS keeps certificate
verification enabled. HTTP is supported for isolated camera networks, but Digest
does not encrypt payloads. The initial unauthenticated request is retried only
for a Digest challenge. Stale nonces may be refreshed within a three-attempt
budget; invalid credentials do not cause an unbounded retry loop.

Ordinary requests have a 15-second total deadline and a 256 KiB response limit.
`alert_stream` adds a total lifetime of up to 300 seconds, including authentication
and reads. `subscribe` holds the response continuously, without a periodic
reconnect. Both use a complete-part progress timeout of 30 seconds by default.
Partial input does not extend that deadline. Timeout is an error, not a motion clear.

Use `Client::builder(origin, credentials).cancelled(check).idle_timeout(duration).build()`
to supply a quick cancellation callback and customize the progress timeout.
Read waits check cancellation at intervals of at most 100 ms; connect/send phases
remain bounded to five seconds. Each stream has independent deadline state.
Dropping a stream releases its response. The crate does not reconnect automatically;
the KeepPeek camera worker owns bounded backoff and stops permanent auth failures.
The adapter pins `ureq` because its custom transport hook is explicitly unversioned.

`execute` is the raw-byte escape hatch. Use `Client::query` with a typed
`management::Query` for reads and `Client::command` for writes. They validate the
response media type and device `ResponseStatus`, including failures returned with
HTTP 200. Status 0/1 means accepted; status 7 is accepted with `reboot_required()`.
Other device codes are available through `Error::device_status()`. A transport
failure leaves the write outcome unknown: read back state before retrying.

## Typed Management

`management` exposes `DeviceInfo`, `Time`, `Stream`, `Motion`, `Rule`,
`Capabilities`, `PtzCapabilities`, `PtzStatus`, `Preset`, `AudioChannel`,
`AudioSession`, `AudioCodec`, and `CallbackHost`. Queries provide a
transport-independent request and response parser. Clock, stream, motion and rule
objects preserve unknown XML/JSON fields when their typed setters change values.
Read device capabilities before writing model-specific values.

```no_run
# #[cfg(feature = "ureq")]
fn configure(client: &mut isapi::blocking::Client) -> Result<(), isapi::Error> {
  use isapi::management::{DeviceInfo, Motion};
  let device = client.query(&DeviceInfo::query()?)?;
  let mut motion = client.query(&Motion::query(1)?)?;
  motion.set_sensitivity(60)?;
  let status = client.command(&motion.update(1)?)?;
  assert!(status.success());
  Ok(())
}
```

`Ptz` supports momentary, continuous, absolute and preset-recall requests. Stop
continuous movement explicitly with zero speeds. Absolute angles use tenths of
degrees; zoom uses camera-specific units. `Client::snapshot` requires JPEG media
type and framing and caps its original bytes at 1 MiB. Applications must validate
decoded dimensions before displaying or storing peer-supplied images.

## Two-way Audio

```no_run
# #[cfg(feature = "ureq")]
fn talk(origin: &str, credentials: isapi::Credentials) -> Result<(), isapi::Error> {
  use std::time::Duration;
  use isapi::{blocking::Client, management::AudioCodec};

  let mut session = Client::new(origin, credentials)?.open_audio(1, Duration::from_secs(30))?;
  let mut speaker = session.speaker()?;
  let silence = match speaker.codec() {
    AudioCodec::G711Ulaw => 0xff,
    AudioCodec::G711Alaw => 0xd5,
    _ => unreachable!("speaker accepts only G.711"),
  };
  speaker.send(&[silence; 160])?;
  drop(speaker);
  session.close()?;
  Ok(())
}
```

Channel discovery never opens audio or changes settings. An enabled channel and a
supported configured codec are required. `speaker()` uses one authenticated empty
binary PUT, then writes raw G.711 on that exact connection, outside the HTTP pool.
`microphone()` returns an independent receive handle; both can run concurrently.
The caller supplies encoded 8 kHz mono samples and consumes the reported codec.
No device codec change, browser microphone capture or PCM conversion is implied.

Sessions are bounded to 300 seconds, with at most 8,000 encoded bytes per second
of remaining lifetime per direction. Frames are at most 1,600 bytes; speaker
writes have a 100 ms absolute budget and microphone reads a two-second idle budget.
Cancellation and partial-write handling never replay already sent media.
`Talk::close` reports its result; `Drop` attempts cleanup within two seconds.
ISAPI close is channel-wide: coordinate ownership with other applications and use
`abandon` after known external ownership loss. Deadline expiry prevents further I/O
but does not spawn an automatic close worker. See the
[control and audio guide](../../docs/camera-controls.md) for lifecycle limits and
the difference between device capability and application talkback support.

## Analytics and Images

`Event::objects` exposes supported regional targets, target collections and ANPR
observations. Each object retains its target/region IDs, explicit classification,
confidence, scalar attributes, plate text, box units and image reference. The
validated document, including unknown nested extensions, remains in `Event::data`.
Unknown nested fields do not become guessed classifications.

`targetRect` defaults to 0..1000 coordinates; explicit `coordinateSystem` can name
pixels or normalized units. ANPR `plateRect` is in pixels. Pixel boxes cannot be
normalized without matching image dimensions. `confidenceLevel` uses 0..100;
`confidence` uses 0..1. Invalid values and duplicate target identities are rejected.

`Assembler` consumes `Part`s with caller-supplied `Duration` timestamps. It matches
only explicit image references against Content-ID or filename, using the form name
only when neither stronger identifier exists. `cid:` and angle brackets are
normalized; identifiers are never opened as filesystem paths. Ambiguous claims
and conflicting duplicate images fail closed. Use `next_deadline` to schedule
`expire`; `finish` emits incomplete metadata and resets the transport scope.
Starts and clears preserve channel order even when image arrival is interleaved.

`CallbackAuth` verifies the documented MD5 Digest callback mode using supplied
nonces/time and constant-time hash comparison. The owning HTTP application must
also enforce peer identity, TLS/network policy, body limits, concurrency and
acknowledgment rules; see the KeepPeek operational guide.

## Bounds and semantics

- Input chunks: at most 64 KiB; drain `Decoder::next_part` before feeding more.
- Buffered input length: at most 1 MiB + 72 KiB, excluding returned parts and
  allocator/transport overhead. The decoder has no event queue.
- Part headers: at most 8 KiB and 32 fields; MIME boundary: at most 70 bytes.
- XML/JSON part bodies: at most 256 KiB; JPEG/other bodies: at most 1 MiB.
- XML: depth 32, 8,192 elements, 32 attributes per element, 4 KiB scalar fields.
- JSON: depth 32, 8,192 values, unique object keys, and the same scalar-field limit.
  Validation runs on the raw JSON before typed decoding, so overwritten fields
  cannot hide excessive structure. Numeric channel/count fields may be strings
  or unsigned integers.
- `multipart/mixed`, `multipart/x-mixed-replace` and `multipart/form-data` accept declared lengths
  or delimiter-framed bodies. Arbitrary preambles/epilogues, nested multipart,
  and MIME transfer decoding are not supported.
- XML/JSON decoding supports UTF-8/ASCII, UTF-16 LE/BE and GB2312/GBK/GB18030.
  GB2312 labels use the maintained codec's GBK mapping. BOM, MIME charset and XML
  declarations must agree; ambiguous UTF-16 needs byte-order evidence. Invalid
  sequences, unsupported labels, DTDs, unknown entities and duplicate scalar
  fields fail closed. Encoded text is capped at 256 KiB, decoded text at 1 MiB.
- `Event` extracts direct `eventType`, `eventState`, `channelID`, `dynChannelID`,
  `dateTime`, `activePostCount`, `detectionTarget`, and `channelName` fields.
  Unknown extensions remain in `Event::data()` and original bytes in `Part::body()`.
- Unknown types and states are preserved. Only `active` and `inactive` map to
  Boolean state. `VMD` means motion, not a person detection. No classification
  is inferred from camera settings, and no snapshot/event association is assumed.
- Camera timestamps remain strings, including nonstandard offset formatting.
  Clock-skew normalization, interval lifecycle, and event deduplication belong to
  the consumer. A disconnect never implies that motion stopped.
- Image association: 16 pending/recovered notifications, 32 JPEGs, 8 MiB JPEG
  bytes and 512 pending/retired identifiers. Association expires after 5 seconds;
  used identifiers are reserved for 30 seconds. Reused ambiguous filenames require
  a fresh transport scope. Rejected new parts are not accepted into state; earlier
  pending/recovered metadata remains available through `expire` or `finish`.

The normal KeepPeek server selects a camera-owned ISAPI worker for compatible
cameras and feeds its normalized observations to durable storage and live event
delivery. See the [runtime policy](../../docs/hikvision-isapi.md#runtime-selection-and-event-policy).
Generic VMD retention is opt-in through `record_generic_motion_events`; explicit
targets are never inferred from camera-side filters.

## Validation

From the repository root:

```sh
cargo test --locked -p isapi --no-default-features
cargo test --locked -p isapi
cargo clippy --locked -p isapi --all-targets -- -D warnings
```

Network integration tests use the shared [fake Hikvision camera](../test-hikvision/README.md),
including its real Digest verifier and stateful configuration. Pure protocol tests
remain socket-free. Tests use synthetic data, not physical cameras. See the
[operational guide](../../docs/hikvision-isapi.md) for the read-only KeepPeek
diagnostic command and camera-specific findings. See [UPSTREAM.md](UPSTREAM.md)
for attribution, inspected reference clients, and licensing boundaries.
