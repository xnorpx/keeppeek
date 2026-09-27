"""Summarize raw pre-roll qualification runs without concealing failed budgets."""

import json
import random
import statistics
import sys
from pathlib import Path


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))]


def difference(baseline: list[float], result: list[float]) -> dict:
    before = statistics.median(baseline)
    after = statistics.median(result)
    if before == 0:
        return {
            "baseline_us": before,
            "result_us": after,
            "delta_percent": None,
            "budget_pass": False,
            "reason": "timer resolution cannot qualify zero baseline",
        }
    randomizer = random.Random(172)
    deltas = []
    for _ in range(5000):
        left = statistics.median(randomizer.choices(baseline, k=len(baseline)))
        right = statistics.median(randomizer.choices(result, k=len(result)))
        if left > 0:
            deltas.append((right / left - 1) * 100)
    low, high = percentile(deltas, 0.025), percentile(deltas, 0.975)
    delta = (after / before - 1) * 100
    return {
        "baseline_us": before,
        "result_us": after,
        "delta_percent": delta,
        "baseline_spread_us": spread(baseline),
        "result_spread_us": spread(result),
        "bootstrap95_delta_percent": [low, high],
        "budget_percent": 5,
        "budget_pass": delta <= 5,
        "disabled_indistinguishable": low <= 0 <= high,
    }


def spread(values: list[float]) -> dict:
    return {
        "minimum": min(values),
        "p25": percentile(values, 0.25),
        "p75": percentile(values, 0.75),
        "maximum": max(values),
    }


def indexed(path: str) -> dict:
    report = json.loads(Path(path).read_text(encoding="utf-8-sig"))
    return {(item["fixture"], item["camera_count"]): item for item in report["workloads"]}


def main() -> None:
    if len(sys.argv) != 5:
        raise SystemExit("compare.py BASELINE.json DISABLED.json ENABLED.json OUTPUT.json")
    baseline, disabled, enabled = (indexed(path) for path in sys.argv[1:4])
    expected = {
        (f"{height}p-{codec}-gop{gop}.mp4", cameras)
        for height in (1080, 2160)
        for codec in ("h264", "h265")
        for gop in (1, 10)
        for cameras in (1, 127)
    }
    if set(baseline) != expected or set(disabled) != expected or set(enabled) != expected:
        raise SystemExit("Qualification requires the complete matching workload set")
    summaries = []
    for key, reference in baseline.items():
        for mode, candidate in (("disabled", disabled[key]), ("enabled", enabled[key])):
            if candidate["fixture_sha256"] != reference["fixture_sha256"]:
                raise SystemExit(f"Fixture mismatch: {key}")
            if len(candidate["runs"]) < 30 or len(reference["runs"]) < 30:
                raise SystemExit("Qualification requires at least 30 measured runs")
            frame_counts = {run["frames"] for run in reference["runs"]}
            if len(frame_counts) != 1 or frame_counts != {
                run["frames"] for run in candidate["runs"]
            }:
                raise SystemExit(f"Frame count mismatch: {key}")
            metrics = {}
            for metric in ("median_ingest_us", "p95_ingest_us"):
                metrics[metric] = difference(
                    [run[metric] for run in reference["runs"]],
                    [run[metric] for run in candidate["runs"]],
                )
            summaries.append(
                {
                    "fixture": key[0],
                    "cameras": key[1],
                    "mode": mode,
                    "runs": len(candidate["runs"]),
                    "metrics": metrics,
                    "peak_history_bytes": max(
                        run["peak_history_bytes"] for run in candidate["runs"]
                    ),
                    "peak_stream_bytes": max(run["peak_stream_bytes"] for run in candidate["runs"]),
                    "history_instances": max(run["history_instances"] for run in candidate["runs"]),
                    "maximum_pending_channel_bytes": max(
                        run["pending_channel_bytes"] for run in candidate["runs"]
                    ),
                }
            )
    Path(sys.argv[4]).write_text(json.dumps(summaries, indent=2) + "\n", encoding="utf-8")
    for row in summaries:
        metrics = row["metrics"]
        print(
            row["fixture"],
            row["cameras"],
            row["mode"],
            "median delta",
            metrics["median_ingest_us"]["delta_percent"],
            "p95 delta",
            metrics["p95_ingest_us"]["delta_percent"],
        )


if __name__ == "__main__":
    main()
