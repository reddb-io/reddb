#!/usr/bin/env python3
"""Count sync syscalls around checked writes; traced timings are not capacity measurements."""

import argparse
import collections
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "kv"))
from local_matrix import command, inspect


def parse_trace(text):
    pending = {}
    calls = []
    unmatched = []
    for line in text.splitlines():
        if line.lstrip().startswith(">"):
            continue  # Optional userspace call stacks remain in the raw trace.
        prefix = re.match(r"\s*(\d+)\s+\d\d:\d\d:\d\d\.\d+\s+(.*)", line)
        if not prefix:
            if line.strip():
                unmatched.append(line)
            continue
        thread, body = prefix.groups()
        if "<unfinished ...>" in body:
            pending[thread] = body.split("<unfinished ...>")[0].rstrip()
            continue
        if body.startswith("<... "):
            beginning = pending.pop(thread, None)
            if beginning is None:
                unmatched.append(line)
                continue
            body = beginning + body.split("resumed>", 1)[1]
        match = re.match(r"(fsync|fdatasync)\(\d+(?:<(.+)>)?\)\s+=\s+(-?\d+).*<([0-9.]+)>$", body)
        if match:
            operation, path, returned, duration = match.groups()
            calls.append({"thread": int(thread), "operation": operation, "path": path,
                          "returned": int(returned), "seconds": float(duration)})
        elif "fsync(" in body or "fdatasync(" in body or "resumed>" in body:
            unmatched.append(line)
        # Signal and process-exit annotations are retained in the raw trace.
    return {"calls": calls, "pending": pending, "unmatched": unmatched}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="reddb-write-profile-20261020")
    parser.add_argument("--client", default="reddb-query-client-20261020")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--engine-revision", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--stack-traces", action="store_true", help="trace eight writes with userspace call stacks")
    args = parser.parse_args()
    result = {"schema": "reddb.checked-write-sync-profile.v1", "status": "invalid", "phases": [],
              "classification": "strace diagnostic; syscall duration includes tracing overhead; no throughput claim",
              "server": inspect(args.server), "client": inspect(args.client),
              "engine_revision": args.engine_revision,
              "binary_sha256": args.binary_sha256,
              "started_unix_ms": int(time.time() * 1000)}

    def ready():
        code = ("import sys;sys.path.insert(0,'/bench/kv');from compare import RedClient;"
                f"c=RedClient('{args.server}',5000,'probe');print(c.request('GET','/health/live'));c.close()")
        for _ in range(30):
            try:
                command("docker", "exec", args.client, "python", "-c", code, timeout=35)
                return
            except RuntimeError:
                time.sleep(1)
        raise RuntimeError("profiling server not ready")

    def run(name, operation="get", workers=1, *extra):
        return command("docker", "exec", args.client, "python", "/bench/kv/compare.py",
                       "--engine", "reddb-rest", "--host", args.server, "--port", "5000",
                       "--collection", "kv_write_probe", "--keys", "64", "--operations", "8" if args.stack_traces else "64",
                       "--rounds", "1", "--warmup", "0", "--profile", "strace",
                       "--workers", str(workers), "--operation", operation,
                       "--out", "/results/" + name + ".json", *extra, timeout=180)

    try:
        command("docker", "start", args.client, args.server)
        ready()
        phases = [("trace-put-stacks", "put", 1)] if args.stack_traces else [
            ("trace-get-control", "get", 1), ("trace-put-c1", "put", 1), ("trace-put-c8", "put", 8)]
        for name, operation, workers in phases:
            run(name + "-seed", "get", 1, "--prepare-only")
            trace_file = args.out / (name + ".trace")
            log_file = args.out / (name + ".strace.log")
            command("docker", "exec", "-d", args.server, "sh", "-c",
                    "echo $$ > /tmp/query-trace-pid; exec strace -f -tt -T -yy " + ("-k " if args.stack_traces else "") + "-e trace=fsync,fdatasync "
                    f"-o /results/{trace_file.name} -p 1 2>/results/{log_file.name}")
            for _ in range(50):
                if log_file.exists() and "attached" in log_file.read_text():
                    break
                time.sleep(.1)
            else:
                raise RuntimeError("strace did not attach")
            tracer = int(command("docker", "exec", args.server, "cat", "/tmp/query-trace-pid"))
            try:
                run(name, operation, workers)
            finally:
                command("docker", "exec", args.server, "sh", "-c", f"kill -INT {tracer}")
                for _ in range(50):
                    state = command("docker", "exec", args.server, "sh", "-c",
                                    f"if test -e /proc/{tracer}; then echo active; else echo done; fi")
                    if state == "done":
                        break
                    time.sleep(.1)
                else:
                    raise RuntimeError("strace did not detach")
            parsed = parse_trace(trace_file.read_text())
            if parsed["pending"] or parsed["unmatched"] or any(call["returned"] != 0 for call in parsed["calls"]):
                raise RuntimeError(f"incomplete/failed sync trace: {parsed}")
            counts = collections.Counter(call["path"] for call in parsed["calls"])
            measured = json.loads((args.out / (name + ".json")).read_text())
            if measured["status"] != "valid-diagnostic":
                raise RuntimeError("invalid checked workload")
            phase = {"name": name, "operations": measured["rounds"][0]["operations"],
                     "sync_calls": len(parsed["calls"]), "calls_by_path": dict(counts), "calls": parsed["calls"]}
            result["phases"].append(phase)
            if operation == "put":
                command("docker", "kill", args.server)
                command("docker", "start", args.server)
                ready()
                run(name + "-recovery", "get", 1, "--verify-only", "--recovery-from", f"/results/{name}.json")
                phase["recovery"] = "verified"
            print(name, phase["sync_calls"], phase["calls_by_path"], flush=True)
        result["status"] = "valid-diagnostic"
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        for container in [args.server, args.client]:
            command("docker", "stop", container)
        result["finished_unix_ms"] = int(time.time() * 1000)
        filename = "write-profile-stacks.json" if args.stack_traces else "write-profile.json"
        (args.out / filename).write_bytes((json.dumps(result, indent=2) + "\n").encode())


if __name__ == "__main__":
    main()
