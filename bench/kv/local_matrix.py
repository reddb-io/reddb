#!/usr/bin/env python3
"""Run compare.py against pre-created, isolated Docker containers sequentially."""

import argparse
import json
import platform
import re
import subprocess
import threading
import time
from pathlib import Path


def command(*parts, timeout=180):
    result = subprocess.run(parts, text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{' '.join(parts[:5])}: {result.stderr[-1500:]} {result.stdout[-1000:]}")
    return result.stdout.strip()


def inspect(name):
    value = json.loads(command("docker", "inspect", name))[0]
    limits = value["HostConfig"]
    image = json.loads(command("docker", "image", "inspect", value["Image"]))[0]
    return {"container": name, "image_id": value["Image"], "image_digests": image.get("RepoDigests"),
            "command": value["Config"]["Cmd"], "environment": value["Config"]["Env"],
            "cpu_set": limits["CpusetCpus"], "nano_cpus": limits["NanoCpus"],
            "memory_limit": limits["Memory"], "mounts": value["Mounts"]}


def resource_sample(line):
    # Docker's Windows CLI emits cursor escapes even into a pipe.
    return json.loads(re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", line))


class Resources:
    def __init__(self, directory, server, client):
        label = server if isinstance(server, str) else "matrix"
        servers = [server] if isinstance(server, str) else server
        self.path = directory / f"resources-{label}.jsonl"
        self.error_file = (directory / f"resources-{label}.stderr.log").open("w")
        self.process = subprocess.Popen(
            ["docker", "stats", "--format", "{{json .}}", *servers, client],
            stdout=subprocess.PIPE, stderr=self.error_file, text=True, bufsize=1)
        self.thread = threading.Thread(target=self.consume, daemon=True)
        self.thread.start()

    def consume(self):
        with self.path.open("w", encoding="utf-8") as output:
            for line in self.process.stdout:
                try:
                    value = resource_sample(line)
                except json.JSONDecodeError:
                    continue
                output.write(json.dumps({"unix_ms": int(time.time() * 1000), **value}) + "\n")
                output.flush()

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.thread.join(timeout=10)
        self.process.stdout.close()
        self.error_file.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", default="reddb-kv-client-20261020")
    parser.add_argument("--redis", default="reddb-kv-redis-20261020")
    parser.add_argument("--kividb", default="reddb-kv-kivi-20261020")
    parser.add_argument("--reddb", default="reddb-kv-red-20261020")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--engine-revision", required=True)
    parser.add_argument("--engine-sha256", required=True)
    parser.add_argument("--keys", default="1000,10000")
    parser.add_argument("--workers", default="1,8,32")
    parser.add_argument("--seconds", type=float, default=2)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--sample-only", action="store_true")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    sizes = [int(value) for value in args.keys.split(",")]
    workers = [int(value) for value in args.workers.split(",")]
    servers = [args.redis, args.kividb, args.reddb]
    if args.sample_only:
        monitor = Resources(args.out, servers, args.client)
        try:
            while True:
                try:
                    if json.loads((args.out / "manifest.json").read_text())["status"] != "running":
                        break
                except json.JSONDecodeError:
                    pass
                time.sleep(1)
        finally:
            monitor.close()
        return
    manifest = {
        "schema": "reddb.kv-local-matrix.v1", "status": "running",
        "classification": "shared-host-diagnostic", "primary_metric": "GET request latency at pipeline=1",
        "started_unix_ms": int(time.time() * 1000), "host": platform.platform(),
        "docker": json.loads(command("docker", "info", "--format", "{{json .}}")),
        "red_engine_revision": args.engine_revision, "red_binary_sha256": args.engine_sha256,
        "red_build": "cargo build --release --locked --bin red -j 4; Rust 1.95.0",
        "containers": {name: inspect(name) for name in [*servers, args.client]},
        "conditions": {"keys": sizes, "value_bytes": 100, "workers": workers,
                       "rounds": args.rounds, "get_seconds_per_round": args.seconds,
                       "transport": "HTTP/1.1 keep-alive for RedDB; RESP2 for Redis/KiviDB",
                       "warmup": "10 GETs per connection; full dataset check before/after each matrix",
                       "server_order": "Redis, KiviDB, RedDB; no simultaneous server load",
                       "write_contract": "RedDB strict WAL; Redis AOF always; KiviDB SET + WAITAOF 1 0 5000",
                       "recovery": "SIGKILL after completion; reopen and verify every expected value",
                       "limits": "Python/GIL and shared Docker VM may limit throughput; no max-throughput or public lead claim"},
        "cases": [], "phases": [], "errors": [],
    }
    # Docker info can include unrelated host paths; retain just hardware/engine identity.
    manifest["docker"] = {key: manifest["docker"].get(key) for key in (
        "ServerVersion", "NCPU", "MemTotal", "Architecture", "KernelVersion", "OperatingSystem", "Driver")}

    def save():
        temporary = args.out / "manifest.tmp"
        temporary.write_text(json.dumps(manifest, indent=2) + "\n")
        temporary.replace(args.out / "manifest.json")

    def run(engine, container, port, collection, keys, name, extra=()):
        target = f"/results/{name}.json"
        parts = ["docker", "exec", args.client, "python", "/bench/compare.py", "--engine", engine,
                 "--host", container, "--port", str(port), "--collection", collection,
                 "--keys", str(keys), "--rounds", str(args.rounds), "--warmup", "10", "--out", target]
        if engine == "kividb":
            parts.append("--fence-writes")
        stdout = command(*parts, *extra, timeout=600)
        (args.out / f"{name}.stdout.log").write_text(stdout + "\n")
        result = json.loads((args.out / f"{name}.json").read_text())
        manifest["cases"].append({"file": f"{name}.json", "status": result["status"],
                                  "engine": engine, "operation": result["operation"]})
        save()
        print(f"{name}: {result['status']} {result.get('median_throughput_ops_s', '')}", flush=True)

    def probe(container, engine):
        if engine.startswith("reddb"):
            code = "from compare import RedClient;c=RedClient('" + container + "',5000,'probe');print(c.request('GET','/health/live'));c.close()"
        else:
            port = 6379 if engine == "redis" else 6380
            code = (f"from compare import RespClient;c=RespClient('{container}',{port},'probe');"
                    "print(c.command('INFO','server').decode());"
                    "print(c.command('CONFIG','GET','appendfsync'));c.close()")
        return command("docker", "exec", args.client, "python", "-c",
                       "import sys;sys.path.insert(0,'/bench');" + code, timeout=40)

    def ready(container, engine):
        for _ in range(30):
            try:
                return probe(container, engine)
            except RuntimeError:
                time.sleep(1)
        raise RuntimeError(f"{container} did not become ready")

    def recover(container, engine, port, collection, keys, name, previous=None):
        command("docker", "kill", container)
        command("docker", "start", container)
        ready(container, engine)
        extra = ["--verify-only"]
        if previous:
            extra += ["--recovery-from", f"/results/{previous}.json"]
        run(engine, container, port, collection, keys, name, extra)

    save()
    try:
        for container in servers:
            command("docker", "stop", container)
        for engine, container, port in [("redis", args.redis, 6379), ("kividb", args.kividb, 6380),
                                        ("reddb-rest", args.reddb, 5000)]:
            monitor = None
            try:
                command("docker", "start", container)
                identity = ready(container, engine)
                phase = {"engine": engine, "identity": identity, "started_unix_ms": int(time.time() * 1000)}
                manifest["phases"].append(phase)
                monitor = Resources(args.out, container, args.client)
                adapters = ["reddb-rest", "reddb-query"] if engine == "reddb-rest" else [engine]
                for keys in sizes:
                    collection = f"kv_diag_{keys}"
                    run(engine, container, port, collection, keys, f"{engine}-{keys}-seed", ["--prepare-only"])
                    command("docker", "restart", container)
                    ready(container, engine)
                    run(engine, container, port, collection, keys, f"{engine}-{keys}-reopen", ["--verify-only"])
                    for adapter in adapters:
                        for concurrency in workers:
                            for depth in [1, 16]:
                                name = f"{adapter}-{keys}-get-c{concurrency}-p{depth}"
                                run(adapter, container, port, collection, keys, name,
                                    ["--workers", str(concurrency), "--pipeline", str(depth),
                                     "--seconds", str(args.seconds), "--verification", "responses"])
                    recover(container, engine, port, collection, keys, f"{engine}-{keys}-get-recovery")
                # Separate bounded durable-overwrite cell, including readback
                # after every round. Native KiviDB SET alone is not sync-durable.
                for concurrency in [1, 8]:
                    collection = "kv_durable"
                    name = f"{engine}-1000-put-c{concurrency}-p1"
                    run(engine, container, port, collection, 1000, f"{name}-seed", ["--prepare-only"])
                    run(engine, container, port, collection, 1000, name,
                        ["--operation", "put", "--operations", "500", "--workers", str(concurrency)])
                    recover(container, engine, port, collection, 1000, f"{name}-recovery", name)
                phase["finished_unix_ms"] = int(time.time() * 1000)
            except Exception as error:
                manifest["errors"].append({"engine": engine, "error": str(error)})
                print(f"{engine}: {error}", flush=True)
            finally:
                if monitor:
                    monitor.close()
                command("docker", "stop", container)
                save()
        manifest["status"] = "valid-diagnostic" if not manifest["errors"] else "incomplete"
    finally:
        manifest["finished_unix_ms"] = int(time.time() * 1000)
        save()
        command("docker", "stop", args.client)


if __name__ == "__main__":
    main()
