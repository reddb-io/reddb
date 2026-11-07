#!/usr/bin/env python3
import json
import os
import sys


THRESHOLD = float(os.environ.get("KV_BENCH_P99_REGRESSION_THRESHOLD", "0.20"))
THROUGHPUT_THRESHOLD = float(os.environ.get("KV_BENCH_THROUGHPUT_REGRESSION_THRESHOLD", "0.20"))
NORMALIZED_CV_MAX = float(os.environ.get("KV_BENCH_NORMALIZED_CV_MAX", "0.50"))
WATCH_TARGET_MS = float(os.environ.get("KV_BENCH_WATCH_P99_TARGET_MS", "10"))
MIN_SAMPLES = int(os.environ.get("KV_BENCH_MIN_SAMPLES", "3"))
WORKLOADS = ("put", "get", "incr")


def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def regression_failures(baseline, current):
    failures = []

    for name, result in (("baseline", baseline), ("current", current)):
        if result.get("schema") != "reddb.kv-bench.aggregate.v1":
            failures.append(f"{name} uses unsupported schema: {result.get('schema')}")
        if int(result.get("sample_count", 0)) < MIN_SAMPLES:
            failures.append(
                f"{name} has {result.get('sample_count', 0)} samples; minimum is {MIN_SAMPLES}"
            )

    if failures:
        return failures

    for workload in WORKLOADS:
        base_p99_ratio = float(baseline["normalized"][workload]["p99_ratio"])
        current_p99_ratio = float(current["normalized"][workload]["p99_ratio"])
        p99_ratio_allowed = base_p99_ratio * (1.0 + THRESHOLD)
        if current_p99_ratio > p99_ratio_allowed:
            failures.append(
                f"reddb {workload} normalized p99 ratio {current_p99_ratio:.3f} "
                f"> allowed {p99_ratio_allowed:.3f}"
            )

        base_throughput_ratio = float(
            baseline["normalized"][workload]["throughput_ratio"]
        )
        current_throughput_ratio = float(
            current["normalized"][workload]["throughput_ratio"]
        )
        throughput_ratio_min = base_throughput_ratio * (1.0 - THROUGHPUT_THRESHOLD)
        if current_throughput_ratio < throughput_ratio_min:
            failures.append(
                f"reddb {workload} normalized throughput ratio {current_throughput_ratio:.3f} "
                f"< allowed {throughput_ratio_min:.3f}"
            )

        for metric in ("p99_ratio", "throughput_ratio"):
            variation = float(current["normalized"][workload][f"{metric}_cv"])
            if variation > NORMALIZED_CV_MAX:
                failures.append(
                    f"reddb {workload} normalized {metric} CV {variation:.3f} "
                    f"> maximum {NORMALIZED_CV_MAX:.3f}"
                )

    watch = current["watch_delivery_lag"]
    complete_samples = int(watch["complete_samples"])
    sample_count = int(current["sample_count"])
    if complete_samples != sample_count:
        failures.append(f"WATCH completed {complete_samples} of {sample_count} samples")
    if int(watch["received_total"]) < int(watch["events_total"]):
        failures.append(
            f"WATCH received {watch['received_total']} events for "
            f"{watch['events_total']} requested writes"
        )
    if int(watch["drops_total"]) != 0:
        failures.append(f"WATCH dropped {watch['drops_total']} events")

    watch_p99 = float(watch["p99_ms"])
    if watch_p99 > WATCH_TARGET_MS:
        failures.append(
            f"watch_delivery_lag p99_ms {watch_p99:.3f} > target {WATCH_TARGET_MS:.3f}"
        )

    return failures


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: check-regression.py <baseline.json> <current.json>")
    baseline = load(sys.argv[1])
    current = load(sys.argv[2])
    failures = regression_failures(baseline, current)
    if failures:
        print("KV benchmark regression gate failed:")
        for failure in failures:
            print(f"- {failure}")
        return 1

    print("KV benchmark regression gate passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
