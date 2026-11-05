#!/usr/bin/env python3
"""Checked HTTP RQL/SQL reads; shared-host diagnostic, never a capacity claim."""

import argparse
import concurrent.futures
import json
import platform
import statistics
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "kv"))
from compare import ProtocolError, RedClient, payload, percentile


def query(connection, sql):
    return connection.request("POST", "/query", {"query": sql})


def statement(case, index, args):
    if case == "kv":
        return f"KV GET {args.kv_collection}.k{index % args.keys}"
    if case == "sql-point":
        return f"SELECT seq, item FROM query_perf WHERE seq = {index % args.keys}"
    count = int(case.split("-")[1])
    return f"SELECT seq, item FROM query_perf ORDER BY seq LIMIT {count}"


def validate(result, case, index, args):
    records = result["result"]["records"]
    if case == "kv":
        expected = payload(index % args.keys, args.value_bytes)
        if len(records) != 1 or records[0]["values"].get("value") != expected:
            raise ProtocolError("wrong KV response")
        return
    indices = [index % args.keys] if case == "sql-point" else range(int(case.split("-")[1]))
    if len(records) != len(indices):
        raise ProtocolError("wrong SQL row count")
    for record, expected_index in zip(records, indices):
        values = record["values"]
        if values.get("seq") != expected_index or values.get("item") != payload(expected_index, args.value_bytes):
            raise ProtocolError("wrong SQL row value or ordering")


def setup(args):
    connection = RedClient(args.host, args.port, args.kv_collection)
    try:
        # The KV dataset is seeded by bench/kv/local_matrix.py. Refuse an
        # accidental empty/wrong dataset before collecting any observations.
        for index in [0, args.keys // 2, args.keys - 1]:
            validate(query(connection, statement("kv", index, args)), "kv", index, args)
        query(connection, "CREATE TABLE query_perf (seq INTEGER, item TEXT, unused TEXT)")
        for start in range(0, args.keys, 500):
            rows = ",".join(f"({index},'{payload(index, args.value_bytes)}','{'x' * 128}')"
                            for index in range(start, min(start + 500, args.keys)))
            query(connection, "INSERT INTO query_perf (seq, item, unused) VALUES " + rows)
        query(connection, "CREATE INDEX query_perf_seq ON query_perf (seq) USING HASH")
        # Save the actual planner output instead of assuming the index is used.
        plan = query(connection, "EXPLAIN SELECT seq, item FROM query_perf WHERE seq = 42")
        verify_table(connection, args)
        return plan
    finally:
        connection.close()


def verify_table(connection, args):
    result = query(connection, f"SELECT seq, item FROM query_perf ORDER BY seq LIMIT {args.keys}")
    records = result["result"]["records"]
    if len(records) != args.keys:
        raise ProtocolError("incomplete SQL dataset")
    for index, record in enumerate(records):
        if record["values"].get("seq") != index or record["values"].get("item") != payload(index, args.value_bytes):
            raise ProtocolError("wrong full SQL dataset")


def sample(args, case, round_index):
    ready = threading.Barrier(args.workers + 1)
    start = threading.Event()
    deadline = [0.0]

    def worker(worker_index):
        connection = RedClient(args.host, args.port, args.kv_collection)
        try:
            for index in range(10):
                validate(query(connection, statement(case, index, args)), case, index, args)
            ready.wait(timeout=60)
            start.wait(timeout=60)
            latencies = []
            index = worker_index + round_index * 7919
            while time.perf_counter() < deadline[0]:
                begin = time.perf_counter_ns()
                validate(query(connection, statement(case, index, args)), case, index, args)
                latencies.append((time.perf_counter_ns() - begin) / 1000)
                index += args.workers
            return latencies
        except Exception:
            ready.abort()
            raise
        finally:
            connection.close()

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = [pool.submit(worker, index) for index in range(args.workers)]
        ready.wait(timeout=60)
        begin = time.perf_counter()
        deadline[0] = begin + args.seconds
        start.set()
        latencies = [latency for future in futures for latency in future.result()]
        elapsed = time.perf_counter() - begin
    if not latencies:
        raise ProtocolError("no completed queries")
    return {"queries": len(latencies), "seconds": elapsed, "queries_s": len(latencies) / elapsed,
            "p50_us": percentile(latencies, .5), "p95_us": percentile(latencies, .95),
            "p99_us": percentile(latencies, .99), "errors": 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=5000)
    parser.add_argument("--kv-collection", default="kv_diag_10000")
    parser.add_argument("--keys", type=int, default=10000)
    parser.add_argument("--value-bytes", type=int, default=100)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--seconds", type=float, default=2)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--case", choices=["kv", "sql-point", "sql-100", "sql-1000"], default="kv")
    parser.add_argument("--setup", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.setup and args.verify_only:
        parser.error("choose one setup mode")
    if not (1000 <= args.keys <= 100000 and 1 <= args.workers <= 32 and 0 < args.seconds <= 60
            and 1 <= args.rounds <= 20 and 1 <= args.value_bytes <= 4096):
        parser.error("invalid bounded workload")
    if not args.kv_collection.replace("_", "").isalnum() or not args.kv_collection[0].isalpha():
        parser.error("invalid collection identifier")
    result = {"schema": "reddb.query-read-paths.v1", "status": "invalid", "rounds": [],
              "config": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
              "client": {"python": platform.python_version(), "platform": platform.platform()},
              "started_unix_ms": int(time.time() * 1000),
              "classification": "shared-host diagnostic; fixed duration; every response checked; result cache off"}
    try:
        if args.setup:
            result["explain"] = setup(args)
            result["status"] = "prepared"
        elif args.verify_only:
            connection = RedClient(args.host, args.port, args.kv_collection)
            try:
                verify_table(connection, args)
            finally:
                connection.close()
            result["status"] = "verified"
        else:
            for index in range(args.rounds):
                observation = sample(args, args.case, index)
                result["rounds"].append(observation)
                print(json.dumps({"case": args.case, "round": index, **observation}), flush=True)
            result["median_queries_s"] = statistics.median(r["queries_s"] for r in result["rounds"])
            result["median_p99_us"] = statistics.median(r["p99_us"] for r in result["rounds"])
            result["status"] = "valid-diagnostic"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        result["finished_unix_ms"] = int(time.time() * 1000)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_bytes((json.dumps(result, indent=2) + "\n").encode())


if __name__ == "__main__":
    main()
