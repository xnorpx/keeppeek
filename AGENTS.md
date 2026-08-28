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

## RepoWise Repository Intelligence

- RepoWise is required developer tooling. The version pinned by `REPOWISE_VERSION` in `.github/workflows/repowise.yml` is authoritative, and the `repowise` executable must be available on `PATH` because `.vscode/mcp.json` launches it directly.
- Load the `setup-repo` skill when RepoWise or another workstation prerequisite is missing, outdated, or being installed. That skill owns cross-platform installation and setup validation.
- Prefer the available RepoWise MCP tools for architecture, contextual documentation, symbol lookup, design rationale, semantic search, change risk, code health, and dead-code analysis before broad manual exploration.
- Use `repowise status` to check the local index. If the repository has no index, ask the user to run `repowise init --yes --no-prose --no-editor-setup` from the repository root. Refresh a stale index with `repowise update --index-only` after significant changes.
- Read `repowise.md` before significant architecture or tooling changes. It is the committed source of truth for durable decisions and score history; `.repowise/` is a derived local cache.
- Keep `repowise.md` current after an explicitly approved decision or a fresh comparable health analysis. Preserve superseded decisions, append score history, and record the indexed commit and RepoWise version so tool changes are not mistaken for code changes.
- Treat the working tree and executable tests as the source of truth. Verify RepoWise findings against live files when the index may lag local edits.

## Package Registries

- Use Bun as the only package manager and script runner under `ui/`; do not create npm, pnpm, or Yarn lockfiles.
- Do not commit JavaScript lockfiles. Install with `--no-save` and keep direct dependency versions exact where reproducibility matters.
- Resolve JavaScript packages only from the public npm registry configured in `.npmrc` and `ui/bunfig.toml`.
- Resolve Rust packages only from public crates.io using the repository `.cargo/config.toml`.
- Do not replace either public registry with a private mirror or proxy.

## Python Environment

- Never create, activate, or rely on a Python virtual environment. Do not run `python -m venv`, `uv venv`, or `virtualenv`, and do not add `.venv` handling to any script or document.
- Install `examples/object_detection_service/requirements.txt` directly into the Python 3.12 interpreter that `fix` and `check` resolve, falling back to `--break-system-packages` on externally managed interpreters.
- Do not pin versions in `examples/object_detection_service/requirements.txt`. The example tracks the newest releases on purpose so upstream breakage surfaces immediately instead of accumulating.
- `fix` upgrades every requirement on each run, matching what CI resolves. When a new Black, `ruff`, or `mypy` release fails the build, fix the code it broke; do not add a pin to silence it.
- Point `KEEPPEEK_PYTHON` at an absolute interpreter path when the default resolution is wrong; never point it at a virtual environment.

## Protected API Contract

- Treat `api/` as read-only. Do not modify its schemas, protocol documentation, or generated contract sources.
- Preserve API and protobuf contracts through implementation changes elsewhere in the repository.

## Code Style

- Do not write comments that restate what the code already says.
- Do not use decorative separators or section headers in comments.
- Add comments only when they explain a non-obvious reason or constraint.
- Use doc comments on public API items when they add context beyond the signature.
