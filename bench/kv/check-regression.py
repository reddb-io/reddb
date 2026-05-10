#!/usr/bin/env python3
import json
import os
import sys


THRESHOLD = float(os.environ.get("KV_BENCH_P99_REGRESSION_THRESHOLD", "0.20"))
WATCH_TARGET_MS = float(os.environ.get("KV_BENCH_WATCH_P99_TARGET_MS", "10"))
WORKLOADS = ("put", "get", "incr")


def load(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: check-regression.py <baseline.json> <current.json>")
    baseline = load(sys.argv[1])
    current = load(sys.argv[2])
    failures = []

    for workload in WORKLOADS:
        base_p99 = float(baseline["reddb"][workload]["p99_us"])
        current_p99 = float(current["reddb"][workload]["p99_us"])
        allowed = base_p99 * (1.0 + THRESHOLD)
        if current_p99 > allowed:
            failures.append(
                f"reddb {workload} p99_us {current_p99:.3f} > allowed {allowed:.3f}"
            )

    watch_p99 = float(current["watch_delivery_lag"]["p99_ms"])
    if watch_p99 > WATCH_TARGET_MS:
        failures.append(
            f"watch_delivery_lag p99_ms {watch_p99:.3f} > target {WATCH_TARGET_MS:.3f}"
        )

    if failures:
        print("KV benchmark regression gate failed:")
        for failure in failures:
            print(f"- {failure}")
        return 1

    print("KV benchmark regression gate passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
