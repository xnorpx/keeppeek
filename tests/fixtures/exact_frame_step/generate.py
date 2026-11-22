"""Build the twelve-frame H.264 timing fixtures. Requires FFmpeg with libx264."""

import struct
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent
FRAME_COUNT = 12


def frames():
    result = bytearray()
    for number in range(FRAME_COUNT):
        # Four high-contrast luma stripes encode the frame number, low bit first.
        row = bytes(224 if number & (1 << (x // 16)) else 32 for x in range(64))
        result.extend(row * 32)
        result.extend(bytes([128]) * (64 * 32 // 2))
    return result


def encode(name, timing):
    path = ROOT / (name + ".mp4")
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            "64x32",
            "-framerate",
            "25",
            "-i",
            "pipe:0",
            "-vf",
            timing,
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-qp",
            "15",
            "-x264-params",
            "bframes=2:b-adapt=0:b-pyramid=none:scenecut=0:keyint=6:min-keyint=6",
            "-video_track_timescale",
            "1000",
            "-movflags",
            "+frag_keyframe+empty_moov+negative_cts_offsets",
            "-map_metadata",
            "-1",
            str(path),
        ],
        input=frames(),
        check=True,
    )
    return path


def duplicate_presentation_time(path):
    data = bytearray(path.read_bytes())
    # In the first six-sample run, give the second B picture the first B picture's PTS.
    run = data.index(b"trun") + 4
    if struct.unpack_from(">II", data, run) != (0x01000B05, 6):
        raise ValueError("Unexpected fragment layout from the encoder")
    offset = run + 16 + 3 * 12 + 8
    if struct.unpack_from(">i", data, offset)[0] != -40:
        raise ValueError("Unexpected composition offset")
    struct.pack_into(">i", data, offset, -80)
    # In the second GOP, reference picture 11 is decoded before B picture 10.
    # Give them the same PTS while retaining the encoder's display order.
    second_run = data.index(b"trun", run) + 4
    reference_offset = second_run + 16 + 4 * 12 + 8
    if struct.unpack_from(">II", data, second_run) != (0x01000B05, 6) or struct.unpack_from(">i", data, reference_offset)[0] != 40:
        raise ValueError("Unexpected second fragment layout")
    struct.pack_into(">i", data, reference_offset, 0)
    (ROOT / "duplicate-pts.mp4").write_bytes(data)


if __name__ == "__main__":
    encode("cfr", "setpts=N")
    duplicate_presentation_time(encode("vfr", "setpts=N+2*floor(N/3)"))
