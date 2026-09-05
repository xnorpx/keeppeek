# KeepPeek Paper Storyboard v34

This directory is the versioned source snapshot of the Paper file
**KeepPeek — NVR Design System & Spec**. It is review evidence and an implementation input; it is
not application source code.

## Contents

- `storyboard.json` records the Paper file, page, token hash, board geometry, source paths, byte
  counts, and SHA-256 hashes.
- `tokens.json` preserves all token names, values, types, and descriptions returned by Paper.
- `tokens.css` is the same token set as directly consumable CSS custom properties.
- `scenarios.json` maps every board to route, state, fixture, theme, viewport, exact capabilities,
  planned Storybook story, and Playwright owner.
- `boards/*.jsx.txt` contains one complete inline-style JSX snapshot per Paper artboard. The `.txt`
  suffix deliberately keeps generated Paper markup outside the Svelte build and lint graph.
- `references/` contains hash-locked lossless exports of implementation frames used for Paper
  overlay review and Loki approval.

Registered references also identify their deterministic Storybook source. Accepted Paper overlays
record their reviewed comparison thresholds and pixel counts in `storyboard.json`; these review
metrics do not replace the later canonical Linux Loki baseline.

A route capture may be registered as a blocked candidate before Storybook extraction. It must name
its capture owner and blockers, and it cannot approve or seed a Loki reference.

Do not edit generated board or token files by hand. Change the design in Paper, re-export the
complete bundle, review the manifest and visual references, and commit those changes together.

Run the integrity check from `ui/`:

```sh
bun run paper:check
```

## Configuration ZIP Reference

Board 20 includes the configuration ZIP states: selected but unconfirmed, staged for restart,
and rejected with the selected file retained at a 390px width.

![Configuration ZIP states](references/20-configuration-zip-states@2x.png)

`configuration-zip.json` records the exact Paper frames, export checksum, HTTP routes, runtime
component, and Playwright tests. The checker validates this supplementary design-and-test contract
alongside the board snapshot. It is not an approved Loki pixel baseline.

The board was refreshed from Paper token revision `b35ec365`. Its 25 used tokens match the pinned
NVR token values, so the shared token snapshot and unrelated boards are unchanged.

The checker verifies the board count, unique IDs and paths, token hash, token CSS, byte counts,
every board and reference SHA-256 hash, complete scenario coverage, and exact capability
identifiers.

## Importing MCP Exports

Paper MCP JSX responses and image exports are imported without hand-editing generated output:

```sh
bun scripts/import-paper-node-export.ts jsx response.json boards/35-example.jsx.txt
bun scripts/import-paper-node-export.ts reference frame.png references/35-example.png
```

Add the emitted byte count and SHA-256 to `storyboard.json`, then run `bun run paper:check`.
