# SPDX-License-Identifier: AGPL-3.0-only

from conformance_client import (
    BENCHMARK_RUNS,
    COMMIT_LATENCY_P95_BUDGET_MS,
    FANOUT_LATENCY_P95_BUDGET_MS,
    MEMORY_SAMPLE_INTERVAL_SECONDS,
    MEMORY_SAMPLES_MAXIMUM,
    MEMORY_SAMPLES_MINIMUM,
    PROCESS_MEMORY_DELTA_P95_BUDGET_BYTES,
    QUEUE_DEPTH_BUDGET,
    QUEUE_PENDING_BYTES_BUDGET,
    nearest_rank_percentile_ms,
)


def test_conformance_performance_budgets_are_bounded() -> None:
    assert BENCHMARK_RUNS == 20
    assert COMMIT_LATENCY_P95_BUDGET_MS == 2_000
    assert FANOUT_LATENCY_P95_BUDGET_MS == 2_500
    assert QUEUE_DEPTH_BUDGET == 64
    assert QUEUE_PENDING_BYTES_BUDGET == 8 * 1024 * 1024 + 64 * 1024
    assert PROCESS_MEMORY_DELTA_P95_BUDGET_BYTES == 128 * 1024 * 1024
    assert MEMORY_SAMPLE_INTERVAL_SECONDS == 0.05
    assert MEMORY_SAMPLES_MINIMUM == 20
    assert MEMORY_SAMPLES_MAXIMUM == 64


def test_nearest_rank_percentile_reports_p50_and_p95_milliseconds() -> None:
    samples_ns = [value * 1_000_000 for value in range(1, BENCHMARK_RUNS + 1)]

    assert nearest_rank_percentile_ms(samples_ns, 50) == 10.0
    assert nearest_rank_percentile_ms(samples_ns, 95) == 19.0
