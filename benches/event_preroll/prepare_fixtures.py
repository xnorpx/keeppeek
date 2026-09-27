"""Generate deterministic pre-roll benchmark fixtures with the installed FFmpeg."""

import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "target" / "event-preroll-fixtures"


def encoder_command(height: int, codec: str, gop_seconds: int, output: Path) -> list[str]:
    source = ROOT / "crates/test-camera/testdata/cc-4k-3840x2160-h264.mp4"
    bitrate = "4M" if height == 1080 else "12M"
    command = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-stream_loop",
        "11",
        "-i",
        str(source),
        "-an",
        "-t",
        "12",
        "-vf",
        f"scale=-2:{height}",
        "-r",
        "15",
        "-c:v",
        "libx264" if codec == "h264" else "libx265",
        "-preset",
        "ultrafast",
        "-threads",
        "2",
        "-b:v",
        bitrate,
        "-minrate",
        bitrate,
        "-maxrate",
        bitrate,
        "-bufsize",
        "24M",
        "-g",
        str(15 * gop_seconds),
        "-keyint_min",
        str(15 * gop_seconds),
        "-sc_threshold",
        "0",
        "-bf",
        "0",
    ]
    if codec == "h265":
        command += [
            "-x265-params",
            "pools=2:frame-threads=1:scenecut=0:log-level=error",
        ]
    command.append(str(output))
    return command


def fixture(height: int, codec: str, gop_seconds: int) -> dict:
    output = OUTPUT / f"{height}p-{codec}-gop{gop_seconds}.mp4"
    command = encoder_command(height, codec, gop_seconds, output)
    if not output.exists():
        subprocess.run(command, check=True, timeout=600)
    probe = subprocess.check_output(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,codec_name,avg_frame_rate,bit_rate,nb_frames",
            "-of",
            "json",
            str(output),
        ],
        text=True,
    )
    return {
        "file": output.name,
        "sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        "gop_seconds": gop_seconds,
        "probe": json.loads(probe),
        "command": command,
    }


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    fixtures = []
    for height in (1080, 2160):
        for codec in ("h264", "h265"):
            for gop in (1, 10):
                print(f"Preparing {height}p {codec} GOP {gop}s", flush=True)
                fixtures.append(fixture(height, codec, gop))
    manifest = {
        "ffmpeg": subprocess.check_output(["ffmpeg", "-version"], text=True).splitlines()[0],
        "fixtures": fixtures,
    }
    (OUTPUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
