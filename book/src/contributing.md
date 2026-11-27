# Contributing

This chapter defines KeepPeek's product-scope bar, repository engineering policies (`AGENTS.md`), local formatting and validation commands (`fix.sh`/`fix.bat`, `check.sh`/`check.bat`), AI-assisted contribution rules, and pull-request workflow. Read [Users and design choices](./users-and-design-choices.md) before proposing scope changes; file defects through [Reporting bugs](./reporting-bugs.md) and ask questions or propose features in [GitHub Discussions](https://github.com/xnorpx/keeppeek/discussions).

## Scope boundaries and high-fit work

KeepPeek guards its core feature set strictly—a completed implementation does not justify expanding scope. Open a [GitHub Discussion](https://github.com/xnorpx/keeppeek/discussions) **before** implementing any change to product scope, the public API, storage format, platform contract, or long-term maintenance surface.

Feature proposals are evaluated against five questions:

1. Does this belong in a focused recorder and WebRTC media gateway?
2. Can an independent external service provide it over the MIT-licensed public API instead?
3. Which documented users need it, and does it degrade another user's core workflow?
4. What permanent maintenance, security, storage, resource, compatibility, and licensing cost does it add?
5. Can it be tested deterministically without depending on one private device, account, or cloud service?

### Contributions that fit well

- **Bug fixes, diagnostics, tests, and docs**: High-signal fixes with deterministic regression coverage that preserve existing boundaries.
- **Camera protocols & RTSP interoperability**: Narrow parser, transport, or adapter fixes backed by model/firmware/transport details, a sanitized minimal repro, and a deterministic fixture or fake-camera test. Never weaken validation globally for one camera quirk, never submit secrets/URLs/private captures, and validate real-device main/sub frames, keyframes, MP4 finalization, and independent decode when recording changes.
- **Protected `api/` contract**: `api/` (OpenAPI, protobuf schemas, protocol docs, generated contract sources) is **read-only by default**. Contract changes require explicit approval for the task, must stay within approved scope, preserve unrelated contracts, and keep docs, generated bindings, and contract tests synchronized. New detectors, transcoders, and bridges belong outside core first.

## Repository policies and tooling

Follow [`AGENTS.md`](https://github.com/xnorpx/keeppeek/blob/main/AGENTS.md) and the language guidelines below for every contribution:

| Area                  | Requirement                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Rust**              | Stable Rust, `cargo-nextest`, `cargo-machete`, public crates.io only (`.cargo/config.toml`). Follow [Pragmatic Rust Guidelines](https://github.com/xnorpx/keeppeek/blob/main/.github/instructions/rust_pragmatic_guidelines.md).                                                                                                                                                                                                                        |
| **Frontend (`ui/`)**  | **Bun only** (pinned `1.4.5` in `ui/.bun-version`), public npm registry only (`.npmrc`, `ui/bunfig.toml`), `bun install --no-save`, **no npm/pnpm/Yarn lockfiles** and no committed JS lockfiles. Follow [Pragmatic Svelte 5 Guidelines](https://github.com/xnorpx/keeppeek/blob/main/.github/instructions/svelte5_pragmatic_guidelines.md).                                                                                                            |
| **Python**            | **Python `3.12`** (`examples/object_detection_service/.python-version`). **Never create or use a virtual environment (`.venv`)**. Install `examples/object_detection_service/requirements.txt` directly into the resolved Python 3.12 interpreter (falling back to `--break-system-packages`). Requirements stay intentionally unpinned; fix upstream lint/type breaks instead of pinning. Override path via absolute `KEEPPEEK_PYTHON` (never a venv). |
| **Configuration**     | Store settings in `config.toml` and private strings in `secrets.toml` (`{secret:KEY}`, `{secret:KEY\|url}`). Never add separate settings files; keep [Configuration reference](./configuration-reference.md) synchronized.                                                                                                                                                                                                                              |
| **Engineering style** | Apply TigerStyle safety/boundedness (excluding static allocation) and Ponytail minimal-diff discipline; write comments only for non-obvious constraints.                                                                                                                                                                                                                                                                                                |

## Formatting, validation, and CI gates

Run the narrowest relevant test during development, then pass the canonical root validation before requesting review:

| Goal                                  | macOS / Linux                                                                                                                                                                                | Windows (PowerShell / cmd)                                        |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| **Auto-format & refresh Python deps** | `./fix.sh`                                                                                                                                                                                   | `.\fix.bat`                                                       |
| **Canonical full check**              | `./check.sh`                                                                                                                                                                                 | `.\check.bat`                                                     |
| **Full check + slow Rust tests**      | `KEEPPEEK_RUN_SLOW_TESTS=1 ./check.sh`                                                                                                                                                       | `$env:KEEPPEEK_RUN_SLOW_TESTS = '1'; .\check.bat`                 |
| **Docs, mdBook, and Wiki checks**     | `bun run --cwd ui format:markdown:check && mdbook build book && bun run --cwd ui wiki:typecheck && bun run --cwd ui wiki:test && bun run --cwd ui wiki:check && bun run --cwd ui wiki:build` | Same commands (requires `mdbook` and `mdbook-mermaid` on `PATH`). |

`check.sh` / `check.bat` builds the Rust workspace, runs `cargo nextest` (`--features macos-test-aws-crypto` on macOS), runs `cargo clippy --all --all-targets -- -D warnings`, `cargo machete`, `cargo fmt --check`, `taplo fmt --check`, Python `black --check`, and in `ui/` runs `bun run quality:check`, `bun run test:e2e`, and `bun run perf:playback:run`. `fix.sh` / `fix.bat` upgrades Python requirements, formats Rust, TOML, Python, Markdown, and UI files, and runs `lint:fix`. Document any test you could not run locally in your PR.

### CI workflow coverage

| Workflow                  | Trigger                                  | What runs                                                                                                                                                                                                                                                                                                                                                                           |
| ------------------------- | ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`CI` (PR / merge)**     | Pull requests & merge groups             | Path-filtered jobs: Format, Book (mdBook + Mermaid + Wiki checks), Clippy, `cargo-machete`, doctests, Python object-detection example & integration, macOS/Windows builds + service/desktop smoke checks, full Linux Rust suites via `nextest`, OS-targeted macOS & Windows Rust core/durable-state subsets, and UI static/unit/coverage/Linux Playwright/Home Assistant Container. |
| **`CI` (push to `main`)** | Push to `main`                           | Full Rust core & durable-state suites across Linux, macOS, and Windows, plus signed x64 and ARM64 Windows release artifacts.                                                                                                                                                                                                                                                        |
| **`CodeQL`**              | PRs to `main`, push, merge group, weekly | Actions, JavaScript/TypeScript, Python, and Rust static analysis.                                                                                                                                                                                                                                                                                                                   |
| **`Main`**                | Push to `main`, hourly cron              | Opt-in slow Rust tests, Rust coverage (`cargo-llvm-cov`), Linux ARM64 & Windows x64/ARM64 builds/tests + NSIS installer lifecycle, and macOS/Windows Playwright E2E shards.                                                                                                                                                                                                         |

## AI-assisted contributions

AI tools are permitted under the exact same review bar as handwritten code—responsibility never transfers to the model. The submitter is the PR's first reviewer and must:

- inspect, understand, and be able to explain every changed line (including tests and configuration);
- verify API, security, resource, failure, and compatibility assumptions directly against the repository;
- remove invented behavior, unnecessary abstractions, unrelated cleanup, and filler commentary;
- run validation locally and investigate failures rather than asking reviewers to debug raw model output.

"The AI wrote it" is not an acceptable design rationale; if the submitter cannot defend control flow, ownership, protocol behavior, or tests, the PR is not ready.

## Pull request workflow and review cadence

Use [`.github/pull_request_template.md`](https://github.com/xnorpx/keeppeek/blob/main/.github/pull_request_template.md) for every PR and complete all sections before requesting review:

1. **Summary**: Describe the user-visible outcome and link the feature Discussion or reproducible bug issue.
2. **Acceptance criteria verification**: Include one row per criterion from the linked context with observable outcome, verification method, and observed result.
3. **Performance evidence**: Provide reproducible before/after numbers (baseline, environment, workload, command, runs, median/p95), or `Performance: N/A` explaining which paths cannot change.
4. **Validation & Review evidence**: Confirm canonical checks, slow tests, focused tests, and sanitized artifacts/CI links (zero secrets or private media).

Keep each PR scoped to one coherent behavioral change with regression tests and synchronized docs; omit unrelated formatting or refactors. Use Draft PRs for early feedback once scope is agreed.

**Review cadence**: Pull requests are reviewed on **Sundays**. If a Sunday window passes without review, leave one short, polite ping on the existing PR—do not open duplicate PRs or Discussions.

## Book, Wiki, and Paper design evidence

- **Single Markdown source**: Edit documentation in `book/src/` (`SUMMARY.md` controls navigation) and selected `docs/` files via PR—never edit the [GitHub Wiki](https://github.com/xnorpx/keeppeek/wiki) directly. Both the Wiki and the [mdBook site](https://xnorpx.github.io/keeppeek/) are generated from repository Markdown (see [`docs/wiki-publishing.md`](https://github.com/xnorpx/keeppeek/blob/main/docs/wiki-publishing.md)). Update [Feature guide and navigation](./feature-guide.md) when adding or moving screens, and verify live handlers before documenting a feature as available.
- **Paper design references**: Versioned UI specs live under [`ui/design/paper/`](https://github.com/xnorpx/keeppeek/tree/main/ui/design/paper). `keeppeek-nvr-alpha` is the active NVR reference; `keeppeek-nvr-v34` remains the historical input for the existing visual harness and Linux Loki baselines. A Paper export records design intent, not an approved baseline or proof of working behavior—keep unavailable, loading, denied, empty, and error states explicit.

## Licensing

Contributions are accepted under the license of the area modified; see [Open source and licensing](./open-source-and-licensing.md#licensing-model). Submit only work you have the right to contribute under that license, including fixtures, generated files, and AI-assisted output.
