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
