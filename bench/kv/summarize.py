#!/usr/bin/env python3
"""Summarize diagnostic round samples without pooling correlated operations."""

import argparse
import hashlib
import json
import statistics
from pathlib import Path


def byte_count(value):
    value = value.strip()
    for unit, multiplier in [("GiB", 2**30), ("MiB", 2**20), ("KiB", 2**10),
                             ("GB", 10**9), ("MB", 10**6), ("kB", 10**3), ("B", 1)]:
        if value.endswith(unit):
            return float(value[:-len(unit)]) * multiplier
    raise ValueError(f"unknown byte unit: {value}")


def summarize_case(case, events, server, client):
    result = {"engine": case["engine"], "operation": case["operation"], "status": case["status"],
              "profile": case["config"].get("profile", "default"),
              "keys": case["config"]["keys"], "workers": case["config"]["workers"],
              "pipeline": case["config"]["pipeline"], "fence_writes": case["config"]["fence_writes"]}
    if case["status"] != "valid-diagnostic" or not case["rounds"]:
        result["reason"] = case.get("reason", case.get("error"))
        return result
    samples = case["rounds"]
    if len(samples) != case["config"]["rounds"]:
        raise ValueError("incomplete round set cannot become a successful summary")
    for key in ("throughput_ops_s", "batch_p50_us", "batch_p95_us", "batch_p99_us"):
        result[f"median_{key}"] = statistics.median(sample[key] for sample in samples)
        result[f"range_{key}"] = [min(sample[key] for sample in samples), max(sample[key] for sample in samples)]
    relevant = [event for event in events if any(sample["started_unix_ms"] <= event["unix_ms"]
                <= sample["finished_unix_ms"] for sample in samples)]
    for name, container in [("server", server), ("client", client)]:
        observations = [event for event in relevant if event["Name"] == container]
        result[f"{name}_resource_samples"] = len(observations)
        cpus = [float(event["CPUPerc"].rstrip("%")) for event in observations]
        # Windows Docker stats can expose zero-only samples just after restart.
        # Preserve raw samples, but do not present a busy request stream as 0% CPU.
        zero_only = bool(cpus) and not any(cpus) and sum(sample["operations"] for sample in samples) >= 100
        result[f"{name}_cpu_zero_only_samples"] = zero_only
        result[f"{name}_median_cpu_percent"] = statistics.median(cpus) if cpus and not zero_only else None
        result[f"{name}_peak_working_set_mib"] = (max(
            byte_count(event["MemUsage"].split("/")[0]) for event in observations) / 2**20
            if observations else None)
        result[f"{name}_io"] = [{key: event[key] for key in ("unix_ms", "BlockIO", "NetIO")}
                                 for event in observations]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    directory = args.directory
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest["status"] == "running":
        raise ValueError("wait for the matrix to finish")
    # A supplementary sampler repaired the initial Windows CLI ANSI parsing
    # issue in the first capture. Prefer its one consistent sample stream.
    paths = list(directory.glob("resources-*.jsonl"))
    events = [json.loads(line) for path in paths for line in path.read_text().splitlines() if line]
    measurements = []
    artifacts = {}
    for path in sorted(directory.glob("*.json")):
        case = json.loads(path.read_text())
        if case.get("schema") != "reddb.kv-compare.v1":
            continue
        artifacts[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        if case["rounds"] or case["status"] in ("invalid", "unsupported"):
            server = case["config"]["host"]
            clients = [name for name in manifest["containers"] if "client" in name]
            summary = summarize_case(case, events, server, clients[0])
            summary["file"] = path.name
            measurements.append(summary)
    for path in paths:
        artifacts[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    summary = {"schema": "reddb.kv-comparison-summary.v1", "status": manifest["status"],
               "aggregation": "median and min/max of round-level metrics; no operation-level pooling",
               "measurements": measurements, "artifact_sha256": artifacts}
    (directory / "summary.json").write_bytes((json.dumps(summary, indent=2) + "\n").encode())

    def number(value):
        return "unavailable" if value is None else f"{value:,.1f}"

    lines = ["# Local KV comparison — verified product paths", "",
             f"Engine revision: `{manifest['red_engine_revision']}`. Classification: **shared-host diagnostic**.", "",
             "The table contains medians of five round-level metrics, not pooled request percentiles. "
             "All raw round samples and their ranges remain in the JSON. CPU percentages use 100% per core; "
             "memory is sampled cgroup working set, not per-key memory or RSS. Missing telemetry is unavailable.", ""]
    for title, operation, pipeline in [("GET without pipelining", "get", 1),
                                       ("RESP GET, pipeline 16 — batch completion latency", "get", 16),
                                       ("Durable overwrites — KiviDB includes SET + WAITAOF", "put", 1)]:
        lines += [f"## {title}", "",
                  "| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |",
                  "| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |"]
        selected = sorted((value for value in measurements if value["operation"] == operation
                           and value["pipeline"] == pipeline),
                          key=lambda value: (value["keys"], value["workers"], value["engine"]))
        for value in selected:
            label = value["engine"] + (f" ({value['profile']})" if value["profile"] != "default" else "")
            if value["status"] != "valid-diagnostic":
                lines.append(f"| {value['keys']} | {value['workers']} | {label} | "
                             f"{value['status']} | — | — | — | — | — |")
                continue
            fields = [str(value["keys"]), str(value["workers"]), label,
                      number(value["median_throughput_ops_s"]), number(value["median_batch_p50_us"]),
                      number(value["median_batch_p95_us"]), number(value["median_batch_p99_us"]),
                      f"{number(value['server_median_cpu_percent'])} / {number(value['client_median_cpu_percent'])}",
                      number(value["server_peak_working_set_mib"])]
            lines.append("| " + " | ".join(fields) + " |")
        lines.append("")
    lines += ["## Qualification", "",
              "HTTP/JSON and RESP are different product paths. Python threads and the two-core client budget "
              "can limit throughput. Pipeline-16 latency refers to a complete batch; it is not directly "
              "comparable with a single-request percentile. Server order is fixed and the host is shared. "
              "These measurements cannot establish maximum engine throughput or ADR 0076 leadership.", "",
              "See [the experiment contract](../../kv/COMPARE.md), `manifest.json`, `summary.json`, "
              "the raw cell JSON and resource JSONL files for setup, unsupported cells, failures and recovery checks.", ""]
    (directory / "SUMMARY.md").write_bytes("\n".join(lines).encode())


if __name__ == "__main__":
    main()
