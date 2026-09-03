# SPDX-License-Identifier: AGPL-3.0-only

import asyncio
import gzip
from datetime import UTC, datetime

import pytest
from google.protobuf import timestamp_pb2

from generated import webrtc_pb2 as pb
from keeppeek_client import (
    AtomicEventPublisher,
    EventAttachment,
    MediaConfigurationTracker,
    ProtocolError,
    decode_session_body,
    read_stream_limited,
)


def media_delivery(codec: str = "h264", revision: int = 1) -> pb.MediaDataDelivery:
    return pb.MediaDataDelivery(
        stream_binding_id="media:detector",
        channel=pb.DATA_CHANNEL_KIND_RELIABLE_DATA,
        codec=pb.CodecDescriptor(name=codec),
        format=pb.MediaDataFormat(
            video=pb.VideoDataFormat(width=640, height=360, decoder_config=b"config")
        ),
        configuration_revision=revision,
    )


def test_session_response_reader_enforces_byte_limit() -> None:
    async def scenario() -> None:
        reader = asyncio.StreamReader()
        reader.feed_data(b"123456")
        reader.feed_eof()

        with pytest.raises(ProtocolError, match="exceeds"):
            await read_stream_limited(reader, 5)

    asyncio.run(scenario())


def test_session_response_gzip_expansion_is_bounded() -> None:
    compressed = gzip.compress(b"x" * 6)

    with pytest.raises(ProtocolError, match="decompressed response exceeds"):
        decode_session_body(compressed, 5)


def test_session_response_decoder_accepts_plain_and_bounded_gzip() -> None:
    assert decode_session_body(b"plain", 5) == b"plain"
    assert decode_session_body(gzip.compress(b"plain"), 5) == b"plain"


def test_media_configuration_tracker_applies_ordered_codec_updates() -> None:
    tracker = MediaConfigurationTracker()
    initial = tracker.initialize(media_delivery())
    assert initial.codec == "h264"
    assert (
        tracker.codec_for(
            pb.VideoDataFrame(stream_binding_id="media:detector", configuration_revision=1)
        )
        == "h264"
    )

    updated = tracker.apply(
        pb.MediaDataConfiguration(
            stream_binding_id="media:detector",
            codec=pb.CodecDescriptor(name="h265"),
            format=pb.MediaDataFormat(
                video=pb.VideoDataFormat(width=640, height=360, decoder_config=b"hvcC")
            ),
            configuration_revision=2,
        )
    )

    assert updated.codec == "h265"
    assert (
        tracker.codec_for(
            pb.VideoDataFrame(stream_binding_id="media:detector", configuration_revision=2)
        )
        == "h265"
    )
    assert (
        tracker.codec_for(
            pb.VideoDataFrame(stream_binding_id="media:detector", configuration_revision=1)
        )
        is None
    )


def test_media_configuration_tracker_rejects_incomplete_and_stale_updates() -> None:
    tracker = MediaConfigurationTracker()
    invalid = media_delivery()
    invalid.format.video.decoder_config = b""
    with pytest.raises(ProtocolError, match="non-decodable"):
        tracker.initialize(invalid)

    tracker.initialize(media_delivery())
    with pytest.raises(ProtocolError, match="did not increase"):
        tracker.apply(
            pb.MediaDataConfiguration(
                stream_binding_id="media:detector",
                codec=pb.CodecDescriptor(name="h264"),
                format=pb.MediaDataFormat(
                    video=pb.VideoDataFormat(width=640, height=360, decoder_config=b"config")
                ),
                configuration_revision=1,
            )
        )


def test_atomic_event_publisher_chunks_once_and_retries_the_durable_commit() -> None:
    async def scenario() -> None:
        requests: list[pb.Request] = []
        messages: list[pb.Message] = []
        committed = False

        async def request(value: pb.Request) -> pb.Response:
            nonlocal committed
            copy = pb.Request()
            copy.CopyFrom(value)
            requests.append(copy)
            action = value.event_publication_command.WhichOneof("action")
            if action == "start":
                status = (
                    pb.EVENT_PUBLICATION_STATUS_COMMITTED
                    if committed
                    else pb.EVENT_PUBLICATION_STATUS_ACCEPTING_ATTACHMENTS
                )
            else:
                assert action == "commit"
                committed = True
                status = pb.EVENT_PUBLICATION_STATUS_COMMITTED
            command = value.event_publication_command
            publication_id = (
                command.start.publication_id if action == "start" else command.commit.publication_id
            )
            return pb.Response(
                ok=pb.Ok(
                    event_publication_state=pb.EventPublicationState(
                        publication_id=publication_id,
                        status=status,
                        event_id="event-1",
                        revision=1,
                        attachment_channel=pb.DATA_CHANNEL_KIND_RELIABLE_DATA,
                        max_attachment_bytes=256 * 1024,
                        max_event_attachment_bytes=256 * 1024,
                    )
                )
            )

        def send(value: pb.Message) -> None:
            copy = pb.Message()
            copy.CopyFrom(value)
            messages.append(copy)

        timestamp = datetime(2026, 9, 2, 12, 0, tzinfo=UTC)
        protobuf_timestamp = timestamp_pb2.Timestamp()
        protobuf_timestamp.FromDatetime(timestamp)
        event = pb.Event(
            event_id="event-1",
            revision=1,
            source_id="camera-1",
            media_kind=pb.MEDIA_KIND_VIDEO,
            origin=pb.EVENT_ORIGIN_KEEPPEEK,
            event_type="person",
            start_time=protobuf_timestamp,
            source_session_id="camera-1:0",
        )
        attachment = EventAttachment(
            attachment_id="evidence-1",
            attachment_type="snapshot",
            content_type="image/jpeg",
            timestamp=timestamp,
            payload=b"j" * (64 * 1024 + 1),
        )
        publisher = AtomicEventPublisher(request, send)

        await publisher.publish(event, attachment)
        await publisher.publish(event, attachment)

        assert [request.event_publication_command.WhichOneof("action") for request in requests] == [
            "start",
            "commit",
            "start",
        ]
        assert len(messages) == 2
        chunks = [message.event.attachment for message in messages]
        assert [chunk.chunk_index for chunk in chunks] == [0, 1]
        assert all(chunk.chunk_count == 2 for chunk in chunks)
        assert b"".join(chunk.payload for chunk in chunks) == attachment.payload
        start = requests[0].event_publication_command.start
        assert start.event.attachments[0].attachment_id == attachment.attachment_id
        assert start.event.attachments[0].byte_len == len(attachment.payload)
        assert start.event.attachments[0].timestamp.ToDatetime(tzinfo=UTC) == timestamp
        assert chunks[0].publication_id == start.publication_id
        assert chunks[0].timestamp.ToDatetime(tzinfo=UTC) == timestamp

    asyncio.run(scenario())
