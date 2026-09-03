# SPDX-License-Identifier: AGPL-3.0-only

import asyncio
import math
import os
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

import numpy as np
import pytest
from google.protobuf import timestamp_pb2

from detection_pipeline import (
    MAX_JPEG_EVIDENCE_BYTES,
    BoundedLatestQueue,
    DecodedFrame,
    DecodeError,
    Detection,
    DetectionPipeline,
    FrameAssembler,
    UltralyticsDetector,
    avcc_to_annex_b,
    encode_jpeg_evidence,
    fake_detection,
    normalize_bounding_box,
    normalize_object_class,
    parse_ppm,
    read_stream_limited,
    select_detections,
    verify_ffmpeg,
)
from generated import webrtc_pb2 as pb

REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
FIXTURE_DIRECTORY = REPOSITORY_ROOT / "crates" / "test-camera" / "testdata"


def decode_fixture_frame(ffmpeg: Path, fixture: Path) -> np.ndarray:
    completed = subprocess.run(
        [
            str(ffmpeg),
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            str(fixture),
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "ppm",
            "pipe:1",
        ],
        check=False,
        capture_output=True,
        timeout=30,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace")[:300].strip()
        raise DecodeError(f"ffmpeg rejected {fixture.name}: {detail or 'unknown error'}")
    return parse_ppm(completed.stdout)


def test_bounded_latest_queue_evicts_oldest_by_count_and_bytes() -> None:
    async def scenario() -> None:
        queue = BoundedLatestQueue[bytes](2, 5, len)
        assert queue.put_latest(b"aa")
        assert queue.put_latest(b"bb")
        assert queue.put_latest(b"ccc")
        assert queue.count == 2
        assert queue.byte_size == 5
        assert queue.dropped == 1
        assert queue.put_latest(b"dddd")
        assert queue.count == 1
        assert queue.byte_size == 4
        assert queue.dropped == 3
        assert await queue.get() == b"dddd"
        assert queue.byte_size == 0
        assert not queue.put_latest(b"123456")
        assert queue.dropped == 4

    asyncio.run(scenario())


def test_frame_assembler_reassembles_ordered_fragments() -> None:
    assembler = FrameAssembler(max_frame_bytes=32)
    timestamp = timestamp_pb2.Timestamp(seconds=1_700_000_000, nanos=123_000_000)
    first = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=7,
        timestamp=timestamp,
        fragment_index=0,
        fragment_count=2,
        key_frame=True,
        payload=b"abc",
        configuration_revision=1,
    )
    second = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=7,
        timestamp=timestamp,
        fragment_index=1,
        fragment_count=2,
        key_frame=True,
        payload=b"def",
        configuration_revision=1,
    )

    assert assembler.push(first, "h264", 3) is None
    assembled = assembler.push(second, "h264", 3)

    assert assembled is not None
    assert assembled.payload == b"abcdef"
    assert assembled.key_frame
    assert assembled.generation == 3
    assert assembled.timestamp == datetime.fromtimestamp(1_700_000_000.123, tz=UTC)


def test_frame_assembler_discards_duplicate_and_oversized_work() -> None:
    assembler = FrameAssembler(max_frame_bytes=3)
    timestamp = timestamp_pb2.Timestamp(seconds=1)
    fragment = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=1,
        timestamp=timestamp,
        fragment_index=0,
        fragment_count=2,
        key_frame=True,
        payload=b"ab",
        configuration_revision=1,
    )

    assert assembler.push(fragment, "h264", 1) is None
    assert assembler.push(fragment, "h264", 1) is None
    assert assembler.discarded == 1
    oversized = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=2,
        timestamp=timestamp,
        fragment_index=0,
        fragment_count=1,
        key_frame=True,
        payload=b"four",
        configuration_revision=1,
    )
    assert assembler.push(oversized, "h264", 1) is None
    assert assembler.discarded == 2


def test_frame_assembler_never_mixes_reconnect_generations() -> None:
    assembler = FrameAssembler(max_frame_bytes=32)
    timestamp = timestamp_pb2.Timestamp(seconds=1)
    first = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=1,
        timestamp=timestamp,
        fragment_index=0,
        fragment_count=2,
        key_frame=True,
        payload=b"old",
        configuration_revision=1,
    )
    second = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=1,
        timestamp=timestamp,
        fragment_index=1,
        fragment_count=2,
        key_frame=True,
        payload=b"new",
        configuration_revision=1,
    )

    assert assembler.push(first, "h264", 1) is None
    assert assembler.push(second, "h264", 2) is None
    assert assembler.discarded == 1


def test_frame_assembler_discards_invalid_timestamp() -> None:
    assembler = FrameAssembler(max_frame_bytes=32)
    frame = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=1,
        timestamp=timestamp_pb2.Timestamp(seconds=2**63 - 1),
        fragment_index=0,
        fragment_count=1,
        key_frame=True,
        payload=b"frame",
        configuration_revision=1,
    )

    assert assembler.push(frame, "h264", 1) is None
    assert assembler.discarded == 1


def test_avcc_to_annex_b_validates_nal_lengths() -> None:
    assert avcc_to_annex_b(b"\x00\x00\x00\x02ab\x00\x00\x00\x01c") == (
        b"\x00\x00\x00\x01ab\x00\x00\x00\x01c"
    )
    with pytest.raises(DecodeError, match="NAL length"):
        avcc_to_annex_b(b"\x00\x00\x00\x04ab")


@pytest.mark.parametrize("codec", ["h264", "h265"])
def test_external_ffmpeg_decodes_codec_fixture(codec: str) -> None:
    ffmpeg = verify_ffmpeg()
    image = decode_fixture_frame(ffmpeg, FIXTURE_DIRECTORY / f"cc-4k-640x360-{codec}.mp4")

    assert image.shape == (360, 640, 3)
    assert image.dtype == np.uint8
    assert int(image.max()) > int(image.min())


def test_fake_detector_runs_against_decoded_fixture() -> None:
    ffmpeg = verify_ffmpeg()
    image = decode_fixture_frame(ffmpeg, FIXTURE_DIRECTORY / "cc-4k-640x360-h264.mp4")

    detections = fake_detection(image, "person")

    assert detections == [
        Detection(
            object_class="person",
            confidence=0.9,
            bounding_box=(0.2, 0.2, 0.5, 0.6),
        )
    ]


def test_high_quality_frame_encodes_one_bounded_timestamped_jpeg() -> None:
    ffmpeg = verify_ffmpeg()
    image = decode_fixture_frame(ffmpeg, FIXTURE_DIRECTORY / "cc-4k-3840x2160-h264.mp4")
    timestamp = datetime(2026, 9, 2, 12, 0, 0, 123_000, tzinfo=UTC)

    evidence = asyncio.run(encode_jpeg_evidence(ffmpeg, DecodedFrame(timestamp, image, 7)))

    assert evidence.timestamp == timestamp
    assert evidence.width == 3840
    assert evidence.height == 2160
    assert 4 <= len(evidence.payload) <= MAX_JPEG_EVIDENCE_BYTES
    assert evidence.payload.startswith(b"\xff\xd8")
    assert evidence.payload.endswith(b"\xff\xd9")


@pytest.mark.skipif(
    os.environ.get("KEEPPEEK_RUN_ULTRALYTICS") != "1",
    reason="set KEEPPEEK_RUN_ULTRALYTICS=1 to permit model-weight download",
)
def test_real_ultralytics_model_runs_against_decoded_fixture() -> None:
    image = decode_fixture_frame(verify_ffmpeg(), FIXTURE_DIRECTORY / "cc-4k-640x360-h264.mp4")
    detector = UltralyticsDetector(os.environ.get("KEEPPEEK_MODEL", "yolo11n.pt"))

    assert isinstance(detector.detect(image), list)


def test_fake_detector_finds_a_person_in_one_silly_black_pixel() -> None:
    image = np.zeros((1, 1, 3), dtype=np.uint8)

    assert fake_detection(image) == [Detection("person", 0.9, (0.2, 0.2, 0.5, 0.6))]


def test_fake_detector_can_call_the_same_blob_a_vehicle() -> None:
    image = np.full((2, 2, 3), 255, dtype=np.uint8)

    assert fake_detection(image, "vehicle")[0].object_class == "vehicle"


def test_fake_detector_sees_nothing_in_no_pixels() -> None:
    image = np.empty((0, 0, 3), dtype=np.uint8)

    assert fake_detection(image) == []


def test_detection_normalization_tames_a_box_wandering_off_screen() -> None:
    assert normalize_bounding_box((-10, 25, 120, 90), 100, 100) == (0.0, 0.25, 1.0, 0.65)
    assert normalize_object_class("CAR") == "vehicle"
    assert normalize_object_class("toaster") is None


def test_detection_normalization_rejects_non_finite_model_output() -> None:
    with pytest.raises(ValueError, match="finite"):
        normalize_bounding_box((math.nan, 0, 1, 1), 100, 100)

    detections = [Detection("person", math.inf, (0.1, 0.1, 0.2, 0.2))]
    assert (
        select_detections(
            detections,
            datetime(2026, 8, 29, tzinfo=UTC),
            {},
            timedelta(0),
            0.5,
        )
        == []
    )


def test_external_ffmpeg_reports_malformed_fixture(tmp_path: Path) -> None:
    invalid = tmp_path / "invalid.mp4"
    invalid.write_bytes(b"not a media file")

    with pytest.raises(DecodeError, match="ffmpeg rejected"):
        decode_fixture_frame(verify_ffmpeg(), invalid)


def test_parse_ppm_rejects_malformed_pixel_payload() -> None:
    with pytest.raises(DecodeError, match="pixel payload"):
        parse_ppm(b"P6\n2 2\n255\nshort")


def test_subprocess_output_reader_enforces_byte_limit() -> None:
    async def scenario() -> None:
        reader = asyncio.StreamReader()
        reader.feed_data(b"123456")
        reader.feed_eof()

        with pytest.raises(DecodeError, match="exceeded"):
            await read_stream_limited(reader, 5, "decoded frame")

    asyncio.run(scenario())


def test_cooldown_coalesces_each_class_and_keeps_highest_confidence() -> None:
    last_published: dict[str, datetime] = {}
    timestamp = datetime(2026, 8, 25, tzinfo=UTC)
    detections = [
        Detection("person", 0.7, (0.1, 0.1, 0.2, 0.2)),
        Detection("person", 0.9, (0.2, 0.2, 0.3, 0.3)),
        Detection("vehicle", 0.4, (0.0, 0.0, 1.0, 1.0)),
    ]

    assert select_detections(detections, timestamp, last_published, timedelta(seconds=5), 0.5) == [
        detections[1]
    ]
    assert (
        select_detections(
            detections,
            timestamp + timedelta(seconds=4),
            last_published,
            timedelta(seconds=5),
            0.5,
        )
        == []
    )
    assert select_detections(
        detections,
        timestamp + timedelta(seconds=5),
        last_published,
        timedelta(seconds=5),
        0.5,
    ) == [detections[1]]


def test_session_deactivation_clears_all_bounded_work() -> None:
    async def publish(_: pb.Event, __: int) -> None:
        raise AssertionError("stale work must not be published")

    pipeline = DetectionPipeline(
        verify_ffmpeg(),
        fake_detection,
        "deterministic-fake",
        publish,
        timedelta(0),
        timedelta(seconds=1),
        0.5,
    )
    pipeline.activate(1, "camera-1", "session-1", "sub")
    timestamp = timestamp_pb2.Timestamp(seconds=1)
    frame = pb.VideoDataFrame(
        stream_binding_id="media:detector",
        frame_id=1,
        timestamp=timestamp,
        fragment_index=0,
        fragment_count=1,
        key_frame=True,
        payload=b"\x00\x00\x00\x01x",
        configuration_revision=1,
    )
    pipeline.submit_fragment(frame, "h264", 1)
    assert pipeline.encoded_queue.count == 1

    pipeline.deactivate()
    pipeline.activate(2, "camera-1", "session-2", "sub")
    pipeline.submit_fragment(frame, "h264", 1)

    assert pipeline.encoded_queue.count == 0
    assert pipeline.inference_queue.count == 0
    assert pipeline.publication_queue.count == 0


def test_inference_loop_recovers_after_detector_failure() -> None:
    async def scenario() -> None:
        loop = asyncio.get_running_loop()
        first_failure = asyncio.Event()
        published = asyncio.Event()
        calls = 0

        def flaky_detector(image: np.ndarray) -> list[Detection]:
            nonlocal calls
            calls += 1
            if calls == 1:
                loop.call_soon_threadsafe(first_failure.set)
                raise RuntimeError("transient detector failure")
            return fake_detection(image)

        async def publish(_: pb.Event, __: int) -> None:
            published.set()

        pipeline = DetectionPipeline(
            verify_ffmpeg(),
            flaky_detector,
            "flaky-detector",
            publish,
            timedelta(0),
            timedelta(0),
            0.5,
        )
        pipeline.activate(1, "camera-1", "session-1", "sub")
        pipeline.start()
        image = np.zeros((2, 2, 3), dtype=np.uint8)
        try:
            pipeline.inference_queue.put_latest(
                DecodedFrame(datetime(2026, 8, 29, tzinfo=UTC), image, 1)
            )
            await asyncio.wait_for(first_failure.wait(), timeout=1)
            pipeline.inference_queue.put_latest(
                DecodedFrame(datetime(2026, 8, 29, 0, 0, 1, tzinfo=UTC), image, 1)
            )
            await asyncio.wait_for(published.wait(), timeout=1)
        finally:
            await pipeline.stop()

        assert calls == 2

    asyncio.run(scenario())
