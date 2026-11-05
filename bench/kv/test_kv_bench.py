import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


run = load_module("kv_bench_run", "run.py")


def sample(reddb_p99, redis_p99, reddb_throughput, redis_throughput, received=10):
    workloads = {}
    redis_workloads = {}
    for workload in ("put", "get", "incr"):
        workloads[workload] = {
            "ops": 10,
            "p50_us": reddb_p99 / 2,
            "p99_us": reddb_p99,
            "throughput_ops_s": reddb_throughput,
        }
        redis_workloads[workload] = {
            "ops": 10,
            "p50_us": redis_p99 / 2,
            "p99_us": redis_p99,
            "throughput_ops_s": redis_throughput,
        }
    return {
        "schema": "reddb.kv-bench.v1",
        "generated_at_unix_ms": 1,
        "config": {"ops": 10, "watch_events": 10},
        "reddb": workloads,
        "redis": redis_workloads,
        "watch_delivery_lag": {
            "events": 10,
            "received": received,
            "p50_ms": 1.0,
            "p99_ms": 2.0,
            "drops": 0,
        },
    }


class RunTests(unittest.TestCase):
    def test_main_runs_watch_before_workloads_and_writes_result(self):
        calls = []
        watch = {
            "events": 1,
            "received": 1,
            "p50_ms": 1.0,
            "p99_ms": 1.0,
            "drops": 0,
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            with (
                mock.patch.object(run, "bench_watch", side_effect=lambda: calls.append("watch") or watch),
                mock.patch.object(run, "bench_reddb", side_effect=lambda: calls.append("reddb") or {}),
                mock.patch.object(run, "bench_redis", side_effect=lambda: calls.append("redis") or {}),
                mock.patch.object(sys, "argv", ["run.py", str(output)]),
            ):
                run.main()

            self.assertEqual(calls, ["watch", "reddb", "redis"])
            self.assertEqual(json.loads(output.read_text())["watch_delivery_lag"], watch)

    def test_main_writes_result_then_rejects_incomplete_watch(self):
        watch = {
            "events": 2,
            "received": 0,
            "p50_ms": 0.0,
            "p99_ms": 0.0,
            "drops": 0,
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            with (
                mock.patch.object(run, "bench_watch", return_value=watch),
                mock.patch.object(run, "bench_reddb", return_value={}),
                mock.patch.object(run, "bench_redis", return_value={}),
                mock.patch.object(sys, "argv", ["run.py", str(output)]),
            ):
                with self.assertRaisesRegex(RuntimeError, "received 0 of 2"):
                    run.main()

            self.assertTrue(output.exists())

    def test_main_accepts_multiple_events_for_one_versioned_update(self):
        watch = {
            "events": 1,
            "received": 2,
            "p50_ms": 1.0,
            "p99_ms": 1.0,
            "drops": 0,
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            with (
                mock.patch.object(run, "bench_watch", return_value=watch),
                mock.patch.object(run, "bench_reddb", return_value={}),
                mock.patch.object(run, "bench_redis", return_value={}),
                mock.patch.object(sys, "argv", ["run.py", str(output)]),
            ):
                run.main()

            self.assertTrue(output.exists())


class AggregateTests(unittest.TestCase):
    def test_aggregate_reports_medians_variance_and_normalized_ratios(self):
        aggregate = load_module("kv_bench_aggregate", "aggregate.py")
        inputs = [
            sample(100.0, 10.0, 1000.0, 2000.0),
            sample(300.0, 30.0, 800.0, 2000.0),
            sample(200.0, 20.0, 900.0, 2000.0),
        ]

        result = aggregate.aggregate_samples(inputs, ["one.json", "two.json", "three.json"])

        self.assertEqual(result["sample_count"], 3)
        self.assertEqual(result["reddb"]["get"]["p99_us"], 200.0)
        self.assertGreater(result["reddb"]["get"]["variation"]["p99_us_cv"], 0)
        self.assertEqual(result["normalized"]["get"]["p99_ratio"], 10.0)
        self.assertEqual(result["normalized"]["get"]["throughput_ratio"], 0.45)
        self.assertEqual(result["watch_delivery_lag"]["complete_samples"], 3)
        self.assertEqual(result["watch_delivery_lag"]["received_total"], 30)


class RegressionGateTests(unittest.TestCase):
    def test_gate_rejects_incomplete_watch(self):
        aggregate = load_module("kv_bench_aggregate_for_gate", "aggregate.py")
        gate = load_module("kv_bench_gate", "check-regression.py")
        baseline = aggregate.aggregate_samples(
            [sample(100.0, 10.0, 1000.0, 2000.0) for _ in range(3)],
            ["baseline-1.json", "baseline-2.json", "baseline-3.json"],
        )
        current_samples = [sample(100.0, 10.0, 1000.0, 2000.0) for _ in range(3)]
        current_samples[1]["watch_delivery_lag"]["received"] = 0
        current = aggregate.aggregate_samples(
            current_samples,
            ["current-1.json", "current-2.json", "current-3.json"],
        )

        failures = gate.regression_failures(baseline, current)

        self.assertTrue(any("WATCH completed" in failure for failure in failures))

    def test_gate_rejects_normalized_p99_regression(self):
        aggregate = load_module("kv_bench_aggregate_for_regression", "aggregate.py")
        gate = load_module("kv_bench_gate_regression", "check-regression.py")
        baseline = aggregate.aggregate_samples(
            [sample(100.0, 10.0, 1000.0, 2000.0) for _ in range(3)],
            ["baseline-1.json", "baseline-2.json", "baseline-3.json"],
        )
        current = aggregate.aggregate_samples(
            [sample(130.0, 10.0, 1000.0, 2000.0) for _ in range(3)],
            ["current-1.json", "current-2.json", "current-3.json"],
        )

        failures = gate.regression_failures(baseline, current)

        self.assertTrue(any("normalized p99 ratio" in failure for failure in failures))


if __name__ == "__main__":
    unittest.main()
