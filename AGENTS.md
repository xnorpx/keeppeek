# Repository Agent Instructions

## Pragmatic Rust Guidelines

For every Rust implementation, refactor, review, test, or API design task in this repository, read and follow [the Pragmatic Rust Guidelines](.github/instructions/rust_pragmatic_guidelines.md).

Treat every applicable `M-*` guideline as a repository requirement. If a guideline conflicts with existing repository behavior or an explicit user request, identify the conflict before changing behavior.

## Pragmatic Svelte 5 Guidelines

For every Svelte, SvelteKit, or frontend TypeScript implementation, refactor, review, test, or API design task under `ui/`, read and follow [the Pragmatic Svelte 5 Guidelines](.github/instructions/svelte5_pragmatic_guidelines.md).

Treat every applicable Svelte `M-*` guideline as a repository requirement. Prefer modern Svelte 5 runes, snippets, callback props, and SvelteKit patterns over legacy Svelte APIs. If a guideline conflicts with generated third-party component code, existing behavior, or an explicit user request, identify the conflict before changing behavior.

After any change under `ui/`, run the canonical UI validation script from the repository root before considering the work complete:

- macOS or Linux: `./check.sh`
- Windows: `.\check.bat`

CI uses these same entry points. Individual UI commands may be used while diagnosing failures, but the full platform script must pass before completion.

## Package Registries

- Use Bun as the only package manager and script runner under `ui/`; do not create npm, pnpm, or Yarn lockfiles.
- Do not commit JavaScript lockfiles. Install with `--no-save` and keep direct dependency versions exact where reproducibility matters.
- Resolve JavaScript packages only from the public npm registry configured in `.npmrc` and `ui/bunfig.toml`.
- Resolve Rust packages only from public crates.io using the repository `.cargo/config.toml`.
- Do not replace either public registry with a private mirror or proxy.

## Code Style

- Do not write comments that restate what the code already says.
- Do not use decorative separators or section headers in comments.
- Add comments only when they explain a non-obvious reason or constraint.
- Use doc comments on public API items when they add context beyond the signature.
