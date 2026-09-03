# SPDX-License-Identifier: AGPL-3.0-only
"""Run the finite, no-model KeepPeek external-analysis conformance scenario."""

import argparse
import asyncio
import hashlib
import json
import math
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
from keeppeek_client import (
    EventAttachment,
    KeepPeekClient,
    LiteralStream,
    LiveEventDelivery,
    ProtocolError,
)

MAXIMUM_CONFORMANCE_SECONDS = 60.0
MAXIMUM_WITHHOLD_SECONDS = 60.0
DIAGNOSTIC_PAYLOAD_SENTINEL = "keeppeek-private-payload-67-4f1c"
DIAGNOSTIC_ATTACHMENT_SENTINEL = b"keeppeek-private-jpeg-67-9d2a"
DIAGNOSTIC_ACCESS_KEY_SENTINEL = "67a14f1c-9d2a-4e5b-8c3d-0123456789ab"


@dataclass(frozen=True)
class ConformanceConfig:
    url: str
    source_ids: tuple[str, str]
    withhold_seconds: float | None


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


def validate_live_delivery(
    delivery: LiveEventDelivery,
    expected: pb.Event,
    attachment: EventAttachment,
    subscription_id: str,
) -> str:
    event = delivery.event
    if (
        event.subscription_id != subscription_id
        or event.event_id != expected.event_id
        or event.revision != expected.revision
        or event.source_id != expected.source_id
        or event.source_session_id != expected.source_session_id
        or event.media_kind != expected.media_kind
        or event.event_type != expected.event_type
        or event.start_time != expected.start_time
        or event.confidence != expected.confidence
        or event.bounding_box != expected.bounding_box
        or event.text != expected.text
        or event.payload != expected.payload
    ):
        raise ProtocolError("live event metadata does not match the publication")
    if delivery.attachment != attachment.payload or len(event.attachments) != 1:
        raise ProtocolError("live event attachment does not match the publication")
    descriptor = event.attachments[0]
    if (
        event.canonical_attachment_id != attachment.attachment_id
        or event.bounding_box_attachment_id != attachment.attachment_id
        or descriptor.attachment_id != attachment.attachment_id
        or descriptor.attachment_type != attachment.attachment_type
        or descriptor.content_type != attachment.content_type
        or descriptor.byte_len != len(attachment.payload)
        or descriptor.timestamp.ToDatetime(tzinfo=UTC) != attachment.timestamp
    ):
        raise ProtocolError("live event descriptor does not match the publication")
    stream = event.payload.fields.get("stream_id")
    if stream is None or stream.WhichOneof("kind") != "string_value":
        raise ProtocolError("live event omitted its stream identity")
    return stream.string_value


def add_jpeg_comment(payload: bytes, comment: bytes) -> bytes:
    if not payload.startswith(b"\xff\xd8") or not comment or len(comment) > 65_533:
        raise ProtocolError("JPEG diagnostic comment is invalid")
    segment_length = len(comment) + 2
    return payload[:2] + b"\xff\xfe" + segment_length.to_bytes(2, "big") + comment + payload[2:]


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
        live_subscription_id = "conformance-events"
        await low_connections[0][0].subscribe_events(live_subscription_id)

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
        evidence_payload = add_jpeg_comment(evidence.payload, DIAGNOSTIC_ATTACHMENT_SENTINEL)

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
                    "diagnostic_probe": struct_pb2.Value(string_value=DIAGNOSTIC_PAYLOAD_SENTINEL),
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
            payload=evidence_payload,
        )
        await high_client.publish_event_with_attachment(event, attachment, high_client.generation)
        await high_client.publish_event_with_attachment(event, attachment, high_client.generation)
        live_delivery = await low_connections[0][0].wait_for_live_event(MAXIMUM_CONFORMANCE_SECONDS)
        live_stream_id = validate_live_delivery(
            live_delivery, event, attachment, live_subscription_id
        )
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
            "live_event_id": live_delivery.event.event_id,
            "live_revision": live_delivery.event.revision,
            "live_source_id": live_delivery.event.source_id,
            "live_stream_id": live_stream_id,
            "live_attachment_bytes": len(live_delivery.attachment),
            "live_attachment_sha256": hashlib.sha256(live_delivery.attachment).hexdigest(),
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


async def run_withheld_client(config: ConformanceConfig) -> None:
    clients: list[KeepPeekClient] = []
    try:
        connections = await asyncio.gather(
            *(
                connect_capture(config.url, "", source_id, "sub", index + 1)
                for index, source_id in enumerate(config.source_ids)
            )
        )
        clients.extend(client for client, _ in connections)
        frames = await asyncio.gather(*(capture.wait() for _, capture in connections))
        print(
            json.dumps(
                {
                    "status": "withheld",
                    "codecs": [frame.codec for frame in frames],
                    "source_ids": list(config.source_ids),
                },
                sort_keys=True,
                separators=(",", ":"),
            ),
            flush=True,
        )
        if config.withhold_seconds is None:
            raise RuntimeError("withheld client duration is missing")
        await asyncio.sleep(config.withhold_seconds)
    finally:
        await asyncio.gather(*(client.close() for client in clients), return_exceptions=True)


def parse_args(arguments: Sequence[str] | None = None) -> ConformanceConfig:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--source-id", action="append", required=True)
    parser.add_argument("--withhold-seconds", type=float)
    namespace = parser.parse_args(arguments)
    source_ids = tuple(str(source_id) for source_id in namespace.source_id)
    if len(source_ids) != 2 or len(set(source_ids)) != 2 or any(not value for value in source_ids):
        parser.error("--source-id must be provided exactly twice with distinct values")
    withhold_seconds = namespace.withhold_seconds
    if withhold_seconds is not None and (
        not math.isfinite(withhold_seconds)
        or withhold_seconds < 1
        or withhold_seconds > MAXIMUM_WITHHOLD_SECONDS
    ):
        parser.error(f"--withhold-seconds must be from 1 through {MAXIMUM_WITHHOLD_SECONDS:g}")
    return ConformanceConfig(str(namespace.url), (source_ids[0], source_ids[1]), withhold_seconds)


def main(arguments: Sequence[str] | None = None) -> int:
    config = parse_args(arguments)
    if config.withhold_seconds is not None:
        asyncio.run(run_withheld_client(config))
        return 0
    result = asyncio.run(asyncio.wait_for(run_conformance(config), MAXIMUM_CONFORMANCE_SECONDS))
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
