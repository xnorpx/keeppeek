# Recording integrity

The Recording integrity workspace at `/recordings` answers whether each configured camera is recording as requested. It does not treat connectivity as proof of recording.

## Evidence model

A playable interval is a half-open range `[start, end)` from a finalized catalog recording whose fragment is random-access and has an indexed keyframe. Intervals merge when they overlap or are separated by at most 1 ms, which covers integer timestamp rounding without hiding a real gap.

Each response joins four evidence sources:

- effective per-camera recording policy;
- current writer attempts, progress, failures, and frame freshness;
- finalized playable catalog intervals, retained bytes, bounds, and finalization time;
- durable operational events, storage safety state, and the bounded deletion ledger.

Policy-disabled streams use `policy_disabled` writer state and do not produce unexpected gaps. A current trailing gap has `end_ms: null`; `observed_end_ms` records the snapshot boundary without claiming that the gap ended.

Gap causes are bounded to source silence, transport outage, stale frames, decode failure, writer failure, disk pressure, retention deletion, storage migration, catalog mismatch, and unknown. A gap links to footage immediately before or after it when available, camera health evidence, and server logs.

## HTTP endpoint

`GET /recording-coverage` requires User access. Local trusted clients may call it without a bearer token; remote clients use the same bearer authentication as other HTTP operations.

Query parameters:

| Parameter               | Meaning                                                                                                             |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `start_ms`, `end_ms`    | Selected half-open interval. Defaults to the latest 24 hours and cannot exceed 31 days.                             |
| `minimum_gap_ms`        | Omits shorter gaps from detailed results. Defaults to 5 seconds.                                                    |
| `minimum_camera_gap_ms` | Returns only cameras with a requested stream whose largest gap meets this duration. Zero disables the fleet filter. |
| `page_size`             | Camera count per page, from 1 through 50. Defaults to 25.                                                           |
| `search`                | Case-insensitive camera ID or display-name search, limited to 128 characters.                                       |
| `state`                 | `healthy`, `degraded`, `paused_by_policy`, `not_configured`, or `unknown`.                                          |
| `stream`                | `main` or `sub`.                                                                                                    |
| `group`                 | Exact configured camera namespace, matched case-insensitively.                                                      |
| `page_token`            | Opaque continuation token. Other query parameters are ignored when this is present.                                 |

The response binds totals and pages to `catalog_revision`. Tokens include the complete bounded query, expire after 15 minutes, and return HTTP 409 if the catalog changed. Restart pagination after 409.

Each stream reports current policy and writer state, last frame/write/finalize/catalog timestamps, oldest/newest retained media, exact merged playable duration as effective retention, finalized file bytes, estimated bytes/day, exact selected coverage duration and percentage, exact gap count/largest gap, recent exact intervals, and deterministic coverage buckets.

Exact detail retains at most 256 recent ranges per stream. Aggregated strips use 15-minute buckets through 24 hours, 1-hour buckets through 7 days, and 6-hour buckets for longer intervals. Totals remain exact when recent detail is truncated.

## Prometheus and alerts

`GET /metrics` computes the latest 24-hour recording projection from the same catalog snapshot functions. Relevant metrics include:

- `keeppeek_recording_policy_requested`
- `keeppeek_recording_writer_state`
- `keeppeek_recording_last_frame_timestamp_seconds`
- `keeppeek_recording_last_write_timestamp_seconds`
- `keeppeek_recording_last_finalize_timestamp_seconds`
- `keeppeek_recording_last_catalog_commit_timestamp_seconds`
- `keeppeek_recording_oldest_retained_timestamp_seconds`
- `keeppeek_recording_newest_retained_timestamp_seconds`
- `keeppeek_recording_effective_retention_seconds`
- `keeppeek_recording_storage_bytes`
- `keeppeek_recording_estimated_bytes_per_day`
- `keeppeek_recording_selected_coverage_seconds`
- `keeppeek_recording_coverage_ratio`
- `keeppeek_recording_gap_count`
- `keeppeek_recording_largest_gap_seconds`
- `keeppeek_recording_current_gap_seconds`
- `keeppeek_recording_catalog_revision`

Writer and storage alert transitions continue through the existing durable operational-event and health monitors, so notifications and the dashboard share writer/storage evidence and debounce semantics. Coverage, retention, growth, and gap-duration metrics are stable rule inputs for external Prometheus alerting.

## Rebuild and retention

Finalization atomically rebuilds one recording's playable summary. Existing catalogs backfill missing summaries idempotently at startup. Cleanup records exact removed coverage islands and a bounded reason before deleting media rows. The ledger retains the newest 10,000 deletion intervals; each response retains at most 256 matching deletion intervals per stream.

Finalized files store a platform file identity when the filesystem exposes one. Hard-linked catalog
rows share one physical byte total, divided across owners with a deterministic remainder; missing
or unsupported identities fall back to the catalog's unique path. Fleet and per-stream totals
therefore sum to physical finalized media bytes without counting one inode twice.

Summary rows cascade with their recording files. The deletion ledger remains after media deletion so later coverage queries can distinguish cleanup or reconciliation from missing input.
