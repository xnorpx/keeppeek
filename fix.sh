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

if [ -n "${KEEPPEEK_PYTHON:-}" ]; then
        python_cmd=$KEEPPEEK_PYTHON
elif command -v python3.12 >/dev/null 2>&1; then
        python_cmd=python3.12
elif command -v python3 >/dev/null 2>&1; then
        python_cmd=python3
else
        printf '%s\n' 'Python 3.12 is required.' >&2
        exit 1
fi

requirements=examples/object_detection_service/requirements.txt

# Requirements are intentionally unpinned, so refresh them to the versions CI will resolve.
echo "Updating Python requirements..."
if ! "$python_cmd" -m pip install --quiet --upgrade -r "$requirements" >/dev/null 2>&1; then
        # Externally managed interpreters reject installs without --break-system-packages.
        "$python_cmd" -m pip install --quiet --upgrade --break-system-packages -r "$requirements" || true
fi

if ! "$python_cmd" -c 'import black' >/dev/null 2>&1; then
        printf '%s\n' "Black is required: $python_cmd -m pip install -r $requirements" >&2
        exit 1
fi

echo "Formatting Rust files..."
cargo fmt --all

echo "Formatting TOML files..."
bunx @taplo/cli fmt

echo "Formatting Python files..."
"$python_cmd" -m black --config examples/object_detection_service/pyproject.toml .

cd "$repo_dir/ui"
echo "Formatting Markdown files..."
bunx prettier --write "../**/*.md"

echo "Formatting UI files..."
bun run lint:fix
bun run format
