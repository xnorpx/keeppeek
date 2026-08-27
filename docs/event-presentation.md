# Event presentation metadata

Every event revision has one canonical presentation identity. Consumers use the
canonical attachment and semantic icon described here instead of selecting an
attachment from arrival order, cache state, or local file availability.

## Canonical image

An event producer may set `canonical_attachment_id` to an attachment ID in the
same complete event revision. The reference is accepted only when the ID is
unique and names a supported image. Without an explicit reference, KeepPeek
selects the first available precedence level:

1. a `snapshot` attachment;
2. a `story-frame` attachment;
3. a retained `thumbnail` attachment;
4. no image.

Candidates within one level are ordered by ascending `ordinal`, ascending capture
`timestamp`, and stable `attachment_id`. A missing capture timestamp sorts after
a known timestamp. Producer descriptor order is preserved as evidence; it is not
used as a selection tie-breaker.

Supported preview MIME types are `image/jpeg`, `image/png`, and `image/webp`.
Text attachments, unknown logical types, and other MIME types are never selected
as images. Duplicate attachment IDs, invalid explicit references, and explicit
references to unsupported images invalidate the event revision.

`timestamp` on an attachment is its capture time. It does not replace the event
start, end, or revision-update time.

## Revisions and availability

Attachments are a complete snapshot for one event revision. A strictly higher
revision may add, remove, reorder, or replace the canonical attachment. Timeline,
search, detail, notifications, integrations, and event-seeded exports report the
same revision and canonical attachment ID.

Image availability is separate from canonical identity:

- `NONE` means the revision has no canonical image descriptor;
- `AVAILABLE` means the canonical bytes are currently retained;
- `UNAVAILABLE` means the descriptor and identity remain valid but the bytes were
  retained away or cannot be read.

An unavailable image never causes a consumer to select another descriptor.
Retry requests use the same source ID, event ID, revision, and attachment ID.
The server rejects stale revisions and non-canonical attachment IDs. Binary media
remains on bounded, cancellable data-channel transfers and is never inlined into
metadata pagination.

## Semantic icons

`icon_key` is presentation-only. Event type remains authoritative for filtering,
notifications, and automation. KeepPeek accepts this allowlist:

- `event`
- `person`
- `vehicle`
- `animal`
- `package`
- `motion`
- `doorbell`
- `sound`
- `story`
- `alert`

Unknown keys map to a deterministic event-type icon and are retained only as a
bounded, printable diagnostic value. Event input never controls HTML, SVG, URLs,
CSS classes, or colors.

## Bounding boxes and accessibility

`bounding_box_attachment_id` identifies the image coordinate space for the event
bounding box. A consumer draws the box only when that ID equals the canonical
attachment ID. A missing or different ID omits the overlay while retaining the
metadata for diagnostics.

Preview alternative text names the event and current camera. Empty and unavailable
states expose explicit status text; presentation icons are decorative.

## Additional story frames

Cards, timelines, and notifications keep the canonical image as the event's
identity. Detail and story views may place that canonical descriptor first and
then show the remaining descriptors ordered by ordinal, capture timestamp, and
attachment ID. Loading an additional frame does not replace the canonical card
image.
