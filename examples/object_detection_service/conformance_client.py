# SPDX-License-Identifier: AGPL-3.0-only
"""Run the finite, no-model KeepPeek external-analysis conformance scenario."""

import argparse
import asyncio
import hashlib
import json
import os
import uuid
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import UTC

from google.protobuf import struct_pb2, timestamp_pb2

from detection_pipeline import (
    CodecName,
    DecodedFrame,
    EncodedFrame,
    FrameAssembler,
    decode_access_unit,
    encode_jpeg_evidence,
    fake_detection,
    verify_ffmpeg,
)
from generated import webrtc_pb2 as pb
from keeppeek_client import EventAttachment, KeepPeekClient, LiteralStream

MAXIMUM_CONFORMANCE_SECONDS = 60.0


@dataclass(frozen=True)
class ConformanceConfig:
    url: str
    source_ids: tuple[str, str]


class KeyFrameCapture:
    def __init__(self, generation: int) -> None:
        self._generation = generation
        self._assembler = FrameAssembler()
        self._future: asyncio.Future[EncodedFrame] = asyncio.get_running_loop().create_future()

    def receive(self, frame: pb.VideoDataFrame, codec: CodecName, generation: int) -> None:
        assembled = self._assembler.push(frame, codec, generation)
        if assembled is not None and assembled.key_frame and not self._future.done():
            self._future.set_result(assembled)

    async def wait(self) -> EncodedFrame:
        return await asyncio.wait_for(self._future, MAXIMUM_CONFORMANCE_SECONDS)


async def connect_capture(
    url: str,
    access_key: str,
    source_id: str,
    stream: LiteralStream,
    generation: int,
) -> tuple[KeepPeekClient, KeyFrameCapture]:
    capture = KeyFrameCapture(generation)
    client = KeepPeekClient(url, access_key, source_id, stream, generation, capture.receive)
    await client.connect()
    return client, capture


async def run_conformance(config: ConformanceConfig) -> dict[str, object]:
    ffmpeg = verify_ffmpeg()
    access_key = os.environ.get("KEEPPEEK_ACCESS_KEY", "")
    clients: list[KeepPeekClient] = []
    try:
        low_connections = await asyncio.gather(
            *(
                connect_capture(config.url, access_key, source_id, "sub", index + 1)
                for index, source_id in enumerate(config.source_ids)
            )
        )
        clients.extend(client for client, _ in low_connections)
        low_frames = await asyncio.gather(*(capture.wait() for _, capture in low_connections))
        low_decoded = await asyncio.gather(
            *(decode_access_unit(ffmpeg, frame) for frame in low_frames)
        )
        detections = [fake_detection(frame.image, "person") for frame in low_decoded]
        if any(len(result) != 1 for result in detections):
            raise RuntimeError("deterministic low-stream analysis did not produce one detection")

        high_client, high_capture = await connect_capture(
            config.url,
            access_key,
            config.source_ids[0],
            "main",
            len(config.source_ids) + 1,
        )
        clients.append(high_client)
        high_frame = await high_capture.wait()
        high_decoded: DecodedFrame = await decode_access_unit(ffmpeg, high_frame)
        evidence = await encode_jpeg_evidence(ffmpeg, high_decoded)

        low_client = low_connections[0][0]
        low_subscription = low_client.subscription
        high_subscription = high_client.subscription
        detection = detections[0][0]
        event_timestamp = timestamp_pb2.Timestamp()
        event_timestamp.FromDatetime(low_frames[0].timestamp.astimezone(UTC))
        event_id = str(
            uuid.uuid5(
                uuid.NAMESPACE_URL,
                f"keeppeek-conformance:{low_subscription.source_id}:{low_frames[0].timestamp.isoformat()}",
            )
        )
        event = pb.Event(
            event_id=event_id,
            revision=1,
            source_id=high_subscription.source_id,
            media_kind=pb.MEDIA_KIND_VIDEO,
            origin=pb.EVENT_ORIGIN_KEEPPEEK,
            event_type=detection.object_class,
            start_time=event_timestamp,
            confidence=detection.confidence,
            bounding_box=pb.EventBoundingBox(
                x=detection.bounding_box[0],
                y=detection.bounding_box[1],
                width=detection.bounding_box[2],
                height=detection.bounding_box[3],
            ),
            text="deterministic external conformance event",
            payload=struct_pb2.Struct(
                fields={
                    "analysis_stream_id": struct_pb2.Value(string_value=low_subscription.stream_id),
                    "object_class": struct_pb2.Value(string_value=detection.object_class),
                    "stream_id": struct_pb2.Value(string_value=high_subscription.stream_id),
                    "model": struct_pb2.Value(string_value="deterministic-fake"),
                }
            ),
            source_session_id=high_subscription.source_session_id,
        )
        attachment = EventAttachment(
            attachment_id="high-quality-evidence",
            attachment_type="snapshot",
            content_type="image/jpeg",
            timestamp=evidence.timestamp,
            payload=evidence.payload,
        )
        await high_client.publish_event_with_attachment(event, attachment, high_client.generation)
        await high_client.publish_event_with_attachment(event, attachment, high_client.generation)
        await high_client.close()
        clients.remove(high_client)

        return {
            "event_id": event_id,
            "revision": 1,
            "source_id": high_subscription.source_id,
            "source_session_id": high_subscription.source_session_id,
            "stream_id": high_subscription.stream_id,
            "event_timestamp": low_frames[0].timestamp.isoformat(),
            "event_date": low_frames[0].timestamp.date().isoformat(),
            "attachment_timestamp": evidence.timestamp.isoformat(),
            "attachment_id": attachment.attachment_id,
            "attachment_bytes": len(attachment.payload),
            "attachment_sha256": hashlib.sha256(attachment.payload).hexdigest(),
            "evidence_width": evidence.width,
            "evidence_height": evidence.height,
            "low_streams": [
                {
                    "source_id": client.subscription.source_id,
                    "stream_id": client.subscription.stream_id,
                    "codec": encoded.codec,
                    "width": int(frame.image.shape[1]),
                    "height": int(frame.image.shape[0]),
                }
                for (client, _), frame, encoded in zip(
                    low_connections, low_decoded, low_frames, strict=True
                )
            ],
        }
    finally:
        await asyncio.gather(*(client.close() for client in clients), return_exceptions=True)


def parse_args(arguments: Sequence[str] | None = None) -> ConformanceConfig:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--source-id", action="append", required=True)
    namespace = parser.parse_args(arguments)
    source_ids = tuple(str(source_id) for source_id in namespace.source_id)
    if len(source_ids) != 2 or len(set(source_ids)) != 2 or any(not value for value in source_ids):
        parser.error("--source-id must be provided exactly twice with distinct values")
    return ConformanceConfig(str(namespace.url), (source_ids[0], source_ids[1]))


def main(arguments: Sequence[str] | None = None) -> int:
    config = parse_args(arguments)
    result = asyncio.run(asyncio.wait_for(run_conformance(config), MAXIMUM_CONFORMANCE_SECONDS))
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
