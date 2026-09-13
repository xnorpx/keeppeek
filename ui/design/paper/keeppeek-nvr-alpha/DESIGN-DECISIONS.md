# Alpha design decisions

The audit compares current implementation, current Paper source, and historical visual baselines
as distinct evidence. It does not make the application correct by redrawing the reference around
a defect. The original audit kept application code unchanged. Subsequent authorized fixes implement
the compact mobile workflow; their regression evidence and remaining qualification are tracked in
[bugs.md](../../../../bugs.md).

## Style and navigation

Keep the established graphite surfaces, restrained rust accent, Archivo interface text, and IBM
Plex Mono timestamps. Preserve all 80 shared tokens. Rust text uses `--color-primary-soft`;
small text must not use the lower-contrast primary fill color. Motion markers retain their
semantic activity color, and ordinary recording coverage stays neutral. Color is accompanied
by labels, selection borders, or explicit state descriptions.

Use separators and clear type hierarchy instead of adding decorative panels. Footage remains
the main visual content. Keep Dashboard and Viewer as distinct destinations and preserve the
existing mobile navigation. Historical story names and simplified story shells are not proof
that their corresponding production routes match the reference.

## Judgments from the implementation comparison

| Surface              | Decision                                                                                                                                                                       |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Dashboard and Viewer | Preserve the deliberate route separation and working layouts; compare actual routes, including controls and data states, before accepting pixels.                              |
| Mobile Keep          | Retain the implemented horizontal portrait timeline. Supersede the old vertical reference with Board 46's compact layout; track the current command-stack defect as KP-QA-013. |
| Storage              | Use the reviewed storage-setup supplement as the current target. Preserve validation, review, restart expectations, and recovery; the older settings page is historical.       |
| User access          | Use the per-user grant and conflict states, including retained drafts. Preserve default access and the distinction between camera grants and layout audiences.                 |
| Configuration ZIP    | Use the current backup/restore states. Do not weaken the owner-only export requirement to match the Windows ACL defect, KP-QA-001.                                             |
| Recording integrity  | Include Board 45. Confirm real restore, playback, and export before declaring recovery qualified.                                                                              |
| Onboarding           | Keep cancellation easy to find and at least 44 px high on touch. The KP-QA-002 regression checks the actual hit area and cancellation action.                                  |
| Other NVR boards     | Keep their freshly captured source traceable. Review each supported state against production; capture alone does not approve legacy capability assumptions.                    |

## Board 46: mobile Keep

The audited 390×844 implementation placed footage around y=365 and pushed timeline content
below the first viewport. The old Paper frame starts footage much earlier but uses a vertical
timeline and a simplified player. Neither is a complete target for today's controls.

The new target keeps horizontal history and has three 390×844 states:

1. **Default investigation:** all five Keep modes, camera/date selectors, digital zoom, footage,
   play/pause, ±10 seconds, speed, mute, fullscreen, recording position, history, and event preview.
2. **Playback options:** volume, all seven existing rates, Auto/High/Low quality, moment link,
   and recording refresh. Playback and the speed control open this sheet.
3. **Camera and date:** searchable cameras, previous/next camera with a position indicator,
   direct recorded-day selection, previous/next recorded day, selected state, and an explicit
   explanation when no later recording exists. Preserve wrapping camera navigation and disable
   it during switching or when fewer than two cameras are available.

This is a layout and interaction target, not a new media protocol. Keep the existing supported
rates (0.25, 0.5, 1, 1.5, 2, 4, 8), moment-link context, quality fallback, and capability checks.
Do not invent footage where a gap exists or imply that a disconnected camera has recordings.

### Functional acceptance

- Keep visible footage, usable transport, and at least one recording/event row above mobile
  navigation in the initial 390×844 view. Test actual element intersection, not only document width.
- Digital zoom controls stay outside the transformed image. Preserve zoom bounds, disabled
  controls at limits, reset, and panning. Timeline zoom is visibly separate from image zoom.
- Keep the direct recording-position slider; timeline seeking does not replace it. Preserve
  keyboard seeking, canceled drags, gaps, previews, UTC labels, and selected-moment state.
- Camera/date changes preserve the selected moment where available. If unavailable, show the
  gap and existing previous/next-recording actions rather than silently displaying a different time.
- Changes in sheets apply immediately. Done, Escape, and Back close the sheet and restore focus.
  Background controls are inert while it is open; opening a sheet does not itself change playback.
- Keep all existing loading, unavailable-media, permission-loss, copy failure, playback failure,
  and fullscreen failure behavior. Important errors appear in the visible player region with
  actionable copy. Error states may need more space than the healthy reference.

### Responsive and accessible acceptance

- Interactive hit regions are at least 44×44 px on touch, including bottom navigation, sliders,
  the close action, modes, and event entries. Disabled styling must remain understandable.
- Buttons and sliders have accessible names, visible focus, and keyboard operation. Use native
  semantics and announce status/error changes without announcing every playback-time tick.
- At 320 px, preserve access to every mode through a clearly scrollable mode row and keep the
  active mode visible. Do not shrink targets or text to force the 390 px drawing into place.
- At larger text sizes, landscape, or with a software keyboard, allow useful scrolling and bound
  sheet height to the visible viewport. Do not clip content to preserve the healthy-state drawing.
- Respect reduced motion. Check contrast in light and dark themes and exercise a screen reader.
  The three dark 390 px reference frames are not evidence for every device, theme, or input mode.

### Performance acceptance

Retain bounded timeline rendering, cancellation of obsolete requests, and existing latency gates.
Opening a sheet must not decode another player or reload history. Camera and history lists stay
bounded as fleets and recordings grow. Use real route and media tests for this work; lightweight
story screenshots and the offline export-integrity test do not qualify decode or interaction cost.

The compact production layout follows Board 46, with default E2E coverage for initial history
visibility, decoded footage, sheets, controls, and navigation. The existing Loki baselines are
unchanged. Board 46 remains a `proposal` while complete device, accessibility, and visual
qualification is outstanding; the original mobile frame remains available for history.
