# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded decode, inference, and publication pipeline for the demonstration service."""

import asyncio
import contextlib
import io
import logging
import math
import shutil
import subprocess
import uuid
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Literal, cast

import numpy as np
import numpy.typing as npt
from google.protobuf import struct_pb2, timestamp_pb2

from generated import webrtc_pb2 as pb

LOGGER = logging.getLogger(__name__)
CodecName = Literal["h264", "h265"]
Image = npt.NDArray[np.uint8]

FFMPEG_INSTALL_HELP = (
    "Install FFmpeg and put ffmpeg on PATH: macOS `brew install ffmpeg`; "
    "Windows `winget install Gyan.FFmpeg` or `choco install ffmpeg`; "
    "Linux `apt install ffmpeg`, `dnf install ffmpeg`, or `pacman -S ffmpeg`."
)
MAX_DECODED_FRAME_BYTES = 24 * 1024 * 1024
MAX_PPM_HEADER_BYTES = 1024
MAX_FFMPEG_STDERR_BYTES = 300


class ServiceError(RuntimeError):
    """An actionable service failure safe to display to an operator."""


class DecodeError(ServiceError):
    """An encoded frame or FFmpeg decoder failure."""


class BoundedLatestQueue[T]:
    """A count- and byte-bounded queue that evicts the oldest work first."""

    def __init__(self, max_items: int, max_bytes: int, size_of: Callable[[T], int]) -> None:
        if max_items < 1 or max_bytes < 1:
            raise ValueError("queue limits must be positive")
        self._queue: asyncio.Queue[tuple[T, int]] = asyncio.Queue(maxsize=max_items)
        self._max_bytes = max_bytes
        self._size_of = size_of
        self._bytes = 0
        self.dropped = 0

    @property
    def count(self) -> int:
        return self._queue.qsize()

    @property
    def byte_size(self) -> int:
        return self._bytes

    def put_latest(self, item: T) -> bool:
        item_bytes = self._size_of(item)
        if item_bytes < 0:
            raise ValueError("queue item size must not be negative")
        if item_bytes > self._max_bytes:
            self.dropped += 1
            return False
        while self._queue.full() or self._bytes + item_bytes > self._max_bytes:
            _, removed_bytes = self._queue.get_nowait()
            self._bytes -= removed_bytes
            self.dropped += 1
        self._queue.put_nowait((item, item_bytes))
        self._bytes += item_bytes
        return True

    async def get(self) -> T:
        item, item_bytes = await self._queue.get()
        self._bytes -= item_bytes
        return item

    def clear(self) -> None:
        while not self._queue.empty():
            _, item_bytes = self._queue.get_nowait()
            self._bytes -= item_bytes


@dataclass(frozen=True)
class EncodedFrame:
    codec: CodecName
    timestamp: datetime
    payload: bytes
    key_frame: bool
    generation: int


@dataclass(frozen=True)
class DecodedFrame:
    timestamp: datetime
    image: Image
    generation: int


@dataclass(frozen=True)
class Detection:
    object_class: Literal["person", "vehicle"]
    confidence: float
    bounding_box: tuple[float, float, float, float]


class UltralyticsDetector:
    def __init__(self, model_name: str) -> None:
        from ultralytics import YOLO

        self._model = YOLO(model_name)
        self.model_name = model_name

    def detect(self, image: Image) -> list[Detection]:
        results = self._model.predict(source=image, verbose=False)
        detections: list[Detection] = []
        for result in results:
            boxes = result.boxes
            if boxes is None:
                continue
            classes = boxes.cls.tolist()
            confidences = boxes.conf.tolist()
            coordinates = boxes.xyxy.tolist()
            for class_index, confidence, xyxy in zip(
                classes, confidences, coordinates, strict=True
            ):
                source_class = result.names.get(int(class_index), "")
                object_class = normalize_object_class(source_class)
                if object_class is None:
                    continue
                detections.append(
                    Detection(
                        object_class=object_class,
                        confidence=float(confidence),
                        bounding_box=normalize_bounding_box(xyxy, image.shape[1], image.shape[0]),
                    )
                )
        return detections


def fake_detection(
    image: Image, object_class: Literal["person", "vehicle"] = "person"
) -> list[Detection]:
    if image.size == 0:
        return []
    return [Detection(object_class, 0.9, (0.2, 0.2, 0.5, 0.6))]


def normalize_object_class(value: str) -> Literal["person", "vehicle"] | None:
    normalized = value.casefold()
    if normalized == "person":
        return "person"
    if normalized in {"bicycle", "bus", "car", "motorcycle", "truck"}:
        return "vehicle"
    return None


def normalize_bounding_box(
    xyxy: Sequence[float], width: int, height: int
) -> tuple[float, float, float, float]:
    if len(xyxy) != 4 or width < 1 or height < 1:
        raise ValueError("detection bounding box or image dimensions are invalid")
    coordinates = tuple(float(value) for value in xyxy)
    if not all(math.isfinite(value) for value in coordinates):
        raise ValueError("detection bounding box coordinates must be finite")
    x1, y1, x2, y2 = coordinates
    left = min(max(x1 / width, 0.0), 1.0)
    top = min(max(y1 / height, 0.0), 1.0)
    right = min(max(x2 / width, left), 1.0)
    bottom = min(max(y2 / height, top), 1.0)
    return left, top, right - left, bottom - top


@dataclass
class _FragmentedFrame:
    stream_binding_id: str
    frame_id: int
    generation: int
    fragment_count: int
    key_frame: bool
    configuration_revision: int
    timestamp: datetime
    fragments: list[bytes | None]
    byte_size: int = 0


class FrameAssembler:
    """Reassembles one ordered, bounded protobuf video frame at a time."""

    def __init__(self, max_frame_bytes: int = 8 * 1024 * 1024, max_fragments: int = 256) -> None:
        self._max_frame_bytes = max_frame_bytes
        self._max_fragments = max_fragments
        self._current: _FragmentedFrame | None = None
        self.discarded = 0

    def reset(self) -> None:
        self._current = None

    def push(
        self, frame: pb.VideoDataFrame, codec: CodecName, generation: int
    ) -> EncodedFrame | None:
        if (
            frame.fragment_count < 1
            or frame.fragment_count > self._max_fragments
            or frame.fragment_index >= frame.fragment_count
            or not frame.HasField("timestamp")
        ):
            self._discard()
            return None
        try:
            timestamp = frame.timestamp.ToDatetime(tzinfo=UTC)
        except (OSError, OverflowError, ValueError):
            self._discard()
            return None
        if (
            self._current is None
            or self._current.frame_id != frame.frame_id
            or self._current.generation != generation
        ):
            if self._current is not None:
                self.discarded += 1
            self._current = _FragmentedFrame(
                stream_binding_id=frame.stream_binding_id,
                frame_id=frame.frame_id,
                generation=generation,
                fragment_count=frame.fragment_count,
                key_frame=frame.key_frame,
                configuration_revision=frame.configuration_revision,
                timestamp=timestamp,
                fragments=[None] * frame.fragment_count,
            )
        current = self._current
        if (
            current.stream_binding_id != frame.stream_binding_id
            or current.generation != generation
            or current.fragment_count != frame.fragment_count
            or current.key_frame != frame.key_frame
            or current.configuration_revision != frame.configuration_revision
            or current.timestamp != timestamp
            or current.fragments[frame.fragment_index] is not None
        ):
            self._discard()
            return None
        current.fragments[frame.fragment_index] = bytes(frame.payload)
        current.byte_size += len(frame.payload)
        if current.byte_size > self._max_frame_bytes:
            self._discard()
            return None
        if any(fragment is None for fragment in current.fragments):
            return None
        payload = b"".join(cast(bytes, fragment) for fragment in current.fragments)
        assembled = EncodedFrame(
            codec=codec,
            timestamp=current.timestamp,
            payload=payload,
            key_frame=current.key_frame,
            generation=current.generation,
        )
        self._current = None
        return assembled

    def _discard(self) -> None:
        self._current = None
        self.discarded += 1


def avcc_to_annex_b(payload: bytes) -> bytes:
    output = bytearray()
    offset = 0
    while offset < len(payload):
        if len(payload) - offset < 4:
            raise DecodeError("encoded access unit ends inside a NAL length")
        nal_length = int.from_bytes(payload[offset : offset + 4], "big")
        offset += 4
        if nal_length < 1 or nal_length > len(payload) - offset:
            raise DecodeError("encoded access unit contains an invalid NAL length")
        output.extend(b"\x00\x00\x00\x01")
        output.extend(payload[offset : offset + nal_length])
        offset += nal_length
    if not output:
        raise DecodeError("encoded access unit is empty")
    return bytes(output)


def verify_ffmpeg(executable: str | None = None) -> Path:
    resolved = executable or shutil.which("ffmpeg")
    if resolved is None:
        raise ServiceError(f"ffmpeg was not found on PATH. {FFMPEG_INSTALL_HELP}")
    path = Path(resolved)
    try:
        completed = subprocess.run(
            [str(path), "-version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ServiceError(f"ffmpeg could not be executed. {FFMPEG_INSTALL_HELP}") from error
    if completed.returncode != 0 or not completed.stdout.startswith("ffmpeg version"):
        raise ServiceError(f"ffmpeg -version failed. {FFMPEG_INSTALL_HELP}")
    return path


async def read_stream_limited(
    stream: asyncio.StreamReader, max_bytes: int, description: str
) -> bytes:
    output = bytearray()
    while True:
        chunk = await stream.read(min(64 * 1024, max_bytes + 1 - len(output)))
        if not chunk:
            return bytes(output)
        output.extend(chunk)
        if len(output) > max_bytes:
            raise DecodeError(f"ffmpeg {description} exceeded {max_bytes} bytes")


async def _read_stream_prefix(stream: asyncio.StreamReader, max_bytes: int) -> bytes:
    output = bytearray()
    while chunk := await stream.read(64 * 1024):
        remaining = max_bytes - len(output)
        if remaining > 0:
            output.extend(chunk[:remaining])
    return bytes(output)


async def _communicate_limited(
    process: asyncio.subprocess.Process, payload: bytes
) -> tuple[bytes, bytes]:
    stdin = process.stdin
    stdout = process.stdout
    stderr = process.stderr
    if stdin is None or stdout is None or stderr is None:
        raise DecodeError("ffmpeg subprocess pipes are unavailable")
    stdout_task = asyncio.create_task(
        read_stream_limited(
            stdout,
            MAX_DECODED_FRAME_BYTES + MAX_PPM_HEADER_BYTES,
            "decoded frame",
        )
    )
    stderr_task = asyncio.create_task(_read_stream_prefix(stderr, MAX_FFMPEG_STDERR_BYTES))
    try:
        try:
            stdin.write(payload)
            await stdin.drain()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            stdin.close()
        output, error_output = await asyncio.gather(stdout_task, stderr_task)
        await process.wait()
        return output, error_output
    except BaseException:
        if process.returncode is None:
            with contextlib.suppress(ProcessLookupError):
                process.kill()
        await process.wait()
        await asyncio.gather(stdout_task, stderr_task, return_exceptions=True)
        raise


async def decode_access_unit(ffmpeg: Path, frame: EncodedFrame) -> DecodedFrame:
    annex_b = avcc_to_annex_b(frame.payload)
    input_format = "h264" if frame.codec == "h264" else "hevc"
    process = await asyncio.create_subprocess_exec(
        str(ffmpeg),
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        input_format,
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "ppm",
        "pipe:1",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout, stderr = await asyncio.wait_for(_communicate_limited(process, annex_b), timeout=10)
    except TimeoutError as error:
        raise DecodeError("ffmpeg timed out while decoding a camera keyframe") from error
    if process.returncode != 0:
        detail = stderr.decode("utf-8", errors="replace")[:300].strip()
        raise DecodeError(f"ffmpeg rejected a camera keyframe: {detail or 'unknown error'}")
    return DecodedFrame(
        timestamp=frame.timestamp,
        image=parse_ppm(stdout),
        generation=frame.generation,
    )


def parse_ppm(payload: bytes) -> Image:
    stream = io.BytesIO(payload)

    def token() -> bytes:
        while True:
            value = stream.read(1)
            if not value:
                raise DecodeError("ffmpeg returned a truncated PPM frame")
            if value == b"#":
                stream.readline()
                continue
            if not value.isspace():
                break
        result = bytearray(value)
        while True:
            value = stream.read(1)
            if not value or value.isspace():
                return bytes(result)
            result.extend(value)

    if token() != b"P6":
        raise DecodeError("ffmpeg did not return a binary PPM frame")
    try:
        width = int(token())
        height = int(token())
        maximum = int(token())
    except ValueError as error:
        raise DecodeError("ffmpeg returned invalid PPM dimensions") from error
    if width < 1 or height < 1 or maximum != 255:
        raise DecodeError("ffmpeg returned an unsupported PPM frame")
    expected_bytes = width * height * 3
    if expected_bytes > MAX_DECODED_FRAME_BYTES:
        raise DecodeError("ffmpeg returned an oversized PPM frame")
    pixels = stream.read()
    if len(pixels) != expected_bytes:
        raise DecodeError("ffmpeg returned a malformed PPM pixel payload")
    return np.frombuffer(pixels, dtype=np.uint8).reshape((height, width, 3)).copy()


def select_detections(
    detections: Sequence[Detection],
    timestamp: datetime,
    last_published: dict[str, datetime],
    cooldown: timedelta,
    confidence_threshold: float,
) -> list[Detection]:
    highest: dict[str, Detection] = {}
    for detection in detections:
        if not math.isfinite(detection.confidence) or detection.confidence < confidence_threshold:
            continue
        previous = highest.get(detection.object_class)
        if previous is None or detection.confidence > previous.confidence:
            highest[detection.object_class] = detection

    selected: list[Detection] = []
    for object_class in sorted(highest):
        last = last_published.get(object_class)
        if last is not None and timestamp - last < cooldown:
            continue
        last_published[object_class] = timestamp
        selected.append(highest[object_class])
    return selected


Detect = Callable[[Image], list[Detection]]
Publish = Callable[[pb.Event, int], Awaitable[None]]

ENCODED_QUEUE_ITEMS = 4
ENCODED_QUEUE_BYTES = 8 * 1024 * 1024
INFERENCE_QUEUE_ITEMS = 1
INFERENCE_QUEUE_BYTES = MAX_DECODED_FRAME_BYTES
PUBLICATION_QUEUE_ITEMS = 16
PUBLICATION_QUEUE_BYTES = 16 * 1024


class DetectionPipeline:
    def __init__(
        self,
        ffmpeg: Path,
        detect: Detect,
        model_name: str,
        publish: Publish,
        inference_interval: timedelta,
        cooldown: timedelta,
        confidence_threshold: float,
    ) -> None:
        self._ffmpeg = ffmpeg
        self._detect = detect
        self._model_name = model_name
        self._publish = publish
        self._inference_interval = inference_interval
        self._cooldown = cooldown
        self._confidence_threshold = confidence_threshold
        self._assembler = FrameAssembler(max_frame_bytes=ENCODED_QUEUE_BYTES)
        self.encoded_queue = BoundedLatestQueue[EncodedFrame](
            ENCODED_QUEUE_ITEMS, ENCODED_QUEUE_BYTES, lambda frame: len(frame.payload)
        )
        self.inference_queue = BoundedLatestQueue[DecodedFrame](
            INFERENCE_QUEUE_ITEMS,
            INFERENCE_QUEUE_BYTES,
            lambda frame: int(frame.image.nbytes),
        )
        self.publication_queue = BoundedLatestQueue[tuple[pb.Event, int]](
            PUBLICATION_QUEUE_ITEMS,
            PUBLICATION_QUEUE_BYTES,
            lambda item: item[0].ByteSize() + 8,
        )
        self._last_published: dict[str, datetime] = {}
        self._generation = 0
        self._source_id = ""
        self._source_session_id = ""
        self._stream_id = ""
        self._last_submitted: datetime | None = None
        self._tasks: list[asyncio.Task[None]] = []

    def start(self) -> None:
        if self._tasks:
            raise RuntimeError("detection pipeline is already running")
        self._tasks = [
            asyncio.create_task(self._decode_loop(), name="detector-decode"),
            asyncio.create_task(self._inference_loop(), name="detector-inference"),
            asyncio.create_task(self._publication_loop(), name="detector-publication"),
        ]

    async def stop(self) -> None:
        self.deactivate()
        for task in self._tasks:
            task.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)
        self._tasks.clear()

    def activate(
        self, generation: int, source_id: str, source_session_id: str, stream_id: str
    ) -> None:
        self.deactivate()
        self._generation = generation
        self._source_id = source_id
        self._source_session_id = source_session_id
        self._stream_id = stream_id

    def deactivate(self) -> None:
        self._generation = 0
        self._source_id = ""
        self._source_session_id = ""
        self._stream_id = ""
        self._last_submitted = None
        # A source-session replacement invalidates partial frames and every queued result.
        self._assembler.reset()
        self._last_published.clear()
        self.encoded_queue.clear()
        self.inference_queue.clear()
        self.publication_queue.clear()

    def submit_fragment(self, frame: pb.VideoDataFrame, codec: CodecName, generation: int) -> None:
        if generation != self._generation or generation == 0:
            return
        assembled = self._assembler.push(frame, codec, generation)
        if assembled is None or not assembled.key_frame:
            return
        # A fresh one-shot FFmpeg process needs a random-access unit with parameter sets.
        if (
            self._last_submitted is not None
            and assembled.timestamp - self._last_submitted < self._inference_interval
        ):
            return
        self._last_submitted = assembled.timestamp
        self.encoded_queue.put_latest(assembled)

    async def _decode_loop(self) -> None:
        while True:
            frame = await self.encoded_queue.get()
            if frame.generation != self._generation:
                continue
            try:
                decoded = await decode_access_unit(self._ffmpeg, frame)
            except DecodeError as error:
                LOGGER.warning("camera keyframe decode failed: %s", error)
                continue
            if decoded.generation == self._generation:
                self.inference_queue.put_latest(decoded)

    async def _inference_loop(self) -> None:
        while True:
            frame = await self.inference_queue.get()
            if frame.generation != self._generation:
                continue
            try:
                detections = await asyncio.to_thread(self._detect, frame.image)
            except Exception as error:
                LOGGER.warning("detector inference failed: %s", error)
                continue
            if frame.generation != self._generation:
                continue
            selected = select_detections(
                detections,
                frame.timestamp,
                self._last_published,
                self._cooldown,
                self._confidence_threshold,
            )
            for detection in selected:
                timestamp = timestamp_pb2.Timestamp()
                timestamp.FromDatetime(frame.timestamp)
                x, y, width, height = detection.bounding_box
                event = pb.Event(
                    event_id=str(uuid.uuid4()),
                    revision=1,
                    source_id=self._source_id,
                    media_kind=pb.MEDIA_KIND_VIDEO,
                    origin=pb.EVENT_ORIGIN_KEEPPEEK,
                    event_type=detection.object_class,
                    start_time=timestamp,
                    confidence=detection.confidence,
                    bounding_box=pb.EventBoundingBox(x=x, y=y, width=width, height=height),
                    payload=struct_pb2.Struct(
                        fields={
                            "object_class": struct_pb2.Value(string_value=detection.object_class),
                            "stream_id": struct_pb2.Value(string_value=self._stream_id),
                            "model": struct_pb2.Value(string_value=self._model_name),
                        }
                    ),
                    source_session_id=self._source_session_id,
                )
                self.publication_queue.put_latest((event, frame.generation))

    async def _publication_loop(self) -> None:
        while True:
            event, generation = await self.publication_queue.get()
            if generation != self._generation:
                continue
            try:
                await self._publish(event, generation)
            except Exception as error:
                LOGGER.warning(
                    "event publication failed event_id=%s class=%s: %s",
                    event.event_id,
                    event.event_type,
                    error,
                )
                continue
            LOGGER.info(
                "published detection event_id=%s class=%s confidence=%.3f",
                event.event_id,
                event.event_type,
                event.confidence,
            )
