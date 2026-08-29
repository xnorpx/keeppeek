# SPDX-License-Identifier: AGPL-3.0-only

import asyncio
import gzip

import pytest

from keeppeek_client import ProtocolError, decode_session_body, read_stream_limited


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
