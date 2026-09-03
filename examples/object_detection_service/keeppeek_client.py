# SPDX-License-Identifier: AGPL-3.0-only
"""Typed aiortc client for KeepPeek's HTTP and protobuf WebRTC API."""

import asyncio
import gzip
import hashlib
import io
import ipaddress
import json
import logging
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Literal, Protocol, cast
from urllib.parse import urlsplit

import aiohttp
from aiortc import RTCPeerConnection, RTCSessionDescription
from google.protobuf import timestamp_pb2
from google.protobuf.message import DecodeError as ProtobufDecodeError

from detection_pipeline import CodecName, ServiceError
from generated import webrtc_pb2 as pb

LOGGER = logging.getLogger(__name__)
EVENT_PUBLICATION_CAPABILITY = "keeppeek.event-publication.v1"
CONTROL_TIMEOUT_SECONDS = 10.0
CONNECT_TIMEOUT_SECONDS = 15.0
MAX_SESSION_RESPONSE_BYTES = 4 * 1024 * 1024
LiteralStream = Literal["auto", "main", "sub"]


class ProtocolError(ServiceError):
    """KeepPeek rejected or violated the documented protocol."""


class SessionLostError(ServiceError):
    """The active KeepPeek WebRTC session ended."""


@dataclass(frozen=True)
class ActiveSubscription:
    source_id: str
    source_session_id: str
    stream_id: str
    stream_binding_id: str
    codec: CodecName


@dataclass(frozen=True)
class ActiveMediaConfiguration:
    stream_binding_id: str
    revision: int
    codec: CodecName
    width: int
    height: int
    decoder_config: bytes


class MediaConfigurationTracker:
    def __init__(self) -> None:
        self.current: ActiveMediaConfiguration | None = None

    def initialize(self, delivery: pb.MediaDataDelivery) -> ActiveMediaConfiguration:
        configuration = parse_media_configuration(delivery)
        self.current = configuration
        return configuration

    def apply(self, update: pb.MediaDataConfiguration) -> ActiveMediaConfiguration:
        current = self.current
        if current is None:
            raise ProtocolError("KeepPeek sent media configuration before subscription")
        configuration = parse_media_configuration(update)
        if configuration.stream_binding_id != current.stream_binding_id:
            raise ProtocolError("KeepPeek changed the media configuration binding")
        if configuration.revision <= current.revision:
            raise ProtocolError("KeepPeek media configuration revision did not increase")
        self.current = configuration
        return configuration

    def codec_for(self, frame: pb.VideoDataFrame) -> CodecName | None:
        current = self.current
        if (
            current is None
            or frame.stream_binding_id != current.stream_binding_id
            or frame.configuration_revision != current.revision
        ):
            return None
        return current.codec


def parse_media_configuration(
    value: pb.MediaDataDelivery | pb.MediaDataConfiguration,
) -> ActiveMediaConfiguration:
    if not value.stream_binding_id or value.configuration_revision < 1:
        raise ProtocolError("KeepPeek returned invalid media configuration identity")
    if not value.HasField("codec") or not value.HasField("format"):
        raise ProtocolError("KeepPeek returned incomplete media configuration")
    codec_name = value.codec.name.casefold()
    if codec_name not in {"h264", "h265"}:
        raise ProtocolError(f"KeepPeek selected unsupported codec {codec_name}")
    if value.format.WhichOneof("format") != "video":
        raise ProtocolError("KeepPeek returned a non-video media configuration")
    video = value.format.video
    if video.width < 1 or video.height < 1 or not video.decoder_config:
        raise ProtocolError("KeepPeek returned a non-decodable video configuration")
    return ActiveMediaConfiguration(
        stream_binding_id=value.stream_binding_id,
        revision=value.configuration_revision,
        codec=cast(CodecName, codec_name),
        width=video.width,
        height=video.height,
        decoder_config=bytes(video.decoder_config),
    )


FrameHandler = Callable[[pb.VideoDataFrame, CodecName, int], None]
RequestSender = Callable[[pb.Request], Awaitable[pb.Response]]


@dataclass(frozen=True)
class EventAttachment:
    attachment_id: str
    attachment_type: str
    content_type: str
    timestamp: datetime
    payload: bytes


class AtomicEventPublisher:
    """Publishes one deterministic event revision and attachment atomically."""

    CHUNK_BYTES = 64 * 1024
    MAXIMUM_CHUNKS = 256

    def __init__(
        self,
        request: RequestSender,
        send: Callable[[pb.Message], None],
    ) -> None:
        self._request = request
        self._send = send

    async def publish(self, event: pb.Event, attachment: EventAttachment) -> None:
        if event.attachments:
            raise ProtocolError("event already contains attachment descriptors")
        if not attachment.payload:
            raise ProtocolError("event attachment is empty")
        if attachment.timestamp.tzinfo is None:
            raise ProtocolError("event attachment timestamp must include a timezone")
        chunk_count = (len(attachment.payload) + self.CHUNK_BYTES - 1) // self.CHUNK_BYTES
        if chunk_count > self.MAXIMUM_CHUNKS:
            raise ProtocolError("event attachment requires too many chunks")
        publication_id = publication_identity(event)
        publication_event = event_with_attachment(event, attachment)
        response = await self._request(
            pb.Request(
                event_publication_command=pb.EventPublicationCommand(
                    start=pb.StartEventPublication(
                        publication_id=publication_id,
                        event=publication_event,
                        attachment_channel=pb.DATA_CHANNEL_KIND_RELIABLE_DATA,
                    )
                )
            )
        )
        state = publication_state(response, publication_id, event)
        if state.status == pb.EVENT_PUBLICATION_STATUS_COMMITTED:
            return
        if state.status != pb.EVENT_PUBLICATION_STATUS_ACCEPTING_ATTACHMENTS:
            raise ProtocolError("KeepPeek returned an invalid event publication state")
        if (
            len(attachment.payload) > state.max_attachment_bytes
            or len(attachment.payload) > state.max_event_attachment_bytes
        ):
            raise ProtocolError("event attachment exceeds the server publication limit")
        timestamp = timestamp_pb2.Timestamp()
        timestamp.FromDatetime(attachment.timestamp.astimezone(UTC))
        for chunk_index in range(chunk_count):
            start = chunk_index * self.CHUNK_BYTES
            payload = attachment.payload[start : start + self.CHUNK_BYTES]
            self._send(
                pb.Message(
                    event=pb.EventMessage(
                        attachment=pb.EventAttachmentChunk(
                            publication_id=publication_id,
                            event_id=event.event_id,
                            revision=event.revision,
                            attachment_id=attachment.attachment_id,
                            attachment_type=attachment.attachment_type,
                            content_type=attachment.content_type,
                            ordinal=0,
                            timestamp=timestamp,
                            sequence=1,
                            chunk_index=chunk_index,
                            chunk_count=chunk_count,
                            payload=payload,
                        )
                    )
                )
            )
        response = await self._request(
            pb.Request(
                event_publication_command=pb.EventPublicationCommand(
                    commit=pb.CommitEventPublication(publication_id=publication_id)
                )
            )
        )
        state = publication_state(response, publication_id, event)
        if state.status != pb.EVENT_PUBLICATION_STATUS_COMMITTED:
            raise ProtocolError("KeepPeek did not confirm durable event publication")


def publication_identity(event: pb.Event) -> str:
    if not event.event_id or event.revision < 1:
        raise ProtocolError("event publication identity is invalid")
    digest = hashlib.sha256(f"{event.event_id}\0{event.revision}".encode()).hexdigest()
    return f"event-{digest}"


def event_with_attachment(event: pb.Event, attachment: EventAttachment) -> pb.Event:
    timestamp = timestamp_pb2.Timestamp()
    timestamp.FromDatetime(attachment.timestamp.astimezone(UTC))
    result = pb.Event()
    result.CopyFrom(event)
    result.attachments.append(
        pb.EventAttachmentDescriptor(
            attachment_id=attachment.attachment_id,
            attachment_type=attachment.attachment_type,
            content_type=attachment.content_type,
            byte_len=len(attachment.payload),
            ordinal=0,
            timestamp=timestamp,
        )
    )
    result.canonical_attachment_id = attachment.attachment_id
    result.bounding_box_attachment_id = attachment.attachment_id
    return result


def publication_state(
    response: object, publication_id: str, event: pb.Event
) -> pb.EventPublicationState:
    if not isinstance(response, pb.Response):
        raise ProtocolError("KeepPeek returned an invalid publication response")
    if response.ok.WhichOneof("result") != "event_publication_state":
        raise ProtocolError("KeepPeek publication response omitted its state")
    state = response.ok.event_publication_state
    if (
        state.publication_id != publication_id
        or state.event_id != event.event_id
        or state.revision != event.revision
        or state.attachment_channel != pb.DATA_CHANNEL_KIND_RELIABLE_DATA
    ):
        raise ProtocolError("KeepPeek changed the event publication identity")
    return state


class AsyncReader(Protocol):
    async def read(self, n: int = -1) -> bytes: ...


async def read_stream_limited(stream: AsyncReader, max_bytes: int) -> bytes:
    output = bytearray()
    while True:
        chunk = await stream.read(min(64 * 1024, max_bytes + 1 - len(output)))
        if not chunk:
            return bytes(output)
        output.extend(chunk)
        if len(output) > max_bytes:
            raise ProtocolError(f"KeepPeek session response exceeds {max_bytes} bytes")


def decode_session_body(body: bytes, max_bytes: int) -> bytes:
    if not body.startswith(b"\x1f\x8b"):
        if len(body) > max_bytes:
            raise ProtocolError(f"KeepPeek session response exceeds {max_bytes} bytes")
        return body
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(body)) as stream:
            decoded = stream.read(max_bytes + 1)
    except (EOFError, OSError) as error:
        raise ProtocolError("KeepPeek returned invalid gzip session data") from error
    if len(decoded) > max_bytes:
        raise ProtocolError(f"KeepPeek decompressed response exceeds {max_bytes} bytes")
    return decoded


def _mapping(value: object, name: str) -> Mapping[str, object]:
    if not isinstance(value, dict) or not all(isinstance(key, str) for key in value):
        raise ProtocolError(f"KeepPeek returned an invalid {name}")
    return cast(dict[str, object], value)


def _required_string(mapping: Mapping[str, object], key: str) -> str:
    value = mapping.get(key)
    if not isinstance(value, str) or not value:
        raise ProtocolError(f"KeepPeek response is missing {key}")
    return value


def loopback_hostname(hostname: str | None) -> bool:
    if hostname == "localhost":
        return True
    if hostname is None:
        return False
    try:
        return ipaddress.ip_address(hostname).is_loopback
    except ValueError:
        return False


class KeepPeekClient:
    def __init__(
        self,
        base_url: str,
        access_key: str,
        source_id: str,
        stream: LiteralStream,
        generation: int,
        frame_handler: FrameHandler,
    ) -> None:
        parsed = urlsplit(base_url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise ValueError("KeepPeek URL must be an absolute HTTP or HTTPS URL")
        if parsed.username is not None or parsed.password is not None:
            raise ValueError("KeepPeek URL must not contain credentials")
        if not access_key and not loopback_hostname(parsed.hostname):
            raise ValueError("KeepPeek access key is required")
        if not source_id:
            raise ValueError("KeepPeek source ID is required")
        self._base_url = base_url.rstrip("/")
        self._access_key = access_key or None
        self._source_id = source_id
        self._stream = stream
        self._generation = generation
        self._frame_handler = frame_handler
        self._peer = RTCPeerConnection()
        # KeepPeek requires these exact pre-negotiated IDs and reliability modes.
        self._control = self._peer.createDataChannel(
            "control-channel", negotiated=True, id=0, ordered=True
        )
        self._reliable = self._peer.createDataChannel(
            "reliable-data", negotiated=True, id=1, ordered=True
        )
        self._unreliable = self._peer.createDataChannel(
            "unreliable-data",
            negotiated=True,
            id=2,
            ordered=False,
            maxRetransmits=0,
        )
        self._control_open = asyncio.Event()
        self._reliable_open = asyncio.Event()
        self._unreliable_open = asyncio.Event()
        self._capabilities_ready = asyncio.Event()
        self._lost = asyncio.Event()
        self._pending: dict[int, asyncio.Future[pb.Response]] = {}
        self._next_request_id = 1
        self._capabilities: pb.ServerCapabilities | None = None
        self._subscription: ActiveSubscription | None = None
        self._media_configuration = MediaConfigurationTracker()
        self._event_publisher = AtomicEventPublisher(self._request, self._send_reliable_message)
        self._session_id: str | None = None
        self._closed = False
        self._control.on("open", self._control_open.set)
        self._reliable.on("open", self._reliable_open.set)
        self._unreliable.on("open", self._unreliable_open.set)
        self._control.on("message", self._on_control_message)
        self._reliable.on("message", self._on_data_message)
        self._peer.on("connectionstatechange", self._on_connection_state_change)

    @property
    def generation(self) -> int:
        return self._generation

    @property
    def subscription(self) -> ActiveSubscription:
        if self._subscription is None:
            raise RuntimeError("KeepPeek media subscription is not active")
        return self._subscription

    async def connect(self) -> ActiveSubscription:
        offer = await self._peer.createOffer()
        await self._peer.setLocalDescription(offer)
        answer = await self._create_session(self._peer.localDescription)
        await self._peer.setRemoteDescription(answer)
        await self._wait_for(self._control_open, "control data channel")
        await self._wait_for(self._reliable_open, "reliable data channel")
        await self._wait_for(self._unreliable_open, "unreliable data channel")
        await self._wait_for(self._capabilities_ready, "ServerCapabilities")
        capabilities = self._capabilities
        if capabilities is None:
            raise ProtocolError("KeepPeek did not provide ServerCapabilities")
        source = self._select_source(capabilities)
        subscription = await self._subscribe(source)
        self._subscription = subscription
        LOGGER.info(
            "subscribed source_id=%s stream=%s codec=%s",
            subscription.source_id,
            subscription.stream_id,
            subscription.codec,
        )
        return subscription

    async def wait_until_lost(self) -> None:
        await self._lost.wait()

    async def publish_event(self, event: pb.Event, generation: int) -> None:
        subscription = self.subscription
        if generation != self._generation or self._lost.is_set():
            raise SessionLostError("detection belongs to an inactive KeepPeek session")
        if (
            event.source_id != subscription.source_id
            or event.source_session_id != subscription.source_session_id
        ):
            raise ProtocolError("detection identity does not match the active media subscription")
        last_error: Exception | None = None
        for attempt in range(3):
            try:
                await self._request(pb.Request(publish_event=pb.PublishEvent(event=event)))
                return
            except (TimeoutError, SessionLostError) as error:
                last_error = error
                if self._lost.is_set() or attempt == 2:
                    break
                await asyncio.sleep(0.2 * (attempt + 1))
        raise SessionLostError("unable to confirm detection publication") from last_error

    async def publish_event_with_attachment(
        self, event: pb.Event, attachment: EventAttachment, generation: int
    ) -> None:
        subscription = self.subscription
        if generation != self._generation or self._lost.is_set():
            raise SessionLostError("detection belongs to an inactive KeepPeek session")
        if (
            event.source_id != subscription.source_id
            or event.source_session_id != subscription.source_session_id
        ):
            raise ProtocolError("detection identity does not match the active media subscription")
        await self._event_publisher.publish(event, attachment)

    def _send_reliable_message(self, message: pb.Message) -> None:
        if self._lost.is_set() or self._reliable.readyState != "open":
            raise SessionLostError("KeepPeek reliable data channel is unavailable")
        self._reliable.send(message.SerializeToString())

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        session_id = self._session_id
        self._session_id = None
        if session_id is not None:
            try:
                await self._delete_session(session_id)
            except (aiohttp.ClientError, TimeoutError):
                LOGGER.debug("unable to delete ended KeepPeek session")
        await self._peer.close()
        self._signal_lost("KeepPeek session closed")

    async def _create_session(self, offer: RTCSessionDescription) -> RTCSessionDescription:
        body = gzip.compress(
            json.dumps({"offer": {"type": offer.type, "sdp": offer.sdp}}).encode("utf-8")
        )
        headers = {
            "Content-Encoding": "gzip",
            "Content-Type": "application/json",
        }
        if self._access_key is not None:
            headers["Authorization"] = f"Bearer {self._access_key}"
        timeout = aiohttp.ClientTimeout(total=CONNECT_TIMEOUT_SECONDS)
        try:
            async with (
                aiohttp.ClientSession(timeout=timeout) as session,
                session.post(f"{self._base_url}/create", data=body, headers=headers) as response,
            ):
                if response.status != 201:
                    raise ProtocolError(
                        f"KeepPeek session creation failed with HTTP {response.status}; "
                        "check the URL, access key, and server logs"
                    )
                response_body = await read_stream_limited(
                    response.content, MAX_SESSION_RESPONSE_BYTES
                )
        except aiohttp.ClientError as error:
            raise ProtocolError(
                "unable to connect to KeepPeek; check the URL and that the server is running"
            ) from error
        response_body = decode_session_body(response_body, MAX_SESSION_RESPONSE_BYTES)
        try:
            decoded = cast(object, json.loads(response_body.decode("utf-8")))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ProtocolError("KeepPeek returned invalid session JSON") from error
        create = _mapping(decoded, "create response")
        answer = _mapping(create.get("answer"), "SDP answer")
        self._session_id = _required_string(create, "session_id")
        if _required_string(answer, "type") != "answer":
            raise ProtocolError("KeepPeek returned a non-answer SDP description")
        return RTCSessionDescription(sdp=_required_string(answer, "sdp"), type="answer")

    async def _delete_session(self, session_id: str) -> None:
        headers = {"Content-Type": "application/json"}
        if self._access_key is not None:
            headers["Authorization"] = f"Bearer {self._access_key}"
        timeout = aiohttp.ClientTimeout(total=5)
        async with (
            aiohttp.ClientSession(timeout=timeout) as session,
            session.post(
                f"{self._base_url}/delete",
                json={"session_id": session_id},
                headers=headers,
            ) as response,
        ):
            if response.status not in {204, 404}:
                raise ProtocolError(f"KeepPeek session deletion failed with HTTP {response.status}")

    async def _wait_for(self, event: asyncio.Event, name: str) -> None:
        try:
            await asyncio.wait_for(event.wait(), timeout=CONNECT_TIMEOUT_SECONDS)
        except TimeoutError as error:
            raise ProtocolError(f"timed out waiting for KeepPeek {name}") from error
        if self._lost.is_set():
            raise SessionLostError(f"KeepPeek session ended before {name} became ready")

    def _select_source(self, capabilities: pb.ServerCapabilities) -> pb.SourceSession:
        if EVENT_PUBLICATION_CAPABILITY not in capabilities.capability_ids:
            raise ProtocolError(
                f"KeepPeek does not advertise required capability {EVENT_PUBLICATION_CAPABILITY}"
            )
        source = next(
            (
                candidate
                for candidate in capabilities.source_sessions
                if candidate.source_id == self._source_id
            ),
            None,
        )
        if source is None:
            known = any(camera.source_id == self._source_id for camera in capabilities.cameras)
            state = "offline" if known else "unknown"
            raise ProtocolError(f"configured KeepPeek source is {state}: {self._source_id}")
        if not source.HasField("video"):
            raise ProtocolError("configured KeepPeek source has no active video stream")
        event_types = {event_type.event_type for event_type in source.event_types}
        if not event_types.intersection({"person", "vehicle"}):
            raise ProtocolError("configured KeepPeek source accepts no detector event type")
        eligible = [
            variant
            for variant in source.video.variants
            if pb.DELIVERY_TRANSPORT_RELIABLE_DATA in variant.delivery_transports
            and variant.HasField("codec")
            and variant.codec.name.casefold() in {"h264", "h265"}
        ]
        if self._stream != "auto" and not any(
            variant.variant_id == self._stream for variant in eligible
        ):
            raise ProtocolError(
                f"configured stream {self._stream} has no reliable H.264/H.265 variant"
            )
        if not eligible:
            raise ProtocolError("configured source has no reliable H.264/H.265 variant")
        return source

    async def _subscribe(self, source: pb.SourceSession) -> ActiveSubscription:
        exact_variant = "" if self._stream == "auto" else self._stream
        quality = pb.VIDEO_QUALITY_LOW if not exact_variant else pb.VIDEO_QUALITY_AUTO
        response = await self._request(
            pb.Request(
                subscribe_media=pb.SubscribeMedia(
                    subscription_id="object-detector-input",
                    source_session_id=source.source_session_id,
                    kind=pb.MEDIA_KIND_VIDEO,
                    requested_delivery_transport=pb.DELIVERY_TRANSPORT_RELIABLE_DATA,
                    video_quality=quality,
                    variant_id=exact_variant,
                )
            )
        )
        if response.ok.WhichOneof("result") != "subscription_result":
            raise ProtocolError("KeepPeek subscription response omitted its media result")
        result = response.ok.subscription_result
        if result.WhichOneof("delivery") != "media_data":
            raise ProtocolError("KeepPeek did not accept reliable media delivery")
        delivery = result.media_data
        if delivery.channel != pb.DATA_CHANNEL_KIND_RELIABLE_DATA:
            raise ProtocolError("KeepPeek returned the wrong media data channel")
        configuration = self._media_configuration.initialize(delivery)
        return ActiveSubscription(
            source_id=source.source_id,
            source_session_id=source.source_session_id,
            stream_id=result.selected_variant_id,
            stream_binding_id=delivery.stream_binding_id,
            codec=configuration.codec,
        )

    async def _request(self, request: pb.Request) -> pb.Response:
        if self._lost.is_set() or self._control.readyState != "open":
            raise SessionLostError("KeepPeek control channel is unavailable")
        # Client requests use odd IDs; server-originated control requests use even IDs.
        request_id = self._next_request_id
        self._next_request_id += 2
        request.request_id = request_id
        future: asyncio.Future[pb.Response] = asyncio.get_running_loop().create_future()
        self._pending[request_id] = future
        envelope = pb.ControlEnvelope(request=request)
        try:
            self._control.send(envelope.SerializeToString())
            response = await asyncio.wait_for(future, timeout=CONTROL_TIMEOUT_SECONDS)
        finally:
            self._pending.pop(request_id, None)
        if response.WhichOneof("result") == "error":
            message = response.error.message or "unknown error"
            raise ProtocolError(f"KeepPeek rejected request {request_id}: {message}")
        if response.WhichOneof("result") != "ok":
            raise ProtocolError(f"KeepPeek returned an invalid response for request {request_id}")
        return response

    def _on_control_message(self, value: object) -> None:
        if not isinstance(value, bytes):
            self._signal_lost("KeepPeek sent a non-binary control message")
            return
        envelope = pb.ControlEnvelope()
        try:
            envelope.ParseFromString(value)
        except ProtobufDecodeError:
            self._signal_lost("KeepPeek sent malformed control protobuf")
            return
        kind = envelope.WhichOneof("message")
        if kind == "response":
            future = self._pending.get(envelope.response.request_id)
            if future is not None and not future.done():
                response = pb.Response()
                response.CopyFrom(envelope.response)
                future.set_result(response)
        elif kind == "notification":
            notification_kind = envelope.notification.WhichOneof("event")
            if notification_kind == "initial_capabilities":
                capabilities = pb.ServerCapabilities()
                capabilities.CopyFrom(envelope.notification.initial_capabilities)
                self._capabilities = capabilities
                self._capabilities_ready.set()
                subscription = self._subscription
                if subscription is not None and not any(
                    source.source_session_id == subscription.source_session_id
                    for source in capabilities.source_sessions
                ):
                    self._signal_lost("subscribed KeepPeek source session ended")
            elif notification_kind == "media_data_configuration":
                try:
                    configuration = self._media_configuration.apply(
                        envelope.notification.media_data_configuration
                    )
                except ProtocolError as error:
                    self._signal_lost(str(error))
                    return
                LOGGER.info(
                    "media configuration updated binding=%s revision=%d codec=%s",
                    configuration.stream_binding_id,
                    configuration.revision,
                    configuration.codec,
                )

    def _on_data_message(self, value: object) -> None:
        if not isinstance(value, bytes):
            return
        message = pb.Message()
        try:
            message.ParseFromString(value)
        except ProtobufDecodeError:
            LOGGER.warning("discarding malformed reliable-data protobuf")
            return
        subscription = self._subscription
        codec = (
            self._media_configuration.codec_for(message.video.frame)
            if message.WhichOneof("message") == "video"
            and message.video.WhichOneof("message") == "frame"
            else None
        )
        if (
            subscription is None
            or codec is None
            or message.WhichOneof("message") != "video"
            or message.video.WhichOneof("message") != "frame"
            or message.video.frame.stream_binding_id != subscription.stream_binding_id
        ):
            return
        self._frame_handler(message.video.frame, codec, self._generation)

    def _on_connection_state_change(self) -> None:
        if self._peer.connectionState in {"closed", "failed", "disconnected"}:
            self._signal_lost(f"KeepPeek peer connection is {self._peer.connectionState}")

    def _signal_lost(self, reason: str) -> None:
        if self._lost.is_set():
            return
        self._lost.set()
        error = SessionLostError(reason)
        for future in self._pending.values():
            if not future.done():
                future.set_exception(error)
