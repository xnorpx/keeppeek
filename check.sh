#!/usr/bin/env sh

set -eu

if ! command -v bun >/dev/null 2>&1; then
        printf '%s\n' 'Bun is required: https://bun.sh/' >&2
        exit 1
fi

if ! command -v cargo-machete >/dev/null 2>&1; then
        printf '%s\n' 'cargo-machete is required: cargo install cargo-machete' >&2
        exit 1
fi

if ! command -v cargo-nextest >/dev/null 2>&1; then
        printf '%s\n' 'cargo-nextest is required: cargo install cargo-nextest' >&2
        exit 1
fi

repo_dir=$(
        unset CDPATH
        cd -- "$(dirname -- "$0")"
        pwd
)

# Start in the repository root for Rust/Formatting checks
cd "$repo_dir"

if [ -n "${KEEPPEEK_PYTHON:-}" ]; then
        python_cmd=$KEEPPEEK_PYTHON
elif [ -x "$repo_dir/example/object_detection_service/.venv/bin/python" ]; then
        python_cmd=$repo_dir/example/object_detection_service/.venv/bin/python
elif command -v python3.12 >/dev/null 2>&1; then
        python_cmd=python3.12
elif command -v python3 >/dev/null 2>&1; then
        python_cmd=python3
else
        printf '%s\n' 'Python 3.12 is required.' >&2
        exit 1
fi

if ! "$python_cmd" -c 'import black' >/dev/null 2>&1; then
        printf '%s\n' 'Black is required: python -m pip install -r example/object_detection_service/requirements.txt' >&2
        exit 1
fi

echo "Building and Testing Rust..."
cargo build --all
if [ "$(uname -s)" = "Darwin" ]; then
        cargo nextest run --all --features macos-test-aws-crypto
else
        cargo nextest run --all
fi

echo "Running Rust Clippy..."
cargo clippy --all --all-targets -- -D warnings

echo "Checking for unused Rust dependencies..."
cargo machete

echo "Formatting checks..."
cargo fmt --all -- --check
bunx @taplo/cli fmt --check
"$python_cmd" -m black --check --config example/object_detection_service/pyproject.toml .

cd "$repo_dir/ui"
bunx prettier --check "../**/*.md"
echo "Running UI Quality checks..."
bun run quality:check
bun run test:e2e
