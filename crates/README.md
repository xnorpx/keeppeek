# Support crates

These are some of the crates KeepPeek depends on. A few are forks, usually because the upstream project has gone quiet or because KeepPeek's changes are too large and specific to ask the maintainers to take on. Keeping them here saves everyone time.

## Special shoutout

- [Retina](https://github.com/scottlamb/retina) is an excellent high-level RTSP library and did
  much of the difficult camera-interoperability work that KeepPeek builds on. KeepPeek carries a
  local fork for its recording and compatibility needs.
- [ONVIF-rs](https://github.com/lumeohq/onvif-rs) did the hard work of making ONVIF discovery,
  schemas, authentication, and camera operations usable from Rust. KeepPeek carries a local fork
  for its supported camera surface.
- [str0m](https://github.com/algesten/str0m) is the upstream WebRTC implementation used by KeepPeek.
  Its Sans I/O design, explicit inputs, RTP and frame APIs, and data-channel support make it an
  exceptional fit for a focused Rust media service.

Thank you to the original authors, maintainers, and everyone who contributed to these projects. The
local forks retain their upstream authorship and licenses.

## ISAPI

[isapi](isapi/README.md) implements Hikvision/Annke HTTP event parsing in plain
Rust, with a Sans-I/O protocol core and an optional `ureq` adapter. Its
[provenance notes](isapi/UPSTREAM.md) distinguish reused PTZ payload conventions
from inspected Python, JavaScript, and native SDK references. It does not load a
Hikvision SDK binary. The [operational guide](../docs/hikvision-isapi.md) documents
the Rust camera diagnostic, typed management operations, callback activation and
device compatibility limits.

## Fake Hikvision

[test-hikvision](test-hikvision/README.md) provides the shared, stateful ISAPI HTTP
camera used by integration tests. It is independent of KeepPeek and the ISAPI
client, verifies Digest authentication, supports configurable event streams and
callbacks, and binds only to loopback with ephemeral ports. The existing
`test_camera hikvision` command exposes it for local protocol testing.
