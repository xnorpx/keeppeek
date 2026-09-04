# SPDX-License-Identifier: AGPL-3.0-only

import base64
import hashlib
import json
import os
import platform
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

from conformance_client import (
    BENCHMARK_RUNS,
    COMMIT_LATENCY_P95_BUDGET_MS,
    DIAGNOSTIC_ACCESS_KEY_SENTINEL,
    DIAGNOSTIC_ATTACHMENT_SENTINEL,
    DIAGNOSTIC_PAYLOAD_SENTINEL,
    FANOUT_LATENCY_P95_BUDGET_MS,
    MEMORY_SAMPLE_INTERVAL_SECONDS,
    MEMORY_SAMPLES_MAXIMUM,
    MEMORY_SAMPLES_MINIMUM,
    PROCESS_MEMORY_DELTA_P95_BUDGET_BYTES,
    QUEUE_DEPTH_BUDGET,
    QUEUE_PENDING_BYTES_BUDGET,
)

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
    keeppeek_binary = Path(
        os.environ.get(
            "KEEPPEEK_CONFORMANCE_KEEPPEEK_BIN",
            REPOSITORY_ROOT / "target" / "debug" / f"keeppeek{executable_suffix}",
        )
    )
    camera_binary = Path(
        os.environ.get(
            "KEEPPEEK_CONFORMANCE_CAMERA_BIN",
            REPOSITORY_ROOT / "target" / "debug" / f"test_camera{executable_suffix}",
        )
    )
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
        server_environment["KEEPPEEK_SECRET_KEEPPEEK_ACCESS_KEY"] = DIAGNOSTIC_ACCESS_KEY_SENTINEL
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
        client_environment["KEEPPEEK_ACCESS_KEY"] = DIAGNOSTIC_ACCESS_KEY_SENTINEL
        command = [
            sys.executable,
            str(EXAMPLE_ROOT / "conformance_client.py"),
            "--url",
            f"http://127.0.0.1:{port}",
            "--source-id",
            CONFORMANCE_SOURCE_IDS[0],
            "--source-id",
            CONFORMANCE_SOURCE_IDS[1],
        ]
        resident_memory_baseline = int(
            required_metric(metrics_url, "keeppeek_process_resident_memory_bytes")
        )
        completed, resident_memory_samples = run_monitored_client(
            command,
            EXAMPLE_ROOT,
            client_environment,
            metrics_url,
            300,
        )
        assert completed.returncode == 0, completed.stderr
        summary = json.loads(completed.stdout)
        assert summary["source_id"] == CONFORMANCE_SOURCE_IDS[0]
        assert summary["revision"] == BENCHMARK_RUNS
        assert summary["live_event_id"] == summary["event_id"]
        assert summary["live_revision"] == summary["revision"]
        assert summary["live_source_id"] == summary["source_id"]
        assert summary["live_stream_id"] == summary["stream_id"]
        assert summary["live_attachment_bytes"] == summary["attachment_bytes"]
        assert summary["live_attachment_sha256"] == summary["attachment_sha256"]
        assert summary["evidence_width"] == 3840
        assert summary["evidence_height"] == 2160
        assert [(item["width"], item["height"]) for item in summary["low_streams"]] == [
            (640, 360),
            (640, 360),
        ]
        assert {item["codec"] for item in summary["low_streams"]} == {"h264", "h265"}
        performance = summary["performance"]
        for phase in ("baseline", "commit", "fanout"):
            assert performance[phase]["samples"] == BENCHMARK_RUNS
            assert 0 <= performance[phase]["p50_ms"] <= performance[phase]["p95_ms"]
            assert performance[phase]["p95_ms"] <= performance[phase]["max_ms"]
        assert performance["commit"]["p95_ms"] <= COMMIT_LATENCY_P95_BUDGET_MS
        assert performance["fanout"]["p95_ms"] <= FANOUT_LATENCY_P95_BUDGET_MS
        assert performance["commit_p95_budget_ms"] == COMMIT_LATENCY_P95_BUDGET_MS
        assert performance["fanout_p95_budget_ms"] == FANOUT_LATENCY_P95_BUDGET_MS
        assert len(resident_memory_samples) >= MEMORY_SAMPLES_MINIMUM
        resident_memory_p50 = nearest_rank_value(resident_memory_samples, 50)
        resident_memory_p95 = nearest_rank_value(resident_memory_samples, 95)
        resident_memory_delta_p95 = max(0, resident_memory_p95 - resident_memory_baseline)
        assert resident_memory_delta_p95 <= PROCESS_MEMORY_DELTA_P95_BUDGET_BYTES
        performance["resident_memory"] = {
            "samples": len(resident_memory_samples),
            "baseline_bytes": resident_memory_baseline,
            "p50_bytes": resident_memory_p50,
            "p95_bytes": resident_memory_p95,
            "max_bytes": max(resident_memory_samples),
            "p95_delta_bytes": resident_memory_delta_p95,
            "p95_delta_budget_bytes": PROCESS_MEMORY_DELTA_P95_BUDGET_BYTES,
        }

        withheld, withheld_ready = start_withheld_client(
            port,
            tmp_path / "withheld-client.log",
            processes,
            handles,
        )
        assert withheld_ready["codecs"] == ["h264", "h265"]
        recording_bytes_before = wait_for(
            lambda: current_recording_bytes(storage_path) or None,
            timeout=15,
            description="recording bytes before withholding the client",
            failed_process=server,
            log_path=server_log_path,
        )
        assert required_metric(metrics_url, "keeppeek_external_analysis_sessions_active") >= 2
        assert (
            required_metric(metrics_url, "keeppeek_external_analysis_media_subscriptions_active")
            >= 2
        )
        assert (
            required_metric(
                metrics_url, "keeppeek_external_analysis_event_publication_commits_total"
            )
            == BENCHMARK_RUNS
        )
        assert (
            required_metric(metrics_url, "keeppeek_external_analysis_event_deliveries_queued_total")
            >= BENCHMARK_RUNS
        )
        server_commit_p50_ms = required_metric(
            metrics_url,
            "keeppeek_external_analysis_event_publication_commit_latency_milliseconds",
            '{quantile="p50"}',
        )
        server_commit_p95_ms = required_metric(
            metrics_url,
            "keeppeek_external_analysis_event_publication_commit_latency_milliseconds",
            '{quantile="p95"}',
        )
        assert server_commit_p50_ms <= server_commit_p95_ms
        assert server_commit_p95_ms <= COMMIT_LATENCY_P95_BUDGET_MS
        queue_depth_high_water = required_metric(
            metrics_url, "keeppeek_external_analysis_event_delivery_queue_depth_high_water"
        )
        queue_pending_bytes_high_water = required_metric(
            metrics_url,
            "keeppeek_external_analysis_event_delivery_pending_bytes_high_water",
        )
        assert queue_depth_high_water <= QUEUE_DEPTH_BUDGET
        assert queue_pending_bytes_high_water <= QUEUE_PENDING_BYTES_BUDGET
        assert performance["queue_depth_budget"] == QUEUE_DEPTH_BUDGET
        assert performance["queue_pending_bytes_budget"] == QUEUE_PENDING_BYTES_BUDGET
        performance["server_commit"] = {
            "samples": BENCHMARK_RUNS,
            "p50_ms": server_commit_p50_ms,
            "p95_ms": server_commit_p95_ms,
            "p95_budget_ms": COMMIT_LATENCY_P95_BUDGET_MS,
        }
        performance["queue"] = {
            "depth_high_water": queue_depth_high_water,
            "depth_budget": QUEUE_DEPTH_BUDGET,
            "pending_bytes_high_water": queue_pending_bytes_high_water,
            "pending_bytes_budget": QUEUE_PENDING_BYTES_BUDGET,
        }
        summary["environment"] = {
            "platform": platform.platform(),
            "python": platform.python_version(),
        }
        (tmp_path / "external-analysis-report.json").write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

        browser_environment = os.environ.copy()
        browser_environment.update(
            {
                "KEEPPEEK_CONFORMANCE_BACKEND_URL": f"http://127.0.0.1:{port}",
                "KEEPPEEK_CONFORMANCE_EVENT_ID": str(summary["event_id"]),
                "KEEPPEEK_CONFORMANCE_EVENT_DATE": str(summary["event_date"]),
                "KEEPPEEK_CONFORMANCE_EVENT_REVISION": str(summary["revision"]),
                "KEEPPEEK_CONFORMANCE_EVENT_TIMESTAMP": str(summary["event_timestamp"]),
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
        recording_bytes_after = wait_for(
            lambda: (
                current
                if (current := current_recording_bytes(storage_path)) > recording_bytes_before
                else None
            ),
            timeout=15,
            description="recording progress while withholding the client",
            failed_process=server,
            log_path=server_log_path,
        )
        assert recording_bytes_after > recording_bytes_before
        crash_process(withheld)
        processes.remove(withheld)
        assert server.poll() is None
        for source_id in CONFORMANCE_SOURCE_IDS:
            assert camera_ingress_metrics(metrics_url, source_id)

        reconnected, reconnect_ready = start_withheld_client(
            port,
            tmp_path / "reconnected-client.log",
            processes,
            handles,
        )
        assert reconnect_ready["codecs"] == ["h264", "h265"]
        crash_process(reconnected)
        processes.remove(reconnected)
        assert server.poll() is None
        for handle in handles:
            handle.flush()
        assert_diagnostic_hygiene(
            tmp_path,
            fetch_metrics(metrics_url),
            completed.stdout,
            completed.stderr,
            browser.stdout,
            browser.stderr,
            source_frame_probe(h264_sub),
        )

        stop_process(server)
        processes.remove(server)
        server = None
        event = published_event_with_attachment(catalog_path, str(summary["event_id"]))
        assert event is not None
        event_id, source_id, stream_id, revision, filename, attachments_json = event
        assert event_id == summary["event_id"]
        assert source_id == CONFORMANCE_SOURCE_IDS[0]
        assert stream_id == summary["stream_id"] == "main"
        assert revision == summary["revision"]
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


def start_withheld_client(
    port: int,
    log_path: Path,
    processes: list[subprocess.Popen[str]],
    handles: list[IO[str]],
) -> tuple[subprocess.Popen[str], dict[str, object]]:
    log = log_path.open("w", encoding="utf-8")
    handles.append(log)
    environment = os.environ.copy()
    environment["KEEPPEEK_ACCESS_KEY"] = DIAGNOSTIC_ACCESS_KEY_SENTINEL
    client = subprocess.Popen(
        [
            sys.executable,
            str(EXAMPLE_ROOT / "conformance_client.py"),
            "--url",
            f"http://127.0.0.1:{port}",
            "--source-id",
            CONFORMANCE_SOURCE_IDS[0],
            "--source-id",
            CONFORMANCE_SOURCE_IDS[1],
            "--withhold-seconds",
            "60",
        ],
        cwd=EXAMPLE_ROOT,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=log,
        text=True,
    )
    processes.append(client)
    ready = json.loads(read_process_line(client, 30, "withheld client readiness", log_path))
    if not isinstance(ready, dict) or ready.get("status") != "withheld":
        raise AssertionError(f"invalid withheld client readiness: {ready!r}")
    return client, cast(dict[str, object], ready)


def read_process_line(
    process: subprocess.Popen[str], timeout: float, description: str, log_path: Path
) -> str:
    stdout = process.stdout
    if stdout is None:
        raise AssertionError(f"{description} stdout was not captured")
    output: queue.Queue[str] = queue.Queue(maxsize=1)
    threading.Thread(target=lambda: output.put(stdout.readline()), daemon=True).start()
    try:
        line = output.get(timeout=timeout)
    except queue.Empty as error:
        raise AssertionError(
            f"timed out waiting for {description}\n{read_log(log_path)}"
        ) from error
    if not line:
        exit_code = process.poll()
        log = read_log(log_path)
        raise AssertionError(
            f"process exited with {exit_code} while waiting for {description}\n{log}"
        )
    return line


def run_monitored_client(
    command: list[str],
    working_directory: Path,
    environment: dict[str, str],
    metrics_url: str,
    timeout: float,
) -> tuple[subprocess.CompletedProcess[str], list[int]]:
    process = subprocess.Popen(
        command,
        cwd=working_directory,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    samples: list[int] = []
    deadline = time.monotonic() + timeout
    while process.poll() is None or len(samples) < MEMORY_SAMPLES_MINIMUM:
        if time.monotonic() >= deadline:
            process.kill()
            stdout, stderr = process.communicate(timeout=10)
            raise AssertionError(f"conformance client timed out\n{stdout}\n{stderr}")
        if len(samples) < MEMORY_SAMPLES_MAXIMUM:
            try:
                samples.append(
                    int(required_metric(metrics_url, "keeppeek_process_resident_memory_bytes"))
                )
            except (OSError, urllib.error.URLError):
                if process.poll() is None:
                    raise
                break
        time.sleep(MEMORY_SAMPLE_INTERVAL_SECONDS)
    stdout, stderr = process.communicate(timeout=10)
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr), samples


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


def required_metric(url: str, name: str, labels: str = "") -> float:
    metrics = fetch_metrics(url)
    prefix = f"{name}{labels} "
    for line in metrics.splitlines():
        if line.startswith(prefix):
            return metric_value(line)
    raise AssertionError(f"missing Prometheus metric {name}")


def nearest_rank_value(samples: list[int], percentile: int) -> int:
    if not samples or percentile < 1 or percentile > 100:
        raise AssertionError("nearest-rank samples and percentile must be valid")
    ordered = sorted(samples)
    rank = (len(ordered) * percentile + 99) // 100
    return ordered[rank - 1]


def fetch_metrics(url: str) -> str:
    with urllib.request.urlopen(url, timeout=2) as response:
        return cast(str, response.read().decode("utf-8"))


def source_frame_probe(video_path: Path) -> bytes:
    payload = video_path.read_bytes()
    start = len(payload) // 2
    probe = payload[start : start + 24]
    if len(probe) != 24 or len(set(probe)) < 8:
        raise AssertionError("source frame diagnostic probe lacks entropy")
    return probe


def assert_diagnostic_hygiene(
    tmp_path: Path,
    metrics: str,
    *diagnostics: str | bytes,
) -> None:
    generated = [
        EXAMPLE_ROOT / "generated" / "webrtc_pb2.py",
        EXAMPLE_ROOT / "generated" / "webrtc_pb2.pyi",
    ]
    diagnostic_bytes = b"\n".join(
        [path.read_bytes() for path in sorted(tmp_path.glob("*.log"))]
        + [metrics.encode()]
        + [value.encode() if isinstance(value, str) else value for value in diagnostics[:-1]]
    )
    frame_probe = diagnostics[-1]
    if not isinstance(frame_probe, bytes):
        raise AssertionError("source frame diagnostic probe must contain bytes")
    binding_bytes = b"\n".join(path.read_bytes() for path in generated)
    probes = {
        "access key": DIAGNOSTIC_ACCESS_KEY_SENTINEL.encode(),
        "structured payload": DIAGNOSTIC_PAYLOAD_SENTINEL.encode(),
        "attachment payload": DIAGNOSTIC_ATTACHMENT_SENTINEL,
        "source frame": frame_probe,
    }
    for name, probe in probes.items():
        variants = (
            probe,
            probe.hex().encode(),
            base64.b64encode(probe),
            b" ".join(f"{byte:02x}".encode() for byte in probe),
            str(list(probe)).encode(),
        )
        if any(variant in diagnostic_bytes for variant in variants):
            raise AssertionError(f"runtime diagnostics exposed the {name} probe")
        if any(variant in binding_bytes for variant in variants):
            raise AssertionError(f"generated bindings exposed the {name} probe")
    lowered = diagnostic_bytes.lower()
    for marker in (b"a=ice-pwd:", b"a=ice-ufrag:"):
        if marker in lowered:
            raise AssertionError("runtime diagnostics exposed full SDP credentials")


def current_recording_bytes(storage_path: Path) -> int:
    return sum(
        file_path.stat().st_size
        for file_path in storage_path.rglob("*.mp4*")
        if file_path.is_file()
    )


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


def crash_process(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        raise AssertionError(f"client exited before forced crash with {process.returncode}")
    process.kill()
    process.wait(timeout=10)
