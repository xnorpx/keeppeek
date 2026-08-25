# Event browsing, search, and encoded previews

The storage event-search API supports filtered metadata browsing, structured text search, and
model-scoped semantic similarity. Search results contain event metadata and immutable keyframe
descriptors for a bounded preview interval. Encoded bytes are fetched separately so callers can
lazy-load, prefetch, cancel, or cache media.

## Metadata browsing

`EventSearch::search_metadata` returns events newest-first using a bounded time range and opaque
keyset continuation token. Ordinary filters compose with `AND` semantics:

- exact event and source IDs;
- event type and origin;
- zone and minimum confidence;
- presence or absence of a stored image attachment;
- normalized indexed-text prefixes across event type and producer-supplied search terms.

Repeated values within one filter use `OR` semantics. A metadata page contains at most 128 hits,
does not include attachment or encoded keyframe bytes, and does not compute an unbounded exact
total. Exact event IDs allow a selected detail deep link to resolve in one bounded query without
walking continuation pages.

## Searchable metadata

`EventSearch::replace_terms` assigns normalized terms to an event. Supported fields are:

- `event_type`: detection type such as `motion`, `face`, or `vehicle`. This field is indexed
  automatically when an event is inserted.
- `face_name`: an identified person's display name.
- `object_class`: an object or model class such as `person`, `delivery van`, or `dog`.
- `text`: a description or other producer-supplied searchable phrase.

Values are whitespace-normalized and matched case-insensitively by prefix. Display spelling is
preserved. Replacing producer terms does not remove the event's automatic `event_type` term.

```rust,no_run
use keeppeek::storage::{
    EventSearch, EventSearchField, EventSearchTerm, EventTextSearchQuery,
    RecordingCatalog,
};
use std::path::Path;

let catalog = RecordingCatalog::open(Path::new("recordings.db"))?;
let search = EventSearch::new(catalog.handle());
search.replace_terms(
    "event-42",
    &[
        EventSearchTerm {
            field: EventSearchField::FaceName,
            value: "Alice Example".to_owned(),
        },
        EventSearchTerm {
            field: EventSearchField::ObjectClass,
            value: "Person".to_owned(),
        },
    ],
)?;

let mut query = EventTextSearchQuery::new("alice", "main", 1_782_864_000_000, 1_782_950_400_000);
query.field = Some(EventSearchField::FaceName);
let hits = search.search_text(query)?;
# anyhow::Ok(())
```

All query modes return `EventSearchPage`. When `next_page_token` is present, assign it to the
next query's `page_token` to load another page. The opaque token binds the original query and
catalog snapshot, so concurrent inserts do not shift later pages. Repeat the original page size
and preview durations. If an existing snapshotted event's terms, embedding, or end time changes,
the token expires instead of returning an inconsistent page; restart the query. Pages contain at
most 128 hits.

WebRTC clients receive a server-signed envelope around the catalog cursor. The envelope expires
after 15 minutes and is verified in constant time before the catalog cursor is decoded. Modifying
the token, changing its filter/source/time scope, using it after expiry, or using it after a
relevant catalog mutation fails closed. Clients should discard retained pages and restart from
the newest page when the server reports expiry or a changed snapshot.

## Semantic similarity

A producer may attach an embedding with `EventSearch::set_embedding`. The model ID must identify
the embedding model and version. Query embeddings only compare with rows having the same model ID
and dimensions; embeddings from different spaces are never mixed.

```rust,no_run
use keeppeek::storage::{EventEmbedding, EventSearch, EventSemanticSearchQuery};
# fn example(search: EventSearch) -> anyhow::Result<()> {
search.set_embedding(
    "event-42",
    EventEmbedding {
        model_id: "vision-embedding".to_owned(),
        values: vec![0.25, 0.5, 0.75],
    },
)?;

let hits = search.search_semantic(EventSemanticSearchQuery::new(
    EventEmbedding {
        model_id: "vision-embedding".to_owned(),
        values: vec![0.2, 0.55, 0.7],
    },
    "main",
    1_782_864_000_000,
    1_782_950_400_000,
))?;
# Ok(())
# }
```

Semantic score is cosine similarity, where larger values are closer. KeepPeek performs exact
ranking within the 10,000 most recent compatible embeddings in the requested source/time snapshot.
`candidates_truncated` reports when older compatible embeddings were excluded. Semantic queries
use a bounded interval of at most 31 days and return at most 128 hits per page.

KeepPeek stores and ranks embeddings but does not generate them. The computer-vision or face
recognition producer owns embedding generation and model compatibility.

## Preview media

Each `EventSearchHit` contains:

- event identity, source, type, and timestamps;
- optional semantic score;
- the preview interval;
- ordered `EventKeyframeLocation` descriptors whose GOPs overlap that interval.

The default interval is five seconds before event start through ten seconds after event end. A
caller may request a different interval up to 60 seconds. Missing or retained-away media produces
an empty descriptor list without deleting the event result. `keyframes_truncated` reports when a
long event reaches the 60-second bound or the interval contains more than 64 GOPs.

`EventSearch::read_preview` reads every descriptor into owned AVCC/HVCC keyframe bytes. Callers
that need tighter lazy loading can read individual descriptors with
`EventKeyframeLookup::read_location`. Search pages never inline encoded media.

Every protobuf media chunk repeats decoder-ready video metadata. `codec` is the RFC 6381 codec
string, `width` and `height` are coded dimensions, and `decoder_config` is the raw AVC or HEVC
decoder configuration record accepted as `VideoDecoderConfig.description`. `nal_length_size`
describes the length prefixes in an `ENCODED_KEYFRAME` payload. A browser can configure
`VideoDecoder` from these fields and submit the reassembled payload as an `EncodedVideoChunk` of
type `key`; it does not need to parse the fragmented-MP4 initialization object.

The WebRTC protocol advertises `keeppeek.event-search` and exposes this boundary through
`EventSearchCommand`. Queries return metadata and immutable keyframe descriptors on
`reliable-data`. `FetchEventSearchMedia` lazily returns encoded keyframes, fragmented-MP4
initialization ranges, or complete GOP ranges over the selected data channel. Paths and byte
offsets never cross the wire. Transfers are streamed through a bounded session queue, limited to
32 MiB, and cancellable between chunks. Stable source/stream identity resolves archived objects
independently of later storage-label changes.

The Events route requests 18 metadata hits at a time and retains one result page. It stores at
most 32 continuation tokens in browser history state, not in the URL. Date, UTC time range,
camera, type, origin, zone, confidence, image, indexed text, and selected event remain in the URL.
Result cards request canonical keyframes only within one viewport of intersection overscan, with
at most two media transfers active. Filter, date, page, selection, route, and browser-history
changes abort stale metadata and media work. Object URLs are revoked after their consuming card
or detail element detaches.
