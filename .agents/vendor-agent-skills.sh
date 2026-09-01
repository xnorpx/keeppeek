#!/usr/bin/env bash

set -euo pipefail

readonly upstream_url="https://github.com/addyosmani/agent-skills.git"
readonly bootstrap_ref="d2c37ef6225dd8726cdd369a8030307f48592d26"

usage() {
	cat <<'EOF'
Usage: .agents/vendor-agent-skills.sh [--check] [--ref <git-ref>]

Vendor addyosmani/agent-skills into .agents/skills and .agents/references.

  --check      Compare the vendored files with the selected upstream revision.
  --ref REF    Resolve and vendor REF instead of the locked commit.
  -h, --help   Show this help.
EOF
}

fail() {
	printf 'vendor-agent-skills: %s\n' "$*" >&2
	exit 1
}

for required_command in awk cat cmp cp diff dirname find git grep mkdir mktemp mv rm sort; do
	command -v "$required_command" >/dev/null 2>&1 || fail "missing required command: $required_command"
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"
agents_dir="$repo_root/.agents"
skills_dir="$agents_dir/skills"
references_dir="$agents_dir/references"
vendor_dir="$agents_dir/vendor/agent-skills"
lock_file="$vendor_dir/UPSTREAM_COMMIT"
managed_skills_file="$vendor_dir/SKILLS"
rust_guidelines="$repo_root/.github/instructions/rust_pragmatic_guidelines.md"
svelte_guidelines="$repo_root/.github/instructions/svelte5_pragmatic_guidelines.md"
tiger_style_skill="$skills_dir/tiger-style/SKILL.md"

[[ -f "$rust_guidelines" ]] || fail "missing Pragmatic Rust Guidelines: ${rust_guidelines#"$repo_root/"}"
[[ -f "$svelte_guidelines" ]] || fail "missing Pragmatic Svelte 5 Guidelines: ${svelte_guidelines#"$repo_root/"}"
[[ -f "$tiger_style_skill" ]] ||
	fail "missing TigerStyle skill: ${tiger_style_skill#"$repo_root/"}"

mode="sync"
requested_ref=""
while (($# > 0)); do
	case "$1" in
		--check)
			mode="check"
			shift
			;;
		--ref)
			(($# >= 2)) || fail "--ref requires a value"
			requested_ref="$2"
			shift 2
			;;
		-h | --help)
			usage
			exit 0
			;;
		*)
			fail "unknown argument: $1"
			;;
	esac
done

if [[ -z "$requested_ref" ]]; then
	if [[ -f "$lock_file" ]]; then
		IFS= read -r requested_ref <"$lock_file"
	else
		requested_ref="$bootstrap_ref"
	fi
fi

[[ "$requested_ref" =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] || fail "invalid git ref: $requested_ref"
[[ "$requested_ref" != *..* && "$requested_ref" != *//* && "$requested_ref" != */ ]] ||
	fail "invalid git ref: $requested_ref"

temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/keeppeek-agent-skills.XXXXXX")"
trap 'rm -rf -- "$temporary_dir"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

source_dir="$temporary_dir/source"
stage_dir="$temporary_dir/stage"
mkdir -p "$source_dir" "$stage_dir/skills" "$stage_dir/references" "$stage_dir/vendor/agent-skills"

git -C "$source_dir" init --quiet
git -C "$source_dir" remote add origin "$upstream_url"
git -C "$source_dir" fetch --quiet --depth 1 origin "$requested_ref" ||
	fail "could not fetch $requested_ref from $upstream_url"
git -C "$source_dir" checkout --quiet --detach FETCH_HEAD
resolved_commit="$(git -C "$source_dir" rev-parse HEAD)"

[[ -d "$source_dir/skills" ]] || fail "upstream revision has no skills directory"
[[ -d "$source_dir/references" ]] || fail "upstream revision has no references directory"
[[ -f "$source_dir/LICENSE" ]] || fail "upstream revision has no LICENSE"

if [[ -n "$(find "$source_dir/skills" "$source_dir/references" -type l -print -quit)" ]]; then
	fail "upstream skills or references contain a symbolic link"
fi

cp -R "$source_dir/skills/." "$stage_dir/skills/"
cp -R "$source_dir/references/." "$stage_dir/references/"

integration_file="$temporary_dir/keeppeek-integration.md"
cat >"$integration_file" <<'EOF'
## KeepPeek Repository Integration

Before applying this skill:

- Treat KeepPeek repository instructions as authoritative when they differ from generic examples in this skill.
- For any Rust implementation, refactor, review, test, or API design work, read and follow the [Pragmatic Rust Guidelines](../../../.github/instructions/rust_pragmatic_guidelines.md). Every applicable `M-*` rule is required.
- For any Svelte, SvelteKit, or frontend TypeScript work under `ui/`, read and follow the [Pragmatic Svelte 5 Guidelines](../../../.github/instructions/svelte5_pragmatic_guidelines.md). Every applicable Svelte `M-*` rule is required. Use Bun under `ui/` and finish UI changes by running `./check.sh` from the repository root.
EOF

tiger_style_integration_file="$temporary_dir/keeppeek-tiger-style-integration.md"
cp "$integration_file" "$tiger_style_integration_file"
cat >>"$tiger_style_integration_file" <<'EOF'
- For executable designs and production code, read and follow
  [TigerStyle](../tiger-style/SKILL.md). Apply its explicit exclusion of TigerBeetle's
  static-allocation policy.
EOF

using_agent_overlay_file="$temporary_dir/keeppeek-using-agent-overlay.md"
cat >"$using_agent_overlay_file" <<'EOF'
### 7. Write Comments in Simplified Technical English

Write every new or changed comment in
[Simplified Technical English](https://en.wikipedia.org/wiki/Simplified_Technical_English)
(ASD-STE100). This rule applies to source comments, doc comments, script comments, and
configuration comments.

- Use active voice unless the actor is unknown.
- Use short, complete sentences.
- Put one instruction or one topic in each sentence.
- Use clear and specific words. Keep necessary technical terms.
- Do not omit articles, subjects, or verbs to make a sentence shorter.
EOF

skill_names=()
for skill_source_dir in "$stage_dir/skills"/*; do
	[[ -d "$skill_source_dir" ]] || continue
	skill_name="${skill_source_dir##*/}"
	[[ "$skill_name" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] || fail "invalid upstream skill name: $skill_name"
	skill_file="$skill_source_dir/SKILL.md"
	[[ -f "$skill_file" ]] || fail "upstream skill has no SKILL.md: $skill_name"

	case "$skill_name" in
		api-and-interface-design | code-review-and-quality | code-simplification | \
			debugging-and-error-recovery | incremental-implementation | performance-optimization | \
			security-and-hardening | spec-driven-development | test-driven-development | \
			using-agent-skills)
			skill_integration_file="$tiger_style_integration_file"
			;;
		*)
			skill_integration_file="$integration_file"
			;;
	esac

	rewritten_skill="$skill_file.keeppeek"
	if ! awk -v integration_file="$skill_integration_file" '
		$0 == "---" && separators < 2 { separators += 1 }
		{ print }
		separators == 2 && !inserted && $0 ~ /^# / {
			print ""
			while ((getline line < integration_file) > 0) print line
			close(integration_file)
			inserted = 1
		}
		END { if (separators < 2 || !inserted) exit 42 }
	' "$skill_file" >"$rewritten_skill"; then
		rm -f "$rewritten_skill"
		fail "could not locate YAML frontmatter and a top-level heading in $skill_name/SKILL.md"
	fi
	mv "$rewritten_skill" "$skill_file"
	grep -Fqx "## KeepPeek Repository Integration" "$skill_file" ||
		fail "KeepPeek integration is missing from $skill_name/SKILL.md"
	if [[ "$skill_integration_file" == "$tiger_style_integration_file" ]]; then
		grep -Fq "[TigerStyle](../tiger-style/SKILL.md)" "$skill_file" ||
			fail "TigerStyle integration is missing from $skill_name/SKILL.md"
	fi

	if [[ "$skill_name" == "using-agent-skills" ]]; then
		if ! awk -v overlay_file="$using_agent_overlay_file" '
			$0 == "## Failure Modes to Avoid" && !inserted {
				while ((getline line < overlay_file) > 0) print line
				close(overlay_file)
				print ""
				inserted = 1
			}
			{ print }
			END { if (!inserted) exit 42 }
		' "$skill_file" >"$rewritten_skill"; then
			rm -f "$rewritten_skill"
			fail "could not locate the core-behavior insertion point in $skill_name/SKILL.md"
		fi
		mv "$rewritten_skill" "$skill_file"
		grep -Fqx "### 7. Write Comments in Simplified Technical English" "$skill_file" ||
			fail "using-agent-skills overlay is missing from $skill_name/SKILL.md"
	fi
	skill_names+=("$skill_name")
done

((${#skill_names[@]} > 0)) || fail "upstream revision contains no skills"
printf '%s\n' "${skill_names[@]}" | LC_ALL=C sort >"$stage_dir/vendor/agent-skills/SKILLS"
printf '%s\n' "$resolved_commit" >"$stage_dir/vendor/agent-skills/UPSTREAM_COMMIT"
printf '%s\n' "$upstream_url" >"$stage_dir/vendor/agent-skills/SOURCE"
cp "$source_dir/LICENSE" "$stage_dir/vendor/agent-skills/LICENSE"

compare_tree() {
	local expected_dir="$1"
	local actual_dir="$2"
	local label="$3"

	if [[ ! -d "$actual_dir" ]]; then
		printf 'Missing %s: %s\n' "$label" "${actual_dir#"$repo_root/"}" >&2
		return 1
	fi

	diff -qr "$expected_dir" "$actual_dir"
}

check_vendor() {
	local failed=0

	if [[ ! -f "$managed_skills_file" ]] ||
		! cmp -s "$stage_dir/vendor/agent-skills/SKILLS" "$managed_skills_file"; then
		printf 'Vendored skill manifest differs from upstream %s.\n' "$resolved_commit" >&2
		failed=1
	fi

	while IFS= read -r skill_name; do
		compare_tree "$stage_dir/skills/$skill_name" "$skills_dir/$skill_name" "skill $skill_name" || failed=1
	done <"$stage_dir/vendor/agent-skills/SKILLS"

	compare_tree "$stage_dir/references" "$references_dir" "shared references" || failed=1
	compare_tree "$stage_dir/vendor/agent-skills" "$vendor_dir" "vendor metadata" || failed=1

	((failed == 0)) || fail "vendored files are stale; run .agents/vendor-agent-skills.sh --ref $requested_ref"
	printf 'Agent skills match upstream commit %s with KeepPeek overlays.\n' "$resolved_commit"
}

if [[ "$mode" == "check" ]]; then
	check_vendor
	exit 0
fi

if [[ -e "$references_dir" && ! -f "$managed_skills_file" ]]; then
	fail ".agents/references exists but is not managed by this script"
fi

while IFS= read -r skill_name; do
	if [[ -e "$skills_dir/$skill_name" ]] &&
		{ [[ ! -f "$managed_skills_file" ]] || ! grep -Fqx "$skill_name" "$managed_skills_file"; }; then
		fail ".agents/skills/$skill_name exists and is not managed by this script"
	fi
done <"$stage_dir/vendor/agent-skills/SKILLS"

if [[ -f "$managed_skills_file" ]]; then
	while IFS= read -r skill_name; do
		[[ "$skill_name" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] || fail "unsafe managed skill name: $skill_name"
		rm -rf -- "${skills_dir:?}/$skill_name"
	done <"$managed_skills_file"
fi

mkdir -p "$skills_dir" "$agents_dir/vendor"
while IFS= read -r skill_name; do
	cp -R "$stage_dir/skills/$skill_name" "$skills_dir/$skill_name"
done <"$stage_dir/vendor/agent-skills/SKILLS"

rm -rf -- "$references_dir" "$vendor_dir"
cp -R "$stage_dir/references" "$references_dir"
cp -R "$stage_dir/vendor/agent-skills" "$vendor_dir"

printf 'Vendored %s agent skills from commit %s.\n' "${#skill_names[@]}" "$resolved_commit"