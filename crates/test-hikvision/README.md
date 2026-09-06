# Fake Hikvision Camera

`test-hikvision` is a reusable, loopback-only ISAPI HTTP device for integration
tests. It has no dependency on KeepPeek or the production `isapi` crate, so both
can use it without a dependency cycle. It verifies Digest responses rather than
accepting any Authorization header.

## Use in Tests

```rust
use test_hikvision::{EventPart, FakeHikvision, Reply};

let camera = FakeHikvision::builder()
    .credentials("test", "test")
    .alert_streams([Reply::alert([
        EventPart::motion(true),
        EventPart::motion(false),
    ], true)])
    .start()?;
assert!(camera.address().ip().is_loopback());
# Ok::<(), anyhow::Error>(())
```

- `FakeHikvision::builder()` binds `127.0.0.1:0`; parallel tests get distinct ports.
- `alert_streams` and `enqueue_alert` provide one reply per authenticated
  subscription, including reconnect scenarios.
- `Reply::fragmented`, `then` and `hold_open` simulate split reads, delayed input,
  partial bodies and cancellation. `Reply::raw` intentionally permits malformed
  protocol data for negative tests.
- `replies` and `enqueue` override the next request before normal routing and
  authentication. Use these only when testing a specific HTTP/authentication fault.
- `resource` returns retained bytes for assertions independent of the client parser.
- `set_resource` changes synthetic XML capabilities/configuration for model-variation tests.
- Audio open/close retains one owner and returns busy for a second open. Speaker
  uploads verify Digest and the exact session ID before accepting raw bytes after
  the empty PUT handshake. `audio_output` and `wait_for_audio_bytes` expose bounded
  byte evidence; `set_audio_input` supplies microphone bytes.
- `set_audio_input_after_output` gates microphone delivery on speaker progress,
  so a duplex test cannot pass with serialized send/receive operations.
- `requests` and `wait_for_requests` expose bounded request evidence. Debug output
  excludes credentials, authorization headers, paths and bodies.
- `post_callback` independently signs a Digest callback. `callback_connection`
  supports partial and malformed uploads. Both reject non-loopback destinations.
- Dropping the fixture closes active sockets, interrupts delayed replies and joins
  its workers. No camera discovery, physical device or real credentials are used.

The normal device supports device information, clock, main/sub stream configuration,
motion and smart-rule configuration, PTZ/presets, JPEG snapshots, callback hosts and
capability responses. Configuration writes are retained in memory; nothing writes
camera settings or repository files. Scripted responses cover unsupported resources,
device-level errors, nonce refresh, redirects and framing failures.

Limits: 16 connections, 256 captured requests, 128 queued replies, 16 MiB scripted
payload, 16 KiB/32 request headers and 256 KiB request bodies. Socket waits are two
seconds and request reads have a five-second total budget. This is a test fixture,
not a production server or an exact emulator of every Hikvision firmware.
Audio captures are bounded to 2,400,000 bytes per direction and 300 seconds per
connection. Dropping the fixture interrupts audio as well as HTTP workers.

## Local Command

From the repository root:

```sh
cargo run --locked -p test-camera --bin test_camera -- hikvision
```

This starts the HTTP fixture and prints its ephemeral address and a test-only
configuration entry. Credentials default to `test` / `test`. Ctrl+C stops it.
The standalone command does not emulate video or detection from images; use the
existing RTSP test-camera mode for media and scripted `EventPart`s for ISAPI events.

## Test Ownership

All network-facing ISAPI client and camera-worker tests use this fixture. Pure
Sans-I/O parser, lifecycle, geometry and authentication unit tests continue to use
byte arrays and explicit time, without unnecessarily opening a socket.

```sh
cargo test --locked -p isapi --test fake_hikvision --test client --test audio_transport
cargo test --locked --lib isapi
cargo clippy --locked -p test-hikvision --all-targets -- -D warnings
```

The fixture's independent Digest, XML-list and stateful resource behavior is tested
through the production client. KeepPeek integration tests cover ANPR with interleaved
JPEGs, durable revisions, real callback uploads, source removal and clean shutdown.
