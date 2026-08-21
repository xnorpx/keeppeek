---
target: the Paper design boards
total_score: 26
max_score: 40
na_heuristics:
p0_count: 2
p1_count: 3
timestamp: 2026-08-19T04-46-08Z
slug: p-paper-design-file-01m0b0vbh78tmtx40gcyyq37sg-1-0
---
# Critique — KeepPeek NVR design specification, 28 Paper boards

Method: dual-agent (A: Explore/design-review · B: Explore/detector-evidence). Assessment A ran
fully isolated across all 28 boards plus PRODUCT.md and features.md. Assessment B ran isolated and
completed its token cross-check for real, but its session exposed no command-execution tool; its
three scan tasks were re-executed in the parent after A returned. Detector, contrast and grep
figures below are executed output.

## Design Health Score

| # | Heuristic | Score | Key issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | 3 | Per-tile state, gap rendering and "last frame 41s ago" are exceptional — but `Loading` appears 0 times across 28 boards. No pending, scanning or seeking state exists anywhere, including a wizard with a 5-second discovery scan. |
| 2 | Match System / Real World | 3 | Board 11's fleet list shows "PUBLISHED BY SERVICES" and "DERIVED FROM front-door · MAIN" — wire-contract vocabulary in the list a shop owner scans. |
| 3 | User Control and Freedom | 2 | `Undo` = 1 occurrence, `Cancel` = 2, both only in board 28 prose. Board 20's "Erase all recordings" is guarded by a sentence; board 08 fully draws a typed-name dialog for deleting a layout. Safeguards inverted. |
| 4 | Consistency and Standards | 2 | Two icon families against board 05's "Never mix icon families"; 40 distinct raw colour literals against a stated sixteen; 18 distinct font sizes against an 8-value scale. |
| 5 | Error Prevention | 3 | Board 21 asserts space and write permission were checked "before this screen let you continue" — the not-writable failure is never drawn. Board 11's bulk Remove sits beside Restart stream with no confirmation. |
| 6 | Recognition Rather Than Recall | 3 | 23 "Server update required" pills across 11 boards; boards 16, 17 and 18 each place three on one screen, none naming the command it replaced. |
| 7 | Flexibility and Efficiency | 2 | `keyboard` = 0, `shortcut` = 0 across 28 boards. `kbd` is on board 05's manifest and never used, while board 08 asserts "ARROW KEYS NUDGE ONE COLUMN". |
| 8 | Aesthetic and Minimalist Design | 3 | Board 13's disk bar is a five-segment stack requiring a five-item legend beneath it to be readable. |
| 9 | Error Recovery | 3 | Best copy in the spec ("Switching this camera to TCP usually fixes it → Switch to TCP"), but Diagnose appears on 6 boards and its destination is drawn only at 390px. |
| 10 | Help and Documentation | 2 | Six destinations in the IA, none is help. Settings search promised on boards 05 and 13, drawn only on mobile board 27. |
| **Total** | | **26/40** | **Competent — strong ideas, real delivery gaps** |

All ten scored; none `n/a`. Applicable maximum 40.

## Design Specificity Verdict

LLM assessment: authored for this product in three places, borrowed in the other twenty-five.
Boards 04, 12 and 14 could not be lifted into another product. Board 04's vertical right-edge
timeline (live edge pinned to top, time downward, 17px leader line, two follow states, "one wheel
notch downward stops the follow") is a specific answer to "silence is the failure mode". Board 14
exists only because KeepPeek does not detect, and turns that boundary into a working
`event_type → SHOWS AS → COLOUR → SWIMLANE` surface with an `anything else` catch-all. Board 12's
"Every step ends in evidence, not a claim" with `FIRST KEYFRAME 0.7s · 43 FRAMES IN 1.8s` is
NVR-specific.

Structural sameness is severe: boards 13, 14, 16, 17, 18, 19 and 23 are the same frame seven
times, three carrying an identical sentence about the anchor rail. Board 11's fleet table would
survive find-and-replacing "camera" with "scrape target".

Largest missed opportunity: the movement from Peek to Keep — "take me back forty seconds" — is
drawn on none of the 28 boards. The only live-to-past jump in the spec is a phone notification
action on board 18.

Deterministic scan: `detect.mjs --json /tmp/kp-critique-2/boards` → `[]`, exit 0; same with
`--no-config`. NOT a clean bill of health. A planted control containing `fontFamily: "Inter"`,
`transition-all duration-300`, `shadow-md` and `text-[13.5px]` in the same JSX shape also returned
exit 0; the identical `font-family: Inter` in a .css file returned `[overused-font]`, exit 2. The
detector is functional; its JSX rule coverage does not reach Paper-exported markup. Recorded as
deterministic scan of limited applicability for this target class.

Executed contrast audit (contrast.mjs, 16 tokens resolved, alpha composited over real ancestor
backgrounds): 2,157 text pairings scored, 109 failures (5.1%) in 24 groups. Six failures are
annotations printed onto simulated video surfaces (2.16–2.62:1) — candidate false positives of the
spec medium. ~103 real failures.

Two previous P1s are fixed: `--color-availability` #39414A → #5A6470 now measures 3.23:1 on ground
(clears the 3:1 graphics threshold); `--color-live` #E5484D → #C93B40 now gives white-on-live
5.02:1 (clears 4.5).

Visual inspection: no browser injection applies — the target is a Paper file, not a URL. Board 09's
export panel and board 16's access screen were inspected directly in Paper at 1x and 2x; both
confirm the findings below.

## Overall Impression

A genuinely good specification with one hole in exactly the wrong place. The thinking is causal
rather than decorative — board 23 pre-writes a future error's cause, board 15 puts the remedy in
the row that reports the fault, board 04 designs failure modes alongside the happy path. Four of
five jobs are complete end to end. The fifth — get that moment out as a file — is reserved, not
designed, and it is the one PRODUCT.md marks MVP-mandatory.

## What's Working

1. Board 04's timeline is a real invention specified to build depth: live edge as a marker at a
   time rather than a header label; cards absolutely positioned so the column never reflows against
   incoming data; overlapping cards collapsing into a counted stack; playhead stopping at the card
   lane so it never crosses a thumbnail; five fixed zoom levels with px/hr, tick interval and bucket
   size; a performance budget containing a zero (0 layout shifts while scrubbing).
2. Board 14 converts the biggest constraint into the most distinctive screen: who may publish, with
   which token, at what scope, plus a vocabulary table normalising `person_detected`, `vehicle` and
   `tns:RuleEngine/Motion` onto one timeline with a catch-all for event types nobody has written yet.
3. Failure copy carries the number and the fix in the same row: "Attempt 3 · last frame 41s ago",
   "Offline for 2h 14m. Not recording. No footage since 04:23", and board 15 ruling out the recorder
   before the user asks.

## Priority Issues

### [P0] The evidence workflow has no designed ending
Job 4 exists only as a disabled control. Board 09 produces range, duration, size, container and a
timestamp-burn checkbox, then terminates in `Server update required · media-export.v1` (verified
visually). Board 10's event drawer has a gated button with no verb.
Deterministic: `Export clip` = 1 occurrence (board 28 contract cell). `Download` = 3, of which the
only interactive one is "Download diagnostics" on board 20. `Progress` = 2, both board 28 prose and
a shadcn chip. Board 28 promises create/progress/cancel/expiry/download-URL and retryable failure;
none of those states appears on any board.
Why: PRODUCT.md makes export MVP-mandatory. The small-business operator's entire reason for being
at the machine is this step.
Fix: one board with four states — created (progress, elapsed/remaining, Cancel, "you can leave this
page"); ready (filename carrying camera + ISO timestamp, size, checksum, expiry countdown,
Download); failed (exact server error, range preserved, Retry); partial (range crosses a gap, with
the gap drawn on the range handle). Give board 10's button its verb.
Command: /impeccable shape

### [P0] Every desktop "Diagnose" is a dead end
`Diagnose` appears 7 times across 7 board files (05, 06, 07, 11, 14, 15, 26). Six are entry points;
the destination is drawn only on board 26 at 390px.
Why: Job 5 is one of the five, and the administrator is at a desktop. Five CTAs pointing at nothing
means five ad-hoc diagnostic screens that diverge.
Fix: promote board 26's fact block, "Try in this order" ladder and stream-evidence chart to 1440 and
bind it as the single named destination. Already designed; drawn at the wrong breakpoint.
Command: /impeccable adapt

### [P1] The failure signal fails contrast exactly where failure is stated
`--color-live #C93B40` on `--color-surface #141619` = 3.61:1 vs 4.5 required, across 17 occurrences
on 6 boards, carrying `OFFLINE 2h 14m · SINCE 04:23`, `NO FOOTAGE SINCE 04:23:07`, `NOTHING RECEIVED
IN 3d 11h`, `CRITICAL`. Separately `--color-primary #B7410E` on ground = 3.50:1 carrying the eyebrow
label on 19 of 28 boards (38 instances). `--color-text-faint` on `--color-raised` = 4.37:1 across 45
instances, used at 9/10/11/12px against board 02's own "never below 13px".
Partial regression: darkening live to #C93B40 fixed the previous run's "now"-pill P1 and pushed
`text-live` further below AA. One token, two opposite contrast requirements.
Fix: split the role — keep #C93B40 as the fill token, add `--color-live-text` ≈ #E8656A (≈5.6:1 on
surface) for red text; lift eyebrow labels to `--color-primary-soft #D67B53` (6.31:1 on ground);
raise text-faint to 12px minimum on raised surfaces.
Command: /impeccable colorize

### [P1] The specification breaks four of its own explicit rules
Icons: boards 03/04/06/07/08/10 use Lucide at 1.75; boards 11, 13–19, 23 and all mobile boards use
Feather geometry at 1.6/1.7/1.8, against board 05's "Icons are Lucide only… Never mix icon families".
Colour count: board 05 bans colours outside the sixteen; executed count is 40 distinct raw hex
literals, 160 occurrences — `#0A0B0C` is an opaque seventeenth neutral used 43 times, `#6B7178` an
eighteenth.
Type scale: 8 declared sizes, 18 in use, 900 off-scale occurrences (12px ×362, 10px ×250, 14px ×167,
9px ×54).
No mystery buttons: board 28 forbids the disabled mystery button; board 16 places two byte-identical
"Server update required" pills side by side plus a third in the banner (verified visually), boards 17
and 18 the same, board 27 four. Board 09 shows the correct pattern by suffixing the capability id.
Also: board 02 assigns `live` to "Failure, offline… Never REC", yet boards 04 and 19 render REC with
a `bg-live` dot while boards 06 and 07 correctly use bone.
Fix: one conformance pass — Lucide at 1.75 everywhere; promote #0A0B0C to a declared `--color-video`
token; declare or collapse 12/10/14px; pair every gate pill with its verb.
Command: /impeccable polish

### [P1] Cross-board fixture contradictions produce two different state machines
Board 06 puts the 2h 14m outage on Workshop and the 14% drop on Back Yard; boards 09, 11, 15, 22 and
26 put the outage on Back Yard and the drop on Porch. Board 15 labels Back Yard "Reconnecting" with a
red dot; boards 11 and 22 label the same camera "OFFLINE 2h 14m"; board 06 defines those as distinct
states with different colours and reserves red for Offline. Board 26 says Back Yard has its own login
override; board 23's authoritative list is Workshop, Till, Porch.
Contract conflict: board 13 gates the storage-path command behind "Server update required" while
board 28 states `keeppeek.runtime-config.v1` SHIPS and unlocks storage paths.
Fix: one fixture — named fleet, fixed states, addresses, credentials, a single incident timeline —
and re-render every board from it. Resolve board 13 against board 28 explicitly.
Command: /impeccable harden

## Persona Red Flags

Alex (power user, 60 cameras on a wall display): board 08 offers "Everything · 127 CAMERAS" as a
saved layout while its preset rail tops out at 8-up and no board draws 127 tiles. Board 06 answers
the 127-source ceiling with pagination and a "Next page" button — wrong model for an unattended
screen; board 08's Activity Focus is the right answer and lives in the editor. `keyboard` and
`shortcut` appear zero times despite ⌘K on board 06 and "ARROW KEYS NUDGE ONE COLUMN" on board 08.
Board 11's bulk Remove sits in red beside Restart stream with no confirmation, while board 08 draws
a typed-name dialog to delete a layout.

Jordan (first-timer, one Reolink doorbell): board 21 asks three questions before a camera exists and
puts the least-understood one — "Remote sign-in (optional)" — first, ahead of the two that are
pre-answered and correct. The claimed write-permission check has no failure state drawn. Board 12
step 3 advises changing the camera's audio codec "if exports need to travel" — correct advice about
a capability boards 09, 10, 16 and 28 all say is unavailable. After "Save and start recording", the
moment of first success is a sentence, not a screen.

Sam (small-business operator, morning after a break-in): cannot complete their job — board 09 hands
them an estimate and a dead rectangle. Search grammar changes between devices: board 22's mobile
placeholder is natural language while board 10's desktop search is structured tokens, and
natural-language search is features.md rank 31, a Later item PRODUCT.md does not commit to. Board 16
ticks "Export a clip or a still" for the User role, and `Audit` appears zero times in 28 boards; the
Tokens table shows LAST USED but never what was done. Board 13's "Stop recording and raise a critical
issue — choose this only where footage is evidence" is right for them and is not the preselected
default, and nothing in first run raises the question.

## Minor Observations

- Asserted in prose, never drawn: board 04's collapsed card stack; board 08's four-second rust corner
  marker; board 10's clickable REV 3 diff; board 28's own COMMAND FAILED state and mid-draft
  capability-loss freeze; board 20's Erase confirmation; board 21's storage-writability failure;
  board 10's "Save as view" with no saved-views surface anywhere.
- `Loading` = 0, `Skeleton` = 1, `aria-` = 0 across 28 boards. No live-region guidance for the
  continuously-updating data on boards 06, 11 and 15.
- No zero-results state for Events anywhere.
- Board 10's Bookmark is drawn as a live button while board 03's IA marks Bookmarks NEEDS BACKEND
  WORK; by board 28's own rule it must read "Server update required".
- Light theme is offered on board 20 and drawn on zero of 28 boards.
- Board 04 calls its performance budget "measured"; PRODUCT.md states no benchmarks or tested
  capacity profile exist. These are targets.
- Board 18 never names Pushover despite it being the only first-party API citation in the evidence
  register and the source of the priority/ack/retry semantics the board implies.
- Board 16's role matrix is 8 rows expressing 2 shapes.
- Dead markup in the exports: `border-l-[#00000000]` on nav items across boards 13–19 and 23;
  `<svg style={{display:'none'}}>` left inside the gate pills on boards 16 and 18; inline
  `style={{stroke}}` contradicting the `stroke=` attribute on the same element.
- Board 06's and board 11's status bars are identical in content but differ in typography
  (`text-[12px]` vs `text-xs`). Fixed furniture should be pixel-identical.
- Token drift against the codebase (executed): board 02 states all 58 tokens map to CSS variables in
  ui/src/app.css; actual count of Paper-token CSS variables in that file is 0. The six brand tokens
  live in ui/src/styles/style.css, not app.css as PRODUCT.md also states. Three values match under
  different names (#B7410E = primary = --keeppeek-rust-bright); 13 of 16 design tokens have no
  counterpart in code at all.

## Questions to Consider

1. If export is the terminal step of the only workflow the product defines, why is it the one MVP
   capability with no happy path drawn? What if "make me a file" were a first-class destination?
2. Peek and Keep are the same surface separated by a few seconds. What breaks if they become one view
   whose mode is decided by where the playhead sits?
3. Seven boards prove the same settings frame. What would you learn by drawing the three hardest
   settings screens at 3x fidelity — including failure, conflict and mid-edit capability loss — and
   deleting the other four?
4. "Silence is the failure mode" — yet every failure surface here requires someone to be looking at a
   screen. What is the design of the moment nobody is looking?
5. Board 16 grants User the export right; audit appears nowhere. Is two-fixed-roles the simplest
   correct model, or just the simplest?
