#!/usr/bin/env python3
"""Verified, persistent-connection KV diagnostics; no competitor-lead claims."""

import argparse
import concurrent.futures
import hashlib
import http.client
import json
import math
import platform
import socket
import statistics
import threading
import time
from pathlib import Path


class ProtocolError(RuntimeError):
    pass


def payload(index, size, generation=0):
    token = hashlib.sha256(f"{generation}:{index}".encode()).hexdigest()
    return (token * math.ceil(size / len(token)))[:size]


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


class RespClient:
    def __init__(self, host, port, collection, fence_writes=False):
        self.socket = socket.create_connection((host, port), timeout=30)
        self.socket.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.reader = self.socket.makefile("rb")
        self.collection = collection
        self.fence_writes = fence_writes

    def close(self):
        self.reader.close()
        self.socket.close()

    @staticmethod
    def encode(parts):
        result = [f"*{len(parts)}\r\n".encode()]
        for part in parts:
            raw = str(part).encode()
            result.extend((f"${len(raw)}\r\n".encode(), raw, b"\r\n"))
        return b"".join(result)

    def command(self, *parts):
        self.socket.sendall(self.encode(parts))
        return self.read()

    def read(self, depth=0):
        if depth > 8:
            raise ProtocolError("RESP nesting limit exceeded")
        line = self.reader.readline(65537)
        if not line.endswith(b"\r\n") or len(line) > 65536:
            raise ProtocolError("truncated or oversized RESP header")
        prefix, value = line[:1], line[1:-2]
        if prefix == b"-":
            raise ProtocolError(value.decode(errors="replace"))
        if prefix == b"+":
            return value
        if prefix == b":":
            return int(value)
        if prefix == b"$":
            size = int(value)
            if size == -1:
                return None
            if not 0 <= size <= 16 * 1024 * 1024:
                raise ProtocolError("invalid RESP bulk size")
            raw = self.reader.read(size + 2)
            if len(raw) != size + 2 or raw[-2:] != b"\r\n":
                raise ProtocolError("truncated RESP bulk value")
            return raw[:-2]
        if prefix == b"*":
            count = int(value)
            if not 0 <= count <= 10000:
                raise ProtocolError("invalid RESP array size")
            return [self.read(depth + 1) for _ in range(count)]
        raise ProtocolError(f"unsupported RESP prefix {prefix!r}")

    def batch(self, operation, indices, size, generation=0):
        commands = []
        expected = []
        for index in indices:
            key = f"{self.collection}:k{index}"
            value = payload(index, size, generation)
            commands.append(("GET", key) if operation == "get" else ("SET", key, value))
            expected.append(value.encode() if operation == "get" else b"OK")
            if operation == "put" and self.fence_writes:
                commands.append(("WAITAOF", 1, 0, 5000))
                expected.append([1, 0])
        self.socket.sendall(b"".join(self.encode(command) for command in commands))
        for value in expected:
            actual = self.read()
            if actual != value:
                raise ProtocolError(f"incorrect {operation} response: {actual!r}")


class RedClient:
    def __init__(self, host, port, collection, query=False):
        self.connection = http.client.HTTPConnection(host, port, timeout=30)
        self.connection.connect()
        self.connection.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.collection = collection
        self.query = query

    def close(self):
        self.connection.close()

    def request(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        self.connection.request(method, path, data, {"Content-Type": "application/json"})
        response = self.connection.getresponse()
        raw = response.read()
        if response.status not in (200, 201) or response.will_close:
            raise ProtocolError(f"HTTP {response.status}, closes={response.will_close}: {raw[:512]!r}")
        result = json.loads(raw)
        if result.get("ok") is False or "error" in result:
            raise ProtocolError(f"query error: {result}")
        return result

    def batch(self, operation, indices, size, generation=0):
        if len(indices) != 1:
            raise ProtocolError("HTTP pipelining is not implemented by this harness")
        index = indices[0]
        value = payload(index, size, generation)
        if self.query:
            key = f"{self.collection}.k{index}"
            sql = f"KV GET {key}" if operation == "get" else f"KV PUT {key} = '{value}'"
            result = self.request("POST", "/query", {"query": sql})
            if operation == "get":
                rows = result["result"]["records"]
                if len(rows) != 1 or rows[0]["values"]["value"] != value:
                    raise ProtocolError(f"incorrect query readback: {result}")
        else:
            path = f"/collections/{self.collection}/kvs/k{index}"
            result = self.request("GET" if operation == "get" else "PUT", path,
                                  None if operation == "get" else {"value": value})
            if operation == "get" and result.get("value") != value:
                raise ProtocolError(f"incorrect REST readback: {result}")


def client(args):
    if args.engine.startswith("reddb"):
        return RedClient(args.host, args.port, args.collection, args.engine == "reddb-query")
    return RespClient(args.host, args.port, args.collection, args.fence_writes)


def verify(args, generations=None, indices=None):
    indices = range(args.keys) if indices is None else sorted(set(indices))
    connection = client(args)
    try:
        for index in indices:
            generation = 0 if generations is None else generations[index]
            connection.batch("get", [index], args.value_bytes, generation)
    finally:
        connection.close()
    digest = hashlib.sha256()
    for index in indices:
        generation = 0 if generations is None else generations[index]
        digest.update(f"k{index}\0{payload(index, args.value_bytes, generation)}\n".encode())
    return {"keys_checked": len(indices), "expected_sha256": digest.hexdigest()}


def prepare(args):
    connection = client(args)
    try:
        for index in range(args.keys):
            connection.batch("put", [index], args.value_bytes)
    finally:
        connection.close()
    return verify(args)


def measure(args, round_index, generations):
    clock = {}
    barrier = threading.Barrier(args.workers + 1, timeout=60,
                                action=lambda: clock.update(start=time.perf_counter(),
                                                            unix_ms=int(time.time() * 1000)))

    def worker(worker_index):
        connection = client(args)
        try:
            # Warm only reads; setup and connection establishment are untimed.
            for index in range(args.warmup):
                key_index = index % args.keys
                connection.batch("get", [key_index], args.value_bytes, generations[key_index])
            barrier.wait()
            latencies = []
            completed = 0
            quota = args.operations // args.workers + (worker_index < args.operations % args.workers)
            while ((time.perf_counter() - clock["start"] < args.seconds)
                   if args.seconds else completed < quota):
                count = args.pipeline if args.seconds else min(args.pipeline, quota - completed)
                indices = [((completed + offset) * args.workers + worker_index
                            + round_index * 7919) % args.keys for offset in range(count)]
                started = time.perf_counter_ns()
                connection.batch(args.operation, indices, args.value_bytes,
                                 round_index + 1 if args.operation == "put" else 0)
                # Batch latency is time until all replies were validated. It is
                # never divided by depth and mislabeled as per-request latency.
                latencies.append((time.perf_counter_ns() - started) / 1000)
                completed += count
            return completed, latencies
        except BaseException:
            barrier.abort()
            raise
        finally:
            connection.close()

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = [executor.submit(worker, index) for index in range(args.workers)]
        try:
            barrier.wait()
        except threading.BrokenBarrierError:
            errors = [future.exception() for future in futures]
            raise next((error for error in errors if error is not None
                        and not isinstance(error, threading.BrokenBarrierError)),
                       threading.BrokenBarrierError())
        results = [future.result() for future in futures]
        seconds = time.perf_counter() - clock["start"]
    latencies = [latency for _, samples in results for latency in samples]
    completed = sum(count for count, _ in results)
    return {
        "started_unix_ms": clock["unix_ms"], "finished_unix_ms": int(time.time() * 1000),
        "operations": completed, "seconds": seconds,
        "throughput_ops_s": completed / seconds,
        "batch_p50_us": percentile(latencies, .50),
        "batch_p95_us": percentile(latencies, .95),
        "batch_p99_us": percentile(latencies, .99), "errors": 0,
    }


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=["reddb-rest", "reddb-query", "redis", "kividb"], required=True)
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--collection", default="kv_compare")
    parser.add_argument("--keys", type=int, default=1000)
    parser.add_argument("--value-bytes", type=int, default=100)
    parser.add_argument("--operations", type=int, default=2000)
    parser.add_argument("--seconds", type=float, default=0,
                        help="fixed duration per round instead of fixed operation count; GET only")
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--warmup", type=int, default=25)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--pipeline", type=int, default=1)
    parser.add_argument("--operation", choices=["get", "put"], default="get")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--fence-writes", action="store_true",
                        help="require SET + WAITAOF 1 0 5000; logical operation includes the fence")
    parser.add_argument("--recovery-from", type=Path,
                        help="verify a completed PUT result's final generations after restart")
    parser.add_argument("--verification", choices=["full", "responses"], default="full",
                        help="responses requires caller-owned full dataset checks before/after the matrix")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.prepare_only and args.verify_only:
        parser.error("choose one setup mode")
    if args.fence_writes and args.engine.startswith("reddb"):
        parser.error("WAITAOF fencing is RESP-only")
    if args.recovery_from and not args.verify_only:
        parser.error("recovery-from requires verify-only")
    if args.seconds < 0 or args.seconds > 3600 or (args.seconds and args.operation != "get"):
        parser.error("duration mode is GET-only and must be in [0, 3600]")
    if args.verification == "responses" and args.operation != "get":
        parser.error("writes require full verification")
    if min(args.keys, args.value_bytes, args.operations, args.workers, args.rounds, args.pipeline) < 1:
        parser.error("counts must be positive")
    if args.warmup < 0 or args.operations < args.workers or args.value_bytes > 1024 * 1024:
        parser.error("invalid warmup, operations per worker, or value size")
    if not args.collection.replace("_", "").isalnum() or not args.collection[0].isalpha():
        parser.error("collection must be an unquoted identifier")
    return args


def main():
    args = arguments()
    result = {
        "schema": "reddb.kv-compare.v1", "classification": "shared-host-diagnostic",
        "engine": args.engine, "operation": args.operation,
        "config": {key: str(value) if isinstance(value, Path) else value
                   for key, value in vars(args).items() if key != "out"},
        "client": {"python": platform.python_version(), "platform": platform.platform(),
                   "implementation": "stdlib threads; client saturation must be checked externally"},
        "created_unix_ms": int(time.time() * 1000), "status": "invalid", "rounds": [],
        "latency_contract": "batch completion including response validation; pipeline=1 is one request",
    }
    try:
        if args.engine.startswith("reddb") and args.pipeline != 1:
            result.update(status="unsupported", reason="this harness does not implement HTTP pipelining")
        elif args.prepare_only:
            result.update(verification=prepare(args), status="prepared")
        elif args.verify_only:
            generations = None
            if args.recovery_from:
                previous = json.loads(args.recovery_from.read_text())
                if previous["status"] != "valid-diagnostic" or previous["operation"] != "put":
                    raise ProtocolError("recovery input must be a valid PUT result")
                if any(previous["config"][key] != getattr(args, key)
                       for key in ("keys", "collection", "value_bytes")):
                    raise ProtocolError("recovery workload differs from the recorded PUT result")
                generations = previous["final_generations"]
                if len(generations) != args.keys:
                    raise ProtocolError("invalid recovery generation count")
            result.update(verification=verify(args, generations), status="valid-diagnostic")
        else:
            result["verification_before"] = (verify(args) if args.verification == "full" or args.verify_only
                                             else {"scope": "caller-owned-dataset"})
            generations = [0] * args.keys
            if not args.verify_only:
                for index in range(args.rounds):
                    sample = measure(args, index, generations)
                    result["rounds"].append(sample)
                    if args.operation == "put":
                        changed = {(operation + index * 7919) % args.keys
                                   for operation in range(args.operations)}
                        for key_index in changed:
                            generations[key_index] = index + 1
                        sample["readback"] = verify(args, generations, changed)
                    print(json.dumps({"round": index, **sample}), flush=True)
                result["median_throughput_ops_s"] = statistics.median(
                    sample["throughput_ops_s"] for sample in result["rounds"])
                if args.operation == "put":
                    result["final_generations"] = generations
            result.update(verification_after=(verify(args, generations)
                          if args.verification == "full" or args.verify_only
                          else {"scope": "caller-owned-dataset", "every_measured_reply_checked": True}),
                          status="valid-diagnostic")
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
