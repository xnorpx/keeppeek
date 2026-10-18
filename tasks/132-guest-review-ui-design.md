# Guest review UI design handoff (#132)

Status: Boards 51-56 are authored and visually reviewed. Six original JSX sources and
eighteen original lossless 1x PNGs were exported at `2026-10-18T06:55:40.352Z`.
Exact dimensions, byte counts and hashes are registered in the existing manifest. The original
design-export checkpoint contained no runtime UI or protected API edits. Runtime integration now
exists in this branch as described below. These designs remain proposals, not approved visual
baselines or functional issue completion. The root agent coordinates browser and canonical checks
around the shared archive gate.

## Scope and source

Use the existing [KeepPeek Paper file](https://app.paper.design/file/01M0B0VBH78TMTX40GCYYQ37SG/1-0),
page `1-0`, token revision `b35ec365`. New Boards 51-56 extend the current NVR references.
Existing boards and their exported files remain unchanged. The
[manifest](../ui/design/paper/keeppeek-nvr-alpha/manifest.json) records the final original JSX,
lossless PNG exports, dimensions, byte counts and SHA-256 values.

Completed-export sharing is the first implementation increment. All four creator entry points
remain required: ready export, Keep selected range, event detail and bookmark. They open the same
creation surface with the chosen evidence preserved. An event or bookmark resolves to a supported
bounded recording range before creation; a missing or unsupported range shows a local unavailable
state rather than silently choosing different footage. Full #132 acceptance includes exact-range
creation and does not close after completed-export sharing alone.

| Entry point         | Placement and evidence ownership                                                                       |
| ------------------- | ------------------------------------------------------------------------------------------------------ |
| Ready export        | Add **Share guest link** beside **Download** after the immutable artifact is ready.                    |
| Keep selected range | Add **Share guest link** to the selected-range actions; preserve the selection, source and coverage.   |
| Event detail        | Add **Share guest link** to the event actions; resolve its supported bounded clip before preview.      |
| Bookmark            | Add **Share guest link** to the bookmark row actions; preserve that bookmark's supported bounded clip. |

The proposed guest route is `/share`. Creator management stays in the authenticated Keep surface.
The guest route has only its granted footage, permitted playback/download controls and access
states. Cross the ordinary-app/guest boundary with a full-document navigation or complete owned
client/logger disposal so an earlier SPA visit cannot retain ordinary browser logging or control
initialization. It has no recorder navigation, normal control client, capability/health query, live camera,
search, settings, logs, snapshot, PTZ or talk controls.

## Established components and tokens

Reuse Board 29's ready-export card, Board 33's pending/error language, the existing native dialog
and button patterns, and the current mobile shell spacing. The cloned ready card in these new
boards is updated to the same example as the target preview: Front step, 17 October 2026,
06:11:48-06:12:48 UTC, one minute, 24.6 MB. Its artifact expires later than the guest link. Old
Board 29 is unchanged. This fixture stays below the inherited two-minute/512 MiB ceiling.

Use existing ground/surface/raised, hairline, text/text-muted/text-faint, primary/primary-soft and
healthy tokens. Archivo is the interface face; IBM Plex Mono is for timestamps, evidence labels
and the displayed address. All authored text has an explicit foreground token because Paper's
unknown inherited text color can render black on a dark surface. Load bundled fonts and assets
from the same origin; the guest page must not contact an external font or analytics service.

## Exported states

| Board | Paper node | Original JSX                                                                                  |
| ----- | ---------- | --------------------------------------------------------------------------------------------- |
| 51    | `BPP-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/51-guest-review-target-preview.jsx.txt) |
| 52    | `BWV-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/52-guest-review-copy-once.jsx.txt)      |
| 53    | `C1I-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/53-guest-review-list-revoke.jsx.txt)    |
| 54    | `C62-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/54-guest-review-ready.jsx.txt)          |
| 55    | `CA2-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/55-guest-review-expired.jsx.txt)        |
| 56    | `CCO-0`    | [Source](../ui/design/paper/keeppeek-nvr-alpha/boards/56-guest-review-unavailable.jsx.txt)    |

Every board has 1440px, 390px and 320px references in the manifest. All eighteen
frames were visually reviewed for spacing, type, contrast, alignment and wrapping. The
320px preview is 918px tall; other mobile captures are 844px. No old board, JSX, PNG or
token snapshot was changed. Local checkpoints remain in `target/agent-artifacts/paper-132/`.
All six new boards released their Paper working indicators.

## Creator flow

The preview shows the exact immutable evidence, source/time bounds, duration, size, coverage,
expiry, review limit and optional-download permission before **Create link**. A recording-range
preview must describe the actual delivered interval. Earlier GOP footage cannot be hidden by the
player while the UI claims an exact range. Unsupported exact bounds must be rejected explicitly.

The configured external HTTPS recipient address is visible before creation. Explain that the
recipient must be able to reach it and that KeepPeek provides no tunnel or relay. Do not derive
the public address from an arbitrary Host header or the current private browser origin. Disabled
or invalid sharing policy gives a visible explanation and disables creation.

Retention wording says that deletion or artifact expiry can end access sooner. Creating a link
adds no implicit evidence hold. The preview caps link expiry at the artifact horizon. Creating,
failed, indeterminate and successful states remain distinct. After an indeterminate creation
result, reconcile through the safe list and permit revocation; do not retry by producing a second
secret or reveal an earlier secret again.

After successful creation, show the fragment URL once with **Copy link**, explicit clipboard
feedback, a selectable manual-copy fallback and a clear warning before closing. Closing clears
the displayed secret. **Manage links** lists safe metadata, expiry, state and usage only; it never
reveals or copies an existing secret. Revoke uses a confirmation with the evidence label, effect
on active viewing and **Cancel / Revoke link**. Cancellation receives initial focus; Escape cancels
and returns focus to the initiating action. Pending revocation retains its selected target and blocks conflicting list changes.
A failed or conflicting revoke keeps the row and a visible retryable error.

One review is counted when a guest session first receives media; seeking in that same session
does not count again. Downloads count separately when their first bytes are committed. The list
must use the server's durable counters and distinguish an exhausted limit from a transient error.

## Guest flow

Remove the URL fragment immediately and exchange the secret through the dedicated handoff.
Use the isolated short-lived guest session; never convert it into a normal KeepPeek principal or
fall back to ordinary API authentication. No secret is retained in browser persistent storage,
rendered diagnostics, referrers, status URLs or safe metadata lists.

Ready shows the granted title, exact time range, media, a bounded position control, play/pause,
expiry and **End review**. This view-only example states **Download not permitted**. When the
server grants download permission, replace that message with a 44px **Download MP4** action;
never show the normal authenticated download link. There is no adjacent timeline, camera picker,
previous/next clip, still image or recorder navigation. Media not yet ready shows a bounded loading
state; an error keeps its owned status and recovery action rather than an empty video rectangle.

Unknown, expired and revoked links receive the same first-access **This link is unavailable**
response with no evidence title, date, thumbnail or reason oracle. After a successful handoff,
only that grant's safe status may explain expiry, revocation, retained-away evidence or a consumed
limit. The expired board represents this post-handoff case: stop playback, clear usable media and
say the link expired. A revoked session uses the same layout with a safe revoked explanation.
Recovery asks the creator for a new link and never opens another recorder surface.

## Responsive and accessibility requirements

Each state has a desktop, 390px and 320px reference. Phone content forms one column. Long URLs,
labels and timestamps wrap without horizontal scrolling. Full-content mobile captures may exceed
844px to show the whole vertical flow; runtime dialogs and documents must scroll with their title,
close/cancel and primary actions reachable. Do not shrink text or touch targets to fit one screen.

Use 44px minimum actions and a 44px hit area around the thin painted seek track, visible keyboard focus, native semantic controls and an explicit heading
for the modal/page. Restore focus after close, trap focus only while a native dialog is open, and
support Escape for non-destructive dismissal. Announce asynchronous copy/create/revoke results as
status, failures as alerts, and playback access loss once without moving focus unexpectedly.
Reduce motion and keep access meaning visible in text, independent of color.

## Implementation and verification boundary

The Paper designs define presentation and behavior. Current-task user approval now covers the
additive sharing management contract and dedicated guest transport. Follow the synchronized
management protocol and `/guest/session` exchange/status/delete, `/guest/media`, and
`/guest/download` contracts. The approved guest route is exactly `/share`. Reuse the isolated
grant/session/domain interface; no undocumented endpoint or overloaded normal command is implied
here.

Before completion, run the additive Paper integrity checks and the canonical Windows `check.bat`.
Then verify actual HTTPS creator and guest flows at desktop/390/320, keyboard and clipboard
fallback, role/source denial, all four entry points, exact range bounds, permission-specific
download, stale response ownership, expiry/revoke during playback, retention deletion and restart.
Verify fresh guest navigation and normal-app-to-guest navigation produce no normal control,
health, profile, capability or external-resource requests. Check CSP, no-store, no-referrer,
frame prohibition and same-origin assets from actual responses. Paper captures do not prove
runtime accessibility, transport security, pixel parity, revocation latency or issue completion.

## Current runtime source and evidence

The thin root layout lazy-loads the normal AppShell and renders exactly `/share` without the
ordinary control client, logging store or shell. Crossing that boundary uses a new document,
including the queued SvelteKit navigation fallback. The trusted early app.html script removes
fragments before Kit starts and provides a one-use memory handoff. Invalid fragments cannot
resume the existing guest cookie. Status, playback and download remain bound to the returned
nonsecret session UUID; a changed cookie context cannot silently switch the evidence.

The first creator path now adds the shared dialog to ready Keep exports and puts deployment
policy plus safe grant pages in Settings Access. Creation shows the full immutable aligned MP4,
its actual bounds and artifact expiry, optional download, positive quotas and the configured
external HTTPS destination. It does not claim the original requested recording range is exact.
The URL stays in the dialog only, and is cleared on close, unmount, principal change or pagehide.
Unconfirmed creation exposes only the safe grant reference and list/revoke recovery. Grant pages
replace the current 50-row page rather than accumulating records. Revision and evidence-wide
revocation require deliberate confirmation, with Cancel receiving initial focus.

The earlier viewer-only checkpoint passed Svelte diagnostics, 32 server/unit/Paper tests
and seven guest/layout component tests. Its `guest-ui-*.log` files remain historical evidence.
The current creator and isolation checkpoint uses separate `guest-ui-current-*.log` files:

- `bun run check`: zero Svelte errors and warnings after preserving and fixing the initial errors.
- Focused server/helper/Paper/capability checks: 29 Bun tests and 22 compatibility tests passed.
- Canonical test-selection guards: five passed; every source unit spec has one CI owner.
- Guest and creator Chromium components: all 21 tests passed in 3.79 seconds.
- Real-Kit isolation E2E: all five cases passed in 16.0 seconds using mocked guest HTTP.
- The E2E TypeScript project passes with an explicit installed-binding guard for its storage audit.

The real navigation regression first exposed a queued-route failure: Kit had already changed the
URL, so replacing that same URL did not create the required document. The fallback now reloads,
and the unchanged new-document, token-storage, logging and module-isolation assertions pass.
The ignored UI-only E2E configuration starts Vite without a backend build. These development
HTTP checks do not prove actual HTTPS transport, server headers or backend authorization.

The remaining creator entry points, exact bounded recording derivatives, real backend/browser
integration, HTTPS response/header proof, runtime 320/390 visual checks, performance and the
canonical Windows gate remain required. No current source-only or focused-component evidence
closes those acceptance items.
