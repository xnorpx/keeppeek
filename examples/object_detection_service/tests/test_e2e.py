# SPDX-License-Identifier: AGPL-3.0-only

import hashlib
import json
import os
import queue
import signal
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from functools import partial
from pathlib import Path
from typing import IO, cast

import pytest

REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
EXAMPLE_ROOT = Path(__file__).resolve().parents[1]
SAMPLE_VIDEO_ROOT = REPOSITORY_ROOT / "data" / "sample-videos"
FIXTURE = SAMPLE_VIDEO_ROOT / "person-bicycle-car-detection.mp4"
MODEL = Path(os.environ.get("KEEPPEEK_E2E_MODEL", REPOSITORY_ROOT / "target" / "yolo11n.pt"))
TEST_ACCESS_KEY = "550e8400-e29b-41d4-a716-446655440000"
SOURCE_ID = "127.0.0.1"
CONFORMANCE_SOURCE_IDS = ("192.0.2.101", "192.0.2.102")
TEST_CAMERA_FIXTURES = REPOSITORY_ROOT / "crates" / "test-camera" / "testdata"


@pytest.mark.skipif(
    os.environ.get("KEEPPEEK_RUN_EXTERNAL_CONFORMANCE") != "1",
    reason="set KEEPPEEK_RUN_EXTERNAL_CONFORMANCE=1 after building conformance binaries",
)
def test_two_stream_no_model_client_publishes_high_quality_evidence(tmp_path: Path) -> None:
    executable_suffix = ".exe" if os.name == "nt" else ""
    keeppeek_binary = REPOSITORY_ROOT / "target" / "debug" / f"keeppeek{executable_suffix}"
    camera_binary = REPOSITORY_ROOT / "target" / "debug" / f"test_camera{executable_suffix}"
    h264_main = TEST_CAMERA_FIXTURES / "cc-4k-3840x2160-h264.mp4"
    h264_sub = TEST_CAMERA_FIXTURES / "cc-4k-640x360-h264.mp4"
    h265_sub = TEST_CAMERA_FIXTURES / "cc-4k-640x360-h265.mp4"
    for required_file in (keeppeek_binary, camera_binary, h264_main, h264_sub, h265_sub):
        assert required_file.is_file(), f"missing conformance input: {required_file}"

    processes: list[subprocess.Popen[str]] = []
    handles: list[IO[str]] = []
    server: subprocess.Popen[str] | None = None
    try:
        camera_configs: list[str] = []
        for index, (source_id, main, sub) in enumerate(
            (
                (CONFORMANCE_SOURCE_IDS[0], h264_main, h264_sub),
                (CONFORMANCE_SOURCE_IDS[1], h265_sub, h265_sub),
            )
        ):
            camera_log = (tmp_path / f"camera-{index}.log").open("w", encoding="utf-8")
            handles.append(camera_log)
            camera = subprocess.Popen(
                [
                    str(camera_binary),
                    "rtsp",
                    "--main",
                    str(main),
                    "--sub",
                    str(sub),
                    "--config-ip",
                    source_id,
                    "--name",
                    f"conformance-{index}",
                    "--start-at-seconds",
                    str(index),
                ],
                cwd=REPOSITORY_ROOT,
                stdout=subprocess.PIPE,
                stderr=camera_log,
                text=True,
            )
            processes.append(camera)
            camera_configs.append(read_camera_config(camera))

        port = unused_loopback_port()
        catalog_path = tmp_path / "recordings.db"
        storage_path = tmp_path / "recordings"
        thumbnail_path = tmp_path / "event-thumbnails"
        storage_path.mkdir()
        config_path = tmp_path / "config.toml"
        config_path.write_text(
            f"""host = "127.0.0.1"
port = {port}

[storage]
medium_term_path = {json.dumps(str(storage_path))}
long_term_path = {json.dumps(str(storage_path))}
recording_catalog_path = {json.dumps(str(catalog_path))}
event_thumbnail_path = {json.dumps(str(thumbnail_path))}
event_thumbnail_max_mb = 16
short_term_secs = 5
medium_term_secs = 60
flush_interval_secs = 1
write_buffer_bytes = 8192
long_term_max_gb = 0

{"".join(camera_configs)}
""",
            encoding="utf-8",
        )

        server_log_path = tmp_path / "server.log"
        server_log = server_log_path.open("w", encoding="utf-8")
        handles.append(server_log)
        server_environment = os.environ.copy()
        server_environment.pop("KEEPPEEK_ACCESS_KEY", None)
        server_environment.pop("KEEPPEEK_SECRET_KEEPPEEK_ACCESS_KEY", None)
        server_environment["RUST_LOG"] = "info,keeppeek=debug"
        server = subprocess.Popen(
            [str(keeppeek_binary), f"--config={config_path}"],
            cwd=REPOSITORY_ROOT,
            env=server_environment,
            stdout=server_log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        processes.append(server)
        metrics_url = f"http://127.0.0.1:{port}/metrics"
        for source_id in CONFORMANCE_SOURCE_IDS:
            wait_for(
                partial(camera_ingress_metrics, metrics_url, source_id),
                timeout=60,
                description=f"fixture camera ingress for {source_id}",
                failed_process=server,
                log_path=server_log_path,
            )

        client_environment = os.environ.copy()
        client_environment.pop("KEEPPEEK_ACCESS_KEY", None)
        completed = subprocess.run(
            [
                sys.executable,
                str(EXAMPLE_ROOT / "conformance_client.py"),
                "--url",
                f"http://127.0.0.1:{port}",
                "--source-id",
                CONFORMANCE_SOURCE_IDS[0],
                "--source-id",
                CONFORMANCE_SOURCE_IDS[1],
            ],
            cwd=EXAMPLE_ROOT,
            env=client_environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=90,
        )
        assert completed.returncode == 0, completed.stderr
        summary = json.loads(completed.stdout)
        assert summary["source_id"] == CONFORMANCE_SOURCE_IDS[0]
        assert summary["revision"] == 1
        assert summary["evidence_width"] == 3840
        assert summary["evidence_height"] == 2160
        assert [(item["width"], item["height"]) for item in summary["low_streams"]] == [
            (640, 360),
            (640, 360),
        ]
        assert {item["codec"] for item in summary["low_streams"]} == {"h264", "h265"}

        browser_environment = os.environ.copy()
        browser_environment.update(
            {
                "KEEPPEEK_CONFORMANCE_BACKEND_URL": f"http://127.0.0.1:{port}",
                "KEEPPEEK_CONFORMANCE_EVENT_ID": str(summary["event_id"]),
                "KEEPPEEK_CONFORMANCE_EVENT_DATE": str(summary["event_date"]),
                "KEEPPEEK_CONFORMANCE_SOURCE_ID": str(summary["source_id"]),
            }
        )
        browser = subprocess.run(
            [
                "bunx",
                "playwright",
                "test",
                "--config",
                "playwright.external-analysis.config.ts",
            ],
            cwd=REPOSITORY_ROOT / "ui",
            env=browser_environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=120,
        )
        assert browser.returncode == 0, f"{browser.stdout}\n{browser.stderr}"
        assert server.poll() is None
        for source_id in CONFORMANCE_SOURCE_IDS:
            assert camera_ingress_metrics(metrics_url, source_id)

        stop_process(server)
        processes.remove(server)
        server = None
        event = published_event_with_attachment(catalog_path, str(summary["event_id"]))
        assert event is not None
        event_id, source_id, stream_id, revision, filename, attachments_json = event
        assert event_id == summary["event_id"]
        assert source_id == CONFORMANCE_SOURCE_IDS[0]
        assert stream_id == summary["stream_id"] == "main"
        assert revision == 1
        attachments = json.loads(attachments_json)
        assert attachments[0]["id"] == summary["attachment_id"]
        jpeg = (thumbnail_path / filename).read_bytes()
        assert len(jpeg) == summary["attachment_bytes"]
        assert hashlib.sha256(jpeg).hexdigest() == summary["attachment_sha256"]
        assert jpeg.startswith(b"\xff\xd8")
        assert jpeg.endswith(b"\xff\xd9")
    finally:
        for process in reversed(processes):
            stop_process(process)
        for handle in handles:
            handle.close()


@pytest.mark.skipif(
    os.environ.get("KEEPPEEK_RUN_OBJECT_DETECTION_E2E") != "1",
    reason="set KEEPPEEK_RUN_OBJECT_DETECTION_E2E=1 after building E2E binaries",
)
def test_ultralytics_detection_reaches_local_keeppeek_catalog(tmp_path: Path) -> None:
    executable_suffix = ".exe" if os.name == "nt" else ""
    keeppeek_binary = Path(
        os.environ.get(
            "KEEPPEEK_E2E_KEEPPEEK_BIN",
            REPOSITORY_ROOT / "target" / "debug" / f"keeppeek{executable_suffix}",
        )
    )
    camera_binary = Path(
        os.environ.get(
            "KEEPPEEK_E2E_CAMERA_BIN",
            REPOSITORY_ROOT / "target" / "debug" / f"test_camera{executable_suffix}",
        )
    )
    for required_file in (
        keeppeek_binary,
        camera_binary,
        FIXTURE,
        MODEL,
        SAMPLE_VIDEO_ROOT / "LICENSE",
        SAMPLE_VIDEO_ROOT / "README.md",
    ):
        assert required_file.is_file(), f"missing E2E input: {required_file}"

    camera_log_path = tmp_path / "camera.log"
    server_log_path = tmp_path / "server.log"
    detector_log_path = tmp_path / "detector.log"
    processes: list[subprocess.Popen[str]] = []
    handles: list[IO[str]] = []
    detector: subprocess.Popen[str] | None = None
    server: subprocess.Popen[str] | None = None
    started_at_ms = int(time.time() * 1_000)
    try:
        camera_log = camera_log_path.open("w", encoding="utf-8")
        handles.append(camera_log)
        camera = subprocess.Popen(
            [
                str(camera_binary),
                "rtsp",
                "--main",
                str(FIXTURE),
                "--sub",
                str(FIXTURE),
                "--name",
                "object-detector-e2e",
                "--start-at-seconds",
                "0",
            ],
            cwd=REPOSITORY_ROOT,
            stdout=subprocess.PIPE,
            stderr=camera_log,
            text=True,
        )
        processes.append(camera)
        camera_config = read_camera_config(camera)

        port = unused_loopback_port()
        catalog_path = tmp_path / "recordings.db"
        storage_path = tmp_path / "recordings"
        storage_path.mkdir()
        config_path = tmp_path / "config.toml"
        config_path.write_text(
            f"""host = "127.0.0.1"
port = {port}

[storage]
medium_term_path = {json.dumps(str(storage_path))}
long_term_path = {json.dumps(str(storage_path))}
recording_catalog_path = {json.dumps(str(catalog_path))}
event_thumbnail_path = {json.dumps(str(tmp_path / "event-thumbnails"))}
event_thumbnail_max_mb = 16
short_term_secs = 5
medium_term_secs = 60
flush_interval_secs = 1
write_buffer_bytes = 8192
long_term_max_gb = 0

{camera_config}
""",
            encoding="utf-8",
        )

        server_log = server_log_path.open("w", encoding="utf-8")
        handles.append(server_log)
        server_environment = os.environ.copy()
        server_environment["KEEPPEEK_SECRET_KEEPPEEK_ACCESS_KEY"] = TEST_ACCESS_KEY
        server_environment["RUST_LOG"] = "info,keeppeek=debug"
        server = subprocess.Popen(
            [str(keeppeek_binary), f"--config={config_path}"],
            cwd=REPOSITORY_ROOT,
            env=server_environment,
            stdout=server_log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        processes.append(server)
        metrics_url = f"http://127.0.0.1:{port}/metrics"
        wait_for(
            lambda: camera_ingress_metrics(metrics_url),
            timeout=60,
            description="fixture camera ingress",
            failed_process=server,
            log_path=server_log_path,
        )

        detector_log = detector_log_path.open("w", encoding="utf-8")
        handles.append(detector_log)
        detector_environment = os.environ.copy()
        detector_environment["KEEPPEEK_ACCESS_KEY"] = TEST_ACCESS_KEY
        detector = subprocess.Popen(
            [
                sys.executable,
                str(EXAMPLE_ROOT / "object_detection_service.py"),
                "--url",
                f"http://127.0.0.1:{port}",
                "--source-id",
                SOURCE_ID,
                "--stream",
                "sub",
                "--detector",
                "ultralytics",
                "--model",
                str(MODEL),
                "--confidence",
                "0.25",
                "--inference-fps",
                "15",
                "--cooldown-seconds",
                "1",
            ],
            cwd=EXAMPLE_ROOT,
            env=detector_environment,
            stdout=detector_log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        processes.append(detector)
        wait_for(
            lambda: detector_published(detector_log_path),
            timeout=120,
            description="acknowledged Ultralytics detection",
            failed_process=detector,
            log_path=detector_log_path,
        )

        stop_process(detector)
        processes.remove(detector)
        detector = None
        assert server.poll() is None
        wait_for(
            lambda: camera_ingress_metrics(metrics_url),
            timeout=15,
            description="camera ingress after detector shutdown",
            failed_process=server,
            log_path=server_log_path,
        )
        stop_process(server)
        processes.remove(server)
        server = None

        # Turso's live WAL is not readable by Python's sqlite3. A graceful server stop
        # checkpoints it, which is also the durable state a restarted KeepPeek observes.
        event = published_event(catalog_path)
        assert event is not None
        event_id, camera_id, stream_id, source, kind, timestamp_ms, confidence, bbox_json = event
        assert event_id
        assert camera_id == SOURCE_ID
        assert stream_id == "sub"
        assert source == "keeppeek"
        assert kind in {"person", "vehicle"}
        assert started_at_ms <= timestamp_ms <= int(time.time() * 1_000)
        assert 0.25 <= confidence <= 1.0
        x, y, width, height = json.loads(bbox_json)
        assert all(0.0 <= value <= 1.0 for value in (x, y, width, height))
        assert width > 0.0
        assert height > 0.0
        assert x + width <= 1.0
        assert y + height <= 1.0
    finally:
        for process in reversed(processes):
            stop_process(process)
        for handle in handles:
            handle.close()


def read_camera_config(camera: subprocess.Popen[str]) -> str:
    stdout = camera.stdout
    if stdout is None:
        raise AssertionError("test camera stdout was not captured")
    output: queue.Queue[str | None] = queue.Queue()

    def read_lines() -> None:
        for line in stdout:
            output.put(line)
        output.put(None)

    threading.Thread(target=read_lines, daemon=True).start()
    lines: list[str] = []
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try:
            line = output.get(timeout=max(deadline - time.monotonic(), 0.01))
        except queue.Empty:
            break
        if line is None:
            raise AssertionError(f"test camera exited before configuration: {camera.returncode}")
        lines.append(line)
        if line == 'transport = "tcp"\n':
            return "".join(lines)
    raise AssertionError("timed out waiting for test camera configuration")


def unused_loopback_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def camera_ingress_metrics(url: str, source_id: str = SOURCE_ID) -> bool:
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            metrics = response.read().decode("utf-8")
    except (OSError, urllib.error.URLError):
        return False
    return camera_ingress_metrics_text(metrics, source_id)


def camera_ingress_metrics_text(metrics: str, source_id: str = SOURCE_ID) -> bool:
    transport_connected = False
    transport_known = False
    ingress = False
    for line in metrics.splitlines():
        if f'camera_id="{source_id}"' not in line:
            continue
        if 'dimension="transport_connected"' in line:
            if line.startswith("keeppeek_camera_health_dimension{"):
                transport_connected = metric_value(line) == 1
            if line.startswith("keeppeek_camera_health_dimension_known{"):
                transport_known = metric_value(line) == 1
        if (
            line.startswith("keeppeek_camera_ingress_frames_per_second{")
            and 'stream="video_sub"' in line
            and metric_value(line) > 0
        ):
            ingress = True
    return transport_connected and transport_known and ingress


@pytest.mark.parametrize(
    ("metrics", "expected"),
    [
        (
            'keeppeek_camera_health_dimension{camera_id="127.0.0.1",'
            'dimension="transport_connected"} 1\n'
            'keeppeek_camera_health_dimension_known{camera_id="127.0.0.1",'
            'dimension="transport_connected"} 1\n'
            'keeppeek_camera_ingress_frames_per_second{camera_id="127.0.0.1",'
            'stream="video_sub"} 12.0\n',
            True,
        ),
        (
            'keeppeek_camera_health_dimension{camera_id="127.0.0.1",'
            'dimension="transport_connected"} 1\n'
            'keeppeek_camera_health_dimension_known{camera_id="127.0.0.1",'
            'dimension="transport_connected"} 0\n'
            'keeppeek_camera_ingress_frames_per_second{camera_id="127.0.0.1",'
            'stream="video_sub"} 12.0\n',
            False,
        ),
        (
            'keeppeek_camera_online{camera_id="127.0.0.1"} 1\n'
            'keeppeek_camera_ingress_frames_per_second{camera_id="127.0.0.1",'
            'stream="video_sub"} 12.0\n',
            False,
        ),
    ],
)
def test_camera_ingress_metrics_require_canonical_transport_evidence(
    metrics: str, expected: bool
) -> None:
    assert camera_ingress_metrics_text(metrics) is expected


def metric_value(line: str) -> float:
    try:
        return float(line.rsplit(" ", 1)[1])
    except (IndexError, ValueError):
        return 0.0


def detector_published(log_path: Path) -> bool:
    log = read_log(log_path)
    return "loading Ultralytics model" in log and "published detection event_id=" in log


def published_event(catalog_path: Path) -> tuple[str, str, str, str, str, int, float, str] | None:
    if not catalog_path.is_file():
        return None
    try:
        connection = sqlite3.connect(f"file:{catalog_path}?mode=ro", uri=True, timeout=1)
        try:
            row = connection.execute(
                "SELECT id, camera_id, stream, source, kind, start_time_ms, confidence, bbox_json "
                "FROM recording_events WHERE source = 'keeppeek' ORDER BY start_time_ms LIMIT 1"
            ).fetchone()
        finally:
            connection.close()
    except sqlite3.Error:
        return None
    if row is None:
        return None
    event_id, camera_id, stream_id, source, kind, timestamp_ms, confidence, bbox_json = row
    if not (
        isinstance(event_id, str)
        and isinstance(camera_id, str)
        and isinstance(stream_id, str)
        and isinstance(source, str)
        and isinstance(kind, str)
        and isinstance(timestamp_ms, int)
        and isinstance(confidence, float)
        and isinstance(bbox_json, str)
    ):
        raise AssertionError("published event row has unexpected SQLite types")
    return event_id, camera_id, stream_id, source, kind, timestamp_ms, confidence, bbox_json


def published_event_with_attachment(
    catalog_path: Path, event_id: str
) -> tuple[str, str, str, int, str, str] | None:
    connection = sqlite3.connect(f"file:{catalog_path}?mode=ro", uri=True, timeout=1)
    try:
        row = connection.execute(
            "SELECT id, camera_id, stream, revision, thumbnail_filename, attachments_json "
            "FROM recording_events WHERE id = ?1",
            (event_id,),
        ).fetchone()
    finally:
        connection.close()
    if row is None:
        return None
    values = cast(tuple[object, object, object, object, object, object], row)
    stored_id, source_id, stream_id, revision, filename, attachments_json = values
    if not (
        isinstance(stored_id, str)
        and isinstance(source_id, str)
        and isinstance(stream_id, str)
        and isinstance(revision, int)
        and isinstance(filename, str)
        and isinstance(attachments_json, str)
    ):
        raise AssertionError("published attachment row has unexpected SQLite types")
    return stored_id, source_id, stream_id, revision, filename, attachments_json


def wait_for[T](
    probe: Callable[[], T | None],
    *,
    timeout: float,
    description: str,
    failed_process: subprocess.Popen[str],
    log_path: Path,
) -> T:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        exit_code = failed_process.poll()
        if exit_code is not None:
            message = f"process exited with {exit_code} while waiting for {description}"
            raise AssertionError(f"{message}\n{read_log(log_path)}")
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {description}\n{read_log(log_path)}")


def read_log(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")[-8_000:]
    except OSError:
        return "log unavailable"


def stop_process(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.terminate()
    else:
        process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)
