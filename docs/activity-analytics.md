# Event Activity Summaries and Temporal Analytics

KeepPeek aggregates canonical stored event metadata from `recording_events` into
bounded, revision-correct activity summaries. Counts represent producer-reported
event metadata within current retention and authorization scope, never
authoritative real-world identity, occupancy, or threat conclusions.

## Scope boundary

- Aggregates only normalized events accepted into `recording_events`.
- Never reprocesses video, runs object detection, deduplicates real-world
  objects across cameras, or infers identity or threats.
- Enforces camera authorization before summing, comparing, or ranking.
- Enforces principal-local privacy for `reviewed` and `dismissed` workflow
  dimensions; global reviewed counts are never computed.

## Measure definitions

- **Event count (`event_count`)**: Number of distinct current logical event rows
  whose normalized `start_time_ms` falls in the UTC half-open window
  `[start_time_ms, end_time_ms)`.
- **Distinct event count (`distinct_event_count`)**: Number of distinct logical
  event IDs after revision collapse. Because canonical `recording_events` holds
  one current row per event ID, `distinct_event_count == event_count`.
- **Interval duration (`total_duration_ms`, `average_duration_ms`)**:
  - Zero-length point events (`end_time_ms == Some(start_time_ms)`) have
    `is_interval = false` and are excluded from `interval_event_count` and
    `average_duration_ms`.
  - Closed intervals (`end_time_ms > start_time_ms`) contribute clipped overlap
    `overlap([start, end), [window_start, window_end))`.
  - Open intervals (`end_time_ms IS NULL`) increment `open_interval_event_count`
    and contribute clipped overlap against `[window_start, min(window_end, generated_at_ms))`.
  - `average_duration_ms = total_duration_ms / interval_event_count` when
    `interval_event_count > 0`, otherwise absent.
- **Metadata completeness percentages**:
  - `percent_with_image`: `100 * with_image_count / event_count` (based on
    present `thumbnail_filename` or `canonical_attachment_id`).
  - `percent_with_confidence`: `100 * with_confidence_count / event_count`
    (finite confidence in `[0.0, 1.0]`).
  - `percent_with_zone`: `100 * with_zone_count / event_count` (non-empty
    normalized zone label).
  - All percentages are absent when `event_count == 0`.
- **Fixed confidence bands**:
  - `unknown`: missing, non-finite, or out-of-range confidence.
  - `low`: `[0.0, 0.5)`.
  - `medium`: `[0.5, 0.8)`.
  - `high`: `[0.8, 1.0]`.
- **Group membership policy**: Evaluated at query time from current authorized
  group memberships and returned with `group_policy_revision`.

## Timezone, DST, and comparison semantics

- Storage queries always use UTC half-open bounds `[start_time_ms, end_time_ms)`.
- Display buckets convert using one explicit IANA timezone (`chrono-tz`) and
  record `tzdb_version` and `week_start` (`monday` or `sunday`).
- Hour buckets record `utc_offset_seconds` and `dst_fold` (`0` for the first or
  unique occurrence, `1` for a repeated fall-back hour). Spring-forward missing
  local hours do not exist in UTC and are never emitted as synthetic zero
  buckets.
- Day, week, and month buckets follow exact civil calendar boundaries (including
  23-hour/25-hour DST days and leap days), not fixed 86,400,000 ms arithmetic.
- Non-hour-offset timezones (for example `Asia/Kolkata`, `Asia/Kathmandu`, and
  `Australia/Lord_Howe`) never sum a UTC-hour rollup across a local boundary it
  does not fit. Long-range queries sum interior UTC-hour rollups and scan only
  partial UTC-hour edge slices from `recording_events`.
- Previous-period comparison supports both `previous_duration` (equal UTC span
  immediately preceding `[start, end)`) and `previous_calendar` (previous local
  calendar day/week/month equivalent in the query timezone).

## Incremental rollups, failure isolation, and shadow rebuilds

- Derived tables are double-buffered by `active_generation % 2`:
  - `analytics_ledger_{a|b}` (one contribution row per canonical event ID)
  - `analytics_hourly_rollups_{a|b}`
  - `analytics_daily_rollups_{a|b}`
- Every canonical event write (`insert_event`, `insert_published_event`,
  `close_event`, `close_native_events`, thumbnail attach/detach) and canonical
  deletion (`delete_event`, `delete_events_before`) updates the ledger and
  active rollups inside the owning catalog write transaction.
- Newer revisions subtract the prior ledger contribution and add the new
  contribution atomically. Stale or duplicate revisions do not double-count.
- Workflow mutations (`event_reviews`, `event_bookmarks`) advance
  `analytics_generation`.
- **Failure isolation**: Quota exhaustion (`max_rollup_rows`) or detected
  derived-counter corruption marks `analytics_state.state = 'degraded'` without
  rolling back or blocking canonical event writes. Direct SQL deletions outside
  the catalog helper trigger `analytics_external_event_delete`, marking rollups
  `degraded` so stale totals are never served as exact.
- **Shadow rebuild**: Rebuilds into the inactive generation in bounded batches
  (`<= 512` canonical events per transaction) with a persisted cursor and
  bounded dirty catch-up queue (`<= 65,536` IDs), then swaps `active_generation`
  atomically. Canonical `recording_events` are never modified.

## Completeness and source-health evidence

- **Retention completeness**: Event deletions record bounded intervals in
  `analytics_event_deletions` (capped at `10,000` rows). Buckets overlapping
  deleted ranges are marked `retention_partial = true` and
  `completeness = partial_retention`.
- **Expected event-source health**: Real health transitions are recorded in
  `analytics_source_health_intervals` (capped at `50,000` rows). Present health
  is never projected backward as historical fact:
  - `reporting`: authorized camera reported at least one event with no missing
    health interval in the window.
  - `healthy_no_reports`: observed healthy with zero reported events.
  - `missing_health`: observed outage/missing expected event-source health.
  - `no_expected_source`: camera has no configured/expected event source or is
    privacy-disabled.
  - `unknown_history`: no health evidence covers the camera in the window.

## Operational bounds

| Bound                               | Limit                                   |
| ----------------------------------- | --------------------------------------- |
| Returned time buckets               | `<= 366`                                |
| Grouped series                      | `<= 64`                                 |
| Ranked breakdown rows per dimension | `<= 64`                                 |
| Authorized cameras                  | `<= 127`                                |
| `group_by` dimensions               | `<= 8` from fixed allowlist             |
| Query UTC range                     | `<= 10 years`                           |
| Short-range canonical fallback      | `<= 7 days`                             |
| Exact workflow scan ceiling         | `<= 250,000` events                     |
| Serialized response payload         | `<= 1 MiB`                              |
| Aggregate export rows               | `<= 25,000`                             |
| Derived rollup row quota            | `500,000` default (`1,000..=5,000,000`) |
| Rebuild batch size                  | `<= 512` events per transaction         |
| Rebuild dirty catch-up queue        | `<= 65,536` event IDs                   |
