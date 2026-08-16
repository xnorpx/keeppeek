# CC Video Fixtures

`cc-4k-*.mp4` are one-second, video-only derivatives of
[4K resolution sample](https://commons.wikimedia.org/wiki/File:4K_resolution_sample.ogv)
by Jihei (2011). The source and all four derivatives are licensed under
[Creative Commons Attribution-ShareAlike 3.0 Unported](https://creativecommons.org/licenses/by-sa/3.0/).

The source is 4096x2304. Each fixture was decoded from its first second,
scaled with FFmpeg's Lanczos filter, encoded at 15 fps with a 15-frame GOP,
and stripped of audio. The H.265 fixtures use no B-frames so their MP4 sample
timeline stays linear for deterministic RTP packetization.

`checked_in_mp4_fixtures_stream_through_both_backends` starts every fixture once
through Retina RTSP and once through Reo-proto. It requires both main and sub
profiles to deliver non-empty video, covering all eight backend-source pairs.

| File                       | Codec          | Resolution | SHA-256                                                            |
| -------------------------- | -------------- | ---------- | ------------------------------------------------------------------ |
| `cc-4k-640x360-h264.mp4`   | H.264 (`avc1`) | 640x360    | `a76b29f58123742bc2cc21c26e3758068f4230537ec15ceb082698fd12569570` |
| `cc-4k-640x360-h265.mp4`   | H.265 (`hvc1`) | 640x360    | `0ce9f4b28a4305a191e3210d8691e14b370fa91eb7e14fef7e8f8aabd674a141` |
| `cc-4k-3840x2160-h264.mp4` | H.264 (`avc1`) | 3840x2160  | `f3c5893d87a6559cc494a41dd328965a4095f6994ec09e90d4186b8573d2ae49` |
| `cc-4k-3840x2160-h265.mp4` | H.265 (`hvc1`) | 3840x2160  | `81fc67e32fb8354b4374b3358caca2be1f45bf89c2deb50048dc1f1eae4352a4` |
