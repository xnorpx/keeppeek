# Issue 96: Generic camera events

## Contract

Implement the required native paths from [#96](https://github.com/xnorpx/keeppeek/issues/96)
alongside the existing [ISAPI work](isapi-completion-plan.md). Preserve the staged
changes, existing media behavior, protected `api/`, and generic-motion retention
defaults. No physical-camera setting writes are authorized. The user is unavailable;
this separate tracker preserves the incomplete #67 plan and checklist.

Build order: protocol boundaries -> normalized lifecycle -> PullPoint runtime ->
RTSP metadata -> shared capability/health -> integration and evidence.

| Module           | Responsibility                                                                           | Depends on               |
| ---------------- | ---------------------------------------------------------------------------------------- | ------------------------ |
| onvif-protocol   | Bounded XML, endpoint policy, subscription reference parameters and SOAP operations      | Existing ONVIF crate     |
| native-lifecycle | Topic/class mapping, channel identity, baseline, timestamps, deduplication and intervals | onvif-protocol           |
| pullpoint        | One cancellable subscription per device, lease renewal, fault negotiation and cleanup    | native-lifecycle         |
| rtsp-metadata    | Bounded RTP assembly/gzip, metadata ownership, EventStream and analytics                 | native-lifecycle, Retina |
| native-runtime   | Vendor priority, event configuration, snapshots, evidence and observability              | pullpoint, rtsp-metadata |

## Tasks

- [x] Endpoint validation and reusable subscription request/response parsing. ONVIF endpoint, protocol, XML safety, client, authentication, and snapshot tests pass.
- [x] Normalize fixture-driven topics/classes and source identities. Lifecycle tests verify kinds, baselines, replay, clock domains, source backing, and independent clears.
- [x] Implement PullPoint create/synchronize/pull/renew/unsubscribe with bounded backoff and shutdown. Independent short-lease, fault, and saturation tests pass.
- [x] Retain event-service capability/topic evidence internally and preserve generation-fenced discovery. Discovery/cache and serialization regressions pass.
- [x] Integrate one event owner per configured IP with ordered committed-prefix delivery and optional bounded snapshots. Native storage, retired-owner cleanup, live ordering, and real-video tests pass.
- [x] Bound Retina metadata assembly and support standard/legacy gzip encodings. Fragment, loss, oversize, recovery, and both feature-mode suites pass.
- [x] Consume metadata separately from media; parse EventStream and bounded Profile T/M evidence. Transformation, partial-frame, delete, cross-source, and profile-handoff tests pass.
- [x] Add file-only event configuration, independent event health, safe logs, and fixed-cardinality metrics. Policy, event-only replacement, Reolink hot toggles, and HTTP metric tests pass.
- [x] Preserve the existing external-CV path and prove camera/software origin separation. Publication/idempotence, attachment, and native-capability isolation tests pass; no model or service provisioner was added.
- [x] Document supported topics, source-token binding, network policy, mode priority, conditional push, and unverified hardware in [the operator guide](../docs/onvif-events.md).
- [x] Record the disabled/enabled performance comparison, criterion-by-criterion evidence, focused regressions, and the complete canonical gate.

## Verification checkpoint

The final complete canonical gate passes: 2,157 Rust tests, including the 100
native runtime tests, with 19 existing skips; 222 Bun, 110 browser/visual,
28 compatibility, and 189 Playwright tests, with two existing codec skips.
Strict workspace Clippy, dependency, format, registry, and UI static checks pass.
Command: `TMPDIR=/Volumes/KeepPeekCheckTmp NEXTEST_TEST_THREADS=4 ./check.sh`.
Log: `target/issue96-final-check-4.log`. Concurrency changed only scheduling,
not the test set, assertions, deadlines, or skip policy.
The final-build [performance comparison](../docs/native-event-performance.md) uses
ten paired disabled/enabled runs. Its p95 fragment-gap delta is +3.196 ms against
the unchanged 86.667-ms guard; all 40 MP4s and 150 enabled events validated.
It is not a pre-PR commit comparison. See the
[acceptance evidence table](../docs/onvif-event-verification.md) for per-criterion
results and the unverified hardware/CI requirements.

- [x] Review and regress receipt-time fallback, class-conflicting duplicate backing,
      queue saturation, and metadata-owner handoff.
- [x] Preserve staged ISAPI work and protected `api/`; no branch, commit, push, PR,
      or issue-closing operation has been performed.
- [x] Final protocol feature/doctest checks.
- [x] Final complete canonical gate.
- [x] Final-build performance confirmation and acceptance-criteria evidence table.
- [ ] Physical-device matrix and final-head CI evidence; local fixtures do not replace them.

## Bounds and failure policy

- One PullPoint subscription and one metadata consumer per physical camera.
- SOAP/XML input <=256 KiB, depth <=32, <=256 messages per response.
- Pull timeout initially 2 seconds, transport budget 5 seconds; lower advertised
  fault limits are honored. Subscription lifetime and renew calculations use the
  camera's current/termination time difference, not host wall-clock agreement.
- Metadata compressed input <=256 KiB, decoded output <=1 MiB, bounded expansion
  ratio and per-document work. EXI remains explicitly unsupported.
- State, replay cache, metadata queue and snapshot queue have hard count/byte bounds.
  Unavailable snapshots never discard a committed event or restart media.
- Unknown/malformed notifications are not motion. Event failure cannot reconnect
  healthy video/audio. Shutdown closes intervals and best-effort unsubscribes.
- Telemetry must identify mode, evidence, lease/pull/reconnect errors, metadata
  loss/parse/limits, deduplication and snapshot failures without raw payloads,
  credentials, reference headers or recognition text.

## Scope gates

ONVIF push is conditional in #96: add it only when hardware testing demonstrates
a need. New external-CV provisioning is not required for native ingestion; preserve
and test the existing publication contract. These optional phases are not silently
counted as implemented. The broad physical-device matrix, final-head CI and a
published combined PR remain unverified until actually performed. No issue will be
closed solely because local fixtures pass.
