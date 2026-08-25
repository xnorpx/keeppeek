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

echo "Formatting Rust files..."
cargo fmt --all

echo "Formatting TOML files..."
bunx @taplo/cli fmt

echo "Formatting Python files..."
"$python_cmd" -m black --config example/object_detection_service/pyproject.toml .

cd "$repo_dir/ui"
echo "Formatting Markdown files..."
bunx prettier --write "../**/*.md"

echo "Formatting UI files..."
bun run lint:fix
bun run format
