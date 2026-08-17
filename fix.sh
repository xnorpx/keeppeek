#!/usr/bin/env sh

set -eu

if ! command -v bun >/dev/null 2>&1; then
        printf '%s\n' 'Bun is required: https://bun.sh/' >&2
        exit 1
fi

repo_dir=$(
        unset CDPATH
        cd -- "$(dirname -- "$0")"
        pwd
)

# Start in the repository root
cd "$repo_dir"

echo "Formatting Rust files..."
cargo fmt --all

echo "Formatting TOML files..."
bunx @taplo/cli fmt

echo "Formatting Markdown files..."
bunx prettier --write "**/*.md"

cd "$repo_dir/ui"
echo "Formatting UI files..."
bun run lint:fix
bun run format
