# Live Auto diagnostics proposal

Boards 61–62 were authored and visually reviewed in the
[shared Paper file](https://app.paper.design/file/01M0B0VBH78TMTX40GCYYQ37SG/1-0).
This extension preserves the existing NVR bundle and all 96 prior artboard identities,
names, dimensions, positions, and child counts. The token revision remains `b35ec365`.

The boards reuse the existing LiveVideo diagnostics popover. Each contains a 1280px
desktop viewport and 390px/320px viewports. The states cover steady Auto, a pending
Sub handover, an unknown network estimate, recovery hysteresis, manual High, and a
server that does not supply optional quality telemetry. All figures are illustrative.

The design keeps these quantities separate:

- **Session network estimate:** the server's bandwidth estimate for the shared connection.
- **Camera budget:** the share assigned to one Auto video subscription after reservations.
- **Receive bitrate:** bytes measured by the browser for the received video track.

Requested, active, and pending variants retain their existing owners. The UI reports
the server's decision; opening diagnostics does not start an adaptation controller.
Manual quality choices remain explicit. Recovery text describes a stability hold
without inventing a countdown. Network, jitter, and receiver values do not establish
camera-to-screen delay or decoder capacity.

An absent optional diagnostics message is unavailable. A present message without a
bandwidth estimate is unknown. A present allocation of zero is a real zero budget.
The existing `ConnectionUpdate` can supply the session estimate independently; no
update means unknown, and a reported zero remains zero. Convert wire `uint64` values
only when they fit a JavaScript safe integer. Disconnects and replaced peers discard
their telemetry, and messages from abandoned peers cannot repopulate it.

The [manifest](manifest.json) records the exact source nodes, native export times,
dimensions, byte counts, and SHA-256 hashes. PNGs are the original lossless Paper
exports at its default 2× scale. JSX is the original inline-style `get_jsx` output
with a final newline. Computed styles are retained separately. All six viewports and
both full boards were screenshot-reviewed for fit, wrapping, contrast, and alignment.
Only the two new boards were marked finished.

These are `proposal` references. They do not approve implementation pixels, replace
Linux visual baselines, or complete #131's recorded-playback, decoder, timing, and
performance qualification. Production keyboard, focus, responsive geometry, and
session-isolation checks remain separate runtime evidence.
