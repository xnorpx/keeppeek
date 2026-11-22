# Flash and tone timing fixture

`flash-tone-lead-200ms.mp4` contains only synthetic FFmpeg sources. It has three full white
flashes at input times 1, 2, and 3 seconds, and three 1 kHz tone pulses 200 ms earlier. Each
pulse lasts 80 ms. No third-party camera or microphone content is included.

The fixture is 11,473 bytes. Its SHA-256 is
`67e67796ddb8b2466e06dbeeec2438e79c1974d77d08403f9f30511dd5251038`.
FFmpeg 9.0.2 reports 100 H.264 frames at 160×90 and 25 fps, a 90 kHz video clock,
189 mono AAC packets at 48 kHz, and a container duration of 4.021333 seconds. AAC priming
affects container duration; the tests measure decoded flash and pulse times independently.

The generation command was:

```sh
ffmpeg -nostdin -hide_banner -v error \
  -f lavfi -i "color=c=black:s=160x90:r=25:d=4,drawbox=c=white:t=fill:enable='between(t,1,1.04)+between(t,2,2.04)+between(t,3,3.04)'" \
  -f lavfi -i "aevalsrc='0.8*sin(2*PI*1000*t)*(between(t,0.8,0.88)+between(t,1.8,1.88)+between(t,2.8,2.88))':s=48000:d=4" \
  -map 0:v:0 -map 1:a:0 -c:v libx264 -preset ultrafast -crf 18 \
  -pix_fmt yuv420p -g 25 -keyint_min 25 -sc_threshold 0 -bf 0 -threads 1 \
  -c:a aac -b:a 64k -ar 48000 -ac 1 -t 4 -video_track_timescale 90000 \
  -use_editlist 0 -movflags +faststart flash-tone-lead-200ms.mp4
```

Tests copy the encoded samples into the recording writer's normal fragmented container. They
retain a hash of this recording before exercising export and stored playback correction.
FFmpeg then decodes video and audio. Video flash times come from decoded frame presentation
timestamps and pixel values. PCM uses `-copyts` and `aresample=async=1:first_pts=0` so audio
track delay remains visible instead of disappearing when raw samples are extracted.

The 32 ms assertion bound covers one 21.334 ms AAC packet plus the 10 ms PCM energy window.
Structural tests check exact timestamps separately. This fixture qualifies encoded remux and
decoder timing; it does not measure capture-clock drift, physical lip synchronization, or browser
audio-device latency.
