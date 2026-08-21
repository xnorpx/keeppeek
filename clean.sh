#!/usr/bin/env sh

set -eu

repo_dir=$(
        unset CDPATH
        cd -- "$(dirname -- "$0")"
        pwd
)

clean_dependencies=false
case "${1:-}" in
        "") ;;
        --dependencies) clean_dependencies=true ;;
        *)
                printf 'Usage: %s [--dependencies]\n' "$0" >&2
                exit 2
                ;;
esac

remove_generated() {
        relative_path=$1
        absolute_path=$repo_dir/$relative_path
        if [ -e "$absolute_path" ]; then
                printf 'Removing %s\n' "$relative_path"
                rm -rf -- "$absolute_path"
        fi
}

remove_generated target
remove_generated crates/target
remove_generated __pycache__
remove_generated .mypy_cache
remove_generated .pytest_cache
remove_generated .ruff_cache
remove_generated ui/.svelte-kit
remove_generated ui/build
remove_generated ui/test-results
remove_generated ui/playwright-report
remove_generated ui/blob-report
remove_generated ui/coverage
remove_generated ui/visual-harness/storybook-static
remove_generated ui/visual-harness/.loki/current
remove_generated ui/visual-harness/.loki/difference

if [ "$clean_dependencies" = true ]; then
        remove_generated ui/node_modules
        remove_generated ui/visual-harness/node_modules
else
        printf '%s\n' 'Keeping installed dependencies. Pass --dependencies to remove them.'
fi

printf '%s\n' 'Generated artifacts removed.'