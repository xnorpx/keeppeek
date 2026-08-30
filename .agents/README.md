# Agent Skills

KeepPeek discovers project skills under `.agents/skills/`. The `keeppeek-camera-setup`, `keeppeek-file-bug`, and `setup-repo` directories are maintained locally. Directories listed in `vendor/agent-skills/SKILLS` and the shared `references/` directory are vendored from [addyosmani/agent-skills](https://github.com/addyosmani/agent-skills).

The matching entries under `.claude/skills/` and `.github/skills/` are repository-owned compatibility shims that point agents to the canonical local skills in `.agents/skills/`; they are not part of the upstream vendor set.

Every vendored `SKILL.md` receives a generated KeepPeek integration that makes the repository's Pragmatic Rust and Pragmatic Svelte 5 guidelines authoritative for applicable work. The source guidelines remain in `.github/instructions/`; they are linked rather than duplicated so updates cannot drift between skills.

## Refresh

Reproduce the currently locked revision:

```sh
./.agents/vendor-agent-skills.sh
```

Verify the checked-in copy against that revision without modifying the worktree:

```sh
./.agents/vendor-agent-skills.sh --check
```

Resolve and vendor a newer upstream revision, then review the resulting diff:

```sh
./.agents/vendor-agent-skills.sh --ref main
```

The resolved commit is recorded in `vendor/agent-skills/UPSTREAM_COMMIT`. The script copies complete skill directories, including nested scripts and references, and vendors the upstream shared references and MIT license. It refuses to overwrite an unmanaged skill with the same name, so KeepPeek's local skills remain independent.

Running the script repeatedly with the same revision is safe. If it is interrupted while replacing managed files, rerun it to restore the complete locked tree.

Do not edit managed skill directories, shared references, or vendor metadata by hand. The next refresh replaces those changes.