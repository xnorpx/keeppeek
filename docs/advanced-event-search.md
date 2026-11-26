# Advanced boolean and semantic event search

KeepPeek extends the bounded event-search workflow with versioned nested boolean AST queries
(`expression_version = 1`), optional exact model-space semantic ranking, and durable owner-scoped
saved searches stored in `config.toml`.

## Capabilities

- `keeppeek.event-search`: ordinary metadata, text, and legacy semantic search.
- `keeppeek.event-search.advanced.v1`: versioned boolean AST (`QueryEvents.advanced`) and
  owner-scoped saved-search operations (`ListSavedEventSearches`, `SaveEventSearch`,
  `DeleteSavedEventSearch`).
- `keeppeek.event-search.semantic.v1`: model-versioned exact cosine ranking and hybrid AST +
  semantic queries. Disabled automatically if semantic storage is unavailable, leaving ordinary
  recording and structured event search unaffected.

## Boolean expression model and truth table

Expressions are typed AST trees with `AND`, `OR`, `NOT` (single child), and indexed leaf
predicates:

- `camera_ids`, `group_ids`, `event_types`, `origins`, `zones`;
- `missing_zone`, `missing_confidence`, `has_image`;
- `confidence` (`at_least` or `at_most`, finite `[0.0, 1.0]`);
- `time_range` (half-open UTC `[start, end)`, span at most 31 days);
- `text` (case-insensitive normalized prefix across all fields or one indexed field);
- `workflow` (`reviewed`, `dismissed`, `bookmarked`, `bookmarked_by_me`);
- `producer_ids`, `model_ids` (matches only current-revision non-stale embeddings).

### Null and negation semantics

Every leaf predicate evaluates strictly to boolean `true` or `false` inside the caller's
authorized camera and UTC query scope—never SQL three-valued `UNKNOWN`:

- `zone in [...]` is `false` when `zone` is missing, so `NOT (zone in ['porch'])` matches events
  in other zones and events with a missing zone.
- `confidence >= x` and `confidence <= x` are `false` when `confidence` is missing, so
  `NOT (confidence >= 0.8)` matches low-confidence events and events with missing confidence.
- `NOT (...)` complements only the caller's authorized camera set and UTC window; negation never
  widens camera authorization or time bounds.

### Bounds

- Maximum expression depth: `8`
- Maximum AST nodes: `64`
- Maximum total predicate list values: `64`
- Maximum value bytes: `256` UTF-8 bytes (`128` for model/producer IDs)
- Maximum total expression text bytes: `16 KiB`
- Maximum authorized cameras per query: `127`
- Maximum UTC window: `31 days`
- Maximum candidates evaluated per query: `10,000`
- Maximum page size: `128` hits
- Maximum URL-encoded expression parameter: `2,048` bytes (larger queries use saved-search IDs)

## Semantic ranking, provenance, and privacy

- KeepPeek stores and ranks embeddings supplied by authorized external producers (`Administrator`
  role); KeepPeek never generates embeddings implicitly.
- Each embedding is keyed by `(event_id, model_id, model_version)` and bound to the event's
  current `revision`. Updating or deleting an event invalidates or cascades its embeddings inside
  the same transaction.
- Queries compare vectors only within exact `(model_id, model_version, dimensions)`. Legacy rows
  migrated without `model_version` or `producer_id` keep empty version and report `"unknown"`
  provenance rather than fabricating metadata.
- Hybrid queries evaluate authorized structured predicates first, apply `minimum_score` before
  selecting top-K results, and order deterministically by `(score DESC, start_time_ms DESC,
event_id ASC)`.
- Raw embedding vectors are never returned in search hits, summaries, logs, errors, exports, or
  Administrator planner diagnostics.
- Ordinary config-only backups include `[saved_event_searches]` from `config.toml` and never copy
  the recording catalog database or embedding blobs. Catalog-level operational backups that copy
  `recordings.db` include `recording_event_embeddings` and inherit the same access controls and
  event-deletion retention policy.
