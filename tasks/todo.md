# Issue #67 Tasks

## Atomic publication

- [x] Decode inbound binary event messages on the declared data channel with strict size limits.
- [ ] Start a publication only for an active stable source/session and advertised event policy.
- [ ] Validate descriptors, chunk order/count/metadata, per-file bytes, aggregate bytes, and expiry.
- [ ] Commit one event revision and all attachment bytes atomically before any fanout.
- [ ] Make start/commit retries idempotent and return typed current-revision conflicts.
- [ ] Abort, expire, and disconnect without retained staging state or visible partial events.

## Live and stored visibility

- [ ] Admit bounded source/stream/type/attachment event subscriptions.
- [ ] Route committed revisions to matching live subscribers and MQTT independently.
- [ ] Disconnect or shed one saturated subscriber without delaying persistence or peers.
- [ ] Return the same revision and attachment ordering through stored search and the normal UI.

## Media conformance

- [ ] Verify H.264 and H.265 decoder configuration, fragmentation, timestamps, and keyframe recovery.
- [ ] Run one deterministic external client against two low-bandwidth test-camera streams.
- [ ] Publish a person/vehicle event and request one timestamp-correct high-quality image.
- [ ] Reject malformed media, credentials, source sessions, types, transports, and stale work.
- [ ] Prove client crash/disconnect/reconnect cannot stop ingest, recording, live view, or playback.

## Operations and evidence

- [ ] Expose bounded session/subscription/publication/queue/drop/latency health and metrics.
- [ ] Verify logs, diagnostics, and generated bindings contain no credentials or binary payloads.
- [ ] Keep API definitions/docs/generated bindings MIT-compatible for independent clients.
- [ ] Run every conformance test in default CI with no camera, GPU, model, cloud service, or secret.
- [ ] Record reproducible performance and memory evidence with p50/p95 budgets.
- [ ] Complete fresh-context review and `./check.sh`.
- [ ] Publish the criterion-by-criterion PR and final-head CI evidence.
