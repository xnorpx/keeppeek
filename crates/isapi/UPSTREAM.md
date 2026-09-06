# Upstream provenance

This is a plain-Rust ISAPI implementation with a Sans-I/O protocol core and an
optional `ureq` adapter. It is not a combined SDK wrapper or a drop-in replacement
for either crate reviewed below. No native Hikvision library is loaded or linked.

| Source                                                                      | Published version                                                 | Source commit                              | Retained license                                                                 |
| --------------------------------------------------------------------------- | ----------------------------------------------------------------- | ------------------------------------------ | -------------------------------------------------------------------------------- |
| [PuffyWithEyes/hikvision-rs](https://github.com/PuffyWithEyes/hikvision-rs) | [hikvision-rs 0.1.0](https://crates.io/crates/hikvision-rs/0.1.0) | `08c0bddaee926bb1428188da324f4d7234ae5fcd` | [LICENSE](LICENSE)                                                               |
| [EternalNight/hikvision-rs](https://gitee.com/eternalnight996/hikvision-rs) | [hikvision 0.1.20](https://crates.io/crates/hikvision/0.1.20)     | `427973d2993eede85b2f519cc473dc5d28de1d81` | [MIT, reference only](https://docs.rs/crate/hikvision/0.1.20/source/MIT-LICENSE) |

Both published license files contain the standard MIT text. The `hikvision`
manifest uses `license-file = "MIT-LICENSE"`, which crates.io reports as
`non-standard`; the actual file is MIT. The licenses cover the Rust source, not
Hikvision's separately distributed native SDK binaries.

## Adaptation boundary

- `hikvision-rs/src/lib.rs`: the PTZ momentary request path, pan/tilt/zoom range,
  and XML payload layout. Request construction is independent of sockets and
  clocks; scheduling is the caller's responsibility.
- `hikvision/src/core/net_sdk/net/types.rs` and its `api.rs` were inspected to
  distinguish SDK ABI structures from HTTP protocol data. No implementation from
  this crate is imported. Its `libloading` calls and raw-pointer structures are
  not an ISAPI HTTP implementation and cannot be replaced by swapping HTTP clients.
- Digest authentication and incremental HTTP multipart/XML parsing are new
  implementations built on maintained parser/authentication libraries. Neither
  upstream crate supplies an HTTP alert-stream parser.

## Implementation boundary

The core accepts request descriptions, challenge strings, caller-supplied client
nonces, and input bytes. It must not open sockets, read files, generate entropy,
sleep, or sample the clock. It validates and emits owned protocol data with
explicit size limits. The optional `blocking` module supplies `ureq` networking,
random client nonces, and deadlines. No native SDK or Python runtime is required.

Authentication headers stay out of `Debug` and errors; redirects and malformed
or oversized input are rejected. The diagnostic is read-only and uses KeepPeek's
private credential resolver. The application-owned worker handles reconnection,
event normalization, persistence, and live delivery. Opt-in authenticated callback
reception uses the same parser and event pipeline. Typed camera configuration
requests require explicit transmission; no automatic camera configuration is performed.

`encoding_rs` supplies strict UTF-8, UTF-16 and Chinese legacy encoding conversion
under its `(Apache-2.0 OR MIT) AND BSD-3-Clause` license. No conversion tables were
copied into this crate. The existing `xml`, `serde_json`, `digest_auth`, `chrono`
and `subtle` libraries provide structured parsing, time validation, Digest and
constant-time comparison. The application and fake camera are separate AGPL-3.0-only
workspace crates; the protocol library remains MIT.

Verification uses `cargo test --locked -p isapi --no-default-features` for pure
protocol tests and `cargo test --locked -p isapi` for the local HTTP adapter tests.
Use `cargo clippy --locked -p isapi --all-targets -- -D warnings` and
`cargo fmt --all -- --check` before completion. Tests live under `tests/`, use
synthetic payloads, and must not require a physical camera or real credentials.
Network-facing tests share [test-hikvision](../test-hikvision/README.md), a
loopback-only independent fake device with stateful endpoints and verified Digest.
It does not depend on or reuse the production ISAPI request/response parsers.

## Additional public references

These clients were read for interoperability evidence. Their implementation code
was not copied or translated into this crate. Observed behavior in an example is
not a guarantee about all Hikvision firmware.

| Reference                                                                                                  | Verified license                                                                              | Useful evidence and limits                                                                                                                                                                                                                                                                                |
| ---------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [corvis/homeassistant_hikvision, Python](https://github.com/corvis/homeassistant_hikvision)                | [MIT, Dmitry Berezovsky](https://github.com/corvis/homeassistant_hikvision/blob/main/LICENSE) | `src/hikvision_isapi/isapi/client.py` uses a persistent multipart alert stream and handles length-delimited and lengthless parts. `model.py` exposes type, state, channel, and camera timestamp. It uses Basic auth and permits disabling TLS checks; neither behavior is copied here.                    |
| [bert-y/hik_isapi_tools, JavaScript](https://github.com/bert-y/hik_isapi_tools)                            | [MIT, bert-y](https://github.com/bert-y/hik_isapi_tools/blob/main/LICENSE)                    | `lib/hik_base.js` waits for an authentication challenge and parses XML; `hik_device.js` covers channel configuration. This is not a complete alert-stream parser. Error-body logging and certificate-validation bypasses are not adopted.                                                                 |
| [ragingcomputer/node-hikvision-api, JavaScript](https://github.com/ragingcomputer/node-hikvision-api)      | GPL-3.0                                                                                       | Its README describes alarm start/stop, motion, line-crossing, and video-loss callbacks. No code is copied into this MIT crate.                                                                                                                                                                            |
| [multipart-stream 0.1.2, Rust](https://docs.rs/multipart-stream/0.1.2/src/multipart_stream/parser.rs.html) | MIT OR Apache-2.0                                                                             | Contains Hikvision framing fixtures, including unusual timestamp offsets. Its public parser is stream-based, requires per-part Content-Length, and documents incomplete header-limit enforcement; it was not imported. The new decoder uses existing `mime` and `httparse` crates for structured parsing. |

## Official material

The user-supplied [ISAPI General Application Developer Guide](../../docs/isapi.pdf)
is the protocol reference for persistent arming subscriptions (PDF pages 131-132),
heartbeat recognition (page 192), and alarm/heartbeat XML (pages 626-628). It
specifies ten-second heartbeats on newer firmware and a 30-second timeout. The
Rust implementation also accepts the `www.isapi.com` namespace in its examples.
The document's availability does not imply an MIT license for its text or examples.
Callback-host configuration follows PDF pages 133 and 645-648; XML and JSON
ResponseStatus follow pages 695-696 and 501. The general guide does not specify
every analytics application's payload. Named target/region and ANPR paths have
synthetic coverage, not a claim of support on every model.

Additional interoperability references were inspected for field paths only:
[HikSink's event parser](https://github.com/CornerBit/HikSink/blob/master/src/hikapi/alert_parser.rs)
for `DetectionRegionList/DetectionRegionEntry`, and
[ParkPow's Hikvision example](https://github.com/parkpow/deep-license-plate-recognition/blob/master/parkpow/hikvision_lpr/app.py)
for `ANPR/licensePlate`, `confidenceLevel` and `pictureInfoList/plateRect`.
No implementation or fixtures from these references were copied into the MIT crate.

The supplied [Go reference](https://github.com/loozhengyuan/hikvision-sdk/tree/540af54696c2770ab590652c9dfea61631507d52/hikvision)
is GPL-3.0 and archived. Its `client.go` and `http.go` were read for HTTP/Digest and
ResponseStatus conventions, not copied or translated. It implements a subset of
device/time APIs, not the continuous alert worker. Its `Put` helper constructs a
GET request, reinforcing why example implementations must be checked against the
protocol rather than treated as authoritative behavior.

[Hikvision Open Capabilities](https://tpp.hikvision.com/tpp/OpenCapabilities)
describes ISAPI as HTTP-based and separately describes the native SDK. The
[official training portal](https://tpp.hikvision.com/tpp/Training) redirected to
partner authentication during this investigation. No publicly accessible,
appropriately licensed official reference implementation was verified. Official
examples must have their redistribution terms checked before any code is reused;
public access alone does not grant an MIT license.

Library behavior was checked against the official
[`ureq` 3.4 configuration documentation](https://docs.rs/ureq/3.4.0/ureq/config/struct.ConfigBuilder.html),
[`digest_auth` context API](https://docs.rs/digest_auth/0.3.1/digest_auth/struct.AuthContext.html),
and [`xml` 1.4 parser configuration](https://docs.rs/xml/1.4.0/xml/reader/struct.ParserConfig.html).
In `ureq` 3.4, response-header timing participates in later body deadlines. The
adapter uses explicit per-operation/per-stream deadlines and a cancellable raw
transport below TLS. Continuous streams have no global lifetime cap. A localhost
test with an event after 5.5 seconds guards the early-stream-timeout regression;
idle/cancellation tests verify progress bounds and shutdown without disconnect polling.
Core errors use disabled backtraces so constructing an error does not consult
environment variables or sample the operating-system stack. Diagnostics discard
peer-controlled error text; callers can inspect the explicit error predicates.

Two-way audio follows sections 8.3, 15.10.185-190 and 16.2.301-304 of the supplied
guide (PDF pages 123, 395-399 and 728-730). The upload table contains a conflicting
method label; its PUT URL, workflow and example agree. Its empty PUT handshake
followed by raw socket audio is also corroborated by the
[go2rtc ISAPI client](https://github.com/AlexxIT/go2rtc/blob/master/pkg/isapi/client.go).
That client is interoperability evidence only; none of its source was copied.
Its close-before-open behavior is not adopted because it can disrupt another owner.
The speaker keeps a dedicated verified ureq transport outside connection reuse and
uses an audio-only bounded partial-write socket layer beneath TLS. The implementation
uses ureq's documented [transport interfaces](https://docs.rs/ureq/3.4.0/ureq/unversioned/transport/index.html).
