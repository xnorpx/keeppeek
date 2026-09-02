# SPDX-License-Identifier: AGPL-3.0-only

import asyncio
import gzip

import pytest

from generated import webrtc_pb2 as pb
from keeppeek_client import (
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
