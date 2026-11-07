#!/usr/bin/env python3
"""Measure pre-created task containers; stop peers while each server is timed.

Never deletes containers or data. Names must refer to this benchmark's isolated
containers. See BATCHING.md for creation commands and durability contracts.
"""
import argparse
import hashlib
import json
import subprocess
import tempfile
import time
from pathlib import Path

from local_matrix import Resources, inspect


def command(*args, timeout=180):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{args[:5]}: {result.stdout[-1000:]} {result.stderr[-1000:]}")
    return result.stdout


def wait_ready(client, server, port):
    deadline = time.monotonic() + 60
    while True:
        try:
            command("docker", "exec", client, "python", "-c",
                    f"import socket; socket.create_connection(({server!r}, {port}), timeout=1).close()",
                    timeout=5)
            return
        except RuntimeError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(.5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prefix", default="reddb-batch")
    parser.add_argument("--suffix", default="1107")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--operations", type=int, default=256)
    parser.add_argument("--queues-only", action="store_true")
    parser.add_argument("--queue-baseline", action="store_true", help="include native-before in a queue-only capture")
    parser.add_argument("--red-ack-batch", action="store_true", help="use atomic ACK only on native-after")
    parser.add_argument("--queue-settle-ms", type=int, default=0)
    parser.add_argument("--resume-queues", action="store_true",
                        help="resume only between fully completed engines, preserving all prior samples")
    args = parser.parse_args()
    if args.queues_only and args.resume_queues:
        parser.error("queue-only captures use a fresh output directory")
    args.out.mkdir(parents=True, exist_ok=args.resume_queues)
    names = {key: f"{args.prefix}-{key}-{args.suffix}" for key in
             (["client", "native-after", "rabbit", "kafka"] + (["native-before"] if args.queue_baseline else []) if args.queues_only else
              ["client", "native-before", "native-after", "redis", "rabbit", "kafka"])}
    manifest = {"containers": {key: inspect(name) for key, name in names.items()},
                "classification": "shared-host diagnostic; single-node local durability",
                "git_head": command("git", "rev-parse", "HEAD").strip(),
                "dirty_diff_sha256": hashlib.sha256(command("git", "diff").encode()).hexdigest(),
                "operations": args.operations, "queue_rounds": 3, "red_ack_batch": args.red_ack_batch,
                "queue_settle_ms": args.queue_settle_ms, "kv_rounds": 0 if args.queues_only else 3}
    with tempfile.TemporaryDirectory() as temporary:
        for key in ("native-before", "native-after"):
            if key not in names:
                continue
            binary = Path(temporary) / key
            command("docker", "cp", f"{names[key]}:/usr/local/bin/red", str(binary))
            manifest["containers"][key]["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    resume_stamp = str(int(time.time()))
    if args.resume_queues:
        previous = json.loads((args.out / "manifest.json").read_text())
        if (previous["containers"] != manifest["containers"]
                or previous["operations"] != args.operations
                or previous.get("queue_settle_ms", 0) != args.queue_settle_ms
                or previous.get("red_ack_batch", False) != args.red_ack_batch):
            raise RuntimeError("resume requires identical containers, binaries and workload")
        for key in ("native-before", "native-after", "redis"):
            for workers in (1, 8):
                for pipeline in (1, 16):
                    recovery = args.out / f"{key}-w{workers}-p{pipeline}-recovery.json"
                    if json.loads(recovery.read_text())["status"] != "valid-diagnostic":
                        raise RuntimeError("resume requires all KV recoveries to have completed")
        (args.out / f"resume-{resume_stamp}.json").write_text(json.dumps(manifest, indent=2))
    else:
        (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2))
    def save(label, result):
        (args.out / f"{label}.json").write_text(json.dumps(result, indent=2))
    def activate(key):
        for other, name in names.items():
            if other != "client" and other != key:
                command("docker", "stop", name)
        command("docker", "start", names[key])
        # Adapter warmup additionally checks the full protocol and coordinator.
        wait_ready(names["client"], names[key], {"redis": 6379, "rabbit": 5672, "kafka": 9092}.get(key, 5000))
    def restart(key):
        command("docker", "kill", "--signal=KILL", names[key])
        command("docker", "start", names[key])
        wait_ready(names["client"], names[key], {"redis": 6379, "rabbit": 5672, "kafka": 9092}.get(key, 5000))
    def kv(label, key, flags):
        path = f"/tmp/{label}.json"
        command("docker", "exec", names["client"], "python", "/source/bench/kv/compare.py",
                "--engine", "redis" if key == "redis" else "reddb-rest", "--host", names[key],
                "--port", "6379" if key == "redis" else "5000", "--keys", str(args.operations),
                "--operations", str(args.operations), "--rounds", "3", "--out", path, *flags,
                timeout=900)
        result = json.loads(command("docker", "exec", names["client"], "cat", path))
        save(label, result)
        if result["status"] not in ("prepared", "valid-diagnostic"):
            raise RuntimeError(f"invalid KV result: {label}")
        return path
    for key in ([] if args.resume_queues or args.queues_only else ["native-before", "native-after", "redis"]):
        activate(key)
        resources = Resources(args.out, names[key], names["client"])
        try:
            for workers in (1, 8):
                for pipeline in (1, 16):
                    label = f"{key}-w{workers}-p{pipeline}"
                    flags = ["--collection", f"kv_w{workers}_p{pipeline}"]
                    kv(label + "-seed", key, [*flags, "--prepare-only"])
                    path = kv(label, key, [*flags, "--operation", "put", "--workers", str(workers),
                                          "--pipeline", str(pipeline)])
                    restart(key)
                    kv(label + "-recovery", key, [*flags, "--verify-only", "--recovery-from", path])
                    print(f"verified {label}", flush=True)
        finally:
            resources.close()
    def queue(key, label, batch, mode="run", count=None, name=None):
        result = json.loads(command("docker", "exec", names["client"], "python",
            "/source/bench/kv/queue_compare.py", "--engine",
            {"native-before": "reddb", "native-after": "reddb", "rabbit": "rabbitmq", "kafka": "kafka"}[key],
            "--host", names[key], "--name", name or label.replace("-", "_"),
            "--batch", str(batch), "--mode", mode,
            "--settle-ms", str(args.queue_settle_ms),
            "--operations", str(count or args.operations),
            *(["--red-ack-batch"] if args.red_ack_batch and key == "native-after" else []), timeout=900))
        save(label, result)
        if result["status"] != "valid-diagnostic":
            raise RuntimeError(f"invalid queue result: {label}")
    # Full captures compare queue lifecycle performance before and after too.
    for key in [key for key in ("native-before", "native-after", "rabbit", "kafka") if key in names]:
        if args.resume_queues:
            expected = [f"queue-{key}-b{batch}-r{repeat}" for batch in (1, 16) for repeat in range(3)]
            expected += [f"recovery_{key.replace('-', '_')}_{batch}_{mode}"
                         for batch in (1, 16) for mode in ("seed", "recover", "empty")]
            completed = [args.out / f"{label}.json" for label in expected
                         if (args.out / f"{label}.json").exists()]
            if completed:
                if len(completed) != len(expected) or any(
                        json.loads(path.read_text())["status"] != "valid-diagnostic" for path in completed):
                    raise RuntimeError(f"partial {key} engine cannot be resumed; use fresh containers/capture")
                continue
        activate(key)
        resource_dir = args.out / (f"queue-resources-resume-{resume_stamp}" if args.resume_queues else "queue-resources")
        resource_dir.mkdir(exist_ok=True)
        resources = Resources(resource_dir, names[key], names["client"])
        try:
            for batch in (1, 16):
                for repeat in range(3):
                    label = f"queue-{key}-b{batch}-r{repeat}"
                    queue(key, label, batch)
                    print(f"verified {label}", flush=True)
                name = f"recovery_{key.replace('-', '_')}_{batch}"
                queue(key, name + "_seed", batch, "seed", 32, name)
                restart(key)
                queue(key, name + "_recover", batch, "recover", 32, name)
                restart(key)
                queue(key, name + "_empty", batch, "empty", 1, name)
        finally:
            resources.close()
    save("complete", {"status": "passed", "finished_unix_ms": int(time.time() * 1000)})


if __name__ == "__main__":
    main()
