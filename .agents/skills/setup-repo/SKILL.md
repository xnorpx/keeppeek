---
name: setup-repo
description: "Install, repair, and verify every KeepPeek developer prerequisite on macOS, Linux, or Windows. Use when: setting up a fresh clone or workstation; onboarding; installing Rust, Cargo, Bun, Node.js, Python 3.12, FFmpeg, Playwright Chromium, RepoWise, cargo-nextest, or cargo-machete; restoring PATH; installing repository dependencies; or fixing check.sh/check.bat failures caused by missing tools."
argument-hint: "[full|repair|verify] [macos|linux|windows]"
---

# Set Up the KeepPeek Repository

Bring a KeepPeek checkout to a state where its canonical validation script can run on macOS,
Linux, or Windows. Run the workflow rather than merely printing setup instructions, except for an
operation that needs a password, elevation, a GUI installer, or a shell restart.

## Modes

- `full` installs or upgrades every prerequisite and repository dependency, then validates the
  checkout. This is the default for a fresh clone.
- `repair` starts from a concrete missing command or failed setup check, changes only the affected
  prerequisite chain, and then runs the focused check plus canonical validation.
- `verify` performs all version, PATH, dependency, RepoWise, and repository checks without changing
  the machine.

## Sources of Truth

Read these files at the start of every invocation. Values shown here explain the current contract
but the files remain authoritative:

| Requirement         | Authoritative source                                 | Current contract                                         |
| ------------------- | ---------------------------------------------------- | -------------------------------------------------------- |
| Rust                | `Cargo.toml`, `.github/workflows/ci.yml`             | Stable toolchain, Rust 2024 edition, `rustfmt`, `clippy` |
| Cargo tools         | `.github/workflows/ci.yml`                           | `cargo-machete` 0.9.2, `cargo-nextest` 0.9.143           |
| Bun                 | `ui/.bun-version`, `ui/package.json`                 | Bun 1.4.0                                                |
| JavaScript packages | `ui/package.json`, `.npmrc`, `ui/bunfig.toml`        | Bun only, public npm registry, no lockfile               |
| Python              | `examples/object_detection_service/.python-version`  | Python 3.12, no virtual environments                     |
| Python packages     | `examples/object_detection_service/requirements.txt` | Unpinned, always newest releases                         |
| RepoWise            | `.github/workflows/repowise.yml`                     | `REPOWISE_VERSION`, currently 0.45.0                     |
| Final validation    | `check.sh`, `check.bat`                              | Platform-specific canonical check                        |

Node.js is compatibility tooling for editors and scripts outside the canonical Bun workflow. Install
an actively supported Node.js release, preferring active LTS, but never use npm, pnpm, or Yarn to
install this repository's UI dependencies.

## Safety and Package Policy

1. Start in the repository root and inspect `git status --short`. Preserve every existing user
   change and never reset or clean the checkout.
2. Detect the host OS, architecture, shell, and available package manager. Do not run commands for
   a platform that was inferred only from the user's request.
3. Install only missing tools or versions that do not satisfy the authoritative pin. Report tools
   that were already valid.
4. Use official installers or the host's established package manager. Use only crates.io, PyPI, and
   the public npm registry configured by this repository.
5. Never request or relay a password, administrator credential, token, or package-manager secret.
   If a command needs `sudo`, UAC, or interactive GUI approval, give the user that exact command to
   run directly, wait for completion, and then continue with verification.
6. Do not overwrite shell profiles. When PATH setup is needed, show the minimal addition, preserve
   existing content, and tell the user when VS Code or the terminal must be restarted.
7. Do not create npm, pnpm, Yarn, or Bun lockfiles. Always install UI packages with `--no-save` and
   the repository's public registry.
8. Do not install optional deployment, camera, or model assets unless the user asks for that
   workflow. FFmpeg and the Python requirements are development prerequisites; private camera
   credentials are not.

## Phase 1: Preflight

1. Resolve the repository root with `git rev-parse --show-toplevel` and run every later repository
   command from that checkout.
2. Read the sources of truth above and record the expected versions. Do not copy stale pins from
   this skill when repository files differ.
3. On macOS or Linux, inspect `uname -s`, `uname -m`, the current shell, and `command -v` for each
   required executable.
4. On Windows, inspect `$PSVersionTable`,
   `[System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture`, and `Get-Command` for each
   required executable.
5. On Linux, read `/etc/os-release` and choose only the matching distribution branch below.
6. Record versions and executable paths for `git`, `rustup`, `rustc`, `cargo`, `rustfmt`, Bun,
   Node.js, Python, `uv`, FFmpeg, and RepoWise. A command found at an unexpected path is not a
   failure, but note it before replacing or upgrading anything.

In `verify` mode, skip directly to the verification phases after this inventory.

## Phase 2: Install System Prerequisites

Install only missing packages. When elevation is required, the user runs the command directly and
the agent resumes after checking the resulting executables.

### macOS

1. Require macOS 13 or newer for Bun.
2. Check `xcode-select -p`. If Command Line Tools are absent, run `xcode-select --install`, ask the
   user to complete the Apple dialog, and verify it before continuing.
3. Use an existing Homebrew installation. If Homebrew is absent, direct the user to the official
   Homebrew installer rather than inventing a prefix or silently changing their shell profile.
4. Install missing packages:

```sh
brew install git node ffmpeg uv
```

### Debian or Ubuntu

Ask the user to run this privileged step when any listed package is missing:

```sh
sudo apt-get update
sudo apt-get install --yes build-essential pkg-config libssl-dev git curl unzip ffmpeg nodejs
```

Install `uv` from the distribution package when available. Otherwise use Astral's official `uv`
installer in the unprivileged user account and verify its install directory is on PATH.

### Fedora or RHEL Family

Ask the user to run the applicable privileged step:

```sh
sudo dnf install gcc gcc-c++ make pkgconf-pkg-config openssl-devel git curl unzip nodejs ffmpeg-free
```

Use `ffmpeg` instead of `ffmpeg-free` when that is the package supplied by the configured official
repositories. Install `uv` from the distribution package when available or from Astral's official
unprivileged installer.

### Arch Linux

Ask the user to run this privileged step:

```sh
sudo pacman -S --needed base-devel pkgconf openssl git curl unzip ffmpeg nodejs npm
```

Install `uv` from the official Arch package.

### Windows

Use PowerShell and an existing WinGet installation. Install only missing packages:

```powershell
winget install --exact --id Git.Git
winget install --exact --id OpenJS.NodeJS.LTS
winget install --exact --id Python.Python.3.12
winget install --exact --id Gyan.FFmpeg
winget install --exact --id astral-sh.uv
winget install --exact --id Rustlang.Rustup
```

Rust's MSVC target also needs the Visual C++ build tools. If no suitable toolchain is installed,
ask the user to run:

```powershell
winget install --exact --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Wait for WinGet and UAC to finish, open a fresh PowerShell session when PATH changed, and verify
each command before continuing.

For another Linux distribution, use its official equivalents for a C/C++ build toolchain,
`pkg-config`, OpenSSL development headers, Git, curl, unzip, FFmpeg, Node.js, and `uv`. Do not guess
package names when the package manager cannot confirm them.

After a platform package install, check the Node.js release line. If the distribution supplied an
end-of-life version, replace it with active LTS through Node.js's official distribution or the
user's established version manager.

## Phase 3: Install Language Toolchains

### Rust and Cargo Tools

When `rustup` is missing on macOS or Linux, use the official installer with the minimal profile and
stable toolchain:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
   sh -s -- -y --profile minimal --default-toolchain stable
```

On Windows, use the WinGet package above. Then run:

```sh
rustup toolchain install stable --profile minimal
rustup default stable
rustup component add rustfmt clippy
cargo install --locked cargo-machete --version 0.9.2
cargo install --locked cargo-nextest --version 0.9.143
```

Replace the two Cargo tool versions with newer authoritative CI pins when those files change. If an
already-installed Cargo binary has the wrong version, `cargo install --force` the pinned version.
Make `$HOME/.cargo/bin` available to the current Unix shell or `%USERPROFILE%\.cargo\bin` to the
current PowerShell session, then verify command resolution.

### Bun

Read the exact version from `ui/.bun-version`. On macOS or Linux, install that version with Bun's
official versioned installer:

```sh
curl -fsSL https://bun.com/install | bash -s "bun-v1.4.0"
```

On Windows, use Bun's official versioned installer:

```powershell
iex "& {$(irm https://bun.com/install.ps1)} -Version 1.4.0"
```

Substitute the version read from the file. Add `$HOME/.bun/bin` or
`%USERPROFILE%\.bun\bin` to the current process PATH when needed, and verify that `bun --version`
exactly matches the pin.

### Python Environment

Never create or use a Python virtual environment for this repository. Do not run `python -m venv`,
`uv venv`, or `virtualenv`, and do not activate an existing one. A `.venv` silently shadows the
interpreter that `fix` and `check` resolve, and `uv venv` produces environments without `pip`, so
both failure modes surface as a missing Black. Install the requirements directly into the
interpreter those scripts will use.

Install Python 3.12 from `examples/object_detection_service/.python-version` when it is missing,
using the platform package manager, python.org, or `uv python install 3.12`. Then install the
current requirements into that interpreter:

```sh
python3.12 -m pip install -r examples/object_detection_service/requirements.txt
```

On Windows:

```powershell
py -3.12 -m pip install -r examples\object_detection_service\requirements.txt
```

Externally managed interpreters, such as Homebrew and Debian system Python, reject that install.
Retry with `--break-system-packages`, which is what `fix.sh` and `fix.bat` already do:

```sh
python3.12 -m pip install --break-system-packages \
   -r examples/object_detection_service/requirements.txt
```

`requirements.txt` is intentionally unpinned. `fix.sh` and `fix.bat` upgrade every requirement on
each run so local versions match what CI resolves. Never add a version pin to silence a failing
formatter, linter, or type check; fix the code the new release broke.

Set `KEEPPEEK_PYTHON` to an absolute interpreter path when the intended interpreter is not the
first `python3.12` or `python3` on `PATH`. Never point it at a virtual environment.

Do not remove source or generated protobuf files while repairing the Python environment.

### RepoWise

Read `REPOWISE_VERSION` from `.github/workflows/repowise.yml`. Install RepoWise as an isolated user
tool with the repository's Python baseline so its dependencies do not pollute the shared Python
interpreter:

```sh
uv tool install --python 3.12 --force "repowise==0.45.0"
```

Substitute the current workflow pin. If `repowise` is not visible afterward, find the executable
directory with:

```sh
uv tool dir --bin
```

Add that directory to the current process PATH, and offer `uv tool update-shell` only after
explaining that it edits shell PATH configuration. Restart VS Code after persistent PATH changes
because `.vscode/mcp.json` launches `repowise` directly.

If another RepoWise installation shadows the pinned tool, report both executable paths. Upgrade or
remove only the installation that owns the shadowing executable, then require `repowise --version`
to match the workflow pin.

## Phase 4: Install Repository Dependencies

1. Initialize checked-out submodules without changing tracked files:

```sh
git submodule update --init --recursive
```

2. Install UI dependencies from the public npm registry:

```sh
cd ui
bun install --no-save --registry=https://registry.npmjs.org/
```

3. Install the pinned Playwright Chromium build:

```sh
bunx playwright install chromium
```

On Linux, Playwright may also need privileged operating-system packages. Ask the user to run
`bunx playwright install --with-deps chromium` directly when the dependency check identifies them,
then verify Chromium with a focused browser test or Playwright launch.

4. Generate the Python protobuf bindings with the Python 3.12 interpreter:

```sh
python3.12 examples/object_detection_service/generate_protos.py
```

On Windows, use:

```powershell
py -3.12 examples\object_detection_service\generate_protos.py
```

## Phase 5: Configure and Use RepoWise

1. Confirm `.vscode/extensions.json` recommends `repowise-dev.repowise` and `.vscode/mcp.json`
   launches `repowise mcp ${workspaceFolder} --transport stdio`. Do not duplicate those entries.
2. Run `repowise doctor` and `repowise status` from the repository root.
3. If no local index exists, ask the user to run this command directly from the repository root:

```sh
repowise init --yes --no-prose --no-editor-setup
```

The command is local, requires no API key, and leaves the existing repository-owned editor
setup unchanged. Do not enable model-written prose or save an API key during repository setup. 4. If the index exists but is stale, refresh deterministic data with:

```sh
repowise update --index-only
```

5. Restart the RepoWise MCP server or reload VS Code after installing the CLI or rebuilding the
   index.
6. Read `repowise.md` as the committed source of truth for durable decisions and health history.
   Treat `.repowise/` as a derived local cache.
7. Compare `repowise decision list --format json` with the ledger. Add missing decisions as
   `Proposed` and run `repowise decision confirm <id>` only after explicit owner approval. Do not
   duplicate a matching decision title or silently downgrade an active record.
8. When setup establishes a new pinned RepoWise version or materially different index scope,
   refresh the ledger's current snapshot and append a score-history row with the indexed commit and
   RepoWise version.

Use the RepoWise MCP tools before broad manual exploration:

| Need                                   | RepoWise operation               |
| -------------------------------------- | -------------------------------- |
| First view of an unfamiliar area       | `get_overview`                   |
| Context for files, modules, or symbols | Batch targets with `get_context` |
| Exact indexed symbol body              | `get_symbol`                     |
| Concept or implementation search       | `search_codebase`                |
| Architecture rationale and history     | `get_why`                        |
| Blast radius before an edit            | `get_risk`                       |
| Whole-diff risk before completion      | `get_change_risk`                |
| Maintainability or refactoring signals | `get_health`                     |
| Candidate unreachable code             | `get_dead_code`                  |

CLI fallbacks include `repowise context <targets>`, `repowise search "<query>"`, `repowise why
"<query>"`, `repowise health`, `repowise dead-code`, and `repowise risk main..HEAD`. Treat stale
warnings seriously and verify indexed conclusions against the live working tree and executable
tests.

## Phase 6: Verify the Workstation

Check every command in the same shell environment VS Code will inherit:

```sh
git --version
rustup show active-toolchain
rustc --version
cargo --version
rustfmt --version
cargo clippy --version
cargo machete --version
cargo nextest --version
bun --version
node --version
uv --version
ffmpeg -version
repowise --version
```

Require exact matches for Bun, both Cargo tools, and RepoWise. Require Python 3.12 from the
interpreter the entry-point scripts resolve, never from a virtual environment. Node.js must be
actively supported, preferably active LTS, but it is not the package runner for this repository.

Run focused dependency checks before the full suite:

```sh
cargo metadata --format-version 1 --no-deps
python3.12 -c "import black, pytest"
cd ui
bun run registry:check
bunx playwright --version
```

Use `py -3.12` on Windows. Repair a failure in the owning phase and rerun the same focused check
before proceeding.

Finally, return to the repository root and run the canonical validation entry point:

```sh
./check.sh
```

On Windows Command Prompt or PowerShell, run:

```powershell
.\check.bat
```

Do not report setup complete unless the canonical check passes. If it fails for a source defect
rather than workstation setup, stop changing the environment and report the exact failing command
and why it is outside this workflow.

## Completion Report

Report:

- Detected platform and architecture
- Installed, upgraded, already-valid, and user-completed prerequisites
- Resolved executable path and version for every required tool
- Bun, Python, Cargo-tool, and RepoWise pin comparisons
- Dependency and Playwright installation results
- RepoWise index and MCP readiness, including any required VS Code reload
- `repowise.md` decision-store synchronization and score-baseline status
- Focused checks and canonical validation result
- Any remaining manual step, with the exact command and reason it could not be automated
