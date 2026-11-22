# Recorded audio timing UI design

The approved recorded-only increment uses per-camera default, Main, and Sub offsets. Positive
values delay audio; negative values advance it. Whole millisecond offsets are bounded to ±5000 ms,
and magnitudes above 1000 ms warn. Live correction remains separately gated. Settings persist in
the existing application configuration; previews belong to the current stored-media cursor.

The source is the existing [KeepPeek Paper file](https://app.paper.design/file/01M0B0VBH78TMTX40GCYYQ37SG/1-0).
Boards 57–60 were designed before the new controls, using the established Archivo and IBM Plex Mono
fonts and existing token revision `b35ec365`. No hosted fonts or token fork were introduced.

| Board | Paper node | Desktop | Mobile   | Interaction                                                                          |
| ----- | ---------- | ------- | -------- | ------------------------------------------------------------------------------------ |
| 57    | CFA-0      | 1440    | 390, 320 | Saved camera default; inherited Main; explicit-zero Sub; save/discard                |
| 58    | CO6-0      | 1280    | 390, 320 | Configured, applied, and draft offsets; apply/reset a temporary preview              |
| 59    | CZI-0      | 1280    | 390, 320 | Normalized container evidence, captured export correction, unknown legacy timing     |
| 60    | D7B-0      | 1280    | 390, 320 | Retained failed draft, large-offset warning, no audio, unsupported or failed preview |

The desktop and both narrow widths were visually reviewed for all four boards. The review checked
explicit foreground colors, wrapping, actions, and complete mobile content. Board 57's form and
Board 59's long mobile diagnostics were repaired before export to remove clipping. Board 58 keeps
the video/timeline context visible around its calibration surface. Board 60 uses distinct save and
preview failure examples; it does not imply that an unaccepted draft changed saved settings.

Board 60 also includes explicit initial-open recovery, reviewed on desktop and at 320 pixels:
when saved correction prevents opening, a supporting server can retry with an unadjusted zero
preview. This is an opt-in cursor override, with no silent fallback and no configuration or export
change. The action is hidden without `keeppeek.recorded-audio.v1`, because older servers may ignore
an unknown additive preview field. The original generic error and ordinary Retry remain available.

The [existing alpha bundle](../ui/design/paper/keeppeek-nvr-alpha/manifest.json) now contains
44 boards and 79 original lossless PNGs. This increment adds only four JSX files and twelve native
1× PNGs. Each new board has its own UTC export time and every artifact has its actual byte length,
dimensions, and SHA-256 hash. The original 40 board objects and all previous artifact hashes were
verified unchanged during export. The external file contains 96 artboards across products; the
unmerged guest-sharing boards 51–56 were preserved externally and were not imported into this branch.

The settings form must keep inheritance distinct from explicit zero, retain a captured configuration
write revision while a draft is dirty, and send a calibration-only replacement. Omission means
unchanged; an empty settings object resets inheritance. A timing settings fingerprint identifies the
captured source/timing settings and must never be used as the configuration write revision.

Keep preview uses viewer-authorized stored-media state. It does not fetch administrator settings.
Applying a draft seeks to the displayed absolute moment before changing the cursor's preview;
play/pause intent and rate are preserved. Nonzero correction uses timestamp-based MSE segments,
and zero correction retains the existing sequence mode. A new seek or preview generation establishes
its own video anchor. Reset requests configured settings rather than a zero preview.

Diagnostics describe normalized stored container PTS and inspected audio presence. They explicitly
leave raw capture-clock skew and browser buffering unavailable and do not claim audible lip-sync.
Leading clipping counts complete encoded audio samples, not individual PCM frames. Exports display
the server-captured correction snapshot; missing historical metadata is unavailable, never invented
as zero. No export correction mode selector is added.

The additive integrity test preserves the original exact board/scenario, path confinement, hash,
PNG dimension, and token checks, then registers all twelve audio frames. Runtime and decoded media
qualification remain separate from these proposal artifacts. The repository canonical Windows gate
must pass before completion; focused checks and these Paper exports do not replace it.
