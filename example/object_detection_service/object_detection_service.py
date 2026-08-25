# SPDX-License-Identifier: AGPL-3.0-only
"""Run KeepPeek's demonstration/CI-only Ultralytics object-detection example.

This is not production-ready, is not a supported detector product, and will not
evolve into a mature object-detection service. It exists so developers can
implement independent services against KeepPeek's public API.
"""

import argparse
import asyncio
import logging
import os
import stat
import uuid
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import timedelta
from functools import partial
from pathlib import Path
from typing import Literal

from detection_pipeline import (
    CodecName,
    Detect,
    DetectionPipeline,
    ServiceError,
    UltralyticsDetector,
    fake_detection,
    verify_ffmpeg,
)
from generated import webrtc_pb2 as pb
from keeppeek_client import KeepPeekClient, LiteralStream, ProtocolError, SessionLostError

LOGGER = logging.getLogger(__name__)
DetectorKind = Literal["ultralytics", "fake"]


@dataclass(frozen=True)
class ServiceConfig:
    url: str
    source_id: str
    stream: LiteralStream
    detector: DetectorKind
    model: str
    fake_object_class: Literal["person", "vehicle"]
    confidence: float
    cooldown_seconds: float
    inference_fps: float
    reconnect_delay_seconds: float
    access_key_file: Path | None


def parse_args(arguments: Sequence[str] | None = None) -> ServiceConfig:
    parser = argparse.ArgumentParser(
        description="Demonstration-only KeepPeek Ultralytics object detector"
    )
    parser.add_argument("--url", default=os.environ.get("KEEPPEEK_URL", "http://127.0.0.1:8081"))
    parser.add_argument("--source-id", default=os.environ.get("KEEPPEEK_SOURCE_ID", ""))
    parser.add_argument(
        "--stream",
        choices=("auto", "main", "sub"),
        default=os.environ.get("KEEPPEEK_STREAM", "auto"),
    )
    parser.add_argument(
        "--detector",
        choices=("ultralytics", "fake"),
        default=os.environ.get("KEEPPEEK_DETECTOR", "ultralytics"),
    )
    parser.add_argument("--model", default=os.environ.get("KEEPPEEK_MODEL", "yolo11n.pt"))
    parser.add_argument(
        "--fake-object-class",
        choices=("person", "vehicle"),
        default=os.environ.get("KEEPPEEK_FAKE_OBJECT_CLASS", "person"),
    )
    parser.add_argument(
        "--confidence",
        type=float,
        default=float(os.environ.get("KEEPPEEK_CONFIDENCE", "0.5")),
    )
    parser.add_argument(
        "--cooldown-seconds",
        type=float,
        default=float(os.environ.get("KEEPPEEK_COOLDOWN_SECONDS", "5")),
    )
    parser.add_argument(
        "--inference-fps",
        type=float,
        default=float(os.environ.get("KEEPPEEK_INFERENCE_FPS", "1")),
    )
    parser.add_argument(
        "--reconnect-delay-seconds",
        type=float,
        default=float(os.environ.get("KEEPPEEK_RECONNECT_DELAY_SECONDS", "2")),
    )
    parser.add_argument(
        "--access-key-file",
        type=Path,
        default=(Path(value) if (value := os.environ.get("KEEPPEEK_ACCESS_KEY_FILE")) else None),
        help="Owner-only file containing the access key; the environment takes precedence",
    )
    namespace = parser.parse_args(arguments)
    if not namespace.source_id:
        parser.error("--source-id or KEEPPEEK_SOURCE_ID is required")
    if not 0.0 <= namespace.confidence <= 1.0:
        parser.error("--confidence must be between 0 and 1")
    if namespace.cooldown_seconds < 0.0:
        parser.error("--cooldown-seconds must not be negative")
    if namespace.inference_fps <= 0.0:
        parser.error("--inference-fps must be positive")
    if namespace.reconnect_delay_seconds <= 0.0:
        parser.error("--reconnect-delay-seconds must be positive")
    return ServiceConfig(
        url=str(namespace.url),
        source_id=str(namespace.source_id),
        stream=namespace.stream,
        detector=namespace.detector,
        model=str(namespace.model),
        fake_object_class=namespace.fake_object_class,
        confidence=float(namespace.confidence),
        cooldown_seconds=float(namespace.cooldown_seconds),
        inference_fps=float(namespace.inference_fps),
        reconnect_delay_seconds=float(namespace.reconnect_delay_seconds),
        access_key_file=namespace.access_key_file,
    )


def load_access_key(config: ServiceConfig) -> str:
    environment_value = os.environ.get("KEEPPEEK_ACCESS_KEY")
    if environment_value:
        return validate_access_key(environment_value)
    path = config.access_key_file
    if path is None:
        raise ServiceError(
            "set KEEPPEEK_ACCESS_KEY or provide --access-key-file pointing to an owner-only file"
        )
    try:
        metadata = path.stat()
    except OSError as error:
        raise ServiceError(f"unable to read access key file: {path}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ServiceError("access key file must be a regular file")
    if os.name != "nt" and metadata.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise ServiceError("access key file must not grant group or other permissions")
    try:
        value = path.read_text(encoding="utf-8").strip()
    except OSError as error:
        raise ServiceError(f"unable to read access key file: {path}") from error
    return validate_access_key(value)


def validate_access_key(value: str) -> str:
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise ServiceError("KeepPeek access key must be a UUID") from error
    if parsed.int == 0:
        raise ServiceError("KeepPeek access key must not be the zero UUID")
    return str(parsed)


async def run_service(config: ServiceConfig) -> None:
    ffmpeg = verify_ffmpeg()
    access_key = load_access_key(config)
    pipeline: DetectionPipeline | None = None
    active_client: KeepPeekClient | None = None
    generation = 1

    def handle_frame(frame: pb.VideoDataFrame, codec: CodecName, frame_generation: int) -> None:
        current = pipeline
        if current is not None:
            current.submit_fragment(frame, codec, frame_generation)

    async def publish(event: pb.Event, event_generation: int) -> None:
        client = active_client
        if client is None or client.generation != event_generation:
            raise SessionLostError("no active KeepPeek session for detection publication")
        await client.publish_event(event, event_generation)

    async def connect(session_generation: int) -> KeepPeekClient:
        client = KeepPeekClient(
            config.url,
            access_key,
            config.source_id,
            config.stream,
            session_generation,
            handle_frame,
        )
        try:
            await client.connect()
        except Exception:
            await client.close()
            raise
        return client

    try:
        # Do not initialize a model, which may download weights, until KeepPeek has accepted media.
        active_client = await connect(generation)
        detect: Detect
        if config.detector == "fake":
            LOGGER.warning("using deterministic fake detector; this mode is only for tests and CI")
            detect = partial(fake_detection, object_class=config.fake_object_class)
            model_name = "deterministic-fake"
        else:
            LOGGER.info("loading Ultralytics model %s", config.model)
            try:
                detector = await asyncio.to_thread(UltralyticsDetector, config.model)
            except Exception as error:
                raise ServiceError(f"unable to load Ultralytics model {config.model}") from error
            detect = detector.detect
            model_name = detector.model_name
        pipeline = DetectionPipeline(
            ffmpeg,
            detect,
            model_name,
            publish,
            timedelta(seconds=1 / config.inference_fps),
            timedelta(seconds=config.cooldown_seconds),
            config.confidence,
        )
        pipeline.start()
        subscription = active_client.subscription
        pipeline.activate(
            generation,
            subscription.source_id,
            subscription.source_session_id,
            subscription.stream_id,
        )

        while True:
            await active_client.wait_until_lost()
            # Clearing generation-scoped queues prevents old-session results from being published.
            pipeline.deactivate()
            await active_client.close()
            active_client = None
            LOGGER.warning("KeepPeek session lost; inference and publication are paused")
            while active_client is None:
                await asyncio.sleep(config.reconnect_delay_seconds)
                generation += 1
                try:
                    active_client = await connect(generation)
                except (ProtocolError, SessionLostError, OSError, ValueError) as error:
                    LOGGER.warning("KeepPeek reconnect failed: %s", error)
            subscription = active_client.subscription
            pipeline.activate(
                generation,
                subscription.source_id,
                subscription.source_session_id,
                subscription.stream_id,
            )
            LOGGER.info("KeepPeek session and media subscription restored")
    finally:
        if pipeline is not None:
            await pipeline.stop()
        if active_client is not None:
            await active_client.close()


def main(arguments: Sequence[str] | None = None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
    try:
        config = parse_args(arguments)
        asyncio.run(run_service(config))
    except KeyboardInterrupt:
        LOGGER.info("object-detection example stopped")
    except (ServiceError, ValueError) as error:
        LOGGER.error("%s", error)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
