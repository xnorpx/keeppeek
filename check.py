#!/usr/bin/env python3

import importlib.util
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    import resource
except ImportError:
    resource = None


REPOSITORY_ROOT = Path(__file__).resolve().parent
UI_ROOT = REPOSITORY_ROOT / "ui"
REPORT_ROOT = REPOSITORY_ROOT / "target" / "check-report"
LOG_ROOT = REPORT_ROOT / "logs"
CARGO_TIMING_SOURCE = REPOSITORY_ROOT / "target" / "cargo-timings" / "cargo-timing.html"
CARGO_TIMING_REPORT = REPORT_ROOT / "cargo-build-timings.html"
SLOW_TEST_THRESHOLD_SECONDS = 10.0
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")


@dataclass
class PhaseResult:
    name: str
    command: list[str]
    cwd: str
    wall_seconds: float
    child_user_seconds: float | None
    child_system_seconds: float | None
    exit_code: int
    log: str


def require_command(command: str, installation: str) -> None:
    if shutil.which(command) is None:
        raise RuntimeError(f"{command} is required: {installation}")


def child_usage() -> tuple[float, float] | None:
    if resource is None:
        return None
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime, usage.ru_stime


def slug(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")


def run_phase(name: str, command: list[str], cwd: Path) -> PhaseResult:
    print(f"\n{'=' * 72}\n{name}\n$ {' '.join(command)}\n{'=' * 72}", flush=True)
    log_path = LOG_ROOT / f"{len(results) + 1:02d}-{slug(name)}.log"
    usage_before = child_usage()
    started_at = time.perf_counter()
    with log_path.open("w", encoding="utf-8") as log_file:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env={**os.environ, "CARGO_TERM_COLOR": "always"},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert process.stdout is not None
        try:
            for line in process.stdout:
                print(line, end="", flush=True)
                log_file.write(line)
        except KeyboardInterrupt:
            process.terminate()
            process.wait()
            raise
        exit_code = process.wait()
    wall_seconds = time.perf_counter() - started_at
    usage_after = child_usage()
    if usage_before is None or usage_after is None:
        user_seconds = None
        system_seconds = None
    else:
        user_seconds = usage_after[0] - usage_before[0]
        system_seconds = usage_after[1] - usage_before[1]
    result = PhaseResult(
        name=name,
        command=command,
        cwd=str(cwd.relative_to(REPOSITORY_ROOT) or Path(".")),
        wall_seconds=wall_seconds,
        child_user_seconds=user_seconds,
        child_system_seconds=system_seconds,
        exit_code=exit_code,
        log=str(log_path.relative_to(REPORT_ROOT)),
    )
    results.append(result)
    print(f"[{name}] {wall_seconds:.2f}s, exit {exit_code}", flush=True)
    write_reports()
    if exit_code != 0:
        raise RuntimeError(f"{name} failed with exit code {exit_code}")
    return result


def cargo_timing_data() -> dict[str, Any] | None:
    if not CARGO_TIMING_SOURCE.is_file():
        return None
    source = CARGO_TIMING_SOURCE.read_text(encoding="utf-8")
    duration_match = re.search(r"^DURATION = ([0-9.]+);$", source, re.MULTILINE)
    units_match = re.search(
        r"const UNIT_DATA = (\[.*?\]);\nconst CONCURRENCY_DATA", source, re.DOTALL
    )
    if duration_match is None or units_match is None:
        raise RuntimeError("Cargo timing report format was not recognized")
    units = json.loads(units_match.group(1))
    dirty_units = [unit for unit in units if float(unit["duration"]) > 0]
    slowest_units = sorted(dirty_units, key=lambda unit: float(unit["duration"]), reverse=True)
    crates: dict[tuple[str, str], dict[str, Any]] = {}
    for unit in dirty_units:
        key = (unit["name"], unit["version"])
        crate = crates.setdefault(
            key,
            {
                "name": unit["name"],
                "version": unit["version"],
                "unit_count": 0,
                "total_unit_seconds": 0.0,
                "slowest_unit_seconds": 0.0,
                "codegen_seconds": 0.0,
            },
        )
        unit_duration = float(unit["duration"])
        crate["unit_count"] += 1
        crate["total_unit_seconds"] += unit_duration
        crate["slowest_unit_seconds"] = max(crate["slowest_unit_seconds"], unit_duration)
        for section_name, section in unit.get("sections") or []:
            if section_name == "codegen":
                crate["codegen_seconds"] += float(section["end"]) - float(section["start"])
    slowest_crates = sorted(
        crates.values(), key=lambda crate: crate["total_unit_seconds"], reverse=True
    )
    return {
        "wall_seconds": float(duration_match.group(1)),
        "total_units": len(units),
        "dirty_units": len(dirty_units),
        "fresh_units": len(units) - len(dirty_units),
        "slowest_crates": slowest_crates[:30],
        "slowest_units": slowest_units[:30],
    }


def nextest_timing_data() -> dict[str, Any] | None:
    rust_tests = next((result for result in results if result.name == "Rust tests"), None)
    if rust_tests is None:
        return None
    log_path = REPORT_ROOT / rust_tests.log
    if not log_path.is_file():
        return None
    tests: list[dict[str, Any]] = []
    pattern = re.compile(r"^\s*(?:PASS|FAIL|SLOW|LEAK|TIMEOUT)\s+\[\s*([0-9.]+)s\]\s+(.+?)\s*$")
    for line in log_path.read_text(encoding="utf-8").splitlines():
        match = pattern.match(ANSI_ESCAPE.sub("", line))
        if match is None:
            continue
        test_name = re.sub(r"^\(\s*\d+/\d+\)\s+", "", match.group(2))
        tests.append({"name": test_name, "duration_seconds": float(match.group(1))})
    tests.sort(key=lambda test: test["duration_seconds"], reverse=True)
    return {
        "threshold_seconds": SLOW_TEST_THRESHOLD_SECONDS,
        "measured_tests": len(tests),
        "slow_tests": [
            test for test in tests if test["duration_seconds"] >= SLOW_TEST_THRESHOLD_SECONDS
        ],
        "slowest_tests": tests[:30],
    }


def format_seconds(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.2f}s"


def markdown_report(
    status: str,
    cargo_timing: dict[str, Any] | None,
    nextest_timing: dict[str, Any] | None,
) -> str:
    total_seconds = sum(result.wall_seconds for result in results)
    lines = [
        "# KeepPeek Check Profile",
        "",
        f"- Status: **{status}**",
        f"- Generated: `{datetime.now(timezone.utc).isoformat()}`",
        f"- Platform: `{platform.platform()}`",
        f"- Python: `{platform.python_version()}`",
        f"- Total measured phase time: **{total_seconds:.2f}s**",
        "",
        "## Phases",
        "",
        "| Rank | Phase | Wall | Child user | Child system | Exit | Log |",
        "| ---: | --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for rank, result in enumerate(
        sorted(results, key=lambda item: item.wall_seconds, reverse=True), start=1
    ):
        lines.append(
            f"| {rank} | {result.name} | {format_seconds(result.wall_seconds)} | "
            f"{format_seconds(result.child_user_seconds)} | "
            f"{format_seconds(result.child_system_seconds)} | {result.exit_code} | "
            f"[{result.log}]({result.log}) |"
        )
    lines.extend(["", "## Rust Build", ""])
    if cargo_timing is None:
        lines.append("Cargo did not produce a parseable timing report.")
    else:
        lines.extend(
            [
                f"- Cargo build graph wall time: **{cargo_timing['wall_seconds']:.2f}s**",
                f"- Dirty units: **{cargo_timing['dirty_units']}**",
                f"- Fresh units: **{cargo_timing['fresh_units']}**",
                f"- Raw Cargo report: [cargo-build-timings.html](cargo-build-timings.html)",
                "",
                "Unit seconds are additive CPU/build work and can exceed wall time because Cargo runs units in parallel.",
                "",
                "### Slowest Crates",
                "",
                "| Rank | Crate | Units | Total unit time | Slowest unit | Codegen |",
                "| ---: | --- | ---: | ---: | ---: | ---: |",
            ]
        )
        for rank, crate in enumerate(cargo_timing["slowest_crates"], start=1):
            lines.append(
                f"| {rank} | `{crate['name']} {crate['version']}` | {crate['unit_count']} | "
                f"{crate['total_unit_seconds']:.2f}s | {crate['slowest_unit_seconds']:.2f}s | "
                f"{crate['codegen_seconds']:.2f}s |"
            )
        lines.extend(
            [
                "",
                "### Slowest Build Units",
                "",
                "| Rank | Unit | Target | Mode | Start | Duration |",
                "| ---: | --- | --- | --- | ---: | ---: |",
            ]
        )
        for rank, unit in enumerate(cargo_timing["slowest_units"], start=1):
            target = unit["target"] or "lib"
            lines.append(
                f"| {rank} | `{unit['name']} {unit['version']}` | `{target}` | "
                f"`{unit['mode']}` | {float(unit['start']):.2f}s | "
                f"{float(unit['duration']):.2f}s |"
            )
    lines.extend(["", "## Slow Rust Tests", ""])
    if nextest_timing is None:
        lines.append("Nextest has not produced per-test timing data yet.")
    else:
        lines.extend(
            [
                f"- Slow threshold: **{nextest_timing['threshold_seconds']:.2f}s**",
                f"- Measured tests: **{nextest_timing['measured_tests']}**",
                f"- Tests at or above threshold: **{len(nextest_timing['slow_tests'])}**",
                "",
                "| Rank | Test | Duration | Slow |",
                "| ---: | --- | ---: | :---: |",
            ]
        )
        slow_names = {test["name"] for test in nextest_timing["slow_tests"]}
        for rank, test in enumerate(nextest_timing["slowest_tests"], start=1):
            lines.append(
                f"| {rank} | `{test['name']}` | {test['duration_seconds']:.3f}s | "
                f"{'yes' if test['name'] in slow_names else 'no'} |"
            )
    return "\n".join(lines) + "\n"


def write_reports(status: str = "running") -> None:
    cargo_timing = cargo_timing_data()
    nextest_timing = nextest_timing_data()
    if CARGO_TIMING_SOURCE.is_file():
        shutil.copy2(CARGO_TIMING_SOURCE, CARGO_TIMING_REPORT)
    payload = {
        "schemaVersion": 1,
        "status": status,
        "generatedAt": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "pythonVersion": platform.python_version(),
        "phases": [asdict(result) for result in results],
        "cargoBuild": cargo_timing,
        "rustTests": nextest_timing,
    }
    (REPORT_ROOT / "report.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    (REPORT_ROOT / "report.md").write_text(
        markdown_report(status, cargo_timing, nextest_timing), encoding="utf-8"
    )


def phase_commands() -> list[tuple[str, list[str], Path]]:
    macos_feature = ["--features", "macos-test-aws-crypto"] if sys.platform == "darwin" else []
    nextest = ["cargo", "nextest", "run", "--locked", "--all", *macos_feature]
    return [
        ("Rust build", ["cargo", "build", "--locked", "--all", "--timings"], REPOSITORY_ROOT),
        ("Rust tests", nextest, REPOSITORY_ROOT),
        (
            "Rust Clippy",
            [
                "cargo",
                "clippy",
                "--locked",
                "--all",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            REPOSITORY_ROOT,
        ),
        ("Unused Rust dependencies", ["cargo", "machete"], REPOSITORY_ROOT),
        ("Rust formatting", ["cargo", "fmt", "--all", "--", "--check"], REPOSITORY_ROOT),
        ("TOML formatting", ["bunx", "@taplo/cli", "fmt", "--check"], REPOSITORY_ROOT),
        (
            "Python formatting",
            [
                sys.executable,
                "-m",
                "black",
                "--check",
                "--config",
                "examples/object_detection_service/pyproject.toml",
                ".",
            ],
            REPOSITORY_ROOT,
        ),
        (
            "Markdown formatting",
            [
                "bunx",
                "prettier",
                "--check",
                "**/*.md",
                "!ui/**/*.md",
                "!data/sample-videos/**/*.md",
            ],
            REPOSITORY_ROOT,
        ),
        ("UI registry policy", ["bun", "run", "registry:check"], UI_ROOT),
        ("Paper typecheck", ["bun", "run", "paper:typecheck"], UI_ROOT),
        ("Paper contract", ["bun", "run", "paper:check"], UI_ROOT),
        ("Visual harness contract", ["bun", "run", "visual:harness:check"], UI_ROOT),
        ("Demo typecheck", ["bun", "run", "demo:typecheck"], UI_ROOT),
        ("Demo contract", ["bun", "run", "demo:check"], UI_ROOT),
        ("UI formatting", ["bun", "run", "format:check"], UI_ROOT),
        ("UI lint", ["bun", "run", "lint"], UI_ROOT),
        ("Svelte check", ["bun", "run", "check"], UI_ROOT),
        ("Playwright typecheck", ["bun", "run", "test:e2e:typecheck"], UI_ROOT),
        ("UI unit tests", ["bun", "run", "test:unit:check"], UI_ROOT),
        ("Release E2E binaries", ["bun", "run", "test:e2e:prepare"], UI_ROOT),
        ("Playwright E2E", ["bunx", "playwright", "test"], UI_ROOT),
    ]


results: list[PhaseResult] = []


def main() -> int:
    for command, installation in [
        ("bun", "https://bun.sh/"),
        ("cargo", "https://rustup.rs/"),
        ("cargo-machete", "cargo install cargo-machete"),
        ("cargo-nextest", "cargo install cargo-nextest"),
    ]:
        require_command(command, installation)
    if importlib.util.find_spec("black") is None:
        raise RuntimeError(
            "Black is required: python -m pip install -r "
            "examples/object_detection_service/requirements.txt"
        )
    REPORT_ROOT.mkdir(parents=True, exist_ok=True)
    LOG_ROOT.mkdir(parents=True, exist_ok=True)
    try:
        for name, command, cwd in phase_commands():
            run_phase(name, command, cwd)
    except (KeyboardInterrupt, RuntimeError) as error:
        print(f"\nCheck failed: {error}", file=sys.stderr)
        write_reports("failed")
        return 1
    write_reports("passed")
    print(f"\nProfile report: {REPORT_ROOT / 'report.md'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
