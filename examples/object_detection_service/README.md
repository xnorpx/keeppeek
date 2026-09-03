<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# KeepPeek object-detection service example

This is a deliberately small Python 3.12 reference service showing that an external process can
authenticate to KeepPeek, subscribe to encoded H.264 or H.265 camera video over WebRTC, decode
selected keyframes with the user-installed `ffmpeg` executable, run Ultralytics inference, and
publish bounded `person` or `vehicle` events through KeepPeek's protobuf API.

This example exists only for demonstration and CI. It is **not production-ready**, is **not a
supported detector product**, and intentionally **will not evolve into a mature object-detection
service**. KeepPeek owns and hardens the public integration API; detector products, model services,
and their deployment remain independent of this repository. This example exists so community and
third-party developers can implement their own compatible services.

## Prerequisites

- Python 3.12 or newer. CI and version-aware local tools read the pinned 3.12 baseline from
  `.python-version`.
- A current KeepPeek build that advertises `keeppeek.event-publication.v1`.
- One active camera source with an H.264 or H.265 `main` or `sub` variant.
- FFmpeg available as `ffmpeg` on `PATH`.
- A KeepPeek access key supplied through an environment variable or owner-only file.

The service runs `ffmpeg -version` before contacting KeepPeek. It never installs, downloads, or
bundles FFmpeg.

### Install FFmpeg

macOS with Homebrew:

```sh
brew install ffmpeg
```

Windows PowerShell with WinGet or Chocolatey:

```powershell
winget install Gyan.FFmpeg
# Or: choco install ffmpeg
```

Debian or Ubuntu, Fedora, and Arch Linux respectively:

```sh
sudo apt install ffmpeg
# sudo dnf install ffmpeg
# sudo pacman -S ffmpeg
```

Verify the executable independently:

```sh
ffmpeg -version
```

```powershell
ffmpeg -version
```

## Environment setup

Run these commands from `examples/object_detection_service`. This repository does not use Python
virtual environments; install into the Python 3.12 interpreter directly.

macOS or Linux:

```sh
python3.12 -m pip install -r requirements.txt
python3.12 generate_protos.py
```

Add `--break-system-packages` to the install command when the interpreter is externally managed,
which is common for Homebrew and Debian system Python.

Windows PowerShell:

```powershell
py -3.12 -m pip install -r requirements.txt
py -3.12 generate_protos.py
```

`generate_protos.py` deterministically compiles the canonical repository schema at
`../../api/webrtc.proto`. It creates `generated/webrtc_pb2.py` and
`generated/webrtc_pb2.pyi`; both receive an `SPDX-License-Identifier: MIT` header. Do not create
handwritten copies of the wire messages.

## KeepPeek configuration

Find the camera's stable source ID in KeepPeek's Cameras UI. The source must be online before the
service starts. KeepPeek must advertise a reliable-data H.264 or H.265 variant and the `person` or
`vehicle` event type for that source.

Secrets never belong in command history, example command lines, or committed configuration. Use
one of these methods.

macOS or Linux environment variable:

```sh
export KEEPPEEK_ACCESS_KEY="$(cat "$HOME/.config/keeppeek/object-detector-access-key")"
chmod 600 "$HOME/.config/keeppeek/object-detector-access-key"
```

macOS or Linux owner-only file:

```sh
chmod 600 "$HOME/.config/keeppeek/object-detector-access-key"
export KEEPPEEK_ACCESS_KEY_FILE="$HOME/.config/keeppeek/object-detector-access-key"
```

Windows PowerShell environment variable or file:

```powershell
$env:KEEPPEEK_ACCESS_KEY = Get-Content "$HOME\.keeppeek\object-detector-access-key" -Raw
# Or:
$env:KEEPPEEK_ACCESS_KEY_FILE = "$HOME\.keeppeek\object-detector-access-key"
```

On Windows, restrict the file ACL to the current owner. The file contains only the access-key UUID
and a trailing newline is optional. `KEEPPEEK_ACCESS_KEY` takes precedence over the file.

Non-secret settings can also use `KEEPPEEK_URL`, `KEEPPEEK_SOURCE_ID`, `KEEPPEEK_STREAM`,
`KEEPPEEK_MODEL`, `KEEPPEEK_CONFIDENCE`, `KEEPPEEK_COOLDOWN_SECONDS`, and
`KEEPPEEK_INFERENCE_FPS`.

## Run

Copy-pasteable macOS or Linux example:

```sh
export KEEPPEEK_URL="http://127.0.0.1:8081"
export KEEPPEEK_SOURCE_ID="front-door"
python object_detection_service.py --stream sub --model yolo11n.pt
```

Copy-pasteable Windows PowerShell example:

```powershell
$env:KEEPPEEK_URL = "http://127.0.0.1:8081"
$env:KEEPPEEK_SOURCE_ID = "front-door"
python .\object_detection_service.py --stream sub --model yolo11n.pt
```

The default `ultralytics` detector loads `yolo11n.pt` only after KeepPeek authentication,
capability validation, and media-subscription acceptance succeed. If the weights are absent,
Ultralytics downloads them during that load into its configured weights/cache location. The
`ultralytics` package and Ultralytics model weights have upstream licensing terms, including
AGPL-3.0 and an enterprise option; review the current
[Ultralytics licensing documentation](https://www.ultralytics.com/license) before use. KeepPeek
does not distribute or cache model weights.

The deterministic fake is only for tests and CI and never downloads weights:

```sh
python object_detection_service.py --source-id front-door --stream sub --detector fake
```

```powershell
python .\object_detection_service.py --source-id front-door --stream sub --detector fake
```

## Expected output and verification

After an eligible keyframe, normal logs resemble:

```text
INFO subscribed source_id=front-door stream=sub codec=h264
INFO loading Ultralytics model yolo11n.pt
INFO published detection event_id=<uuid> class=person confidence=0.912
```

Open KeepPeek's Events page and select the source and current UTC day. A published result appears
as a `person` or `vehicle` event with:

- stable source ID and selected `main` or `sub` stream identity;
- the camera frame's source timestamp;
- revision 1 and a globally unique event ID;
- normalized `x`, `y`, `width`, and `height` bounding-box values;
- confidence from 0 through 1;
- `keeppeek` origin and structured object-class/model metadata on the wire.

The service keeps the highest-confidence detection per class and enforces a five-second default
cooldown per class, so it does not publish one event per frame. A retry reuses the exact event ID
and revision; KeepPeek treats an identical retry as idempotent.

## Bounded processing

The example intentionally favors recent evidence over backlog:

- encoded queue: 4 frames and 8 MiB;
- decoded/inference queue: 1 frame and 24 MiB;
- publication queue: 16 events and 16 KiB;
- one encoded frame: at most 8 MiB and 256 protobuf fragments.

When a bound is reached, the oldest queued work is removed first. Only eligible keyframes are
decoded, at no more than the configured inference rate. Session loss immediately deactivates the
pipeline, clears all three queues and partial fragments, and discards results carrying the old
session generation. Reconnection must establish a fresh authenticated session and accepted media
subscription before work resumes.

## Tests and checks

macOS or Linux:

```sh
python generate_protos.py
python -m pytest -q
python -m mypy --strict
python -m black --check .
python -m ruff check .
python -m ruff format --check .
```

Windows PowerShell:

```powershell
python .\generate_protos.py
python -m pytest -q
python -m mypy --strict
python -m black --check .
python -m ruff check .
python -m ruff format --check .
```

Apply Python formatting with `python -m black .`, or run the repository-level `./fix.sh` or
`fix.bat` script.

The default tests use repository H.264 and H.265 fixtures, the external `ffmpeg` executable, and a
deterministic fake detector. They require no camera, GPU, cloud service, model download, access
key, or other secret.

### Vendor-neutral conformance

The finite conformance client uses only the public HTTP, SDP, and protobuf contracts. It consumes
two 640x360 low-bandwidth streams, one H.264 and one H.265, and decodes one keyframe from each. A
deterministic fake detection then opens one bounded main-stream subscription, captures one
timestamped 3840x2160 JPEG, and closes that high-quality path after atomic publication.

The process test verifies the same source, stream, timestamp, class, confidence, bounding box,
revision, structured payload, attachment descriptor, and exact JPEG bytes through live fanout,
stored search, and the production Events UI. It also verifies advancing live video, decoded stored
playback, recording growth while another client withholds events, force-kill isolation, and a fresh
two-codec reconnect. The run inspects fixed-cardinality operational metrics and scans runtime
diagnostics and generated bindings for test access-key, metadata, JPEG, source-frame, and full-SDP
credential disclosure.

Build the two local binaries from the repository root. On macOS, use the same AWS-LC test provider
as the browser suite:

```sh
cargo build --locked -p keeppeek -p test-camera \
  --bin keeppeek --bin test_camera \
  --features keeppeek/macos-test-aws-crypto
```

Linux does not need the final `--features` argument. Then run:

```sh
cd examples/object_detection_service
KEEPPEEK_RUN_EXTERNAL_CONFORMANCE=1 \
  KEEPPEEK_CONFORMANCE_KEEPPEEK_BIN="$PWD/../../target/debug/keeppeek" \
  KEEPPEEK_CONFORMANCE_CAMERA_BIN="$PWD/../../target/debug/test_camera" \
  python3.12 -m pytest -q tests/test_e2e.py -k two_stream_no_model
```

This command needs no physical camera, GPU, model, cloud service, or user credential. The fixed UUID
used by the leak scanner is synthetic test data created inside the isolated process run.

The conformance implementation in this directory is an AGPL-3.0-only reference, not a client SDK.
Independent implementations can generate their own bindings solely from the MIT-licensed files in
[`api`](../../api/README.md); doing so does not apply the example or server AGPL license to them.

The real-process integration uses Intel's CC-BY-4.0
`person-bicycle-car-detection.mp4`, the repository fixture-camera server, KeepPeek, aiortc, FFmpeg,
actual Ultralytics inference, and an isolated catalog. Keep the upstream dataset `LICENSE` and
`README.md` in `data/sample-videos`; [data/README.md](../../data/README.md) records the pinned commit,
attribution, hashes, and clone command.

First run the opt-in model test from `target/`. This downloads `yolo11n.pt` into ignored build
output, decodes a fixture, and runs real inference:

```sh
cd target
PYTHONPATH=../examples/object_detection_service \
  KEEPPEEK_RUN_ULTRALYTICS=1 \
  python3.12 -m pytest -q \
  ../examples/object_detection_service/tests/test_detection_pipeline.py \
  -k real_ultralytics
cd ..
```

Then run the complete integration from the repository root:

```sh
cargo build --locked -p keeppeek -p test-camera --bin keeppeek --bin test_camera
cd examples/object_detection_service
KEEPPEEK_RUN_OBJECT_DETECTION_E2E=1 \
  KEEPPEEK_E2E_MODEL="$PWD/../../target/yolo11n.pt" \
  python -m pytest -q tests/test_e2e.py
```

It uses a fixed test-only UUID and loopback processes; no user secret or physical camera is read.
The test requires a real `person` or normalized `vehicle` detection, verifies the persisted source,
stream, timestamp, confidence, and bounding box, and confirms camera ingress stays healthy after
the example stops. CI performs the same flow with pinned hashes for both the Intel video and model.

## Troubleshooting

**`ffmpeg` was not found or `ffmpeg -version` failed:** install it using the platform command above,
open a new shell, and verify `ffmpeg -version`. The service does not fall back to a Python decoder.

**Unable to connect or HTTP 401/403:** verify `KEEPPEEK_URL`, confirm KeepPeek is running, and
replace the environment/file access key without printing it. Access keys, SDP bodies, input frames,
and protobuf attachment bytes are never logged by this example.

**Missing capability:** update and restart KeepPeek. The service requires
`keeppeek.event-publication.v1`; it does not run the model against an incompatible server.

**Source is unknown or offline:** use the stable source ID shown in KeepPeek, then wait for that
camera's source session and video variants to become active.

**Subscription rejected:** confirm the requested `main` or `sub` variant exists and is H.264 or
H.265. Use `--stream auto` to select KeepPeek's low variant.

**FFmpeg rejects a camera keyframe:** verify the camera codec and keyframe interval. A malformed or
incomplete AVCC/HVCC access unit is discarded with a concise error; stale delta frames are not
queued for later decoding.

**Model load fails:** KeepPeek media remains independent and continues ingesting, recording, live
view, and playback. Check the Ultralytics package/model compatibility and upstream weight license,
then restart this example.

## Licensing

The handwritten example is `AGPL-3.0-only`; see [LICENSE](LICENSE) and the repository root
[LICENSE](../../LICENSE). The public API definitions under [api](../../api/README.md) are separately
MIT-licensed. Generated protobuf bindings derived solely from that API remain MIT and do not make
an independent API client subject to the example's AGPL license.
