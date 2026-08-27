# SPDX-License-Identifier: AGPL-3.0-only

import os
import stat
import uuid
from pathlib import Path

import pytest

from detection_pipeline import FFMPEG_INSTALL_HELP, ServiceError, verify_ffmpeg
from object_detection_service import ServiceConfig, load_access_key, parse_args

ACCESS_KEY = "550e8400-e29b-41d4-a716-446655440000"


def config(access_key_file: Path | None = None) -> ServiceConfig:
    return ServiceConfig(
        url="http://127.0.0.1:8081",
        source_id="camera-1",
        stream="sub",
        detector="fake",
        model="unused.pt",
        fake_object_class="person",
        confidence=0.5,
        cooldown_seconds=5,
        inference_fps=1,
        reconnect_delay_seconds=1,
        access_key_file=access_key_file,
    )


def test_access_key_environment_takes_precedence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    key_file = tmp_path / "access-key"
    key_file.write_text(str(uuid.uuid4()), encoding="utf-8")
    monkeypatch.setenv("KEEPPEEK_ACCESS_KEY", ACCESS_KEY)

    assert load_access_key(config(key_file)) == ACCESS_KEY


def test_owner_only_access_key_file_is_accepted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KEEPPEEK_ACCESS_KEY", raising=False)
    key_file = tmp_path / "access-key"
    key_file.write_text(f"{ACCESS_KEY}\n", encoding="utf-8")
    if os.name != "nt":
        key_file.chmod(stat.S_IRUSR | stat.S_IWUSR)

    assert load_access_key(config(key_file)) == ACCESS_KEY


@pytest.mark.skipif(os.name == "nt", reason="POSIX permission bits do not model Windows ACLs")
def test_non_owner_only_access_key_file_is_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KEEPPEEK_ACCESS_KEY", raising=False)
    key_file = tmp_path / "access-key"
    key_file.write_text(ACCESS_KEY, encoding="utf-8")
    key_file.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IRGRP)

    with pytest.raises(ServiceError, match="group or other"):
        load_access_key(config(key_file))


def test_ffmpeg_failure_is_actionable_on_every_supported_platform(tmp_path: Path) -> None:
    with pytest.raises(ServiceError) as failure:
        verify_ffmpeg(str(tmp_path / "missing-ffmpeg"))

    message = str(failure.value)
    assert message.endswith(FFMPEG_INSTALL_HELP)
    assert "brew install ffmpeg" in message
    assert "winget install Gyan.FFmpeg" in message
    assert "choco install ffmpeg" in message
    assert "apt install ffmpeg" in message


def test_parse_args_accepts_documented_non_secret_configuration() -> None:
    parsed = parse_args(
        [
            "--url",
            "http://keeppeek.example:8081",
            "--source-id",
            "front-door",
            "--stream",
            "sub",
            "--detector",
            "fake",
            "--confidence",
            "0.7",
            "--cooldown-seconds",
            "3",
            "--inference-fps",
            "2",
        ]
    )

    assert parsed.url == "http://keeppeek.example:8081"
    assert parsed.source_id == "front-door"
    assert parsed.stream == "sub"
    assert parsed.detector == "fake"
    assert parsed.confidence == 0.7
    assert parsed.cooldown_seconds == 3
    assert parsed.inference_fps == 2
