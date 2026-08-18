# UI Agent Instructions

- Read and follow [the Svelte 5 instructions](../.github/instructions/svelte5.instructions.md) before changing files under `src/` or `e2e/`.
- Use Svelte 5 runes, typed callback props, native semantic controls, and Lucide icons already available in the project.
- Keep API access in `src/lib/api.ts` and shared wire types in `src/lib/types.ts`.
- Use Bun for every package, script, and test command under this directory. Do not create npm, pnpm, or Yarn lockfiles.
- Local packages sourced inside this repository may use Bun workspace or file dependencies; external packages must still resolve through the public npm registry.
- For user-facing UI work, add a focused regression test when there is a meaningful test surface. Run `bun run check` while iterating and finish from the repository root with `./check.sh`.
- Treat live-view layouts as client preferences unless a server-side preference API exists. Do not put user-specific layout state in server module scope.
- Preserve stable video dimensions and avoid recreating live subscriptions for changes unrelated to the active camera set or requested quality.
