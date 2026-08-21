---
target: the paper boards
total_score: 21
max_score: 36
na_heuristics: 3
p0_count: 0
p1_count: 3
timestamp: 2026-08-18T18-53-09Z
slug: app-paper-design-file-01m0b0vbh78tmtx40gcyyq37sg
---
# Critique — KeepPeek design specification boards (Paper)

Method: dual-agent (A: Explore/design-review · B: Explore/detector-evidence).
Assessment B could not execute commands and simulated its evidence; its mechanical work was
re-run for real in the parent. Detector and contrast figures below are executed output.

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | 2 | Availability band sits at 1.88:1 on ground — the one mark that proves footage exists is near-invisible |
| 2 | Match System / Real World | 3 | Domain language precise; "activity" (motion density) never defined for a non-video reader |
| 3 | User Control and Freedom | n/a | Static specification; no undo or exit applies |
| 4 | Consistency and Standards | 3 | Eyebrow, heading and lane treatments identical across all five boards, but the boards break their own "never below 13px" token rule at 9, 10 and 11px |
| 5 | Error Prevention | 2 | Wizard specifies inline validation; no error-state pattern language anywhere |
| 6 | Recognition Rather Than Recall | 2 | Handoff board requires backflips to boards 1, 2 and 4 to be actionable |
| 7 | Flexibility and Efficiency | 3 | Search, breadcrumbs, bulk actions, drag-resize all specified; no keyboard model |
| 8 | Aesthetic and Minimalist Design | 3 | Strong token discipline; "visualised" and "net new" prescriptions are vague |
| 9 | Error Recovery | 1 | Degraded/reconnecting named but never drawn — contradicts Principle 3 |
| 10 | Help and Documentation | 2 | Strong negative spec ("do not invent"), thin positive pattern guidance |
| **Total** | | **21/36** | Competent, with real accessibility debt |

Nine heuristics scored, one n/a. Applicable maximum 36.

## Design Specificity Verdict

**LLM assessment:** Authored for this product, not category-interchangeable. The boundary
statement, the vertical-timeline bet, the signal palette mapped to NVR states, and the explicit
rejection list could not be lifted onto another product. Weakness is not genericness but
self-containment: no board stands alone.

**Deterministic scan:** `node .github/skills/impeccable/scripts/detect.mjs --json /tmp/kp-critique`
returned `[]`, exit 0. Single-file SVG scan also exit 0, no output. This is a category mismatch,
not a clean bill of health: exported SVG carries no scannable source. Recorded as
"deterministic scan unavailable for this target class".

**Executed contrast audit:** 33 pairings, 11 failures. Of those, 1 is a false positive
(decorative hairline), leaving 10 real.

## Priority Issues

**[P1] The availability band is nearly invisible — 1.88:1 against ground**
Why: `--color-availability #39414A` on `--color-ground #0C0D0F` is the mark that says "footage
exists here". Absence of it means a gap. At 1.88:1 against a 3:1 requirement for
information-bearing graphics, the reader cannot reliably tell band from background — so gaps and
footage look alike. This directly contradicts Product Principle 3, "silence is the failure mode",
and board 04's own claim that gaps are "rendered explicitly, never as empty space". It is baked
into the token set, so it ships. Becomes P0 the moment it is product rather than spec.
Fix: lighten availability to roughly #4A535D (~3:1) or raise the gap treatment to carry the
signal instead of relying on the band.
Command: /impeccable colorize

**[P1] The "now" pill fails at 3.91:1**
Why: white on `--color-live #E5484D` at 10px semibold. The most time-critical readout on the
timeline is the one that fails 4.5:1. Fix: darken live for this use, or set the pill text to
#FFFFFF at >=11px bold and darken the fill to ~#C93B40.
Command: /impeccable colorize

**[P1] Board 03 contradicts its own headline**
Why: the board says "Seven destinations. Nothing buried", then lists Settings as 8-9 flat chips
and Cameras as 6 — both past the 4-item working-memory limit, and exactly the flatness the board
criticises in the reference NVRs. A reader either builds the flat menu, invents grouping, or
stops to ask. Fix: show the two- or three-level grouping instead of a flat chip run.
Command: /impeccable layout

**[P2] The boards break their own type-contrast rule**
Why: board 02 states `--color-text-faint` is "never below 13px". The boards use it at 11px
(3.94:1), 10px (3.94:1) and 9px (3.68:1); rust eyebrow labels run 11px and 13px at 3.50:1. All
fail 4.5:1. A spec that violates its own stated floor loses authority and propagates the defect.
Fix: raise faint usage to 12px minimum and lighten to ~#7D848C, or demote those labels to muted.
Command: /impeccable typeset

**[P2] "Do not invent" is binding policy set in the smallest, faintest type on the last board**
Why: fifteen guardrails that prevent scope creep are the least legible content in the set, at the
end of the longest board. A second designer joining mid-project will miss them. Fix: promote to
its own board or distribute each constraint next to the artboard it governs.
Command: /impeccable layout

## Persona Red Flags

**Alex (power user):** Board 03 Settings row, 8 flat chips — skims, misses that Event sources is
net new. Board 05 "do not invent" invisible on a scan pass; designs something that violates it.
Board 05 item 12 references event types defined only on board 01.

**Sam (accessibility):** Ten real contrast failures, including every rust eyebrow label and the
"now" pill. The 9px badge numeral has no mitigation. Signal swatches carry meaning by colour
alone with no second channel.

**Jordan (self-hoster/small-business, per PRODUCT.md):** Board 03's 8-item Settings row reads as
enterprise complexity and prompts "is this over-engineered for five cameras?". "Event sources"
never marked optional, implying a detection service is required to use the product. No factory
defaults stated for storage and retention.

## Minor Observations

- Type notation "56 / 700 / -0.03em" never has its units declared.
- Board 03 status legend does not say whether PARTIAL means backend or UI is partial.
- "Bone hairline" (board 04 playhead) is undefined jargon with no measured thickness.
- Reference shorthand `frigate_nvr/01, 02` never states the files live under `reference/`.
- No mobile guidance beyond "390x844 variant" — the vertical strip's behaviour there is unproven.
- Five principles on board 01 exceed the 4-item chunking limit.

## Questions to Consider

- If the availability band must be visible at 3:1, does the gap become the marked state instead?
- Does the vertical strip actually survive 390px, and who owns that variance?
- Could the signal swatches be shown inside 200x150 micro-mockups so meanings need no captions?
- Is board 03 the current cycle's scope or the whole product vision?
