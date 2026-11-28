#!/usr/bin/env python3
"""Fail the gate when Rust code or Cargo dependencies introduce tokio or async."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from collections import deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPOSITORY_ROOT = Path(__file__).resolve().parent
CARGO_METADATA_TIMEOUT_SECONDS = 60
MAX_CHAR_ESCAPE_LENGTH = 14
SKIP_DIRECTORY_NAMES: frozenset[str] = frozenset({".git", "node_modules", "target"})

TARGET_PLATFORMS: tuple[str, ...] = (
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
)

BANNED_CRATE_EXACT: frozenset[str] = frozenset(
    {
        "actix",
        "async-std",
        "axum",
        "bastion",
        "bb8",
        "compio",
        "deadpool",
        "fred",
        "futures",
        "glommio",
        "hyper",
        "lapin",
        "may",
        "monoio",
        "poem",
        "pollster",
        "reqwest",
        "rocket",
        "rumqttc",
        "salvo",
        "smol",
        "sqlx",
        "tokio",
        "tonic",
        "tower",
        "warp",
    }
)

BANNED_CRATE_PREFIXES: tuple[str, ...] = (
    "actix-",
    "async-",
    "axum-",
    "deadpool-",
    "embassy-",
    "futures-",
    "hyper-",
    "sqlx-",
    "tokio-",
    "tonic-",
    "tower-",
)

# Allow pollster only in the two manifests that drive turso's async SQLite API synchronously.
POLLSTER_ALLOWED_MANIFESTS: frozenset[str] = frozenset(
    {
        "Cargo.toml",
        "examples/usage_reporting_receiver/Cargo.toml",
    }
)

POLLSTER_ALLOWED_PARENTS: frozenset[str] = frozenset(
    {
        "keeppeek",
        "usage-reporting-receiver",
    }
)

# Keep an exact ratchet list of files that bridge turso's async SQLite API via pollster::block_on.
TURSO_ASYNC_FILES: frozenset[str] = frozenset(
    {
        "benches/advanced_event_search.rs",
        "docs/verification/recording-retention/baseline.rs",
        "docs/verification/recording-retention/runtime-archive-baseline-harness.rs",
        "docs/verification/recording-retention/runtime-archive-baseline-support.rs",
        "docs/verification/recording-retention/runtime-ingest-harness.rs",
        "docs/verification/recording-retention/runtime-sql-counter.rs",
        "examples/recording_legacy_bytes_query.rs",
        "examples/recording_retention_cold.rs",
        "examples/recording_retention_ingest/support.rs",
        "examples/recording_retention_query.rs",
        "examples/recording_retention_runtime.rs",
        "examples/recording_retention_sql/counter.rs",
        "examples/usage_reporting_receiver/src/store.rs",
        "src/backup/database.rs",
        "src/backup/restore.rs",
        "src/config.rs",
        "src/notifications/cloud_state.rs",
        "src/notifications/store.rs",
        "src/server/state_store_durable.rs",
        "src/shares/database.rs",
        "src/shares/tests.rs",
        "src/shares/tests/retention.rs",
        "src/storage/archive/worker/tests/deletion.rs",
        "src/storage/benchmark.rs",
        "src/storage/catalog.rs",
        "src/storage/catalog/advanced_search.rs",
        "src/storage/catalog/analytics/delta.rs",
        "src/storage/catalog/analytics/mod.rs",
        "src/storage/catalog/analytics/query.rs",
        "src/storage/catalog/analytics/rebuild.rs",
        "src/storage/catalog/archive.rs",
        "src/storage/catalog/archive/deletion.rs",
        "src/storage/catalog/archive/deletion/eligibility.rs",
        "src/storage/catalog/archive/deletion/execution.rs",
        "src/storage/catalog/archive/dispatch.rs",
        "src/storage/catalog/archive/exports.rs",
        "src/storage/catalog/archive/tests.rs",
        "src/storage/catalog/archive/tests/deletion.rs",
        "src/storage/catalog/archive_playback.rs",
        "src/storage/catalog/authority.rs",
        "src/storage/catalog/authority/metadata.rs",
        "src/storage/catalog/authority/snapshot.rs",
        "src/storage/catalog/authority/tests.rs",
        "src/storage/catalog/event_write.rs",
        "src/storage/catalog/locations.rs",
        "src/storage/catalog/locations/archives.rs",
        "src/storage/catalog/locations/archives_tests.rs",
        "src/storage/catalog/locations/export_cleanup.rs",
        "src/storage/catalog/locations/export_cleanup/journal.rs",
        "src/storage/catalog/locations/finalize_tests.rs",
        "src/storage/catalog/locations/growth.rs",
        "src/storage/catalog/locations/images.rs",
        "src/storage/catalog/locations/images/pressure.rs",
        "src/storage/catalog/locations/images/recovery.rs",
        "src/storage/catalog/locations/images/retirement.rs",
        "src/storage/catalog/locations/materialization.rs",
        "src/storage/catalog/locations/materialization_tests.rs",
        "src/storage/catalog/locations/move_recording_tests.rs",
        "src/storage/catalog/locations/moves.rs",
        "src/storage/catalog/locations/moves/cancellation.rs",
        "src/storage/catalog/locations/objects.rs",
        "src/storage/catalog/locations/ownership.rs",
        "src/storage/catalog/locations/recording_recovery.rs",
        "src/storage/catalog/locations/recording_recovery/abandonment.rs",
        "src/storage/catalog/locations/recording_recovery/conflicts.rs",
        "src/storage/catalog/locations/recordings.rs",
        "src/storage/catalog/locations/recordings/bytes.rs",
        "src/storage/catalog/locations/recordings/bytes_tests.rs",
        "src/storage/catalog/locations/removal.rs",
        "src/storage/catalog/locations/tests.rs",
        "src/storage/catalog/maintenance.rs",
        "src/storage/catalog/maintenance/evidence.rs",
        "src/storage/catalog/maintenance/jobs.rs",
        "src/storage/catalog/maintenance/jobs/claims.rs",
        "src/storage/catalog/maintenance/jobs/execution.rs",
        "src/storage/catalog/maintenance/jobs/execution/tests.rs",
        "src/storage/catalog/maintenance/jobs/history.rs",
        "src/storage/catalog/maintenance/jobs/ledger.rs",
        "src/storage/catalog/maintenance/jobs/preflight.rs",
        "src/storage/catalog/maintenance/jobs/preflight/tests.rs",
        "src/storage/catalog/maintenance/jobs/recovery.rs",
        "src/storage/catalog/maintenance/jobs/tests.rs",
        "src/storage/catalog/maintenance/jobs/tests/ledger_tests.rs",
        "src/storage/catalog/maintenance/reconciliation.rs",
        "src/storage/catalog/maintenance/reconciliation/fingerprint.rs",
        "src/storage/catalog/maintenance/reconciliation/reindex.rs",
        "src/storage/catalog/maintenance/reconciliation/reindex/tests.rs",
        "src/storage/catalog/native_close.rs",
        "src/storage/catalog/native_close/tests.rs",
        "src/storage/catalog/readers.rs",
        "src/storage/catalog/relocation.rs",
        "src/storage/catalog/retention.rs",
        "src/storage/catalog/retention/event_index.rs",
        "src/storage/catalog/retention/runtime.rs",
        "src/storage/catalog/retention/runtime/expiry.rs",
        "src/storage/catalog/retention/runtime/schema.rs",
        "src/storage/catalog/retention/runtime/tests.rs",
        "src/storage/catalog/retention/runtime/work.rs",
        "src/storage/catalog/retention/runtime/work/events.rs",
        "src/storage/catalog/startup_refresh_tests.rs",
        "src/storage/catalog/workflow.rs",
        "src/storage/catalog/workflow/bookmarks.rs",
        "src/storage/catalog/workflow/library.rs",
        "src/storage/catalog/workflow/query.rs",
        "src/storage/engine/tests/finalization.rs",
        "src/storage/volumes/runtime/recording_recovery_tests.rs",
        "src/storage/volumes/runtime/recording_retention_tests.rs",
        "tests/recording_deletion_ledger.rs",
        "tests/recording_retention_catalog.rs",
        "tests/recording_retention_catalog/cold.rs",
        "tests/recording_retention_media.rs",
        "tests/timelapse.rs",
    }
)

# Allow Tauri desktop command async handlers only inside the desktop shell entrypoint.
TAURI_ASYNC_FILES: frozenset[str] = frozenset(
    {
        "crates/keeppeek-desktop/src/desktop_app.rs",
    }
)

_FORBIDDEN_RUNTIME_IDENT = (
    r"(?:tokio(?:_[a-z0-9_]+)?|async_[a-z0-9_]+|smol|monoio|glommio|compio|"
    r"embassy_[a-z0-9_]+|futures(?:_[a-z0-9_]+)?|hyper(?:_[a-z0-9_]+)?|reqwest|"
    r"axum(?:_[a-z0-9_]+)?|actix(?:_[a-z0-9_]+)?|tonic(?:_[a-z0-9_]+)?|"
    r"tower(?:_[a-z0-9_]+)?|rumqttc|sqlx(?:_[a-z0-9_]+)?|deadpool(?:_[a-z0-9_]+)?)"
)

_FORBIDDEN_RUNTIME_RE = re.compile(
    rf"(?<![A-Za-z0-9_#:.]){_FORBIDDEN_RUNTIME_IDENT}\s*::"
    rf"|\b(?:use|extern\s+crate)\s+(?:::\s*)?{_FORBIDDEN_RUNTIME_IDENT}\b"
    r"|#\s*\[\s*(?:tokio|async_std|smol|actix_web|async_trait)\b"
)

_TAURI_ASYNC_RUNTIME_RE = re.compile(r"\btauri\s*::\s*async_runtime\b")

_ASYNC_SYNTAX_RE = re.compile(
    r"(?<!r#)\basync\b"
    r"|\.\s*await\b"
    r"|(?<![A-Za-z0-9_#])pollster\s*::"
    r"|\b(?:use|extern\s+crate)\s+(?:::\s*)?pollster\b"
    r"|(?<![A-Za-z0-9_#])(?:std|core)\s*::\s*future\b"
    r"|(?<![A-Za-z0-9_#])(?:std|core)\s*::\s*task\s*::\s*"
    r"(?:Context|Poll|Waker|\{[^}]*\b(?:Context|Poll|Waker)\b)"
)

_CANDIDATE_RE = re.compile(
    r"async|await|tokio|pollster|futures|smol|monoio|glommio|compio|embassy|"
    r"hyper|reqwest|axum|actix|tonic|tower|rumqttc|sqlx|future|Context|Poll|Waker"
)

# ponytail: Regex token stepping strips comments and literals before keyword checks;
# switch to a full Rust parser only if macro token trees cause false positives.
_RUST_DELIMITER_RE = re.compile(r'//|/\*|(?<![A-Za-z0-9_])[bc]?r#*"|(?:(?<![A-Za-z0-9_])[bc])?"|\'')


@dataclass(frozen=True, order=True)
class Violation:
    rule: str
    location: str
    message: str

    def format_line(self) -> str:
        return f"[{self.rule}] {self.location}: {self.message}"


def normalize_crate_name(name: str) -> str:
    return name.strip().lower().replace("_", "-")


def is_banned_crate(name: str) -> bool:
    normalized = normalize_crate_name(name)
    return normalized in BANNED_CRATE_EXACT or normalized.startswith(BANNED_CRATE_PREFIXES)


def _iter_repo_files(repo_root: Path, filename_or_suffix: str, exact_name: bool) -> list[Path]:
    found: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(repo_root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRECTORY_NAMES)
        for filename in sorted(filenames):
            matches = (
                filename == filename_or_suffix
                if exact_name
                else filename.endswith(filename_or_suffix)
            )
            if matches:
                found.append(Path(dirpath) / filename)
    return found


def _collect_manifest_dep_tables(data: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
    tables: list[tuple[str, dict[str, Any]]] = []
    dep_sections = ("dependencies", "dev-dependencies", "build-dependencies")
    workspace = data.get("workspace")
    if isinstance(workspace, dict) and isinstance(workspace.get("dependencies"), dict):
        tables.append(("workspace.dependencies", workspace["dependencies"]))
    for section in dep_sections:
        table = data.get(section)
        if isinstance(table, dict):
            tables.append((section, table))
    targets = data.get("target")
    if isinstance(targets, dict):
        for cfg_name, cfg_table in targets.items():
            if not isinstance(cfg_table, dict):
                continue
            for section in dep_sections:
                table = cfg_table.get(section)
                if isinstance(table, dict):
                    tables.append((f"target.{cfg_name}.{section}", table))
    patches = data.get("patch")
    if isinstance(patches, dict):
        for registry, patch_table in patches.items():
            if isinstance(patch_table, dict):
                tables.append((f"patch.{registry}", patch_table))
    return tables


def check_manifests(repo_root: Path) -> list[Violation]:
    violations: list[Violation] = []
    for manifest_path in _iter_repo_files(repo_root, "Cargo.toml", exact_name=True):
        rel = manifest_path.relative_to(repo_root).as_posix()
        try:
            data = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            violations.append(Violation("manifest-parse", rel, str(error)))
            continue
        for section, table in _collect_manifest_dep_tables(data):
            for key, spec in table.items():
                candidates = {normalize_crate_name(key)}
                if isinstance(spec, dict) and isinstance(spec.get("package"), str):
                    candidates.add(normalize_crate_name(spec["package"]))
                for crate in sorted(candidates):
                    if not is_banned_crate(crate):
                        continue
                    if crate == "pollster" and rel in POLLSTER_ALLOWED_MANIFESTS:
                        continue
                    violations.append(
                        Violation(
                            "banned-manifest-dep",
                            rel,
                            f"forbidden dependency '{crate}' in [{section}]",
                        )
                    )
    return sorted(violations)


def _is_allowed_graph_edge(
    root_name: str,
    src_name: str,
    dst_name: str,
    is_dev_only: bool,
) -> bool:
    if (
        root_name == "keeppeek-desktop"
        and src_name == "keeppeek-desktop"
        and dst_name == "tauri"
        and not is_dev_only
    ):
        return True
    if root_name == "rouille" and src_name == "rouille" and dst_name == "postgres" and is_dev_only:
        return True
    return dst_name == "pollster" and src_name in POLLSTER_ALLOWED_PARENTS and not is_dev_only


def check_metadata_document(metadata: dict[str, Any], platform: str) -> list[Violation]:
    packages = {pkg["id"]: normalize_crate_name(pkg["name"]) for pkg in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace_ids = sorted(metadata["workspace_members"], key=lambda pkg_id: packages[pkg_id])
    violations: list[Violation] = []

    for root_id in workspace_ids:
        root_name = packages[root_id]
        visited: set[str] = set()
        reported_crates: set[str] = set()
        queue: deque[tuple[str, tuple[str, ...], int, bool]] = deque(
            [(root_id, (root_name,), 0, False)]
        )
        while queue:
            node_id, path, depth, via_dev = queue.popleft()
            if node_id in visited:
                continue
            visited.add(node_id)
            src_name = packages[node_id]
            for dep in nodes[node_id].get("deps", []):
                kinds = [entry.get("kind") for entry in dep.get("dep_kinds", [])]
                is_dev_only = bool(kinds) and all(kind == "dev" for kind in kinds)
                if depth > 0 and is_dev_only:
                    continue
                dst_id = dep["pkg"]
                dst_name = packages[dst_id]
                if _is_allowed_graph_edge(root_name, src_name, dst_name, is_dev_only):
                    continue
                next_via_dev = via_dev or is_dev_only
                next_path = (*path, dst_name)
                if is_banned_crate(dst_name):
                    if dst_name not in reported_crates:
                        reported_crates.add(dst_name)
                        kind_label = "dev" if next_via_dev else "normal/build"
                        chain = " -> ".join(next_path)
                        violations.append(
                            Violation(
                                "banned-cargo-dep",
                                f"{platform}:{root_name}",
                                f"forbidden crate '{dst_name}' ({kind_label}): {chain}",
                            )
                        )
                elif dst_id not in visited:
                    queue.append((dst_id, next_path, depth + 1, next_via_dev))

    return sorted(violations)


def check_cargo_metadata(
    repo_root: Path,
    platforms: tuple[str, ...] = TARGET_PLATFORMS,
) -> list[Violation]:
    violations: list[Violation] = []
    for platform in platforms:
        command = [
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--all-features",
            "--filter-platform",
            platform,
        ]
        try:
            completed = subprocess.run(
                command,
                cwd=repo_root,
                capture_output=True,
                text=True,
                timeout=CARGO_METADATA_TIMEOUT_SECONDS,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            violations.append(
                Violation(
                    "cargo-metadata-error", platform, f"unable to run cargo metadata: {error}"
                )
            )
            continue
        if completed.returncode != 0:
            detail = completed.stderr.strip() or f"exit code {completed.returncode}"
            violations.append(
                Violation("cargo-metadata-error", platform, f"cargo metadata failed: {detail}")
            )
            continue
        try:
            metadata = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            violations.append(
                Violation(
                    "cargo-metadata-error",
                    platform,
                    f"invalid cargo metadata JSON: {error}",
                )
            )
            continue
        violations.extend(check_metadata_document(metadata, platform))
    return sorted(violations)


def _blank_preserving_newlines(text: str) -> str:
    return "".join("\n" if ch == "\n" else " " for ch in text)


def _consume_block_comment(source: str, start: int) -> int:
    depth = 1
    index = start + 2
    length = len(source)
    while index < length and depth > 0:
        if source.startswith("/*", index):
            depth += 1
            index += 2
        elif source.startswith("*/", index):
            depth -= 1
            index += 2
        else:
            index += 1
    return index


def _consume_raw_string(source: str, token: str, token_end: int) -> int:
    hashes = token.count("#")
    closing = '"' + ("#" * hashes)
    found = source.find(closing, token_end)
    return len(source) if found == -1 else found + len(closing)


def _consume_quoted_string(source: str, token_end: int) -> int:
    index = token_end
    length = len(source)
    while index < length:
        ch = source[index]
        if ch == "\\":
            index = min(length, index + 2)
        elif ch == '"':
            return index + 1
        else:
            index += 1
    return length


def _consume_char_literal(source: str, start: int) -> int | None:
    length = len(source)
    if start + 1 >= length:
        return None
    if source[start + 1] == "\\":
        if start + 2 >= length or source[start + 2] == "\n":
            return None
        # Skip the escaped character first so '\'' does not terminate on the inner quote.
        limit = min(length, start + MAX_CHAR_ESCAPE_LENGTH)
        index = start + 3
        while index < limit:
            if source[index] == "\n":
                return None
            if source[index] == "'":
                return index + 1
            index += 1
        return None
    if start + 2 < length and source[start + 1] != "\n" and source[start + 2] == "'":
        return start + 3
    return None


def strip_rust_comments_and_literals(source: str) -> str:
    chunks: list[str] = []
    pos = 0
    length = len(source)
    while pos < length:
        match = _RUST_DELIMITER_RE.search(source, pos)
        if match is None:
            chunks.append(source[pos:])
            break
        start = match.start()
        token = match.group(0)
        chunks.append(source[pos:start])
        if token == "//":
            newline = source.find("\n", match.end())
            end = length if newline == -1 else newline
        elif token == "/*":
            end = _consume_block_comment(source, start)
        elif token.endswith('"') and "r" in token:
            end = _consume_raw_string(source, token, match.end())
        elif token.endswith('"'):
            end = _consume_quoted_string(source, match.end())
        else:
            char_end = _consume_char_literal(source, start)
            if char_end is None:
                chunks.append("'")
                pos = start + 1
                continue
            end = char_end
        chunks.append(_blank_preserving_newlines(source[start:end]))
        pos = end
    return "".join(chunks)


def _line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def _scan_rust_file(
    rel: str,
    stripped: str,
    allow_turso: bool,
    allow_tauri: bool,
) -> tuple[list[Violation], bool]:
    violations: list[Violation] = []
    saw_allowed_async = False

    for match in _FORBIDDEN_RUNTIME_RE.finditer(stripped):
        line = _line_number(stripped, match.start())
        token = " ".join(match.group(0).split())
        violations.append(
            Violation(
                "forbidden-runtime", f"{rel}:{line}", f"forbidden async runtime reference '{token}'"
            )
        )

    for match in _TAURI_ASYNC_RUNTIME_RE.finditer(stripped):
        if allow_tauri:
            saw_allowed_async = True
            continue
        line = _line_number(stripped, match.start())
        violations.append(
            Violation(
                "tauri-async-runtime",
                f"{rel}:{line}",
                "tauri::async_runtime is allowed only in the desktop shell entrypoint",
            )
        )

    for match in _ASYNC_SYNTAX_RE.finditer(stripped):
        if allow_turso or allow_tauri:
            saw_allowed_async = True
            continue
        line = _line_number(stripped, match.start())
        token = " ".join(match.group(0).split())
        violations.append(
            Violation("async-syntax", f"{rel}:{line}", f"forbidden Rust async construct '{token}'")
        )

    return violations, saw_allowed_async


def check_rust_sources(
    repo_root: Path,
    turso_async_files: frozenset[str] = TURSO_ASYNC_FILES,
    tauri_async_files: frozenset[str] = TAURI_ASYNC_FILES,
) -> list[Violation]:
    assert turso_async_files.isdisjoint(tauri_async_files), "allowlists must not overlap"
    violations: list[Violation] = []
    used_allowlist: set[str] = set()
    existing_files: set[str] = set()

    for rust_path in _iter_repo_files(repo_root, ".rs", exact_name=False):
        rel = rust_path.relative_to(repo_root).as_posix()
        existing_files.add(rel)
        raw_text = rust_path.read_text(encoding="utf-8", errors="replace")
        if _CANDIDATE_RE.search(raw_text) is None:
            continue
        stripped = strip_rust_comments_and_literals(raw_text)
        file_violations, saw_allowed = _scan_rust_file(
            rel,
            stripped,
            allow_turso=rel in turso_async_files,
            allow_tauri=rel in tauri_async_files,
        )
        violations.extend(file_violations)
        if saw_allowed:
            used_allowlist.add(rel)

    for allowed in sorted(turso_async_files | tauri_async_files):
        if allowed not in existing_files:
            violations.append(
                Violation(
                    "stale-allowlist",
                    allowed,
                    "allowlisted file no longer exists; remove it from godzilla.py",
                )
            )
        elif allowed not in used_allowlist:
            violations.append(
                Violation(
                    "stale-allowlist",
                    allowed,
                    (
                        "allowlisted file no longer contains async constructs; "
                        "remove it from godzilla.py"
                    ),
                )
            )

    return sorted(violations)


def check_repository(repo_root: Path = REPOSITORY_ROOT) -> list[Violation]:
    violations: list[Violation] = []
    violations.extend(check_manifests(repo_root))
    violations.extend(check_cargo_metadata(repo_root))
    violations.extend(check_rust_sources(repo_root))
    return sorted(violations)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail the gate when Rust code or Cargo dependencies introduce tokio or async."
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=REPOSITORY_ROOT,
        help="Repository root directory to inspect.",
    )
    args = parser.parse_args(argv)
    violations = check_repository(args.repo_root.resolve())
    if violations:
        print(
            f"Godzilla: FAILED — found {len(violations)} tokio/async policy violation(s):",
            file=sys.stderr,
        )
        for violation in violations:
            print(f"  {violation.format_line()}", file=sys.stderr)
        return 1
    print("Godzilla: OK — no forbidden tokio or async usage detected.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
