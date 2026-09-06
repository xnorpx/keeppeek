# Native event acceptance evidence

This record maps the implementation to issue [#96](https://github.com/xnorpx/keeppeek/issues/96)
and the associated [ISAPI work](hikvision-isapi.md). It is a local working-tree
record, not a published PR, successful final-head CI run, or physical-camera
certification. The protected `api/` contracts are unchanged.

## Local gate

The complete canonical gate passes on 2026-09-06. The final log is
`target/issue96-final-check-4.log`. Run from the repository root:

```sh
TMPDIR=/Volumes/KeepPeekCheckTmp NEXTEST_TEST_THREADS=4 ./check.sh
```

| Gate                             | Result                                                                                             |
| -------------------------------- | -------------------------------------------------------------------------------------------------- |
| Rust workspace tests             | 2,157 passed; 19 existing skips; 431.684 seconds                                                   |
| Strict workspace Clippy          | Passed with warnings denied                                                                        |
| Dependency and formatting checks | Cargo Machete, Rustfmt, Taplo, and Black passed                                                    |
| UI static checks                 | Public registries, Paper/visual/demo contracts, formatting, lint, Svelte, and E2E typecheck passed |
| UI unit tests                    | 222 Bun, 110 browser/visual, and 28 compatibility tests passed                                     |
| Playwright                       | 189 passed; two existing codec-capability skips; 51.8 seconds                                      |

The first run, `target/issue96-final-check.log`, stopped after 69 passing tests
and one ISAPI fake-camera failure. Accepted sockets inherited nonblocking mode
on macOS, causing an early read to close the connection before headers arrived.
An explicit blocking-mode setup plus a delayed-request regression fixed the
fixture; all four ISAPI fake-camera integration tests then passed.
The rerun is recorded in `target/issue96-final-check-2.log`.

That second run exposed a software-publication capability regression, which was
fixed by merging software-publication types separately from native device flags.
The existing publication/idempotence test and all 15 capability-ordering tests
pass. The third run, `target/issue96-final-check-3.log`, passes all 2,157 Rust tests
and stopped at three formatting-only Clippy diagnostics. Those diagnostics were
fixed, and the fourth run passed every stage. Four concurrent Nextest processes avoid excessive macOS CoreFoundation
test-directory enumeration; no test, timeout, assertion, or skip policy changes.

Final supplementary checks pass: 48 ISAPI core tests without the HTTP feature,
150 Retina tests without H.265, two Retina doctests, four ISAPI doctests, two
ONVIF doctests, and one fake-ONVIF doctest. Logs are
`target/issue96-isapi-core-final.log`, `target/issue96-retina-minimal-final.log`,
and `target/issue96-protocol-doctests-final.log`.

Staged, unstaged, and new-file whitespace checks pass. A high-signal credential
pattern scan found no matches in the 198 changed text files at that checkpoint;
it is not a comprehensive secret audit. The protected `api/` diff is empty.

The focused checks below use existing repository test entry points. Loopback
fixtures need no camera credentials, multicast, Internet access, or external
inference service. Package tests run with the locked dependency graph.

## Acceptance criteria verification

| Criterion                                                             | Testable outcome                                                                                                                      | Verification                                                                                                                                                                | Observed evidence                                                                                                                  |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Preserve discovery, media, recording, and vendor behavior             | Existing discovery/media tests stay green; native events do not cancel media workers                                                  | Workspace canonical gate; `generic_event_policy_reconfiguration_does_not_cancel_media_workers`; `unchanged_settings_still_require_an_explicit_media_restart`; Reolink tests | Both restart regressions, Reolink tests, and the full canonical gate pass                                                          |
| Retain internal service evidence without secrets                      | Exact namespace and camera-bound endpoint; cached evidence survives a failed refresh without URL serialization                        | `event_service_discovery*`; `camera_events::pullpoint::service_cache_tests`; `discovered_service_is_retained_without_claiming_a_working_subscription`                       | Discovery/cache and sanitized evidence tests pass                                                                                  |
| One complete PullPoint lifecycle per device                           | Create, synchronize, pull, renew, unsubscribe, bounded failure and cancellation                                                       | `camera_events::pullpoint::`; independent `test_hikvision::onvif` fixtures                                                                                                  | 23 focused lease/discovery/fault/queue tests pass; worker test verifies one active subscription                                    |
| Correct motion/tamper intervals and channel attribution               | Exact source filters and independent source/rule/object identities; unrelated clears cannot close other intervals                     | `camera_events::lifecycle::`; `event_normalize`; actual RTSP source-2 fixture                                                                                               | 43 lifecycle tests pass; non-default source-token fixture passes; physical NVR matrix pending                                      |
| Malformed/unknown messages do not become motion or stop healthy media | Invalid messages isolated; valid neighbors retained; unknown namespace/type ignored                                                   | ONVIF XML/normalization/timestamp tests; `malformed_and_unsupported_xml_do_not_stop_events_or_video`                                                                        | Parser and real-video regressions pass                                                                                             |
| Event failure does not reconnect healthy media                        | Event-only replacement, failed images, malformed metadata, and unsupported vendor fallback leave media workers independent            | Root ISAPI/generic event integration; `test-camera` native-event E2E                                                                                                        | Existing focused tests pass; dual-stream recording continues through invalid metadata                                              |
| Native capabilities require evidence                                  | Plain RTSP has no invented kinds; disabled policy stays disabled; new kinds are announced before event delivery                       | `native_event_capabilities_*`; `generic_event_capabilities_and_subscriptions_use_observed_native_kinds`; capability-ordering tests                                          | Capability, ordering, queue-saturation, and source-identity regressions pass                                                       |
| Consume metadata with independent health and bounds                   | One admitted profile; bounded RTP/document queues; metadata cannot satisfy the video watchdog                                         | Retina tests; `camera_events::registry` and `consumer`; HTTP metrics tests                                                                                                  | Registry/consumer loss and handoff regressions pass; HTTP exposes event evidence even without video health                         |
| Support XML/gzip and reject unsupported EXI honestly                  | Standard `+gzip` and legacy `.gzip`; output/ratio/time bounds; no EXI claim                                                           | Retina feature-mode tests; `camera_events::metadata`; real gzip RTSP fixture                                                                                                | Both parser/assembly feature modes and gzip E2E pass in focused runs                                                               |
| Deduplicate transport overlap                                         | Matching PullPoint/metadata observation yields one ID; source-specific loss preserves remaining backing; vendor handoff is sequential | Lifecycle source tests; metadata main/sub test; `automatic_events_fall_back_to_onvif_after_isapi_is_unsupported`                                                            | Replay/class-conflict/profile-handoff regressions pass; no simultaneous vendor/generic reconciliation mode exists                  |
| Preserve only valid Profile T/M evidence                              | Explicit classes, finite confidence, transformations, partial frames, and deletion; no invented image association                     | `event_metadata`; `event_normalize`; `classified_person_box_and_delete_persist_without_image_association`                                                                   | Validated boxes remain structured payloads until an attachment coordinate association is proven                                    |
| Plain RTSP remains usable without native claims                       | Both streams record and no native event/type is fabricated                                                                            | `plain_rtsp_records_both_streams_without_creating_events`; plain-capability regression                                                                                      | Real dual-stream recording and capability checks pass                                                                              |
| Software inference remains distinct                                   | KeepPeek-origin publication does not mutate physical camera capabilities                                                              | `software_origin_does_not_mutate_native_capabilities`; existing `published_detection_is_persisted_and_retry_is_idempotent` and event-publication tests                      | Origin, publication/idempotence, and attachment conformance pass in the canonical gate                                             |
| Clean stop/reconfiguration                                            | Subscription teardown; snapshot cancellation; ordered endings; retired owners cannot accept late starts                               | Consumer pressure tests; PullPoint cleanup; `keeppeek::tests::native`; snapshot queue tests                                                                                 | 11 retired-owner tests pass, including 256 endings and failed commit retries; persistent disk failure remains explicit best effort |
| Reject unsafe URLs and protect sensitive data                         | No foreign endpoint/redirect/downgrade; bounded XML/JPEG; no raw credential or recognition metric labels                              | Endpoint/authentication/snapshot/XML safety tests; camera event policy tests; native metrics tests                                                                          | Canonical security/serialization checks, diff hygiene, protected-API comparison, and the scoped credential-pattern scan pass       |
| Pass required automated gates                                         | Workspace tests, strict Clippy, Rustfmt, UI checks, browser tests, and relevant protocol checks                                       | `./check.sh`; protocol feature/doctest commands                                                                                                                             | Complete canonical gate and supplementary protocol checks pass; existing skips unchanged                                           |
| Document supported behavior and limitations                           | Modes, source tokens, topic support, auth/network limits, push prerequisites, and hardware status are explicit                        | [Operator guide](onvif-events.md), [configuration](configuration-management.md), [ISAPI guide](hikvision-isapi.md)                                                          | Documentation formatting passes                                                                                                    |

## Performance

[The release performance report](native-event-performance.md) contains the
reproducible same-build policy comparison, exact workload, run order, binary and
input hashes, budgets, individual samples, and nearest-rank p50/p95 statistics.
It compares events disabled with RTSP metadata enabled, not two different commits.

The final-build 10-pair run had disabled/enabled maximum-fragment-gap p95 values of
1014.465/1017.661 ms: +3.196 ms against the unchanged 86.667-ms fixture guard.
All 40 finalized MP4 files and all 150 enabled camera events validated. Raw
samples and the final release-binary hash are linked from the report. This does
not establish fleet CPU/RSS, long-soak behavior, or physical-camera performance.

## Unverified hardware

Every row remains pending for this implementation. Existing ONVIF example comments
and previous ISAPI diagnostics are not substituted for new measurements.

| Device scenario              | Required evidence                                                          | Status  |
| ---------------------------- | -------------------------------------------------------------------------- | ------- |
| Axis motion/tamper           | Model/firmware, auth/dialect, start/clear, lease renewal/cleanup           | Pending |
| Dahua/Amcrest forced generic | Same evidence with vendor mode excluded                                    | Pending |
| Hikvision forced generic     | Same evidence independently of ISAPI                                       | Pending |
| Reolink forced generic       | Same evidence independently of Baichuan                                    | Pending |
| Uniview RuleEngine           | Actual advertised rule/topic and stable identity                           | Pending |
| Profile T metadata           | Encoding, RTP loss/recovery, media continuity                              | Pending |
| Profile M classification     | Classes, confidence, transformed boxes, object deletion                    | Pending |
| Gzip metadata device         | Standard/legacy encoding and bounded decode with live media                | Pending |
| Broken/absent PullPoint      | Metadata fallback and continued recording                                  | Pending |
| Non-default NVR channel      | Observed exact source token and correct configured camera association      | Pending |
| Plain manual RTSP            | Native flags false, recording usable, optional external inference separate | Pending |

Use private config-based credentials for read-only probes. Record exact model,
firmware, profile, auth mode, topic dialect, source token, subscription behavior,
encoding, and sanitized results. Verify start/clear and uninterrupted video/store
progress, then stop the process and verify cleanup. No detector settings or audio
output need to be changed for these checks. Multiple independently configured
logical entries at one IP are not introduced by this implementation.

## Combined PR boundary

The same working tree contains owned Rust ISAPI event parsing/transport, analytics,
image correlation, callbacks, management, fake-device coverage, shared motion/PTZ
controls, and protocol-level audio. Its prior full-gate evidence is recorded in
the [control alignment tracker](../tasks/camera-control-alignment.md); the passing
combined gate above supersedes that local checkpoint.

Browser microphone routing remains unimplemented because the protected API has no
camera speaker-session destination. Generic ONVIF push remains conditional on a
demonstrated firmware need. No new inference service or model is provisioned.
No branch, commit, push, published PR, issue edit, or issue closure has been made.
Final-head CI and the hardware matrix must be attached before treating either
broad issue as fully complete.
