# KeepPeek Visual Harness

This isolated Bun package pins Storybook 8 and Vite 6 because Loki 0.35 does not yet support the
Storybook release line required by KeepPeek's production Vite 8 app. Stories import production
Svelte components from `ui/src`; the harness does not fork application components.

From this directory:

```sh
bun install --no-save --registry=https://registry.npmjs.org/
bun run storybook:build
bun run loki:test:ci
```

Loki references belong in `.loki/reference` and are committed only after comparison with their
linked Paper frame. Current captures, differences, dependencies, and static Storybook output are
ignored.

The CI runner serves the built Storybook from an already-bound random HTTP port, then runs desktop
and mobile Loki configurations sequentially. This avoids Loki's `file:` bridge selecting the same
host port as its Docker debugger. It uses `--requireReference=false` and removes the generic CI
environment variables that Loki 0.35 incorrectly uses to override an explicit false value. Existing
approved references still fail on pixel drift. Stories without an approved reference are written
under `.loki/reference` in the ephemeral CI checkout and uploaded as review-only artifacts; they are
not accepted or committed automatically. Loki filenames are stable and derived from the Paper
scenario ID:

```text
.loki/reference/chrome.desktop/peek.desktop.live-wall.png
.loki/reference/chrome.mobile/health.mobile.overview.png
```

To approve a baseline, compare the Linux current image with its hash-locked Paper reference and
recorded overlay decision, then place that exact Linux image at the matching path under
`.loki/reference`. `loki update` is never an approval step by itself.

Boards 29 and 34 can be rendered at their native Paper frames with the production Vite/Svelte
stack even when the isolated Storybook dependencies are unavailable:

```sh
cd ..
bun run paper:board29:capture
bun run paper:board34:capture
```

Each command writes a candidate, 50% Paper overlay, threshold-highlighted difference, and JSON
metrics under `ui/test-results/`. These are review evidence only and never become a Loki reference
without the Paper overlay review.

The first local install is currently blocked by registry connections closing before package
manifests resolve. The GitHub visual workflow performs the same install against the public npm
registry and will provide the decisive static-build and Linux candidate results.
