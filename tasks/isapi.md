# ISAPI production event integration

## Scope

Make the existing plain-Rust ISAPI crate usable as a continuous camera event
source in KeepPeek. Preserve the Sans-I/O protocol core and optional `ureq`
adapter. No Hikvision SDK binaries, Python runtime, or protected `api/` changes.
This work does not implement unrelated vendor management endpoints or mutate
camera-side settings. Existing issue #67 task files remain untouched.

## Acceptance criteria

- One camera-owned event worker receives ISAPI notifications, reconnects with
  bounded backoff, and stops with its camera. An idle read observes cancellation
  within 250 ms; connection and send attempts remain time-bounded.
- Supported motion and explicit human/vehicle notifications use the existing
  timeline-event persistence and live-delivery path. Repeated active states do
  not create duplicate starts. Unknown fields never manufacture classifications.
  Camera/channel identity, timestamp handling, missing clears, and reconnect gaps
  have explicit tested policies.
- Diagnostic output and errors remain credential-safe. Malformed data, slow
  consumers, and camera failures do not stop media recording or other cameras.
  Up to 128 ordered transitions survive event-queue saturation, with 500 ms per
  delivery attempt and no further ingestion until pending delivery recovers.
  Configuration/selection and remaining protocol limits are documented.

## Ordered work

- [x] Add cancellable continuous `ureq` subscriptions and localhost regression tests.
- [x] Implement bounded event normalization/lifecycle using synthetic notifications.
- [x] Add camera-owned ISAPI worker, routing, status logs, and event-pipeline tests.
- [x] Update the CLI and operational documentation; verify read-only camera behavior.
- [x] Run focused tests, strict linting, review, and the canonical repository gate.

## Verification

```sh
cargo test --locked -p isapi
cargo test --locked -p isapi --no-default-features
cargo test --locked --lib isapi
cargo test --locked --bin keeppeek-camera
cargo clippy --locked -p isapi --all-targets -- -D warnings
cargo clippy --locked --lib --bin keeppeek-camera -- -D warnings
TMPDIR=/Volumes/KeepPeekCheckTmp ./check.sh
```

The final command uses the existing local test volume only on the current macOS
workstation. It avoids the host filesystem's 85-percent-full storage-test limit;
the test thresholds and assertions are unchanged.

## Results

- The final canonical gate passed: 1,692 Rust tests, 19 existing skips, 110 UI unit
  tests, 28 server-compatibility tests, and 189 Playwright tests with two codec skips.
  The local log is [isapi-production-check-final.log](../target/isapi-production-check-final.log).
- Both crate feature modes, doctests, strict Clippy, and native event-pipeline
  tests passed. Cancellation includes a regression against `Read::read_to_end`
  retrying `Interrupted` errors; the transport now uses typed cancellation.
- The read-only I91ET Rust diagnostic authenticated, parsed two heartbeat
  notifications, and completed its 15-second observation without camera changes.
  Real-camera person/vehicle classification remains unverified; synthetic tests
  cover classified-event persistence and live routing.
- The supplied PDF and Go client were reviewed as references. The Go client is
  GPL-3.0; no implementation was copied into the MIT crate. Cross-model review
  was offered but not run because the user was unavailable.
