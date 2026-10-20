#!/usr/bin/env python3
"""Run read_paths.py on pre-created Docker servers in before/after/after/before order."""

import argparse
import hashlib
import json
import platform
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "kv"))
from local_matrix import Resources, command, inspect


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", default="reddb-query-client-20261020")
    parser.add_argument("--before", default="reddb-query-before-20261020")
    parser.add_argument("--after", default="reddb-query-after-20261020")
    parser.add_argument("--before-revision", required=True)
    parser.add_argument("--after-revision", required=True)
    parser.add_argument("--before-sha256", required=True)
    parser.add_argument("--after-sha256", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    manifest = {"schema": "reddb.query-projection-matrix.v1", "status": "running",
                "started_unix_ms": int(time.time() * 1000), "host": platform.platform(),
                "classification": "shared-host diagnostic; two process runs/version; no leadership claim",
                "primary_metric": "p99 checked HTTP request completion latency",
                "conditions": {"keys": 10000, "value_bytes": 100, "rounds": 5, "seconds_per_round": 2,
                               "warmup_per_connection": 10, "read_order": ["before-1", "after-1", "after-2", "before-2"],
                               "durability": "strict", "result_cache": "disabled",
                               "dataset": "copies of stopped verified kv_diag_10000 volume; SQL table created separately with identical rows",
                               "limits": "Python/GIL, HTTP JSON, shared host; independent run count too small for a leadership interval"},
                "engines": {"before": {"revision": args.before_revision, "sha256": args.before_sha256},
                            "after": {"revision": args.after_revision, "sha256": args.after_sha256}},
                "containers": {name: inspect(name) for name in [args.before, args.after, args.client]},
                "script_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                  for path in Path(__file__).parent.glob("*.py")},
                "runs": [], "errors": []}

    def save():
        (args.out / "manifest.json").write_bytes((json.dumps(manifest, indent=2) + "\n").encode())

    def ready(server):
        code = ("import sys;sys.path.insert(0,'/bench/kv');from compare import RedClient;"
                f"c=RedClient('{server}',5000,'probe');print(c.request('GET','/health/live'));c.close()")
        for _ in range(30):
            try:
                return command("docker", "exec", args.client, "python", "-c", code, timeout=35)
            except RuntimeError:
                time.sleep(1)
        raise RuntimeError(f"{server} is not ready")

    def run(server, relative, *extra):
        output = command("docker", "exec", args.client, "python", "/bench/query/read_paths.py",
                         "--host", server, "--out", "/results/" + relative, *extra, timeout=600)
        path = args.out / relative
        path.with_suffix(".stdout.log").write_text(output + "\n")
        value = json.loads(path.read_text())
        print(relative, value["status"], value.get("median_queries_s", ""), flush=True)
        return value

    save()
    try:
        command("docker", "start", args.client)
        for server in [args.before, args.after]:
            command("docker", "start", server)
            ready(server)
            run(server, ("before" if server == args.before else "after") + "-setup.json", "--setup")
            command("docker", "stop", server)
        for label, server in [("before-1", args.before), ("after-1", args.after),
                              ("after-2", args.after), ("before-2", args.before)]:
            directory = args.out / label
            directory.mkdir()
            phase = {"label": label, "started_unix_ms": int(time.time() * 1000), "cases": []}
            manifest["runs"].append(phase)
            command("docker", "start", server)
            ready(server)
            resources = Resources(directory, server, args.client)
            try:
                for case in ["kv", "sql-point", "sql-100", "sql-1000"]:
                    for workers in ([1, 8, 32] if case in ["kv", "sql-point"] else [1]):
                        relative = f"{label}/{case}-c{workers}.json"
                        try:
                            value = run(server, relative, "--case", case, "--workers", str(workers))
                            phase["cases"].append({"file": relative, "status": value["status"]})
                        except Exception as error:
                            manifest["errors"].append({"case": relative, "error": str(error)})
                        save()
                command("docker", "kill", server)
                command("docker", "start", server)
                ready(server)
                run(server, f"{label}/sql-recovery.json", "--verify-only")
                command("docker", "exec", args.client, "python", "/bench/kv/compare.py", "--host", server,
                        "--engine", "reddb-rest", "--collection", "kv_diag_10000", "--keys", "10000",
                        "--verify-only", "--out", f"/results/{label}/kv-recovery.json", timeout=180)
            finally:
                resources.close()
                command("docker", "stop", server)
                phase["finished_unix_ms"] = int(time.time() * 1000)
                save()
        manifest["status"] = "valid-diagnostic" if not manifest["errors"] else "completed-with-invalid-cells"
    except Exception as error:
        manifest["errors"].append({"error": str(error)})
        manifest["status"] = "completed-with-invalid-cells"
        raise
    finally:
        for server in [args.before, args.after, args.client]:
            command("docker", "stop", server)
        manifest["finished_unix_ms"] = int(time.time() * 1000)
        save()


if __name__ == "__main__":
    main()
