# CC Video Fixtures

`cc-4k-*.mp4` are one-second, video-only derivatives of
[4K resolution sample](https://commons.wikimedia.org/wiki/File:4K_resolution_sample.ogv)
by Jihei (2011). The source and all four derivatives are licensed under
[Creative Commons Attribution-ShareAlike 3.0 Unported](https://creativecommons.org/licenses/by-sa/3.0/).

The source is 4096x2304. Each fixture was decoded from its first second,
scaled with FFmpeg's Lanczos filter, encoded at 15 fps with a 15-frame GOP,
and stripped of audio. The H.265 fixtures use no B-frames so their MP4 sample
timeline stays linear for deterministic RTP packetization.

`cc-4k-640x360-h264.mp4` uses constrained baseline H.264 at level 3.1 so it is
compatible with browser WebRTC decoders that do not advertise High profile.

`checked_in_mp4_fixtures_stream_through_both_backends` starts every fixture once
through Retina RTSP and once through Reo-proto. It requires both main and sub
profiles to deliver non-empty video, covering all eight backend-source pairs.

| File                       | Codec          | Resolution | SHA-256                                                            |
| -------------------------- | -------------- | ---------- | ------------------------------------------------------------------ |
| `cc-4k-640x360-h264.mp4`   | H.264 (`avc1`) | 640x360    | `94d9415835fa2c41397b85852d94781f3f33ae23a5b9c9ae81d106ccb0bba811` |
| `cc-4k-640x360-h265.mp4`   | H.265 (`hvc1`) | 640x360    | `0ce9f4b28a4305a191e3210d8691e14b370fa91eb7e14fef7e8f8aabd674a141` |
| `cc-4k-3840x2160-h264.mp4` | H.264 (`avc1`) | 3840x2160  | `f3c5893d87a6559cc494a41dd328965a4095f6994ec09e90d4186b8573d2ae49` |
| `cc-4k-3840x2160-h265.mp4` | H.265 (`hvc1`) | 3840x2160  | `81fc67e32fb8354b4374b3358caca2be1f45bf89c2deb50048dc1f1eae4352a4` |

## Big Buck Bunny Camera Fixture

The nine-camera demo uses `big-buck-bunny-3840x2160-h264.mp4`, a committed
48-second, video-only excerpt of Blender Foundation's official 2160p _Big Buck
Bunny_ release. The excerpt is 42,568,277 bytes, uses H.264 High profile at
3840x2160 and 30 fps, and has SHA-256
`21be06202908ddfb5adaa53cb63f8b0564fcab446045bc37be7b8faece6a564c`.

The official source archive is
[`bbb_sunflower_2160p_30fps_normal.mp4.zip`](https://download.blender.org/demo/movies/BBB/bbb_sunflower_2160p_30fps_normal.mp4.zip).
It is 632,204,510 bytes with SHA-256
`750b255c6d9fee1e2a03a6716d4f358bca56e9115bf3e06a66162fc5272ae151`.
Its contained MP4 has SHA-256
`37f0ff251a606c2dcfa26c19fe6bf843234b4e7a8889cfab50bc26f644e55520`.
The committed excerpt was generated from seconds 60 through 108 with:

```sh
ffmpeg -ss 60 -t 48 -i bbb_sunflower_2160p_30fps_normal.mp4 \
	-map 0:v:0 -an -c:v libx264 -preset veryfast \
	-b:v 7500k -maxrate 8000k -bufsize 15000k \
	-profile:v high -level:v 5.1 -pix_fmt yuv420p \
	-g 30 -keyint_min 30 -sc_threshold 0 -bf 0 \
	-map_metadata -1 -movflags +faststart \
	big-buck-bunny-3840x2160-h264.mp4
```

From `ui/`, generate the ignored camera profiles with:

```sh
bun run demo:fixtures:prepare
```

The script verifies the committed source and derives all four requested camera
profiles: 3840x2160 at 25 fps in H.264 and H.265, plus 640x360 at 15 fps in H.264
at 512 Kbps and H.265 at 256 Kbps. The 4K profiles target 8192 Kbps. Generated
H.264 uses Constrained Baseline so Chromium can negotiate it; generated H.265
uses closed GOPs so every boundary is an IDR. The script creates exact one-second
and two-second GOP variants, validates their streams and keyframe timestamps with
FFprobe, and records their output hashes in an ignored manifest. The nine-camera
launcher mixes codec pairs and GOP cadences across its looping sources.

_Big Buck Bunny_ is © 2008 Blender Foundation and is licensed under
[Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/).
