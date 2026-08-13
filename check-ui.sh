#!/usr/bin/env sh

set -eu

repo_dir=$(
	unset CDPATH
	cd -- "$(dirname -- "$0")"
	pwd
)
cd "$repo_dir/ui"

if ! command -v bun >/dev/null 2>&1; then
	printf '%s\n' 'Bun is required: https://bun.sh/' >&2
	exit 1
fi

bun run quality
cargo build --manifest-path "$repo_dir/Cargo.toml" --bin keeppeek
bun run test:e2e