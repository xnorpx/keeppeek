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

repo_dir=$(
        unset CDPATH
        cd -- "$(dirname -- "$0")"
        pwd
)

# Start in the repository root for Rust/Formatting checks
cd "$repo_dir"

echo "Building and Testing Rust..."
cargo build --all
cargo test --all

echo "Running Rust Clippy..."
cargo clippy --all --all-targets -- -D warnings

echo "Checking for unused Rust dependencies..."
cargo machete

echo "Formatting checks..."
cargo fmt --all -- --check
bunx @taplo/cli fmt --check
bunx prettier --check "**/*.md"

cd "$repo_dir/ui"
echo "Running UI Quality checks..."
bun run quality
cargo build --manifest-path "$repo_dir/Cargo.toml" --bin keeppeek
bun run test:e2e
