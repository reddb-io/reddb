#!/usr/bin/env python3
import json
import os
import socket
import statistics
import sys
import time
import urllib.error
import urllib.request


REDDB_URL = os.environ.get("REDDB_URL", "http://127.0.0.1:5000").rstrip("/")
REDIS_HOST = os.environ.get("REDIS_HOST", "127.0.0.1")
REDIS_PORT = int(os.environ.get("REDIS_PORT", "6379"))
OPS = int(os.environ.get("KV_BENCH_OPS", "1000"))
WATCH_EVENTS = int(os.environ.get("KV_BENCH_WATCH_EVENTS", "100000"))


def percentile(values, pct):
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def summary(latencies):
    elapsed = sum(latencies) / 1_000_000.0
    return {
        "ops": len(latencies),
        "p50_us": round(percentile(latencies, 50), 3),
        "p99_us": round(percentile(latencies, 99), 3),
        "throughput_ops_s": round(len(latencies) / elapsed, 3) if elapsed > 0 else 0.0,
    }


def http_json(method, path, payload=None):
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["content-type"] = "application/json"
    req = urllib.request.Request(f"{REDDB_URL}{path}", data=data, method=method, headers=headers)
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = resp.read()
    return json.loads(body.decode("utf-8")) if body else {}


def http_text(method, path):
    req = urllib.request.Request(f"{REDDB_URL}{path}", method=method)
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.read().decode("utf-8")


def reddb_query(sql):
    return http_json("POST", "/query", {"query": sql})


def redis_command(sock, *parts):
    encoded = [f"*{len(parts)}\r\n".encode("ascii")]
    for part in parts:
        raw = str(part).encode("utf-8")
        encoded.append(f"${len(raw)}\r\n".encode("ascii"))
        encoded.append(raw + b"\r\n")
    sock.sendall(b"".join(encoded))
    return read_resp(sock)


def read_resp(sock):
    prefix = sock.recv(1)
    if prefix in (b"+", b"-", b":"):
        return read_line(sock)
    if prefix == b"$":
        size = int(read_line(sock))
        if size < 0:
            return None
        data = recv_exact(sock, size)
        recv_exact(sock, 2)
        return data
    raise RuntimeError(f"unsupported Redis response prefix: {prefix!r}")


def read_line(sock):
    out = bytearray()
    while not out.endswith(b"\r\n"):
        out.extend(sock.recv(1))
    return out[:-2].decode("utf-8")


def recv_exact(sock, size):
    out = bytearray()
    while len(out) < size:
        out.extend(sock.recv(size - len(out)))
    return bytes(out)


def measure(fn, count):
    latencies = []
    for i in range(count):
        start = time.perf_counter_ns()
        fn(i)
        latencies.append((time.perf_counter_ns() - start) / 1000.0)
    return summary(latencies)


def bench_reddb():
    http_json("PUT", "/collections/kv_bench/kvs/warmup", {"value": 1})
    reddb_query("KV INCR kv_bench.counter BY 1")
    return {
        "put": measure(
            lambda i: http_json("PUT", f"/collections/kv_bench/kvs/put_{i}", {"value": i}),
            OPS,
        ),
        "get": measure(lambda i: http_json("GET", f"/collections/kv_bench/kvs/put_{i % OPS}"), OPS),
        "incr": measure(lambda i: reddb_query("KV INCR kv_bench.counter BY 1"), OPS),
    }


def bench_redis():
    with socket.create_connection((REDIS_HOST, REDIS_PORT), timeout=10) as sock:
        redis_command(sock, "PING")
        return {
            "put": measure(lambda i: redis_command(sock, "SET", f"kv_bench:put_{i}", i), OPS),
            "get": measure(lambda i: redis_command(sock, "GET", f"kv_bench:put_{i % OPS}"), OPS),
            "incr": measure(lambda i: redis_command(sock, "INCR", "kv_bench:counter"), OPS),
        }


def parse_sse_events(body):
    events = []
    for block in body.split("\n\n"):
        for line in block.splitlines():
            if line.startswith("data: "):
                events.append(json.loads(line[6:]))
    return events


def bench_watch():
    last_lsn = 0
    lags = []
    received = 0
    for i in range(WATCH_EVENTS):
        http_json("PUT", "/collections/kv_bench/kvs/watch_key", {"value": i})
        body = http_text("GET", f"/collections/kv_bench/kv/watch_key/watch?since_lsn={last_lsn}&limit=1000")
        for event in parse_sse_events(body):
            lsn = int(event["lsn"])
            if lsn <= last_lsn:
                continue
            last_lsn = lsn
            received += 1
            lags.append(max(0.0, time.time() * 1000.0 - float(event["committed_at"])))
    return {
        "events": WATCH_EVENTS,
        "received": received,
        "p50_ms": round(percentile(lags, 50), 3),
        "p99_ms": round(percentile(lags, 99), 3),
        "drops": http_json("GET", "/stats").get("kv", {}).get("watch_drops", 0),
    }


def main():
    out = sys.argv[1]
    result = {
        "schema": "reddb.kv-bench.v1",
        "generated_at_unix_ms": int(time.time() * 1000),
        "config": {
            "ops": OPS,
            "watch_events": WATCH_EVENTS,
            "reddb_url": REDDB_URL,
            "redis": f"{REDIS_HOST}:{REDIS_PORT}",
        },
        "reddb": bench_reddb(),
        "redis": bench_redis(),
        "watch_delivery_lag": bench_watch(),
    }
    with open(out, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    try:
        main()
    except (urllib.error.URLError, OSError, RuntimeError) as exc:
        raise SystemExit(f"kv benchmark failed: {exc}")
