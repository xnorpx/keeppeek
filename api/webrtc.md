# WebRTC Data Channel Protocol

`webrtc.proto` defines messages exchanged after the pre-negotiated WebRTC data
channels open. The server sends a complete `ServerCapabilities` snapshot on the
control channel before sending live-source events, stored-media state, or stream payloads.

## Channels

| SCTP stream ID | Label             | Delivery                       | Payload                           |
| -------------- | ----------------- | ------------------------------ | --------------------------------- |
| `0`            | `control-channel` | Ordered and reliable           | Binary protobuf `ControlEnvelope` |
| `1`            | `reliable-data`   | Ordered and reliable           | Binary protobuf `Message`         |
| `2`            | `unreliable-data` | Unordered, `maxRetransmits: 0` | Binary protobuf `Message`         |

All three channels are pre-negotiated. Both peers create the matching channels locally before
offer/answer exchange. The complete SCTP topology, RTP `StreamId` allocation, and ICE Lite
offer/answer rules are defined in [sdp-offer.md](sdp-offer.md). KeepPeek always answers a
client offer and does not use trickle ICE.

All messages on the `control-channel` are binary protobuf `ControlEnvelope` messages. This avoids
heavy JSON serialization over the unthrottled WebRTC connection and allows native clients to use
zero-copy abstractions. The envelope explicitly models the RPC lifecycle using `Request`, `Response`,
and `Notification` (fire-and-forget). Each binary message on either data channel is one protobuf-encoded
`Message`. Its first `oneof` selects `AudioMessage`, `VideoMessage`, `DataMessage`, `EventMessage`,
`StoredMediaMessage`, `StoredMediaQueryMessage`, or `EventSearchMessage`; that message's nested
`oneof` selects its frame, attachment, fragment, page, search result, or payload subtype. Binary
payload bytes are not base64-expanded.

Every `Request` uses a `request_id`. `Response` messages immediately carry back the exact
same `request_id` to correlate the result. `Notification` messages are server-originated, completely
fire-and-forget state updates, and do not use a `request_id`.

`Response` carries `Ok` for a successful request or `Error` for a rejected request. `Ok` wraps the typed results.

`Ok` acknowledges a successful request and can carry a typed result. `Error` reports a rejected
request with a fixed `ErrorCode` and optional typed error detail. In both messages, the body `request_id` must equal the enclosing
`ControlEnvelope.request_id`.

`ServerCapabilities`, `ConnectionUpdate`, `Event`, `MediaDataConfiguration`,
`PublicationControl`, and unsolicited `PublicationState`, `StoredMediaState`,
`EventPublicationState`, `GroupState`, or `StateStoreWatchUpdate` messages are
server-originated control requests with newly allocated even request IDs.

## Acknowledgements

Every client-originated `Request` receives exactly one `Response`. `Ok` wraps `SubscriptionResult` for a
successful subscription, `StoredMediaQueryDelivery` for a successful timeline query,
`EventSearchDelivery` for a successful event search, and other success types.
Rejected operations use `Error` with their corresponding typed error detail.

`Notification` messages such as `CameraUpdated`, `SourceSessionAdded`, `ConnectionUpdate`, and `Event` are
fire-and-forget. The `control-channel` stream itself provides SCTP-level guaranteed delivery, ordered
transmission, and congestion control, rendering application-level JSON-like ACKs entirely redundant. The client
does not respond to `Notification`s.

The initial `ServerCapabilities` notification is sent when the connection is established over the `control-channel`.

## Capabilities and events

`ServerCapabilities` is a complete snapshot for its receiving connection, not a delta. The server
sends it once after the control channel opens and sends another complete snapshot whenever any
state visible to that connection changes. Each later snapshot replaces the previous snapshot.
Authorization can make snapshots differ between connections, but each snapshot contains every
currently visible source session, media capability, variant, publication capability, camera, and
stored-media source.

Every WebRTC connection owns one server-issued source session. Its ID is returned as
`self_source_session_id` and its `SourceSession` is present in that connection's initial snapshot,
even before it exposes media. A connection that is not allowed to publish receives an empty
`publication_capabilities` list. Other connections see that source session only when policy makes
it visible, normally after at least one media variant becomes active.

`cameras` lists every configured camera, including ones that are currently offline. Each
`CameraInfo` is identity and device capability: `source_id`, display name, manufacturer, model,
firmware, network addresses, `web_url`, and `device_capabilities`. It does not carry fps,
bitrate, reconnect counts, or other numeric health. Those belong on `GET /metrics`.

`CameraInfo.ptz` is how a client learns whether PTZ is available. `ptz.supported` is the
authority. When it is false, KeepPeek rejects every `CameraControlCommand.ptz` for that
`source_id` with `ERROR_CODE_UNSUPPORTED_REQUEST`. KeepPeek advertises support only when camera
discovery reports PTZ and the server owns an executable command transport. `continuous`,
`relative`, `presets`, and `zoom` describe the implemented transport surface. A client shows PTZ
controls from this snapshot and does not probe with rejected commands. A command can still return
`ERROR_CODE_UNAVAILABLE` when the device cannot be reached or authenticated.

### Camera PTZ control

PTZ is a control-channel command, not a media subscription and not an HTTP route. The client
sends `CameraControlCommand` with `ptz` addressed by stable `source_id`. KeepPeek forwards the
verb to the camera over the implemented vendor protocol and replies with `Ok` or `Error`.
`Ok.ptz_result` is used for `list_presets` and otherwise may be empty. Continuous move uses
pan, tilt, and zoom in `-1.0` through `1.0`; `stop` ends motion. Preset list, goto, save, and
delete require `ptz.presets`. There is no PTZ stream, no floor, and no camera-to-camera
forwarding.

Continuous movement is owned by the WebRTC connection that started it. Another connection cannot
replace or stop that movement. Releasing the control sends `stop`; closing the data channel or
WebRTC session also sends `stop` before releasing ownership. The current Reolink transport supports
continuous pan/tilt/zoom, explicit stop, preset list, and preset goto. Relative move and preset
save/delete fail closed with `ERROR_CODE_UNSUPPORTED_REQUEST`.

When a previously advertised media kind or variant is absent from a new snapshot, the server
stops sending its media on the associated RTP MID or data channel. The server does not silently
repurpose that active delivery assignment for another media identity. The client replaces or
unsubscribes the affected subscription before the RTP `StreamId` can bind unrelated media. When a
new variant appears, it is not automatically delivered; an interested client sends a new
`Subscribe` request for its advertised identity.

### Source sessions and media identity

`SourceSession` is the transport-neutral live producer model. KeepPeek assigns every active plain
camera, WebRTC publisher, and group participant a globally unique, opaque `source_session_id`.
Camera protocol adapters and WebRTC ingress bindings terminate at this boundary; subscribers do
not need to know whether media arrived through RTSP, Reo-proto, RTP, or a data channel.

A source session has at most one audio stream and one video stream. Presence of
`SourceSession.audio` or `SourceSession.video` means that media kind is currently available. The
unique live media key is:

```text
(source_session_id, MediaKind)
```

The unique concrete rendition key is:

```text
(source_session_id, MediaKind, variant_id)
```

These are structural keys, not concatenated strings. `source_session_id` is unrelated to the HTTP
session ID returned by `POST /create`, and `MediaKind` is unrelated to the connection-local RTP
`StreamId`. `source_id` is the optional stable identity used to resolve a replacement source
session after a reconnect and to address stored events and media. Stable camera media is selected
as `(source_id, MediaKind)` and resolved through the latest complete server snapshot.

For a plain camera, KeepPeek creates a source session when its media presentation becomes active.
Main, sub, simulcast, transcoded, and alternate-codec renditions of the same camera presentation
are variants of its one video stream rather than separate logical streams. Camera adapters may
span multiple sockets and reconnect a profile without replacing the source session only while
KeepPeek can preserve one normalized presentation timeline. A reconnect or reset that breaks that
continuity removes the old session and creates a new `source_session_id`; the configured
`CameraInfo.source_id` remains unchanged.

For a WebRTC connection, `self_source_session_id` identifies its owned source session. A
`PURPOSE_SOURCE` publication targets that session. Group join can instead provision a group-owned
source session, while an authorized transcoder can target an existing session to add a derived
variant. Closing the owning connection removes its ephemeral source session and triggers complete
replacement snapshots for every connection that could see it.

Each `MediaStreamCapability` contains the concrete variants for one media kind. A variant names
its codec, complete format, delivery transports, nominal bitrate, quality rank, origin, and
lineage. Native camera media has origin `NATIVE` and no lineage. A direct WebRTC publication has
origin `PUBLISHED`; a derived publication with validated input lineage has origin `TRANSCODED`.
Variant IDs are unique within `(source_session_id, MediaKind)` and are never silently reused while
advertised. Audio and video within one source session use the same normalized presentation
timeline; a second presentation requires another source session.

`quality_rank` orders variants within one media kind; a larger nonzero rank means higher intended
quality. `SourceSession.publication_capabilities` declares which media kinds accept client
publications, along with accepted transports, codecs, format limits, bitrate limit, recording
policy, and maximum active variants. At most one publication capability exists per media kind. A
zero format or bitrate limit means no advertised limit, while an empty capability list means the
session accepts no client publications. Start validation remains authoritative when capacity
changes between a snapshot and request.

Each publication capability also declares its recording policy. `PROHIBITED` means KeepPeek will
not record publications for that media kind. `OPTIONAL` allows the publisher to disable,
inherit, or require recording subject to server storage policy. `REQUIRED` means the source policy
requires recording and rejects a publication that explicitly disables it. A source capability is
authoritative; a publisher cannot override retention, evidence, or storage-quota policy merely by
asking to record.

`CodecDescriptor` carries the codec name and signaling or compatibility parameters. The matching
`MediaDataFormat` carries concrete dimensions or audio layout and the binary decoder
configuration used for media-data delivery. A conflict between the two is invalid. For RTP, the
codec descriptor must match negotiated RTP parameters, while KeepPeek derives the concrete format
from validated RTP configuration and the first decodable access units.

Each `EventType` advertises its optional attachments. An `EventAttachmentCapability` identifies
an attachment by logical type and content type, lists its available binary channels, and may
state minimum and maximum counts. A zero maximum means no fixed maximum is advertised; a nonzero
maximum must be at least the minimum. For example, a `motion` event with one required snapshot
advertises minimum and maximum counts of one for `image/jpeg`, while a `story` event can advertise
multiple `story-frame` JPEGs.

`VideoQuality` selects among a source session's video variants by `quality_rank`. `low` selects
the smallest rank and `high` selects the largest, so a single variant resolves both
to that variant. `auto` sorts by `(quality_rank, variant_id)` and, for three or more variants,
selects index `(count - 1) / 2` before applying connection constraints.

A source session advertises generic data payload types directly because payload type names are
unique within that session. Events are selected through an `EventSubscriptionRequest` and
delivered as `Event` messages on `control-channel`, with optional `EventAttachmentChunk` messages
on the accepted binary channels.

A client may send `PublishEvent` for a live source session and event type; the server accepts it
or returns `Error`. Envelope-only `PublishEvent` leaves `subscription_id` and attachments empty.
`Event.source_session_id` names the target live source, `source_id` must match that source when it
has a stable identity, and optional `media_kind` associates the event with that session's audio or
video stream. Events with attachments use `EventPublicationCommand`.

A `StoredMediaSourceCapability` describes persisted media for one stable source ID. Unlike a
`source_session_id`, its `source_id` remains valid while the camera is disconnected and across
camera-session restarts. For camera recordings, it is the configured camera ID stored as
`TimelineEvent.camera_id` in the catalog. Each stored stream advertises its container content type
and available binary delivery channels. Its `data_payloads` advertise timed metadata and event
payload types that can accompany playback or be returned by a timeline query.

Stored-media `media_kind` is a separate persisted-presentation namespace, such as a recorded MP4
profile. It is intentionally not part of live media identity and is not an RTP `StreamId`.

Stored-media capabilities do not include a changing earliest or latest timestamp. A client uses
`QueryStoredMediaTimeline` to discover indexed availability and events for a time range, avoiding
a new complete capability snapshot whenever another fragment is recorded.

### Groups

A group is a named, persistent collection of streams. It serves two purposes at once: it bundles
static camera streams into a saved view, and it can optionally host live participants who exchange
audio, and optionally video, with each other. A group with an audio-only live capability is a voice
channel; one with audio and video is a conference; one with no live capability at all is simply a
camera view. All live groups are full duplex, so a push-to-talk client is just a voice client whose
user interface sends audio only while a button is held.

Groups are defined by server configuration, not over this API. A client can list groups, join one,
and leave it; it cannot create, edit, or delete one. Group definitions therefore change rarely and
independently of any connection, and every client sees the same set. An optional password on the
live capability is the only join restriction; there is no owner, moderator, or per-group permission
model.

Groups have no lifetime of their own. A group exists for as long as it is configured, whether or
not anyone is joined, and survives a server restart. Participants come and go inside a group that
outlives them, and an empty group is still listed.

`GroupCapability` describes one group: its ID, display name, revision, static members, and optional
live capability. `revision` increases whenever the definition changes, so a client holding a cached
copy can tell it is stale and re-list.

#### Members

`GroupMember` names media by stable `(source_id, MediaKind)`. Only static sources are members. A
camera keeps its `source_id` while it is disconnected and across restarts, so a stored group
definition still resolves later. Client publications, transcoded variants, and participant
endpoints can have only an ephemeral `source_session_id`, so a stored reference to one would
dangle on the next reconnect. That is the reason group membership is restricted to static media.

A member reference is resolved, not subscribed. A client reads the group's members, finds each
`source_id` in the current `ServerCapabilities`, and issues ordinary `Subscribe` requests against
the live `source_session_id` and member `MediaKind`. This is the same stable-identity indirection used by the
`keeppeek.media-intent.v1` schema in the [shared state store](../docs/state-store.md). A member whose
camera is currently offline stays in the group and simply has no active source to resolve, which is
why a group listing never doubles as a liveness signal.

#### Live participation

`ListGroups` returns `Ok.group_list` with one `GroupSummary` per group, each carrying the group's
`GroupCapability` and its current `participant_count`. The directory is a point-in-time snapshot
rather than a subscription, so a picker refreshes it on open and after a failed join.
`participant_count` is the only value in a summary that changes without a new `revision`.

Group definitions are deliberately absent from `ServerCapabilities`. That snapshot is a complete
document every client must acknowledge, and groups change on a different schedule than live media
sources; keeping them separate means a configuration edit never forces a capability snapshot to
every connected client, and a camera appearing or disappearing never rewrites the group directory.

`GroupLiveCapability` carries everything needed to join and publish: the fixed audio and optional
video `MediaVariantCapability` entries, allowed publication and
subscription transports, participant limit, independent audio
and video recording policies, participant timeout, and whether a password is required. Each media
profile is an ordinary `MediaVariantCapability`, so packetization details such as an Opus `ptime`
are codec parameters rather than a separate profile message.

A group without a live capability is view-only. `JoinGroup` on one returns
`GROUP_ERROR_CODE_NOT_LIVE`, and the group carries no password or participant settings at
all rather than filling them with values that would never apply.

A group whose `password_required` is true rejects a `JoinGroup` without a password using
`GROUP_ERROR_CODE_PASSWORD_REQUIRED` and a wrong one using `GROUP_ERROR_CODE_PASSWORD_INVALID`.
KeepPeek never returns the password in a capability, summary, or state message, so the directory
stays listable without disclosing how to enter a group.

A client obtains a group's capability from `ListGroups` before joining, which is why `GroupState`
carries only the roster.

When a group advertises both audio and video, each participant's two media kinds belong to that
participant's one source-session timeline. A client pairs them by matching `source_session_id`;
no second presentation identifier exists. An audio-only group omits the video profile.

Joining a group creates one server-owned virtual `SourceSession` for the participant.
`GroupParticipant.source_id` is that participant's stable stored-media identity, while
`source_session_id` exists only for the active membership. The caller publishes to the roster
row whose `participant_id` equals `GroupState.self_participant_id`. The session exposes audio
and, in a video group, video, each with a pre-provisioned variant of
origin `GROUP` matching the live profile exactly.
Its `MediaPublicationCapability` permits only that participant to publish those streams with
purpose `GROUP`; another client cannot claim or publish into the endpoint. A participant endpoint
is itself dynamic, which is why it can never be named as a `GroupMember`.

KeepPeek sends the complete `ServerCapabilities` snapshot containing a newly visible participant
source session before it sends a `GroupState` that references that session. On leave, expiry, or
disconnection it removes the session in a new capability snapshot before sending the resulting
roster state. Only current members receive those virtual source sessions. Members subscribe with
ordinary exact `MediaSubscriptionRequest` messages naming source session, media kind, and variant.
A participant may subscribe to its own active publication through the same path; KeepPeek does
not create a special loopback binding or bypass normal authorization and capacity checks.

`GroupState` is a complete snapshot of the caller's membership and current participants. A command
response reuses the client request ID. An unsolicited state has a new even
request ID and must be acknowledged. The revision increases whenever membership or participant
activity changes, and a client replaces rather than merges a higher revision.
`audio_active` and `video_active` report that KeepPeek currently accepts and routes that medium
for the participant; they are not an audio-level speaking indicator or a client mute setting.

Participants are server-assigned identities. `display_name` is an authorized label, not a
client-controlled caller ID. Local receiver mute is a client playback choice and needs no control
message; a client unsubscribes when it no longer wants a remote stream and leaves the group when it
no longer needs membership.

When a participant's connection is unresponsive for the advertised `participant_timeout_ms`, or
the server default when that value is zero, KeepPeek treats the endpoint as expired. It removes
the virtual source in a complete capability snapshot, then sends an unsolicited higher-revision
roster without that participant. `GROUP_MEMBERSHIP_STATUS_EXPIRED` applies only when KeepPeek can
report the caller's own membership expiry; remote clients reconcile the roster removal. Losing
every participant empties the roster but never removes the group itself.

Live participation is all-to-all at the control plane. KeepPeek fans each published endpoint stream
only to clients that explicitly subscribed to it. It never creates a server-side mixed audio track,
chooses a featured video, or imposes a shared layout. Clients choose their own grid, pinning,
local mute, and retained subscriptions.

### Publishing into a group

A member joins, then starts one `PURPOSE_GROUP` audio publication, and in a video group one
`PURPOSE_GROUP` video publication, against the virtual source on its own `GroupParticipant`
row using that row's `source_session_id`, the applicable `MediaKind`, variant ID, and profile. A group
publication supplies no input subscriptions and sets a nonzero `recording_mode` compatible with
the group's recording policy for that medium.

KeepPeek can return `ACTIVE` for a group publication before its first access unit because the
profile and receiver-ready variant were provisioned at join. A `REQUIRED` recording policy still
waits until the recording path is ready. Audio then routes on its first valid access unit and video
after its first valid keyframe.

Groups are full duplex and KeepPeek arbitrates nothing. Every joined member may publish at the same
time, and simultaneous speech is fanned out unchanged rather than being suppressed, queued, or
mixed. KeepPeek grants no speaking turn and no transmit permission, so a member has nothing to
request before it sends.

Muting and push-to-talk are therefore entirely client-side. A member that does not want to be heard
stops sending access units, or uses DTX, while keeping its publication and binding intact. A
push-to-talk client is an ordinary full-duplex member whose user interface only sends audio while a
button is held; nothing about that behavior reaches the server. Keeping the publication open
between presses matters, because tearing it down and restarting it on every press would pay codec
and recording-path setup cost and clip the start of speech. `audio_active` and `video_active` in
`GroupState` report whether KeepPeek is currently routing that medium for a participant, which
follows from its publication rather than from any permission grant.

RTP is the preferred transport for live group media. Each client's initial offer includes
one browser- or client-assigned `sendonly` audio `StreamId`, one `sendonly` video `StreamId` in a
video group, and enough `recvonly` `StreamId` values of each kind for the remote streams it wants.
Each value is opaque. That offer is the session's complete RTP capacity. Each group publication
names its exact send `StreamId` through `StartPublication.rtp_mid`; the participant's
`source_session_id`, not MID text, pairs its audio and video.
When RTP `StreamId` values are insufficient or unavailable, a group can advertise data-channel transports;
interactive group media uses `UNRELIABLE_DATA` and never reliable ordered delivery, because
retransmission adds head-of-line latency. Capacity is bounded by the group's policy and each
receiver's negotiated `StreamId` values.

A group audio profile normally uses mono Opus at a 48 kHz RTP clock with 20 ms packets,
speech-oriented bitrate, in-band FEC, and DTX declared through `CodecDescriptor`. KeepPeek
validates every publication against the exact profile. A receiver uses a small adaptive jitter
buffer and drops late frames rather than increasing mouth-to-ear latency without bound. The target
mouth-to-ear budget is 100 ms on a healthy local network; it is an operational objective, not a
transport delivery guarantee.

Recording live group media is explicit. `GroupLiveCapability` declares independent audio and video
recording
policies, mirrored by each virtual participant source's publication capability, and every
publication selects a `recording_mode` within them. A `REQUIRED` medium becomes active only after
its recording path is ready and fails rather than continuing unrecorded after a storage error.
When recording is enabled, KeepPeek records each participant's stream separately under that
participant's virtual `source_id`, which is also how timeline queries address it; it never stores
an unauditable mixed track merely because several members joined. Recording a group's static camera
members is unchanged and follows ordinary camera recording policy. Whether a deployment permits or
requires recording participant media is an authorization and retention policy decision, not a
client-side privacy toggle.

### Shared state store

The shared state store is a small durable coordination plane. It holds schema-tagged desired
state, leader leases, user selections, service assignments, and stream intents. It does not hold
media frames, JPEGs, credentials, recording bytes, or the authoritative state of a live media
binding. `ServerCapabilities`, `PublicationState`, `GroupState`, and accepted subscription
results remain the authority for what streams actually exist and can carry media.

Every `StateEntry` has a server-issued owner ID, namespace-local monotonically increasing
revision, update timestamp, optional expiration, schema name, and structured value. Namespace
authorization is server policy. Typical deployments reserve `system/` for KeepPeek, grant
`service/` namespaces to approved service principals, grant `group/` namespaces to authorized
group members, and give users or devices their own private namespaces. A client cannot choose
the owner ID returned by the server.

`PutState` replaces one complete state value. An absent `expected_revision` permits a blind write;
an explicit zero requires the key to be absent; any other explicit value must equal the current
entry revision. `DeleteState` follows the same compare-and-set rule. A revision mismatch returns
`STATE_STORE_ERROR_CODE_CONFLICT` with the current revision, allowing a coordinator to reread and
retry without overwriting another writer's state.

State values are bounded structured documents. `schema` is a nonempty versioned identifier, such
as `keeppeek.media-intent.v1`; KeepPeek validates document size, permitted schemas, and namespace
policy before storing it. Clients must treat documents as untrusted data even when they recognize
the schema. An optional nonzero `ttl_ms` requests expiry subject to server limits. KeepPeek clamps
a requested TTL above the namespace maximum and returns the accepted `expires_at_ms` in the
resulting entry. A TTL below the namespace minimum is rejected with
`STATE_STORE_ERROR_CODE_TTL_INVALID`. On expiry, KeepPeek removes the entry and emits a watch
update with kind `EXPIRE`; expiration is not a successful refresh or ownership transfer.

`WatchState` atomically registers a watch and captures its initial snapshot. The command response
contains every matching entry exactly as they existed at `snapshot_revision`; subsequent
`StateStoreWatchUpdate` messages are ordered on `control-channel` and have strictly greater
revisions for that namespace. A client applies the snapshot, then updates in revision order. A
revision gap, watch error, or reconnection requires a new watch snapshot rather than guessing the
missing state. The client first sends `UnwatchState` when possible, then creates a new watch and
discards its old local copy after installing the new snapshot. Broad snapshots are capped;
KeepPeek rejects a watch that exceeds configured entry or byte limits, and clients narrow the
namespace or key prefix.

Watch updates are server-originated control requests and require `Ok` or `Error`. A `PUT` update
contains the complete replacement entry. `DELETE` and `EXPIRE` carry namespace, key, and revision
without an entry. `UnwatchState` stops updates and returns `StateUnwatchResult`; it is harmless to
unwatch a client-local already closed watch only when the server still recognizes its ID.
`StateStoreError.current_revision` is populated only for `CONFLICT`; all other errors omit it so
authorization failures do not leak entry revision information.

The standard orchestration schema is `keeppeek.media-intent.v1`. One key represents one desired
action and uses a stable source ID, logical stream ID, optional exact variant or output profile,
role (`publish` or `subscribe`), desired boolean, priority, and requested recording mode for a
publish intent. `recording_mode` is required when role is `publish` and is copied to the eventual
nonzero `StartPublication.recording_mode`; target publication capability and server policy remain
authoritative. It names stable identities only; a worker resolves current source-session IDs from
`ServerCapabilities` before sending `Subscribe` or `StartPublication`. Consumers use an intent as
input to normal media commands, then publish their observed result separately if needed. They never
assume that an intent means a stream is active or authorized.

For example, a transcode coordinator can write a desired `browser-h264-720p` publication under a
service namespace; a transcoder watches it, starts or stops the actual publication, and viewers
watch corresponding subscription intent. The actual output becomes usable only after KeepPeek
advertises its variant in `ServerCapabilities`. Similarly, a group client may store its preferred
group or local mute preferences, but membership and permission remain in
`GroupState` and are never inferred from shared state.

## Subscriptions

Each `Subscribe` command requests exactly one media stream, data feed, or event feed by setting
`Subscribe.media_subscription`, `Subscribe.data_subscription`, or
`Subscribe.event_subscription`. The server replies with `Ok` using the same `request_id` and an
embedded `SubscriptionResult` containing the exact opaque RTP MID, data routes, media-data
binding, or event-attachment routes it established. A rejected subscription returns `Error` with a
`SubscriptionError` naming the requested subscription and a fixed `SubscriptionErrorCode`.
`Unsubscribe` removes one or more existing subscription IDs and receives
an `Ok` or `Error` response using the same request ID.

A second `Subscribe` that reuses an active `subscription_id` atomically replaces that
subscription. The replacement must keep the same target kind: a media subscription stays a
media subscription for the same `(source_session_id, MediaKind)`, a data subscription stays a data
subscription for the same source session, and an event subscription stays an event subscription.
A successful replace keeps the existing RTP MID or media-data binding when transport and media
kind still match; otherwise KeepPeek assigns a new compatible binding and releases the old one
atomically, or returns `Error`. The previous variant selection ends at replace. A failed replace
leaves the existing subscription and `StreamId` ownership unchanged. A successful `Unsubscribe`
releases its RTP `StreamId` for a later subscription.

A video subscription's `video_quality` defaults to `auto` when omitted because
`VIDEO_QUALITY_AUTO` is the protobuf zero value. `auto` lets the server select and later change
the rendition of an automatic subscription. When a source has three or more ordered simulcast
renditions, auto initially uses a middle rendition. `high` requests the highest-ranked variant
and `low` requests the lowest-ranked variant advertised for that stream. The server rejects a
manual selection it cannot satisfy with `Error` and a
`SubscriptionError` using `SUBSCRIPTION_ERROR_CODE_VIDEO_QUALITY_UNAVAILABLE`.

An empty `MediaSubscriptionRequest.variant_id` lets KeepPeek select a compatible advertised variant
using negotiated codec support, requested transport, and video quality. A nonempty variant ID
requests that exact variant and requires `video_quality: AUTO`; KeepPeek never substitutes a
different variant. Missing or transport/codec-incompatible variants return
`SUBSCRIPTION_ERROR_CODE_VARIANT_NOT_FOUND` or
`SUBSCRIPTION_ERROR_CODE_VARIANT_INCOMPATIBLE`. Every successful media subscription returns the
exact `selected_variant_id` and resolved `selected_lineage` in `SubscriptionResult`. Audio and
video results with the same `source_session_id` share one presentation timeline. An exact variant
binding is immutable until that subscription is replaced or unsubscribed. Changing quality or
variant therefore uses a replace `Subscribe` with the same `subscription_id`. An invalid
exact-variant plus manual-quality combination returns
`ERROR_CODE_INVALID_REQUEST`.

### Self-subscription

A WebRTC publisher subscribes to its own media exactly as another authorized client does. It
reads `self_source_session_id`, waits until its publication is `ACTIVE` and the complete
`ServerCapabilities` snapshot advertises the variant, then sends a `MediaSubscriptionRequest`
with that source session, media kind, and optional exact variant. KeepPeek performs the ordinary
visibility, authorization, codec, bandwidth, and receive-`StreamId` checks. On success,
`SubscriptionResult.rtp.mid` or `media_data.stream_binding_id` maps the published media identity
back to a local receiver.

Self-subscription is not an in-process shortcut and does not reuse the send `StreamId`. RTP needs
a separate compatible `recvonly` `StreamId` from the same offer. It can therefore fail with the
same capacity errors as any other subscription. The subscription participates in normal demand,
congestion control, recording independence, teardown, and lineage checks. In particular, a
transcoded publication may not use a subscription to its own output as an input because lineage
cycle validation still rejects that graph.

RTP receive capacity is fixed by the client's initial offer. KeepPeek parses every accepted
`recvonly` audio or video section into a session-local `StreamId` record containing its exact MID,
media kind, and negotiated codecs. The client offers only the `StreamId` values it is prepared to
use, within the 256-RTP-section session limit. An RTP subscription that has no unassigned
`StreamId` with the matching kind and a compatible negotiated codec returns
`SUBSCRIPTION_ERROR_CODE_RTP_MID_UNAVAILABLE`. There is no SDP renegotiation to add `StreamId`
values.
An audio or video `MediaSubscriptionRequest` sets `requested_delivery_transport` to a transport
advertised by the selected variant. For RTP, that variant's codec must also be compatible with
the codecs negotiated on the assigned MID.

For RTP delivery, `RtpDelivery.mid` is the exact opaque string from the client's offer. KeepPeek
does not parse it as a number or infer media identity from it. A browser client resolves the
delivery through the `RTCRtpTransceiver` whose read-only `mid` property equals that string; a
native client performs the same lookup in its local `StreamId` registry. Clients maintain explicit
`subscription_id -> StreamId` and `StreamId -> receiver` mappings. Source-session, media-kind,
and variant relationships come from the request and `SubscriptionResult`, never from MID spelling
or order.

One RTP receive `StreamId` belongs to at most one active subscription. KeepPeek never changes that
binding without a successful replace or unsubscribe operation, and never assigns a released
`StreamId` to a later subscription without returning the new mapping in that subscription's
result. For media over data channels, an audio or video subscription requests either
`DELIVERY_TRANSPORT_RELIABLE_DATA` or `DELIVERY_TRANSPORT_UNRELIABLE_DATA`. The server returns
`MediaDataDelivery` with the binding ID, selected codec, complete format configuration, revision,
and channel. It rejects an unavailable or incompatible media transport rather than silently
choosing another one.

The server sends `ConnectionUpdate` with a newly allocated even request ID whenever its connection-health state
changes. `CONNECTION_STATE_LIMITED_CONNECTIVITY` means that connectivity between the client and
KeepPeek cannot currently sustain all requested streams or qualities. Every update carries the
server's current bandwidth-estimation result in `available_bitrate_bps`.

Before sending `LIMITED_CONNECTIVITY`, KeepPeek may lower only automatic video subscriptions
whose `variant_id` is empty. Exact-variant subscriptions, origin `GROUP` subscriptions, and
subscriptions named as publication inputs are not retargeted. If the remaining demand is still
above the estimate, KeepPeek sends `subscription_update_required: true`. The viewer then
chooses which individual subscriptions to remove or replace; KeepPeek does not choose which
streams the viewer gives up and does not silently change an exact binding. A viewer uses the
advertised `available_bitrate_bps` as its current delivery budget when making that decision.

`CONNECTION_STATE_HEALTHY` reports recovery and sets `subscription_update_required: false`. The
viewer can retrieve the server's complete transport metrics through `GET /metrics` when it needs
diagnostic detail.

Data channels have no protocol-level count limit. A `DataSubscriptionRequest` identifies the
source with `source_session_id` and supplies one `DataPayloadRoute` for each
wanted payload type. Each route maps one `payload_type` to either
`DATA_CHANNEL_KIND_RELIABLE_DATA` or `DATA_CHANNEL_KIND_UNRELIABLE_DATA`. The server returns the
accepted routes in `DataSubscriptionDelivery` and sends each payload on its routed data channel.
It either accepts the requested channel route or rejects that payload subscription; it does not
silently reroute a payload to the other data channel. A payload type appears at most once in one
data subscription.

Payload types are not globally unique. Multiple sources can emit the same `payload_type`; every
`DataPayloadFrame` includes its `source_session_id` and payload type so receivers distinguish
payloads by source session. Each generic data payload is carried as a
`Message.data.payload` value.

### Event subscriptions

An `EventSubscriptionRequest` identifies the subscription, selects stable source IDs, media kinds,
and event types, and requests zero or more attachment routes. An empty source list matches every
source, including newly connected sources. Empty media-kind and event-type lists match every
event. A nonempty media-kind filter excludes events without a matching `media_kind`. The event
envelope itself always uses reliable `control-channel`; each attachment
route selects `reliable-data` or `unreliable-data` for one advertised attachment-type and
content-type pair.

KeepPeek returns every accepted route in `EventSubscriptionDelivery`. It rejects an unavailable
source, event type, attachment, content type, or channel instead of silently widening a filter or
changing a route. A subscription with no attachment routes receives structured event envelopes
without binary attachments, which is sufficient for a text-only forwarder.
`backfill_end_timestamp_ms` is an exclusive event-time catalog boundary captured after the live
subscription becomes active. A stored timeline query ending at that boundary covers matching
events committed before activation; matching events committed afterward are delivered live even
when their source timestamp is older than the boundary.

For every matching event, KeepPeek sends one `Event` with the event subscription ID, active
source-session ID, stable source ID when available, globally unique event ID, event type, start
and optional end time, origin, optional media kind, confidence, bounding box, zone, text, and
extensible structured payload. `revision` starts at one and increases whenever the same event is
updated, closed, or gains attachments. A receiver ignores payload fields it does not understand
but retains the typed envelope fields when forwarding or storing the event.

`Event.attachments` is the complete attachment snapshot for the event revision and the
subscription's accepted routes. Each descriptor has a stable attachment ID, logical type,
content type, optional byte length, zero-based ordinal, optional capture timestamp, and optional
caption text. A simple motion event normally has one JPEG descriptor at ordinal zero. A story
event has one descriptor per JPEG; ordinals define display order, while per-image timestamps
preserve capture timing. Attachment IDs, rather than filenames, are the correlation and
deduplication keys. For a nonzero advertised `maximum_count`, the number of matching descriptors
must not exceed that maximum, and it must never be less than `minimum_count`.

Each requested attachment is carried as one or more `Message.event.attachment` values. A single
`EventAttachmentChunk` carries every attachment transfer in this API; its `context` names the
`subscription_id` for live fanout, the `publication_id` for a client publication, or the
`query_id` for a stored timeline result. All chunks
of one attachment have the same context, event, attachment, type, content type,
event revision, sequence, ordinal, timestamp, and nonzero `chunk_count`; `chunk_index` runs from
zero through `chunk_count - 1`. The receiver concatenates chunks in index order and discards the
complete attachment if a chunk is missing, duplicated, out of range, or inconsistent. `sequence`
starts at one and increases once per complete attachment within an event subscription. Every
chunk's `revision` must equal the corresponding `Event.revision`.

Control and binary channels are not mutually ordered. An attachment can arrive before its
`Event`, so clients buffer it by subscription ID, event ID, and attachment ID until the
descriptor arrives. Reliable attachment delivery is appropriate for MQTT, webhooks, and durable
bridges. Unreliable delivery is only appropriate when a missing image is acceptable; the event
envelope still arrives reliably.

A client acknowledges `Event` with `Ok` or `Error` using the event's even request ID. That
acknowledgement means the client accepted the envelope, not that an external broker accepted it.
A durable forwarder persists the envelope before returning `Ok` and uses stored-event timeline
queries to recover a disconnect gap.

## Event publication

`PublishEvent` is the envelope-only shorthand. Its `Event.subscription_id` and attachment
list are empty. `Event.source_session_id` is required and names the live source to publish
into; `source_id` must match that source when present. KeepPeek validates and stores the event,
then enqueues it for matching event subscriptions before returning `Ok`. A storage failure
returns `Error`; an event is never routed only from volatile state.

An event with text or binary attachments uses `EventPublicationCommand`. The publisher first
sends `StartEventPublication` with a connection-unique publication ID, requested attachment
channel, and complete `Event` snapshot. `subscription_id` is empty. `source_session_id`
targets the active source, `source_id` must match that source when present, the client-generated
event ID is globally unique, and revision one creates an event. A later revision must be greater
than the stored revision for that event. A present `media_kind` must be available on the target
source session, and the event type must be advertised as accepted for that source.
KeepPeek records the canonical origin as `KEEPPEEK` for service-produced detections rather than
trusting a client-supplied camera origin. A revision conflict returns
`EVENT_PUBLICATION_ERROR_CODE_REVISION_CONFLICT` with the current stored revision.
Descriptor counts outside an advertised minimum or maximum return
`EVENT_PUBLICATION_ERROR_CODE_ATTACHMENT_COUNT_MISMATCH`.

A successful start returns the complete `EventPublicationState` with status
`ACCEPTING_ATTACHMENTS`, the accepted channel, per-attachment and aggregate byte limits, and an
absolute expiration time. No event row, attachment file, or subscriber notification is visible at
this stage. The publisher waits for that state before sending any
`Message.event.attachment` chunks carrying that `publication_id`. Attachment-bearing publications use
`reliable-data` in this draft; KeepPeek rejects `unreliable-data`. A publication without
attachments may leave the channel unspecified.

Every publication attachment must match one descriptor in the start snapshot. All chunks have
the same publication ID, event ID, revision, attachment ID, type, content type, ordinal,
timestamp, and nonzero `chunk_count`; `chunk_index` runs from zero through
`chunk_count - 1`. KeepPeek rejects inconsistent metadata, duplicate or out-of-range chunks,
undeclared attachments, and size-limit violations. Complete bytes are staged outside the final
event attachment namespace. The accepted publication ID binds every binary chunk to the source
session and stream validated at start, so chunks do not repeat caller-controlled source fields.

After sending every chunk, the publisher sends `CommitEventPublication`. Because control and
binary channels are not mutually ordered, KeepPeek waits up to `wait_timeout_ms` for all declared
attachment chunks instead of assuming they arrived before the commit command. Zero requests a
server default, and every wait is capped by publication expiration. An incomplete bounded wait
returns `EVENT_PUBLICATION_ERROR_CODE_ATTACHMENTS_INCOMPLETE` while leaving an unexpired
publication active so the publisher can resend and retry. A commit at or after expiration returns
`EVENT_PUBLICATION_ERROR_CODE_EXPIRED`.

A successful commit atomically makes the event revision and every attachment durable, then
enqueues that committed revision in the event router. The response is `EventPublicationState`
with status `COMMITTED`. The router creates one subscription-specific `Event` for every
matching source, stream, and event-type filter, sets its `subscription_id`, and sends only the
attachments selected by that subscription's accepted routes. Router fanout never delays or rolls
back the catalog transaction. A disconnected or slow subscriber recovers from the stored timeline
rather than blocking the publisher.

`AbortEventPublication` removes staged bytes and returns status `ABORTED`. KeepPeek also removes
staged bytes at `expires_at_ms` and sends an unsolicited status `EXPIRED` when the connection is
still available. Publication IDs are not reusable within a connection, including after commit,
abort, or expiry. Repeating a commit for an already committed publication returns the same
`COMMITTED` state, making commit retry idempotent.

## Media over data channels

Media over data channels is the fallback when RTP delivery is unavailable or restricted. It uses
either `reliable-data` or `unreliable-data`, as selected in `MediaDataDelivery`. Unreliable
delivery can lose, duplicate, and reorder messages; reliable delivery preserves ordered messages.

`MediaDataDelivery` establishes one `stream_binding_id` on the reliable control channel. It
contains the binary channel, selected `CodecDescriptor`, complete `MediaDataFormat`, and initial
`configuration_revision`. The stream binding identifies every later `AudioDataFrame` or
`VideoDataFrame`; a frame is invalid on a channel other than the one named by its binding.

When a server-delivered stream changes codec configuration, the server sends a complete
`MediaDataConfiguration` on `control-channel` with a higher `configuration_revision` before it
sends frames using that revision. A client-published media-data stream fixes its configuration
through `StartPublication.format`; it must stop and start a new publication to change that format.
A frame always includes the configuration revision needed to decode it.

Each binary SCTP message carrying audio contains a `Message.audio.frame` value. `frame_id` is the
monotonically increasing audio frame number within its stream binding. `timestamp_us` is the
presentation timestamp, and `duration_us` is the frame duration. For AAC,
`CodecDescriptor.name` is `aac`, `AudioDataFormat.decoder_config` contains the MPEG-4
AudioSpecificConfig, and the reassembled payload is exactly one raw AAC access unit without an
ADTS header.

Each binary SCTP message carrying video contains a `Message.video.frame` value. `frame_id` is the
monotonically increasing video frame number within its stream binding. `timestamp_us` is the
presentation timestamp. When `decode_timestamp_us` is absent, it equals `timestamp_us`; otherwise
it is the frame's decode timestamp. `key_frame` identifies a random-access frame.

Large encoded access units may be fragmented across multiple data-channel messages. All fragments
of one frame have the same stream binding, frame ID, and configuration revision;
`fragment_index` runs from zero through `fragment_count - 1`. Receivers concatenate fragments in
index order. A receiver drops the whole frame when any fragment is missing, duplicated,
inconsistent, or cannot be decoded. After losing a video frame, a decoder waits for a later key
frame before resuming output.

## Stored media

Stored media is a random-access presentation over persisted fragmented MP4, not another
`StreamKind` or codec. The MP4 remains a presentation containing its recorded audio and video
tracks. `StoredMediaCommand` adds the state that live media does not have: a stable source, an
absolute timeline, a playback cursor, a rate, a bounded delivery window, and discontinuous
seeks.

### Cursor lifecycle

`OpenStoredMedia` creates one cursor identified by the client-chosen `stored_media_id`. That ID
must be unique among the connection's active stored-media cursors and is distinct from source,
subscription, stream-binding, and HTTP session IDs. The request selects a stable `source_id`, a
stored stream, an absolute Unix timestamp in milliseconds, an optional exclusive end timestamp,
a mode, a media channel, and zero or more timed-data routes. A finite positive
`playback_rate` is required. When present, the end timestamp must follow the initial timestamp.

KeepPeek answers a successful open with `StoredMediaState` using the same request ID. The initial
generation is nonzero. `requested_timestamp_ms` is the requested cursor position, while
`fragment_timestamp_ms` is the beginning of the keyframe-aligned MP4 fragment selected to decode
that position. The fragment timestamp may precede the requested timestamp. The viewer decodes
from the fragment boundary and presents the first frame at or after the requested timestamp.
KeepPeek rejects a timestamp in a recording gap rather than silently jumping to unrelated media.
A seek at or after the cursor's exclusive end timestamp is rejected with
`STORED_MEDIA_PLAYBACK_ERROR_CODE_TIMESTAMP_UNAVAILABLE`.

`SetStoredMediaPlayback` changes the cursor's playing state, forward playback rate, or mode and
returns the complete resulting `StoredMediaState`. `RefillStoredMedia` supplies the browser's
current absolute playback time. When the remaining accepted delivery window reaches half of
`max_buffer_duration`, the client requests another bounded window. A successful refill that
enqueues media advances the cursor generation, returns the complete state, and sends the new
generation's initialization before its fragments. It does not cancel bytes already queued for the
preceding generation. `CloseStoredMedia` releases the cursor and returns `Ok`.

When an explicit end timestamp has been fully delivered, KeepPeek returns status `ENDED` and sends
an unsolicited `StoredMediaState` notification with the same terminal state. The client does not
acknowledge notifications. It waits for every terminal-generation chunk to be appended before
calling `MediaSource.endOfStream()`.

In `PLAYBACK` mode, KeepPeek sends an initial bounded window and accepts client-paced refills while
`playing` is true. `playback_rate` controls presentation but does not enlarge the recorded-time
window. In `SCRUB` mode, the cursor is paused and each successful open or seek produces at most the
one fragment containing the requested timestamp. Scrub mode is intended for rapidly changing
preview positions; playback mode is intended for continuous viewing.

### MP4 and timed-data delivery

KeepPeek sends bytes already present in the recording. A `StoredMediaInitialization` carries the
exact `ftyp`/`moov` initialization range, and a `StoredMediaFragment` carries an exact complete
`moof`/`mdat` range beginning with a video random-access sample. Stored-media delivery does not
remux an entire recording and does not repackage every audio or video sample into a separate data
channel message.

`StoredMediaFragment.start_time` is the absolute recording time corresponding to the
fragment's first video presentation sample. `duration_ms` spans the fragment's recorded media
timeline. Decode and composition timestamps inside the MP4 remain in their original track
timescales; the fragment fields provide the wall-clock mapping used to align timed data.

Initialization chunks always use `reliable-data`, including when media fragments use
`unreliable-data`. Each media fragment references its `initialization_id`. KeepPeek sends the
applicable initialization in every new generation before sending its first fragment; because
different SCTP streams are not mutually ordered, a client receiving an unreliable fragment first
waits for the referenced initialization. A new initialization ID is used when the MP4 track or
codec configuration changes.

An initialization or media fragment may span multiple protobuf messages. Initialization chunks
are grouped by stored-media ID, generation, and initialization ID; their content type and
`chunk_count` must match. Fragment chunks are grouped by stored-media ID, generation, and
sequence; their initialization ID, start timestamp, duration, and `chunk_count` must match.
`chunk_count` is nonzero, and `chunk_index` runs from zero through `chunk_count - 1`. The receiver
concatenates chunks in index order and discards the complete initialization or fragment if a
chunk is missing, duplicated, out of range, or inconsistent. Fragment `sequence` starts at one
and increases within each generation. Chunks for different objects may be interleaved.

Events, detections, zones, and other metadata are not inserted into the MP4 byte range. They use
`StoredMediaTimedData` so each advertised payload type can be requested independently on
`reliable-data` or `unreliable-data`. Every payload uses the same Unix-millisecond timeline and
cursor generation as the media. Its bytes use the content type advertised by the matching
`DataPayloadCapability`. Sequence numbers increase independently for each payload type within a
generation.

Reliable delivery is appropriate for normal playback and data that must be complete. Unreliable
delivery is appropriate for replaceable scrub previews or high-rate metadata where the latest
state matters more than completeness. KeepPeek accepts the requested channel or rejects the
route; it never silently changes channels.

### Fast seeks

Each accepted `SeekStoredMedia` selects the indexed random-access fragment containing the target,
increments the cursor generation, and returns the new complete `StoredMediaState`. KeepPeek
cancels pending file reads for older generations and does not enqueue more old-generation data.
Every initialization, fragment, and timed-data message carries the generation. A client discards
any message whose generation is not the cursor's current generation, so delayed or reordered
packets can never move the visible cursor backward.

Generation filtering cannot remove bytes already queued on an ordered reliable SCTP stream. The
server therefore keeps no more than the accepted `max_buffer_duration` ahead of the playback
cursor, measured in recorded media time rather than wall-clock drain time. Zero in
`OpenStoredMedia` requests a server default, and the complete state response reports the accepted
value. A playback-rate change does not change this recorded-time bound. For the lowest seek
latency during continuous dragging, a viewer uses `SCRUB` mode with `unreliable-data`, then
switches or reopens the cursor in `PLAYBACK` mode on the desired delivery channel when the
position settles.

The storage lookup for an open or seek uses the recording catalog's time index and
keyframe-aligned fragment byte ranges. It must not scan the MP4 or remux the recording. Seek work
is therefore bounded by an indexed lookup, one file open, and one fragment read rather than by
recording duration.

### Timeline queries

`QueryStoredMediaTimeline` searches indexed availability, events, and timed data without opening
a playback cursor or reading MP4 bytes. It names zero or more stable source IDs, a half-open time
range, wanted generic payload types, a result channel, and an optional availability bucket
duration. An empty source list selects every advertised stored-media source, enabling one unified
all-camera timeline query. Zero bucket duration requests exact contiguous availability ranges; a
nonzero duration permits the server to coalesce availability into buckets for a lower-cost
overview.

The `events` submessage is presence-sensitive. Omitting it requests no event records. An empty
`events` message requests every event that overlaps the time range, while nonempty `event_types`
limits the result to those types. `include_attachments` controls only binary transfer; it never
changes which event records, text fields, or attachment descriptors are returned. Events without
an end timestamp are treated as point events at their start timestamp for range matching.

Stored results reuse the same `Event` message as live delivery, so a client parses one event type
for both paths. Its `source_session_id` and `subscription_id` are live-delivery fields and are
absent in a stored result. Results form one catalog snapshot and are ordered by start timestamp
and then event ID across pages. The catalog bounding-box order `[x, y, width, height]` maps
directly to the four named `EventBoundingBox` fields. An event's `attachments` descriptors state
what KeepPeek can return for it, so no separate availability flag is sent; internal filenames and
filesystem paths are never sent.

KeepPeek first returns `Ok.stored_media_query_delivery`, then emits numbered
`StoredMediaQueryPage` messages on the accepted binary channel. Pages carry recording
availability, typed event records, and matching generic metadata payloads. Event information is
emitted before any requested attachments so binary transfer cannot delay rendering the event list
on `reliable-data`. The same emission order is not an arrival-order guarantee on
`unreliable-data`; clients group and reorder those messages by query ID, type, and sequence.

When `include_attachments` is true, KeepPeek follows the pages with `EventAttachmentChunk`
messages carrying the query ID as their context, one attachment transfer per available
attachment. A motion event can produce one message sequence; a story event can
produce several, correlated by event revision and attachment ID and ordered by ordinal. An
attachment may be split across messages with the same query
ID, event ID, event revision, attachment ID, sequence, ordinal, and timestamp; `chunk_index` runs
from zero through `chunk_count - 1`, and the receiver concatenates chunks in index order.
`sequence` starts at one and increases once per complete attachment, not once per chunk.

One `StoredMediaQueryEnd` follows the pages and attachments. `page_count` and `attachment_count`
report the
total numbers of pages and complete attachments, allowing a receiver to detect missing data. A
valid query with no matching availability, events, or payloads succeeds with both counts set to
zero; an empty result is not an error. Results come from the catalog indexes, and querying a long
timeline must not open each recording file. Only requested attachments require thumbnail file
reads.

`CancelStoredMediaTimelineQuery` stops new page and attachment emission, then is a fire-and-forget notification;
messages already in flight may still arrive. Clients use a new query ID when a pan, zoom, or
filter change supersedes a previous query and discard every page, attachment, or end message for a
cancelled ID.
Reliable delivery gives a complete ordered result and should be used to fetch all events. On
unreliable delivery, sequences and final counts expose loss; a client that needs an authoritative
result repeats the query on `reliable-data`.

### Clip exports

The `keeppeek.media-export.v1` capability enables `ExportCommand`. `CreateExportJob` selects one
stable camera source, `main` or `sub`, and a half-open stored-media range of at most two minutes.
KeepPeek rejects longer ranges rather than silently trimming them. The UI applies the same limit
when creating or moving export handles.

Each successful job remuxes the selected fragmented-MP4 ranges into a separate `.mp4` file under
the long-term storage `.exports/<job-id>/` directory; it never modifies a recording. The filename
has the filesystem-safe camera name and exact UTC range:
`Front-Door_2026-08-22T14-30-00-000Z_to_2026-08-22T14-32-00-000Z.mp4`.
`ExportJob.file_name` returns this basename for downloads. Job IDs keep repeated exports of the
same camera and range in separate directories.

Exports preserve source codecs and timestamps without re-encoding. Missing recording intervals
produce `PARTIAL` unless `allow_partial` permits exporting the available portions. Ready artifacts
include their byte count and SHA-256 digest, expire after 24 hours, and can be downloaded only on
`reliable-data`.

## Event search and encoded previews

The `keeppeek.event-search` capability enables `EventSearchCommand`. It searches durable event
metadata without opening a playback cursor and fetches selected immutable media bytes separately.
Search result messages always use `reliable-data`; encoded object chunks use the explicitly
requested data channel.

`ReplaceEventSearchTerms` names the event's stable source and replaces producer-supplied
`FACE_NAME`, `OBJECT_CLASS`, and `TEXT` terms for one event. KeepPeek rejects attempts to mutate
`EVENT_TYPE`, which it maintains from the durable event record. Terms are whitespace-normalized
and matched case-insensitively by prefix. `SetEventSearchEmbedding` likewise names the source and
creates or replaces one dense embedding under its model ID.
The producer owns embedding generation. KeepPeek never compares vectors with different model IDs
or dimensions.

`QueryEvents` has exactly one `search`: `EventTextSearch` for structured prefix matching or
`EventSemanticSearch` for exact cosine ranking. It selects an optional stable source, one logical
stored stream, a half-open time interval, a bounded result page, and a preview interval. Omitted
preview durations use five seconds before event start and ten seconds after event end. Their sum
cannot exceed 60 seconds. Every search is limited to 31 days. Page size defaults to 50 and is
capped at 128. New clients leave the deprecated `offset` at zero and pass the opaque `page_token`
from the preceding page.

A successful query first returns `Ok.event_search_delivery`, then one
`Message.event_search.result` per hit and one `Message.event_search.query_end`. Results are ordered
by the selected search mode and numbered from one within the page. `next_page_token` is present
only when another page exists. A token binds the query and its event/embedding snapshot, so events
inserted between pages neither shift nor duplicate the original result set. Clients repeat the
original page size and preview durations. If an existing event in the snapshot has its terms,
embedding, or end time changed, KeepPeek rejects the token with `ERROR_CODE_REJECTED` instead of
returning an inconsistent page; the client restarts the query. Each hit contains event metadata and zero or more
`EventSearchKeyframe` descriptors whose GOPs overlap its preview interval. A descriptor contains
stable source, stream, recording, and fragment identities but never a path or byte offset. Empty
keyframes mean matching event metadata remains available while its recorded media is unavailable.
`keyframes_truncated` reports the 60-second or 64-descriptor preview bound.

Semantic search ranks at most the 10,000 most recent compatible embeddings in the requested
source/time snapshot. `candidates_truncated` reports when older matching embeddings were outside
that bounded candidate set. Search runs on a catalog connection separate from recording writes.

`FetchEventSearchMedia` accepts at most 64 descriptors copied into `EventSearchMediaObject`
references. Every reference has a client correlation ID and requests exactly one representation:

- `ENCODED_KEYFRAME` returns the indexed AVCC/HVCC sync sample. Its content type identifies H.264
  AVCC or H.265 HVCC framing.
- `FMP4_INITIALIZATION` returns the exact `ftyp`/`moov` range required by its recording.
- `FMP4_GOP` returns the complete indexed `moof`/`mdat` fragment beginning at that keyframe.

Every chunk repeats the source video's decoder configuration. `codec` is a complete RFC 6381
identifier, `width` and `height` are coded dimensions, `decoder_config` is the raw
`AVCDecoderConfigurationRecord` or `HEVCDecoderConfigurationRecord`, and `nal_length_size`
describes each NAL-unit prefix in an encoded-keyframe payload. A WebCodecs client passes `codec`,
the dimensions, and `decoder_config` as `VideoDecoderConfig`, reassembles the object's payload,
then submits it as an `EncodedVideoChunk` with type `key`. The decoder configuration remains
available on fragmented-MP4 representations but is not needed by a MediaSource consumer.

KeepPeek resolves each stable reference against cataloged source and logical-stream identity.
Rows created before stable identities were recorded may use the configured legacy storage label
as a fallback; a later label change does not invalidate identified archive rows. Clients cannot
supply paths or ranges. A transfer is limited to 64 objects and 32 MiB. Success returns
`Ok.event_search_media_delivery`, followed by one or more
`Message.event_search.media_chunk` values per object and a reliable
`Message.event_search.media_end`. Chunk index begins at zero, chunk count is nonzero, and every
chunk repeats object ID, recording/fragment identity, representation, content type, and complete
byte length. KeepPeek orders requested initialization objects before keyframes and GOPs and always
sends initialization over `reliable-data`. Receivers reject an object whose chunks are missing,
duplicated, out of range, or inconsistent.

`CancelEventSearchQuery` and `CancelEventSearchMedia` remove unsent messages from their connection's
outbound queue. Messages already handed to SCTP may still arrive, so clients discard them by query
or transfer ID. Background workers observe cancellation between media chunks, and queued worker
messages remain cancellable until a FIFO completion marker is drained. Cancellation succeeds
without a typed response payload. An accepted operation that later fails emits reliable
`Message.event_search.error` with its query or transfer ID.
Canceled work remains charged against the per-session and global task limits until its worker has
stopped. A bounded worker channel and bounded session byte queue propagate data-channel
backpressure instead of accumulating transfer bytes in memory.

## Media publishing

Clients use `PublicationCommand` to start or stop audio and video publications, and
`PublicationState` reports the resulting delivery assignment or rejection. `StartPublication`
targets an existing media stream through `(source_session_id, kind)` and proposes a
connection-unique publication ID plus a variant ID unique within that media stream. The source's
`MediaPublicationCapability` must accept the kind, codec, and publication transport. A conflicting
variant ID or exhausted variant limit is rejected rather than replacing an active publisher.

An RTP publication also sets `StartPublication.rtp_mid` to the exact opaque `StreamId` of one
`sendonly` section from the client's accepted offer. KeepPeek validates that the `StreamId`
exists, matches the publication's audio or video kind, negotiated the requested codec, and is not
owned by another publication. An unknown MID returns `PUBLICATION_ERROR_CODE_RTP_MID_NOT_FOUND`;
a `StreamId` with the wrong kind, direction, or RTP compatibility returns
`PUBLICATION_ERROR_CODE_RTP_MID_INCOMPATIBLE`; and an already bound `StreamId` returns
`PUBLICATION_ERROR_CODE_RTP_MID_UNAVAILABLE`. A data-channel publication leaves `rtp_mid` empty.
The accepted `PublicationState.rtp.mid` echoes the exact bound value.

Every publication supplies a complete codec and media format, nominal bitrate, and quality rank.
Transcoded publications set purpose `TRANSCODED` and identify one or more active input media
subscriptions from the same connection. KeepPeek resolves those
subscription IDs to concrete variant lineage, rejects unknown inputs and recursive ancestry with
`PUBLICATION_ERROR_CODE_LINEAGE_INVALID`, and exposes only the resolved lineage in capabilities.
Each input must be an immutable exact-variant subscription. Resolution, cycle validation, and
publication reservation occur atomically at start, so a capability change cannot insert a cycle
between validation and ownership. This prevents a transcoder from consuming its own output,
directly or through a longer cycle.

One publication produces one concrete variant. Every media variant in a source session must use
that session's normalized presentation timeline. Separate audio and video publications preserve
compatible input presentation timestamps when they belong to the same source session. An attempt
to publish media with an incompatible timeline returns
`PUBLICATION_ERROR_CODE_TIMELINE_CONFLICT`. A publisher uses a new variant ID when changing codec,
resolution, channel layout, or lineage. Bitrate changes that do not alter decoder configuration
can remain within the same variant.

Every `StartPublication` sets a nonzero `recording_mode`. `INHERIT` accepts the target source's
configured recording policy; `DISABLED` requests live-only delivery; `REQUIRED` requires durable
recording for this publication. `UNSPECIFIED` is invalid and returns
`PUBLICATION_ERROR_CODE_RECORDING_MODE_INVALID`. KeepPeek rejects an incompatible request with
`PUBLICATION_ERROR_CODE_RECORDING_POLICY_REJECTED` or one it cannot currently provision with
`PUBLICATION_ERROR_CODE_RECORDING_UNAVAILABLE`. The accepted `PublicationState.recording` always
reports the requested mode and current effective status, so a publisher never infers recording
from publication success alone.

One connection can offer at most one RTP `sendonly` audio `StreamId` and one RTP `sendonly` video
`StreamId`; their MIDs have no prescribed values or relationship. The RTP video publication may
declare one to four `SimulcastLayer` RIDs; `simulcast_layers` is invalid for audio or data-channel
publications. Additional audio or video publications from the same connection use
`reliable-data` or `unreliable-data`. A multi-camera transcoder therefore uses media-data
publications for all but at most one audio and one video output, or uses separate WebRTC
connections when RTP output is required. A data-channel publication sets the matching
`PublicationTransport`; its accepted `PublicationState.media_data` provides the binding and
channel used by later frames.

A successful start initially returns `PublicationState` with status `STARTING`, the accepted
delivery binding, variant ID, and control revision. KeepPeek does not advertise the variant in
`ServerCapabilities` until it has a complete decoder configuration and a
decodable key frame for video, or the first valid access unit for audio. It then sends an
unsolicited state `ACTIVE` to the publisher and queues complete capability snapshots afterward on
each authorized connection's ordered control channel, including the publishing connection. The
publisher can self-subscribe after its replacement snapshot advertises the active variant. Other
clients discover the publication only through their ready snapshots. A guessed subscription
before advertisement returns variant not found. A startup deadline or publisher failure produces
`FAILED` and removes the binding.

`PublicationRecordingState` begins as `PENDING` when recording must be initialized. For
`REQUIRED`, KeepPeek does not transition the publication to `ACTIVE` until the configured media
is accepted by its recording path and the recording writer is ready to retain it. `RECORDING`
means future accepted frames enter the configured recording pipeline. `NOT_RECORDING` means the
publication is intentionally live-only under an accepted disabled, prohibited, or inherited
policy. KeepPeek sends an unsolicited `PublicationState` whenever recording status changes.

The readiness rule has one fixed-profile exception. A `GROUP` publication binds to its group's
pre-advertised audio or video variant, so it may become active before its first access unit
because listeners already have the exact decoder configuration and stream binding. A group endpoint
routes audio on its first valid access unit and video only after its first valid keyframe; the
`GroupState` active flags update after those gates are reached.

The variant's advertised delivery transports are computed by KeepPeek from its router and
packetizer support for the accepted codec; they are independent of the transport used to ingest
the publication. Receiving complete access units through a data channel can therefore feed RTP
viewers without another decode/encode step when KeepPeek has an RTP packetizer for that codec.

An `AudioDataFrame` or `VideoDataFrame` carries a publication or subscription binding ID, a
monotonically increasing frame ID, timestamp, configuration revision, fragment position, and
encoded payload. A video frame also marks key frames. The active stream capability or accepted
publication declares the codec and complete format for that binding. All fragments for one frame
use the same frame ID; `fragment_index` starts at zero and `fragment_count` is at least one. A
derived publisher preserves the input presentation timeline in `timestamp_us`; it does not reset
timestamps to service startup time. The first frame ID is one and frame IDs increase within the
publication binding.

The accepted media format and decoder configuration are fixed for one publication. A codec,
resolution, sample-rate, channel-layout, or decoder-configuration change stops that publication
and starts a new variant. Encoder target-bitrate changes and keyframe requests do not require a
new variant when decoder configuration remains valid. An RTP publisher must likewise stop and
start for a codec or signaling-parameter change; it cannot alter negotiated RTP configuration in
place.

`PublicationControlCapabilities` declares whether the publisher can pause, apply a target
bitrate, or force a key frame; omitted capabilities mean none are supported. KeepPeek sends only
supported controls through a monotonically increasing `PublicationControl.revision`. A nonzero
`key_frame_request_id` identifies one edge-triggered request. Retries use the same control
revision and request ID, and the publisher emits at most one requested keyframe for that ID.
Target bitrate is a shared variant encoder target, not one individual viewer's bandwidth
estimate.

`active: false` pauses frame production, removes the variant from capabilities, and stops its
current subscriptions after the control is applied. It does not create pending subscriber
requests. `active: true` returns the publication to `STARTING`; KeepPeek re-advertises it only
after fresh configuration and a keyframe or first audio access unit. Pause/resume is therefore an
administrative or server-resource control, not an implicit viewer activation handshake.

The publisher acknowledges each control with `Ok` or `Error` and sends `PublicationReport` after
applying it and periodically while active. Reports carry the applied control revision, health,
actual bitrate, queue delay, and an optional bounded diagnostic string. `DEGRADED` keeps the
variant available; `FAILED` causes KeepPeek to stop the publication, remove its capability, and
notify the publisher with `PublicationState`. Server policy bounds how long a publication may
remain degraded without valid frames; exceeding that deadline transitions it to failed.

If the recording path fails after activation, KeepPeek changes the recording status to `FAILED`
and includes a bounded diagnostic detail. A `REQUIRED` publication is immediately failed and
emits `PublicationState.status: FAILED` before removing the variant and its subscriptions rather
than silently becoming live-only. An `INHERIT` publication may remain `ACTIVE` when the source
policy permits it, but KeepPeek records the storage failure and keeps
`PublicationState.recording.status: FAILED` visible to the publisher and operators. A publisher
can stop and start a new required publication after storage recovers.

`StopPublication` or connection loss removes the variant from the next complete capability
snapshot and stops every subscription bound to it. KeepPeek does not silently bind those
subscriptions to a native or different transcoded variant. Clients that want fallback subscribe
again with an empty variant ID or another explicit variant. Publication state, report, and control
messages do not imply that transcoded media is recorded; storage policy selects live variants
independently through `recording_mode` and the target publication capability.

An RTP publication retains its send `StreamId` while `STARTING`, `ACTIVE`, or paused by
`PublicationControl`. A successful `StopPublication` returns `PublicationState` with status
`STOPPED` and then releases that exact MID for a later publication on the same connection. A
failed publication likewise releases the `StreamId` when its terminal `FAILED` state is sent.
KeepPeek never binds a different publication to the `StreamId` before one of those terminal
states, and connection loss discards the entire session-local `StreamId` registry.

## Compatibility

This is a pre-1.0 draft and its protobuf schema, channel topology, and wire rules may change
freely before 1.0. Starting with the 1.0 release, field numbers, enum values, channel IDs,
labels, and wire encodings are permanent. Later revisions add fields, messages, enum values, or
event types; they never reuse or redefine published values. Deprecated fields and enum values
remain defined and readable; removed field numbers and names are reserved rather than reused.

Implementations ignore unknown protobuf fields, unknown control-envelope bodies, unknown event
types, unknown payload IDs, unknown enum values, and unknown `Message` or nested message
subtypes when their runtime supports doing so. Unknown source events are not protocol errors.
