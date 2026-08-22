# Support crates

These are some of the crates KeepPeek depends on. A few are forks, usually because the upstream project has gone quiet or because KeepPeek's changes are too large and specific to ask the maintainers to take on. Keeping them here saves everyone time.

## Special shoutout

A special shoutout to [Retina](https://github.com/scottlamb/retina) and [ONVIF-rs](https://github.com/lumeohq/onvif-rs). Retina is an excellent RTSP library, and ONVIF-rs did the hard work of making ONVIF usable from Rust. Thanks to both projects and everyone who contributed to them.

`hap-video` is KeepPeek's sans-I/O HomeKit camera protocol crate. It consolidates the required accessory pairing, encrypted control framing, accessory model, and 2026 WebRTC signaling behavior while leaving sockets, persistence, clocks, randomness, and threads to adapters.
