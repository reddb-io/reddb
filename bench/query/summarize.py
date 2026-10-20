#!/usr/bin/env python3
"""Keep process-run samples separate and retain failed orchestration evidence."""

import argparse
import collections
import hashlib
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "kv"))
from summarize import byte_count


def summarize(directory):
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest["status"] == "running":
        raise ValueError("capture is still active")
    client = next(name for name in manifest["containers"] if "client" in name)
    groups = collections.defaultdict(lambda: collections.defaultdict(list))
    cells = []
    for run in manifest["runs"]:
        events = [json.loads(line) for path in (directory / run["label"]).rglob("resources-*.jsonl")
                  for line in path.read_text().splitlines() if line]
        for reference in run["cases"]:
            case = json.loads((directory / reference["file"]).read_text())
            cell = {"file": reference["file"], "run": run["label"], "status": case["status"],
                    "case": case["config"]["case"], "workers": case["config"]["workers"]}
            cells.append(cell)
            if case["status"] != "valid-diagnostic":
                continue
            if len(case["rounds"]) != case["config"]["rounds"]:
                raise ValueError("partial rounds cannot count as a successful cell")
            for key in ["queries_s", "p50_us", "p95_us", "p99_us"]:
                values = [round_[key] for round_ in case["rounds"]]
                cell["median_" + key] = statistics.median(values)
                cell["range_" + key] = [min(values), max(values)]
            for label, container in [("server", case["config"]["host"]), ("client", client)]:
                samples = [event for event in events if event["Name"] == container
                           and case["started_unix_ms"] <= event["unix_ms"] <= case["finished_unix_ms"]]
                cpu = [float(event["CPUPerc"].rstrip("%")) for event in samples]
                cell[label + "_resource_samples"] = len(samples)
                cell[label + "_median_cpu_percent"] = statistics.median(cpu) if cpu and any(cpu) else None
                cell[label + "_peak_working_set_mib"] = max(
                    (byte_count(event["MemUsage"].split("/")[0]) / 2**20 for event in samples), default=None)
                cell[label + "_io_endpoints"] = [{key: event[key] for key in ["unix_ms", "NetIO", "BlockIO"]}
                                                for event in (samples[:1] + samples[-1:])]
            groups[(cell["case"], cell["workers"])][run["label"].split("-")[0]].append(cell)
    comparisons = []
    for (case, workers), versions in sorted(groups.items()):
        if any(len(versions[version]) != 2 for version in ["before", "after"]):
            raise ValueError("each comparison needs both process runs per version")
        comparison = {"case": case, "workers": workers}
        for version in ["before", "after"]:
            comparison[version] = {}
            for key in ["queries_s", "p50_us", "p95_us", "p99_us"]:
                values = [cell["median_" + key] for cell in versions[version]]
                comparison[version][key] = {"process_run_medians": values, "median": statistics.median(values),
                                           "range": [min(values), max(values)]}
        comparison["throughput_ratio"] = comparison["after"]["queries_s"]["median"] / comparison["before"]["queries_s"]["median"]
        comparison["p99_ratio"] = comparison["after"]["p99_us"]["median"] / comparison["before"]["p99_us"]["median"]
        comparisons.append(comparison)
    checksums = {path.relative_to(directory).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                 for path in sorted(directory.rglob("*")) if path.is_file() and path.name != "summary.json"
                 and path.suffix in [".json", ".jsonl", ".trace", ".log", ".py"]
                 and not path.name.endswith((".stdout.log", ".stderr.log"))}
    return {"schema": "reddb.query-projection-summary.v1", "status": manifest["status"],
            "aggregation": "five-round medians per process, then median of two process samples/version; no pooling or confidence interval",
            "resource_scope": "whole cell including warmups and validation; CPU 100% per core; memory cgroup working set; IO endpoints cumulative",
            "measurements": cells, "comparisons": comparisons, "artifact_sha256": checksums}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    result = summarize(args.directory)
    (args.directory / "summary.json").write_bytes((json.dumps(result, indent=2) + "\n").encode())
    for cell in result["comparisons"]:
        print(cell["case"], cell["workers"],
              "queries/s", round(cell["before"]["queries_s"]["median"], 2), round(cell["after"]["queries_s"]["median"], 2),
              "p99 ms", round(cell["before"]["p99_us"]["median"] / 1000, 3), round(cell["after"]["p99_us"]["median"] / 1000, 3))


if __name__ == "__main__":
    main()
