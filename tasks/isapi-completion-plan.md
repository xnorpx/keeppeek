# ISAPI capability completion plan

## Contract

Implement all five requested capabilities in plain Rust. Keep protocol data,
encoding, parsing, correlation, and request construction Sans-I/O. Use `ureq`
for outgoing HTTP and the repository's bounded HTTP server infrastructure for
incoming callbacks. Preserve all existing public contracts under `api/`.

The first event-ingestion milestone is complete; it is not full protocol coverage.
This plan extends it without discarding that milestone's tests or results. The
scope includes richer person/vehicle/ANPR events, correlated JPEGs, legacy text
encodings, authenticated callbacks, and typed camera-management operations.
Unknown vendor fields must remain available without being treated as verified
classifications. No arbitrary camera mutation, SDK binary, Python worker, new
public API schema, commit, or deployment is authorized by implementation work.

## Capability map

| Module     | Responsibility                                                                                        | Dependencies                      |
| ---------- | ----------------------------------------------------------------------------------------------------- | --------------------------------- |
| encodings  | Strict BOM/MIME/XML-declaration decoding and output bounds                                            | Existing byte limits              |
| analytics  | Structured nested events, attributes, coordinates, ANPR, object identity                              | encodings                         |
| images     | Explicitly correlated metadata/JPEG bundles and attachment identity                                   | analytics, multipart              |
| management | Typed endpoints/capabilities, safe serialization, response status                                     | encodings, request/auth transport |
| callbacks  | Authentication, bounded HTTP ingress, replay policy, callback configuration and shared event delivery | images, management                |

Build order: encodings, analytics, images, management, callbacks, integrated validation.

## Detailed checklist

### Encodings

- [x] Decode XML/JSON using MIME charset, XML declaration, and BOM evidence.
- [x] Support UTF-8, UTF-16 LE/BE, GB2312/GBK and GB18030 through a maintained codec.
- [x] Reject contradictions, unsupported labels, invalid sequences, and expansion beyond limits.
- [x] Exercise non-ASCII names, split MIME input, malformed sequences, and output bounds.

### Analytics

- [x] Retain bounded nested XML/JSON data, object attributes and identifiers.
- [x] Parse human, vehicle, ANPR, region and object collections without recursive field guessing.
- [x] Preserve classification evidence, confidence scales, coordinate units and image references.
- [x] Map supported analytics into persisted/live KeepPeek events without exposing raw payloads in logs.
- [x] Test multiple objects, unknown fields, duplicate IDs, invalid geometry and conflicting classifications.

### JPEG association

- [x] Preserve MIME Content-ID, disposition name and filename as opaque correlation identifiers.
- [x] Build bounded event/image bundles using explicit references, not nearest-image guesses.
- [x] Handle images before/after metadata, duplicate/conflicting parts, expiry, and reconnect reset.
- [x] Persist associated snapshots with matching bounding-box/image identity and emit committed revisions.
- [x] Test interleaved events, incomplete bundles, wrong IDs and resource exhaustion.

### Typed management

- [x] Add typed device information, capability, clock and channel queries.
- [x] Add read/modify/write motion/rule and streaming configuration while preserving unknown fields.
- [x] Add validated PTZ status, continuous/absolute/momentary control and presets.
- [x] Add bounded JPEG snapshot retrieval with verified content type.
- [x] Add callback-host capability/list/create/update/delete/test operations.
- [x] Handle GET/POST/PUT/DELETE, XML/JSON bodies, and device-level ResponseStatus on HTTP success/failure.
- [x] Verify exact paths, methods, serialization, request signing and errors with fake HTTP devices.

### HTTP callbacks

- [x] Add opt-in bounded listener configuration and per-camera authentication using private secret references.
- [x] Authenticate before event processing, bind identities to configured senders, reject unsolicited senders.
- [x] Receive multipart/form-data and documented XML/JSON bodies with bounded reads and timeouts.
- [x] Route to the same lifecycle/image/storage/live path, with explicit acknowledgments and deduplication.
- [x] Prevent push and alert-stream delivery from duplicating the same configured event source.
- [x] Test rejection, accepted delivery, replay, malformed bodies, body/connection limits and shutdown.

### Documentation and verification

- [x] Replace earlier unsupported claims only when implementation and tests cover them.
- [x] Document activation, per-operation capabilities, encoding/association policies, auth and rollback.
- [x] Run all crate feature modes, documentation examples, focused application and HTTP tests.
- [x] Run strict Clippy, formatting, dependency audit/review, and fresh-context review of sensitive boundaries.
- [x] Run the unchanged canonical `./check.sh` gate and record actual results.
- [x] Report physical-device tests separately from synthetic capability coverage; no invented device support.

## Resource and failure policy

Keep encoded XML/JSON at 256 KiB and bounded decoded text at 1 MiB. Maintain depth
32 and 8,192-node limits. Bound analytics objects and image references per event,
pending bundles by count and bytes, and expiry by caller-provided monotonic time.
Use explicit rejection rather than silently dropping security/identity fields.
Network requests and callback bodies have finite deadlines; cancellation must
not use retryable Interrupted errors. Failed management writes are not retried
without a read-back policy. HTTP 200 is not device-level success.

The supplied [PDF](../docs/isapi.pdf) is protocol evidence, not a source-code
license. GPL reference clients are not copied into the MIT crate. Retain existing
MIT attribution for adapted PTZ conventions.

## Verification commands

```sh
cargo test --locked -p isapi --no-default-features
cargo test --locked -p isapi
cargo test --locked --lib isapi
cargo test --locked --bin keeppeek-camera
cargo clippy --locked -p isapi --all-targets -- -D warnings
cargo clippy --locked --all-targets -- -D warnings
TMPDIR=/Volumes/KeepPeekCheckTmp ./check.sh
```

The TMPDIR override is specific to the existing mounted macOS test volume, whose
capacity satisfies storage tests without weakening their thresholds. Do not
overwrite the separate issue #67 task files.

## Shared fake Hikvision test device

- [x] Add an independent `test-hikvision` fixture crate to avoid a KeepPeek dependency cycle.
- [x] Verify Digest credentials and retain typed management resource writes.
- [x] Support scripted streams, JPEGs, callbacks, fragmentation, delays and error responses.
- [x] Migrate camera-facing HTTP client, worker and persistence tests to the shared fake.
- [x] Cover real callback uploads, ANPR/image persistence, removal and cancellation with the fake.
- [x] Expose `test_camera hikvision` and document fixture-only credentials and limits.

The remaining operational caveats are explicit contracts: per-process callback
deduplication retains 1,024 successful body hashes for five minutes; the listener
uses an isolated HTTP network or an explicitly trusted HTTPS proxy; storage calls
use the existing synchronous catalog/filesystem infrastructure. No indefinite
exactly-once or hard-cancellable disk-I/O guarantee is claimed.

## Final verification

On 2026-09-05, `TMPDIR=/Volumes/KeepPeekCheckTmp ./check.sh` passed on the
unchanged implementation. The full local log is `target/isapi-completion-check.log`.

- Rust workspace: 1,741 tests passed, 19 existing skips.
- Strict workspace/all-target Clippy, Cargo machete, Rustfmt, Taplo and Black passed.
- UI quality: 222 Bun tests, 110 browser/visual tests and 28 compatibility tests passed.
- Playwright: 189 passed, two existing codec-capability skips.
- Both ISAPI feature modes and all three crate documentation examples passed in
  focused validation. Fake-camera CLI help and real loopback Digest callbacks passed.

No physical camera settings were changed, and no commits or remote changes were made.
