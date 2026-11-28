from __future__ import annotations

import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from typing import Any
from unittest.mock import patch

import godzilla


def _make_metadata(
    packages: list[tuple[str, str]],
    workspace_ids: list[str],
    edges: dict[str, list[tuple[str, list[str | None]]]],
) -> dict[str, Any]:
    return {
        "packages": [{"id": pkg_id, "name": name} for pkg_id, name in packages],
        "workspace_members": workspace_ids,
        "resolve": {
            "nodes": [
                {
                    "id": pkg_id,
                    "deps": [
                        {
                            "pkg": dep_id,
                            "dep_kinds": [{"kind": kind} for kind in kinds],
                        }
                        for dep_id, kinds in edges.get(pkg_id, [])
                    ],
                }
                for pkg_id, _ in packages
            ]
        },
    }


class StripRustCommentsAndLiteralsTests(unittest.TestCase):
    def test_strips_comments_and_preserves_line_numbers(self) -> None:
        source = (
            "// tokio::main async fn fake()\n"
            "/* outer /* nested async fn */ tokio::spawn */\n"
            "fn real() {}\n"
        )
        stripped = godzilla.strip_rust_comments_and_literals(source)
        assert stripped.count("\n") == source.count("\n")
        assert "tokio" not in stripped
        assert "async" not in stripped
        assert "fn real() {}" in stripped

    def test_strips_strings_raw_strings_and_char_literals_without_breaking_lifetimes(
        self,
    ) -> None:
        source = (
            'const A: &str = "async fn tokio::main \\"quoted\\"";\n'
            'const B: &[u8] = b"futures::executor";\n'
            'const C: &core::ffi::CStr = c"smol::block_on";\n'
            'const D: &str = r##"async { x.await }"##;\n'
            "const E: char = '\"';\n"
            "const F: char = '\\'';\n"
            "const G: char = '\\u{22}';\n"
            "fn borrow<'a>(input: &'a str) -> &'a str {\n"
            "    let r#async = input;\n"
            "    r#async\n"
            "}\n"
            "async fn caught() {}\n"
        )
        stripped = godzilla.strip_rust_comments_and_literals(source)
        assert stripped.count("\n") == source.count("\n")
        assert "tokio" not in stripped
        assert "futures" not in stripped
        assert "smol" not in stripped
        assert "r#async" in stripped
        assert "async fn caught()" in stripped


class ManifestPolicyTests(unittest.TestCase):
    def test_rejects_banned_dependencies_across_sections_and_renames(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                """
[workspace.dependencies]
my_rt = { package = "tokio", version = "1" }

[dependencies]
async_std = "1"
rumqttc = "0.24"

[dev-dependencies]
futures_util = "0.3"

[build-dependencies]
hyper-util = "0.1"

[target.'cfg(windows)'.dependencies]
smol = "2"

[patch.crates-io]
sneaky = { package = "tokio-util", path = "crates/sneaky" }
""",
                encoding="utf-8",
            )
            violations = godzilla.check_manifests(root)
            messages = [v.message for v in violations]
            assert len(violations) == 7, messages
            for expected in (
                "tokio",
                "async-std",
                "rumqttc",
                "futures-util",
                "hyper-util",
                "smol",
                "tokio-util",
            ):
                assert any(expected in msg for msg in messages), (expected, messages)

    def test_pollster_allowed_only_in_turso_manifests(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                '[dependencies]\npollster = "1.0"\n',
                encoding="utf-8",
            )
            extra_dir = root / "crates" / "camera"
            extra_dir.mkdir(parents=True)
            (extra_dir / "Cargo.toml").write_text(
                '[dependencies]\npollster = "1.0"\n',
                encoding="utf-8",
            )
            violations = godzilla.check_manifests(root)
            assert len(violations) == 1
            assert violations[0].location == "crates/camera/Cargo.toml"
            assert "pollster" in violations[0].message

    def test_allows_clean_sync_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                """
[package]
name = "keeppeek"
version = "0.1.0"

[dependencies]
mqtt-protocol-core = "0.7.9"
native-tls = "0.2.18"
polling = "3.11.0"
ureq = "3.5.2"
""",
                encoding="utf-8",
            )
            assert godzilla.check_manifests(root) == []


class CargoMetadataPolicyTests(unittest.TestCase):
    def test_catches_transitive_tokio_through_normal_and_root_dev_deps(self) -> None:
        metadata = _make_metadata(
            packages=[
                ("kp", "keeppeek"),
                ("mq", "rumqttc"),
                ("tk", "tokio"),
                ("fu", "futures-util"),
            ],
            workspace_ids=["kp"],
            edges={
                "kp": [("mq", [None]), ("fu", ["dev"])],
                "mq": [("tk", [None])],
            },
        )
        violations = godzilla.check_metadata_document(metadata, "x86_64-unknown-linux-gnu")
        messages = [v.message for v in violations]
        assert any("keeppeek -> rumqttc" in m for m in messages), messages
        assert any("keeppeek -> futures-util" in m for m in messages), messages

    def test_ignores_transitive_dev_deps_and_enforces_narrow_edge_allowlist(self) -> None:
        metadata = _make_metadata(
            packages=[
                ("kp", "keeppeek"),
                ("ro", "rouille"),
                ("pg", "postgres"),
                ("tk", "tokio"),
                ("kd", "keeppeek-desktop"),
                ("ta", "tauri"),
                ("pl", "pollster"),
            ],
            workspace_ids=["kp", "ro", "kd"],
            edges={
                "kp": [("ro", [None]), ("pl", [None])],
                "ro": [("pg", ["dev"])],
                "pg": [("tk", [None])],
                "kd": [("ta", [None])],
                "ta": [("tk", [None])],
            },
        )
        assert godzilla.check_metadata_document(metadata, "x86_64-pc-windows-msvc") == []

    def test_rejects_bypass_outside_allowed_edges(self) -> None:
        metadata = _make_metadata(
            packages=[
                ("kd", "keeppeek-desktop"),
                ("ta", "tauri"),
                ("rq", "reqwest"),
                ("tk", "tokio"),
                ("ro", "rouille"),
                ("pg", "postgres"),
                ("sv", "keeppeek-supervisor"),
                ("pl", "pollster"),
            ],
            workspace_ids=["kd", "ro", "sv"],
            edges={
                # Reject direct reqwest from keeppeek-desktop even though tauri is allowed.
                "kd": [("ta", [None]), ("rq", [None])],
                "ta": [("tk", [None])],
                "rq": [("tk", [None])],
                # Reject promoting rouille -> postgres from dev to normal.
                "ro": [("pg", [None])],
                "pg": [("tk", [None])],
                # Reject adding pollster to keeppeek-supervisor.
                "sv": [("pl", [None])],
            },
        )
        violations = godzilla.check_metadata_document(metadata, "x86_64-pc-windows-msvc")
        messages = [v.message for v in violations]
        assert len(violations) == 3, messages
        assert any("keeppeek-desktop -> reqwest" in m for m in messages), messages
        assert any("rouille -> postgres -> tokio" in m for m in messages), messages
        assert any("keeppeek-supervisor -> pollster" in m for m in messages), messages


class RustSourcePolicyTests(unittest.TestCase):
    def test_rejects_async_and_runtime_constructs_with_exact_line_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root / "src"
            src.mkdir()
            (src / "bad.rs").write_text(
                "fn ok() {\n"
                "    let r#async = 1;\n"
                '    let _ = "async fn tokio::main";\n'
                "    // foo().await\n"
                "}\n"
                "async\n"
                "fn sneaky_async() {\n"
                "    let _ = async move {\n"
                "        work()\n"
                "            .await\n"
                "    };\n"
                "    let _ = async || 1;\n"
                "}\n"
                "#[tokio::main]\n"
                "fn runtime_attr() {\n"
                "    let _ = std::future::ready(1);\n"
                "    let _ = core::task::Poll::Ready(1);\n"
                "    let _ = pollster::block_on(x);\n"
                "    let _ = tokio_util::codec::LinesCodec::new();\n"
                "}\n",
                encoding="utf-8",
            )
            violations = godzilla.check_rust_sources(
                root,
                turso_async_files=frozenset(),
                tauri_async_files=frozenset(),
            )
            rules_and_lines = {(v.rule, v.location) for v in violations}
            assert ("async-syntax", "src/bad.rs:6") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:8") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:10") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:12") in rules_and_lines
            assert ("forbidden-runtime", "src/bad.rs:14") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:16") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:17") in rules_and_lines
            assert ("async-syntax", "src/bad.rs:18") in rules_and_lines
            assert ("forbidden-runtime", "src/bad.rs:19") in rules_and_lines
            # Ensure comments, string literals, and r#async do not trigger violations.
            assert not any(v.location.endswith((":2", ":3", ":4")) for v in violations), violations

    def test_enforces_turso_and_tauri_boundaries_and_ratchets_stale_entries(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / "src" / "turso_ok.rs").write_text(
                "fn db() { pollster::block_on(async { query().await }); }\n",
                encoding="utf-8",
            )
            (root / "src" / "turso_bad_runtime.rs").write_text(
                "fn db() { tokio::spawn(async {}); }\n",
                encoding="utf-8",
            )
            (root / "src" / "turso_bad_tauri.rs").write_text(
                "fn db() { tauri::async_runtime::spawn(async {}); }\n",
                encoding="utf-8",
            )
            (root / "src" / "desktop_ok.rs").write_text(
                "async fn cmd() { tauri::async_runtime::spawn_blocking(|| ()).await; }\n",
                encoding="utf-8",
            )
            (root / "src" / "stale_now_sync.rs").write_text(
                "fn pure_sync() -> i32 { 42 }\n",
                encoding="utf-8",
            )
            violations = godzilla.check_rust_sources(
                root,
                turso_async_files=frozenset(
                    {
                        "src/turso_ok.rs",
                        "src/turso_bad_runtime.rs",
                        "src/turso_bad_tauri.rs",
                        "src/stale_now_sync.rs",
                        "src/deleted_file.rs",
                    }
                ),
                tauri_async_files=frozenset({"src/desktop_ok.rs"}),
            )
            by_loc = {(v.rule, v.location) for v in violations}
            assert ("forbidden-runtime", "src/turso_bad_runtime.rs:1") in by_loc
            assert ("tauri-async-runtime", "src/turso_bad_tauri.rs:1") in by_loc
            assert ("stale-allowlist", "src/stale_now_sync.rs") in by_loc
            assert ("stale-allowlist", "src/deleted_file.rs") in by_loc
            assert not any(
                v.location.startswith(("src/turso_ok.rs", "src/desktop_ok.rs")) for v in violations
            ), violations


class CliAndLiveRepositoryGateTests(unittest.TestCase):
    def test_main_returns_nonzero_when_violations_exist(self) -> None:
        sample = [godzilla.Violation("async-syntax", "src/app.rs:10", "forbidden")]
        stderr_buffer = io.StringIO()
        with (
            patch.object(godzilla, "check_repository", return_value=sample),
            redirect_stderr(stderr_buffer),
        ):
            code = godzilla.main(["--repo-root", "."])
        assert code == 1
        assert "Godzilla: FAILED" in stderr_buffer.getvalue()
        assert "src/app.rs:10" in stderr_buffer.getvalue()

    def test_main_returns_zero_when_clean(self) -> None:
        stdout_buffer = io.StringIO()
        with (
            patch.object(godzilla, "check_repository", return_value=[]),
            redirect_stdout(stdout_buffer),
        ):
            code = godzilla.main(["--repo-root", "."])
        assert code == 0
        assert "Godzilla: OK" in stdout_buffer.getvalue()

    def test_current_repository_passes_godzilla(self) -> None:
        repo_root = Path(__file__).resolve().parent.parent
        assert godzilla.check_repository(repo_root) == []


if __name__ == "__main__":
    unittest.main()
