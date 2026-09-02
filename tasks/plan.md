# Implementation Plan: External Analysis API Hardening

## Overview

Implement issue #67 as a vendor-neutral conformance boundary for external analysis clients. KeepPeek
continues to own camera ingest, recording, event validation, durable storage, and fanout. External
clients consume bounded encoded media and publish normalized event revisions without becoming part
of the recorder lifecycle.

## Architecture Decisions

- Keep `api/webrtc.proto` as the public MIT-licensed contract. Extend it only when an existing
  message cannot express a required invariant.
- Decode inbound `Message` envelopes only on the negotiated reliable/unreliable data channels and
  route them through a session-aware handler. Reject wrong-channel, oversized, malformed, and
  unexpected payloads without closing unrelated media paths.
- Own staged event publications in one bounded server subsystem keyed by API session and publication
  ID. Bind each publication to one source session, stable source, event revision, attachment policy,
  channel, expiry, count limit, and byte budget.
- Reuse the recording catalog's transactional event-revision model. Store attachment bytes in an
  owner-only same-volume staging area, promote them before the catalog commit, and reconcile orphaned
  files after failures.
- Treat a committed catalog revision as the sole durability boundary. Retries return the same
  committed state; live subscribers and MQTT receive only committed revisions.
- Add bounded per-session event subscriptions with independent output queues. A stalled subscriber
  cannot delay persistence or another subscriber.
- Build deterministic Rust/Python conformance fixtures with H.264/H.265 test-camera media and fake
  detections. Do not require a model, GPU, cloud service, physical camera, or secret.
- Keep external-client failure independent from camera workers, recording, playback, and existing
  WebRTC sessions. Session/source replacement cancels only affected subscriptions/publications.

## Task List

### Phase 1: Atomic Attachment Publication

- [x] Route bounded inbound reliable/unreliable `Message` envelopes to a session-aware handler.
- [ ] Start, receive chunks for, commit, retry, abort, and expire one bounded event publication.
- [ ] Persist monotonic event revisions and attachment descriptors/bytes atomically.
- [ ] Reject invalid source/session/type/channel/count/metadata/size/revision transitions with typed
      publication errors.

### Checkpoint: Publication

- [ ] One JPEG remains invisible before commit and is durable after commit.
- [ ] Retried commit is idempotent; stale/conflicting revisions return the current revision.
- [ ] Crash/abort/expiry cleanup leaves no visible event or retained staging bytes.

### Phase 2: Event Subscription and Fanout

- [ ] Implement filtered `SubscribeEvents` admission and typed subscription results.
- [ ] Fan out committed event envelopes and requested attachment routes only after durable commit.
- [ ] Bound subscriber messages/bytes independently and disconnect or shed without blocking commit.
- [ ] Remove subscriptions and staged publications on session/source/capability replacement.

### Checkpoint: Visibility

- [ ] The same committed revision is visible live, through stored event search, and in the normal UI.
- [ ] Viewer and MQTT filters preserve source, stream, type, timestamp, class, confidence, bounding
      box, revision, text, and attachment identity.

### Phase 3: Deterministic Conformance

- [ ] Prove decoder-ready H.264/H.265 reliable-data delivery, fragmentation, timestamps, and
      keyframe recovery with malformed-input rejection.
- [ ] Add a no-model external client that consumes two low-bandwidth streams and publishes a person
      or vehicle event with deterministic evidence.
- [ ] Obtain one timestamp-correct high-quality image without continuously decoding the main stream.
- [ ] Prove rejected credentials, unknown/stale sources, unsupported types/transports, disconnect,
      crash, reconnect, and withheld-client isolation.

### Phase 4: Operations and Publication

- [ ] Add bounded health/metrics for sessions, subscriptions, publications, queues, drops, expiry,
      rejection, commit latency, and storage failure without payloads or credentials.
- [ ] Add secret/binary log scans and independent-client license/conformance documentation.
- [ ] Measure publication/fanout latency and queue memory against explicit p50/p95 budgets.
- [ ] Run fresh-context review, focused suites, `./check.sh`, and final-head CI.
- [ ] Publish a PR with one evidence row for every issue criterion.

## Risks and Mitigations

| Risk                                                  | Impact   | Mitigation                                                                               |
| ----------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------- |
| Metadata becomes visible before attachment durability | Critical | Stage bytes, verify all descriptors, then commit one catalog revision before fanout      |
| Retry creates duplicate or conflicting revisions      | Critical | Bind immutable publication intent and return typed current-revision conflicts            |
| A slow client applies backpressure to recording       | Critical | Independent bounded queues and no waits on camera/storage owner loops                    |
| Stale source sessions publish after reconnect         | High     | Revalidate source/session identity at start, chunk, commit, and session teardown         |
| Binary or model output leaks into logs                | High     | Log bounded identifiers/status only and scan diagnostics with sentinel payloads          |
| H.264/H.265 payloads are not decoder-ready            | High     | Test exact codec config, fragments, timestamps, and keyframe recovery with real fixtures |
| Staging or subscriber state grows without bound       | High     | Cap sessions, publications, attachments, chunks, bytes, waits, and queue age/count       |

## Open Questions

- None. The existing public event-publication and event-subscription messages are sufficient for the
  first slice; contract changes will be additive and justified by a failing conformance case.
