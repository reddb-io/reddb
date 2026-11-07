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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prefix", default="reddb-batch")
    parser.add_argument("--suffix", default="1107")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--operations", type=int, default=256)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    names = {key: f"{args.prefix}-{key}-{args.suffix}" for key in
             ["client", "native-before", "native-after", "redis", "rabbit", "kafka"]}
    manifest = {"containers": {key: inspect(name) for key, name in names.items()},
                "classification": "shared-host diagnostic; single-node local durability",
                "git_head": command("git", "rev-parse", "HEAD").strip(),
                "dirty_diff_sha256": hashlib.sha256(command("git", "diff").encode()).hexdigest(),
                "operations": args.operations, "queue_rounds": 3, "kv_rounds": 3}
    with tempfile.TemporaryDirectory() as temporary:
        for key in ("native-before", "native-after"):
            binary = Path(temporary) / key
            command("docker", "cp", f"{names[key]}:/usr/local/bin/red", str(binary))
            manifest["containers"][key]["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2))
    def save(label, result):
        (args.out / f"{label}.json").write_text(json.dumps(result, indent=2))
    def activate(key):
        for other, name in names.items():
            if other != "client" and other != key:
                command("docker", "stop", name)
        command("docker", "start", names[key])
        # A TCP connect does not guarantee Kafka's coordinator is ready. The
        # adapter's untimed warmup verifies the complete path before sampling.
        time.sleep(5)
    def restart(key):
        command("docker", "kill", "--signal=KILL", names[key])
        command("docker", "start", names[key])
        time.sleep(5)
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
    for key in ["native-before", "native-after", "redis"]:
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
            {"native-after": "reddb", "rabbit": "rabbitmq", "kafka": "kafka"}[key],
            "--host", names[key], "--name", name or label.replace("-", "_"),
            "--batch", str(batch), "--mode", mode,
            "--operations", str(count or args.operations), timeout=900))
        save(label, result)
        if result["status"] != "valid-diagnostic":
            raise RuntimeError(f"invalid queue result: {label}")
    for key in ["native-after", "rabbit", "kafka"]:
        activate(key)
        resource_dir = args.out / "queue-resources"
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
