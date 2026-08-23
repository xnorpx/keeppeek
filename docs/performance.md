# Performance benchmarks

## Event keyframe lookup

The `event_keyframe_lookup` benchmark measures the storage API path from an event ID and logical
stream to owned encoded keyframe bytes:

The target requires the opt-in `event-keyframe-benchmark` Cargo feature. Normal CI test and
coverage jobs do not enable it, so they never generate or execute the benchmark corpus.

1. Resolve the event-to-keyframe link through `RecordingCatalog`.
2. Resolve the recording path and exact keyframe byte range.
3. Open the MP4, seek to the indexed offset, allocate the result, and read the encoded bytes.

It does not use HTTP, WebRTC, protobuf serialization, video decoding, or frontend code.

### Prepare the corpus

The default corpus contains deterministic events for 127 cameras, both `main` and `sub` streams,
and 30 UTC days. Production event, search-term, event-keyframe, recording, fragment, and keyframe
tables and indexes are populated until the logical database size is at least 1 GiB. Each
camera/day/stream has 128 uniformly spaced indexed GOPs, producing roughly one million fragment
and keyframe rows. This samples 30-day index cardinality without pretending that 1 GiB can contain
metadata for every GOP from 127 continuously recording cameras.
The fragment/keyframe indexes impose a corpus floor of roughly 200 MiB even when `--target-mib`
requests a smaller smoke corpus; the target controls the minimum rather than an exact size.

```sh
cargo bench --features event-keyframe-benchmark --bench event_keyframe_lookup -- --rebuild --prepare-only
```

Generated state is stored under `target/perf/event-keyframe-lookup/corpus/` and is not checked in.
Later runs reuse it when its manifest, schema contract, dimensions, and committed MP4 fixture hashes
still match. Use `--rebuild` to replace it explicitly.

A smaller corpus is useful while changing the benchmark:

```sh
cargo bench --features event-keyframe-benchmark --bench event_keyframe_lookup -- \
  --rebuild \
  --prepare-only \
  --target-mib 16
```

### Run measurements

```sh
cargo bench --features event-keyframe-benchmark --bench event_keyframe_lookup
```

The runner validates 10,000 exact recording IDs, fragment sequences, timestamps, byte ranges, and
keyframe reads, warms each workload for five seconds, and then
measures for 20 seconds at concurrency 1, 8, and 32. It measures both:

- `resolve`: catalog lookup and response construction without file I/O.
- `read`: catalog lookup plus MP4 open, seek, allocation, and exact read.

Short local runs can override durations:

```sh
cargo bench --features event-keyframe-benchmark --bench event_keyframe_lookup -- \
  --target-mib 16 \
  --warmup-secs 1 \
  --measurement-secs 2 \
  --correctness-lookups 1000
```

Results include throughput, returned MiB/s, mean, p50, p90, p95, p99, p99.9, maximum latency, and
error count. JSON and Markdown reports are written to
`target/perf/event-keyframe-lookup/results/`, including `latest.json` and `latest.md`.

Timing is descriptive and has no machine-dependent pass threshold. Incorrect, missing, or
truncated keyframe data does fail the run.

### Cache interpretation

The corpus gives every camera/day/stream a unique path, but those paths hard-link four small,
committed H.264/H.265 fixtures. Media pages therefore become warm quickly. Results characterize a
large Turso catalog, its single connection-owning catalog thread, queueing, file open/seek/read,
allocation, and copying under the ambient OS cache. They do not characterize cold random reads
across 30 days of unique video bytes.
