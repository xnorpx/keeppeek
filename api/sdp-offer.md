<!-- SPDX-License-Identifier: MIT -->

# SDP Offer Contract

`POST /create` accepts an SDP offer only when the request and offer conform to this
contract. The server validates the offer before allocating a session and returns a structured
error when it rejects the offer.

## Compressed request

The complete UTF-8 JSON `CreateRequest` body is gzip-compressed. The request must include:

```http
Content-Type: application/json
Content-Encoding: gzip
```

The gzip coding applies to the whole JSON body, including `offer.sdp`; it is not a compressed
string embedded in an otherwise uncompressed JSON body. The server does not accept an
uncompressed fallback.

## Compressed answer

A successful `201` response gzip-compresses the complete UTF-8 JSON `CreateResponse` body,
including `answer.sdp`. The response includes:

```http
Content-Type: application/json
Content-Encoding: gzip
```

The client decompresses the whole JSON body; `answer.sdp` is not a separately compressed
string inside uncompressed JSON. KeepPeek does not return an uncompressed create success body.
Error responses are not required to use gzip.

## ICE and offer/answer

KeepPeek is always the answerer. The client or endpoint is always the offerer. `POST /create`
is the only SDP exchange: the request carries the gzip-compressed offer and the `201` response
carries the gzip-compressed answer. This API has no trickle-ICE, ICE-restart, or candidate
endpoint, and the control channel does not carry ICE candidates.

KeepPeek runs ICE Lite. The answer includes `a=ice-lite` and the server host candidates the
client uses to connect. The offerer does not need to include any `a=candidate` lines. A client
that is still gathering local candidates may send the offer immediately; KeepPeek does not wait
for, request, or accept later candidates. Client candidates present in the offer are unused.

## Data transport

The offer must contain exactly one SCTP-capable `m=application` section for WebRTC data
channels. It must have an SDP `a=mid` distinct from every RTP media MID in the offer.

The client and server each create these three channels locally before offer/answer exchange:

| SCTP stream ID | Label             | Negotiated | Delivery                           | Payload                                    |
| -------------- | ----------------- | ---------- | ---------------------------------- | ------------------------------------------ |
| `0`            | `control-channel` | `true`     | Ordered and reliable               | Binary protobuf `ControlEnvelope` messages |
| `1`            | `reliable-data`   | `true`     | Ordered and reliable               | Binary `Message` messages                  |
| `2`            | `unreliable-data` | `true`     | Unordered with `maxRetransmits: 0` | Binary `Message` messages                  |

The SCTP stream ID namespace is separate from the SDP MID namespace. A data-channel stream ID
and an SDP MID may happen to contain the same characters, but they identify unrelated protocol
objects and clients must not infer any relationship between them.

Pre-negotiated channel IDs, labels, and delivery settings are local WebRTC configuration;
standard SDP advertises the SCTP association but does not carry this three-channel mapping.
The server can reject a missing or invalid SCTP `m=application` section while processing the
offer. It cannot determine from standard SDP alone whether the client created IDs `0` through
`2` with the required labels. A client that fails to establish the required local channels
must be rejected when SCTP becomes available. An HTTP-time check of these exact local channel
settings would require a separate request manifest, which this API does not define. The binary
data and media frame definitions are in [webrtc.md](webrtc.md).

## RTP StreamId and opaque MIDs

This API does not renegotiate SDP. The client's initial `POST /create` offer is the only
allocation of RTP `StreamId` values for the session. A later `Subscribe` cannot add receive
`StreamId` values, and KeepPeek never asks the client to offer additional m-lines. Extra RTP
streams beyond those represented in the offer are unavailable on this connection; the client
uses data-channel media or opens another session.

Each RTP `m=` section creates one session-local `StreamId`. Its value is the section's `a=mid`,
selected by the offerer as an opaque, case-sensitive SDP token. KeepPeek preserves that exact
value in the corresponding answer section and in every `RtpDelivery`; it never converts a MID to
an integer, assigns meaning to its characters, renumbers it, or invents a replacement.

`StreamId` is the RTP transport identifier backed by SDP `a=mid`. It is distinct from protobuf
fields named `media_kind`, which identify logical KeepPeek streams such as a camera's main, sub,
audio, or derived stream. It is also distinct from SCTP stream IDs used by data channels.

The protobuf wire fields remain `RtpDelivery.mid` and `StartPublication.rtp_mid` because they
serialize the SDP MID directly. Implementations convert those strings to their local `StreamId`
type at the protocol boundary and use `StreamId` everywhere in allocation and binding state.

A browser client creates every transceiver it may need before `createOffer`, applies the offer
with `setLocalDescription`, and then records the non-null `RTCRtpTransceiver.mid` values. It sends
the exact `peerConnection.localDescription` SDP to KeepPeek. Browser clients do not rewrite SDP
solely to force chosen MID values. A native client that constructs SDP may choose any valid,
unique SDP tokens, but it follows the same opaque-value rules after sending the offer.

The offer may contain up to 256 RTP media sections in total. Every RTP section is optional, has
media kind `audio` or `video`, and uses exactly one of these directions from the client's point
of view:

| Media | Offer direction |                        Limit | Purpose                                |
| ----- | --------------- | ---------------------------: | -------------------------------------- |
| Audio | `sendonly`      |                            1 | Client audio sent to KeepPeek          |
| Video | `sendonly`      |                            1 | Client video sent to KeepPeek          |
| Audio | `recvonly`      | Within the 256-section total | Audio sent from KeepPeek to the client |
| Video | `recvonly`      | Within the 256-section total | Video sent from KeepPeek to the client |

An unused RTP media section is omitted rather than offered as `inactive` or `sendrecv`. Every SDP
MID is unique across the RTP and application sections. The application MID is otherwise just as
opaque as an RTP MID.

After accepting the offer, KeepPeek builds an immutable session-local `StreamId` registry from the
MID, media kind, offer direction, and negotiated codecs of every RTP section. A `recvonly`
`StreamId` can be assigned to one server-to-client subscription. A `sendonly` `StreamId` can be
bound to one client-to-server publication. `StreamId` ownership can change only through
successful control messages; KeepPeek never infers a source, logical stream, variant, or
presentation from the MID text.

Clients keep the corresponding `StreamId -> transceiver` registry. `subscription_id` and
`publication_id` identify control operations, `source_session_id` and `media_kind` identify media,
`variant_id` identifies a concrete format, and `media_kind` groups synchronized media. MID
values do none of those jobs.

## Send-video simulcast

The offer's optional `sendonly` video section may contain one source with one to four `send`
simulcast layers. Each declared send layer must have a matching send RID. The offer must not
declare a receive simulcast direction on that section.

SDP can validate that all simulcast layers are on the one send-video section; it cannot prove
which physical camera or capture source produced them. The one-source requirement and
one-section limit mean the client must use data-channel media or another connection for
additional video sources.

## Offer rejection

Offer-related failures return a JSON `OfferValidationProblem`:

`error` is always a nonempty human-readable string that says what was wrong with the submitted
offer. It must identify the failed input requirement rather than return a generic rejection.
`reason` is the stable machine-readable code for clients that need programmatic handling.

When KeepPeek cannot accept an otherwise decoded SDP offer after topology checks, it returns
HTTP `400` with reason `sdp_offer_rejected`. `error` explains that rejection in human-readable
form. It may include SDP-stack text and is not a stable programmatic string.

```json
{
  "error": "MID camera-video uses sendrecv; only sendonly or recvonly is allowed",
  "reason": "media_direction_invalid",
  "mid": "camera-video"
}
```

| HTTP status | Reason                                 | Condition                                                                                    |
| ----------- | -------------------------------------- | -------------------------------------------------------------------------------------------- |
| `415`       | `offer_not_gzip_encoded`               | `Content-Encoding` is missing or is not exactly `gzip`                                       |
| `400`       | `offer_gzip_invalid`                   | The request body cannot be decompressed as gzip                                              |
| `400`       | `offer_json_invalid`                   | The decompressed body is not a valid `CreateRequest`                                         |
| `400`       | `offer_sdp_invalid`                    | The request does not contain a valid SDP offer                                               |
| `400`       | `sdp_offer_rejected`                   | KeepPeek rejects the decoded SDP offer after topology checks; `error` explains the rejection |
| `422`       | `application_transport_missing`        | No SCTP-capable application section is present                                               |
| `422`       | `application_transport_invalid`        | The application section is not valid for WebRTC data channels                                |
| `422`       | `media_mid_duplicate`                  | An SDP MID appears in more than one media or application section                             |
| `422`       | `media_mid_count_exceeded`             | More than 256 RTP media MIDs are offered                                                     |
| `422`       | `media_kind_invalid`                   | An RTP section is not audio or video                                                         |
| `422`       | `media_direction_invalid`              | An RTP section is not exactly `sendonly` or `recvonly`                                       |
| `422`       | `media_send_media_kind_limit_exceeded` | More than one sendonly section is offered for one media kind                                 |
| `422`       | `video_simulcast_invalid`              | The sendonly video section has an invalid simulcast declaration                              |
| `422`       | `video_simulcast_layer_limit_exceeded` | The sendonly video section declares more than four send layers                               |

## MID interoperability acceptance

The MID implementation is complete when these cases pass without client-specific protocol
branches:

1. Offers produced by every supported browser are accepted without rewriting browser-assigned
   MIDs solely to fit a KeepPeek numbering scheme.
2. A native offer using nonnumeric MIDs is accepted when its sections otherwise satisfy this
   contract, and the answer preserves every exact value.
3. A video `recvonly` section with MID `"0"` and an audio `recvonly` section with MID `"1"` are
   classified by their SDP media kind and direction, not by numeric parity.
4. An RTP subscription is assigned only to an unbound compatible `recvonly` `StreamId`, and
   `RtpDelivery.mid` exactly matches the offer.
5. An RTP publication succeeds only when `StartPublication.rtp_mid` names an unbound compatible
   `sendonly` `StreamId` from the same offer; unknown, incompatible, and occupied values return their
   documented publication errors.
6. Replacing a subscription keeps its MID when compatible. A successful unsubscribe or terminal
   publication state releases the `StreamId`, and any later binding is announced explicitly before
   media is treated as belonging to the new operation.
7. Reconnection creates a new `StreamId` registry. Neither peer carries bindings from an earlier
   HTTP or WebRTC session into the replacement connection.
8. Audio/video synchronization uses `media_kind`; changing MID spelling or allocation order
   cannot change which streams belong to one presentation.
