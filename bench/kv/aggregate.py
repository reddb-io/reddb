#!/usr/bin/env python3
import json
import statistics
import sys
from pathlib import Path


WORKLOADS = ("put", "get", "incr")
METRICS = ("p50_us", "p99_us", "throughput_ops_s")
MIN_SAMPLES = 3


def coefficient_of_variation(values):
    mean = statistics.fmean(values)
    if mean == 0:
        return 0.0
    return round(statistics.pstdev(values) / mean, 6)


def metric_summary(values):
    return {
        "median": round(statistics.median(values), 6),
        "min": round(min(values), 6),
        "max": round(max(values), 6),
        "cv": coefficient_of_variation(values),
    }


def validate_samples(samples):
    if len(samples) < MIN_SAMPLES:
        raise ValueError(f"expected at least {MIN_SAMPLES} samples, got {len(samples)}")
    expected_config = {
        "ops": samples[0]["config"]["ops"],
        "watch_events": samples[0]["config"]["watch_events"],
    }
    for index, sample in enumerate(samples, start=1):
        if sample.get("schema") != "reddb.kv-bench.v1":
            raise ValueError(f"sample {index} has unsupported schema: {sample.get('schema')}")
        config = {
            "ops": sample["config"]["ops"],
            "watch_events": sample["config"]["watch_events"],
        }
        if config != expected_config:
            raise ValueError(f"sample {index} config differs from the first sample")
    return expected_config


def aggregate_engine(samples, engine):
    result = {}
    for workload in WORKLOADS:
        operations = [int(sample[engine][workload]["ops"]) for sample in samples]
        if len(set(operations)) != 1:
            raise ValueError(f"{engine} {workload} operation counts differ between samples")
        workload_result = {"ops": operations[0], "variation": {}}
        for metric in METRICS:
            values = [float(sample[engine][workload][metric]) for sample in samples]
            summary = metric_summary(values)
            workload_result[metric] = summary["median"]
            workload_result["variation"][f"{metric}_min"] = summary["min"]
            workload_result["variation"][f"{metric}_max"] = summary["max"]
            workload_result["variation"][f"{metric}_cv"] = summary["cv"]
        result[workload] = workload_result
    return result


def aggregate_normalized(samples):
    result = {}
    for workload in WORKLOADS:
        p99_ratios = []
        throughput_ratios = []
        for sample in samples:
            redis_p99 = float(sample["redis"][workload]["p99_us"])
            redis_throughput = float(sample["redis"][workload]["throughput_ops_s"])
            if redis_p99 <= 0 or redis_throughput <= 0:
                raise ValueError(f"redis {workload} reference metrics must be positive")
            p99_ratios.append(float(sample["reddb"][workload]["p99_us"]) / redis_p99)
            throughput_ratios.append(
                float(sample["reddb"][workload]["throughput_ops_s"]) / redis_throughput
            )
        p99 = metric_summary(p99_ratios)
        throughput = metric_summary(throughput_ratios)
        result[workload] = {
            "p99_ratio": p99["median"],
            "p99_ratio_cv": p99["cv"],
            "throughput_ratio": throughput["median"],
            "throughput_ratio_cv": throughput["cv"],
        }
    return result


def aggregate_watch(samples):
    watches = [sample["watch_delivery_lag"] for sample in samples]
    p50 = metric_summary([float(watch["p50_ms"]) for watch in watches])
    p99 = metric_summary([float(watch["p99_ms"]) for watch in watches])
    return {
        "events_total": sum(int(watch["events"]) for watch in watches),
        "received_total": sum(int(watch["received"]) for watch in watches),
        "complete_samples": sum(
            int(watch["received"] >= watch["events"] and int(watch["events"]) > 0)
            for watch in watches
        ),
        "drops_total": sum(int(watch["drops"]) for watch in watches),
        "p50_ms": p50["median"],
        "p99_ms": p99["median"],
        "variation": {
            "p50_ms_min": p50["min"],
            "p50_ms_max": p50["max"],
            "p50_ms_cv": p50["cv"],
            "p99_ms_min": p99["min"],
            "p99_ms_max": p99["max"],
            "p99_ms_cv": p99["cv"],
        },
    }


def aggregate_samples(samples, source_files):
    config = validate_samples(samples)
    return {
        "schema": "reddb.kv-bench.aggregate.v1",
        "generated_at_unix_ms": max(int(sample["generated_at_unix_ms"]) for sample in samples),
        "sample_count": len(samples),
        "source_files": [Path(path).name for path in source_files],
        "config": config,
        "reddb": aggregate_engine(samples, "reddb"),
        "redis": aggregate_engine(samples, "redis"),
        "normalized": aggregate_normalized(samples),
        "watch_delivery_lag": aggregate_watch(samples),
    }


def main():
    if len(sys.argv) < MIN_SAMPLES + 2:
        raise SystemExit("usage: aggregate.py <output.json> <sample.json> <sample.json> <sample.json> [...]")
    output = sys.argv[1]
    source_files = sys.argv[2:]
    samples = []
    for path in source_files:
        with open(path, "r", encoding="utf-8") as file:
            samples.append(json.load(file))
    result = aggregate_samples(samples, source_files)
    with open(output, "w", encoding="utf-8") as file:
        json.dump(result, file, indent=2, sort_keys=True)
        file.write("\n")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, TypeError, ValueError) as error:
        raise SystemExit(f"KV benchmark aggregation failed: {error}")
