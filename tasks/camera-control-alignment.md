# Camera control alignment

## Scope

Prioritize motion detection on/off, verified device capabilities, PTZ and two-way
audio for Hikvision/Annke and Reolink. Do not expand firmware, account, storage,
network, image-tuning or detector-region editing in this task. Preserve `api/`.
All writes and audio tests use local fake cameras, not physical devices.

The shared motion Boolean means detector configuration, not current motion
activity, event subscription state, or generic-event retention. PTZ uses normalized
axes and explicit ownership/stop semantics. Capabilities describe device evidence
separately from what the application can control.

The current protected WebRTC API has a two-way-audio device flag but no camera
talk-session command or speaker media target. This pass implements and tests the
ISAPI protocol/transport, compares it with `reo-proto`, and leaves browser Talk
disabled until an explicit API contract and microphone-routing workflow exist.
The user was unavailable for that scope question; no API changes are assumed.

## Checklist

- [x] Add shared Hikvision motion off/on with read-back and preserved unknown fields.
- [x] Correct Reolink motion configuration reads without using current-motion state.
- [x] Route shared PTZ commands and presets to Hikvision with session ownership and stop.
- [x] Probe and report Hikvision device capabilities without assuming brand-wide support.
- [x] Add bounded ISAPI two-way audio capability, open, stream and close operations.
- [x] Verify controls/audio with the independent fake Hikvision camera.
- [x] Compare `reo-proto` operations and record common meanings, codec/range differences and unsupported controls.
- [x] Document the browser-audio contract gap and disabled UI behavior explicitly.
- [x] Run focused tests, strict checks and the canonical repository gate.

## Review resolutions

- Capability discovery is generation-bound, coalesced and limited to four workers
  and 128 pending cameras. Runtime activation queues rediscovery. Failed probes
  retain verified evidence; explicit negative replies update it.
- PTZ retains the original target through settings replacements, attempts safety
  stops after uncertain movement/preset responses, and does not release ownership
  for reboot-required or unconfirmed stops. Client errors omit peer payloads.
- Audio tests cover one raw authenticated upload connection, dependent full-duplex
  progress, independent codecs, busy ownership, cancellation, partial-write and
  session deadlines, malformed media and transfer codings, and bounded cleanup.
- ISAPI close has no session fence. External applications must coordinate channel
  ownership; `abandon` stops only local handles after known ownership loss. This
  unavoidable protocol limit is documented, not presented as an ownership guarantee.
- No physical-camera writes or speaker output, and no protected API changes.

The user-facing matrix and application boundary are in
[camera-controls.md](../docs/camera-controls.md). Cross-model reviews were skipped
in this non-interactive continuation; two focused in-process review passes and
regression tests informed the fixes above.

## Verification

On 2026-09-05, `TMPDIR=/Volumes/KeepPeekCheckTmp ./check.sh` passed:

- 1,775 Rust tests passed; 19 existing tests skipped.
- Workspace/all-target Clippy, dependency checks and formatting passed.
- Svelte check reported zero errors and warnings.
- 222 Bun tests, 110 browser/visual unit tests and 28 compatibility tests passed.
- 189 Playwright tests passed; two existing codec-capability tests skipped.
- Focused ISAPI core-only tests and all four crate doctests also passed.

The successful full-gate log is
[camera-control-alignment-check-verified.log](../target/camera-control-alignment-check-verified.log).
Earlier logs remain under `target/`. The first run stopped on the existing macOS
MQTT TLS test, which passed in isolation and in subsequent full runs without TLS
changes. Later runs exposed an obsolete Reolink fake assertion and a missing
`const` qualifier. The fake test now verifies detector enable/off/on separately
from idle activity; the initializer is const. No tests or thresholds were weakened.

End-to-end browser talkback remains tracked in open GitHub issues
[#94](https://github.com/xnorpx/keeppeek/issues/94) for Hikvision and
[#95](https://github.com/xnorpx/keeppeek/issues/95) for Reolink. This validation
does not certify physical-camera PTZ/audio or implement the browser Talk workflow.
