#!/usr/bin/env python3
"""Check real container data placement and persistence; pass a locally built image."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def wait_ready(name):
    port = docker("port", name, "5000/tcp").rsplit(":", 1)[1]
    base = f"http://127.0.0.1:{port}"
    for _ in range(120):
        try:
            with urllib.request.urlopen(base + "/health/live", timeout=1):
                return base
        except (urllib.error.URLError, TimeoutError):
            time.sleep(0.5)
    raise RuntimeError(f"container did not become ready: {docker('logs', name)}")


def query(base, sql):
    request = urllib.request.Request(
        base + "/query", json.dumps({"query": sql}).encode(),
        {"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        result = json.load(response)
    assert result.get("ok") is True, result
    return result


def data_files(name):
    process = subprocess.Popen(
        ["docker", "cp", f"{name}:/data/.", "-"], stdout=subprocess.PIPE,
    )
    with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
        files = {item.name.removeprefix("./"): (item.size, item.uid)
                 for item in archive if item.isfile()}
    assert process.wait() == 0
    return files


def check_case(image, case, expected, env=(), args=(), bind=False):
    name = "reddb-path-smoke-" + uuid.uuid4().hex[:12]
    volume = name + "-data"
    with tempfile.TemporaryDirectory(prefix=name) as directory:
        Path(directory).chmod(0o777)  # Empty bind mount, writable by the non-root image UID.
        user = ("--user", f"{os.getuid()}:{os.getgid()}") if bind else ()
        assert not bind or os.getuid() != 0, "Run the bind-mount smoke as a non-root user"
        if not bind:
            docker("volume", "create", volume)
        try:
            docker(
                "run", "-d", "--name", name, "--cpus=1", "--memory=1g",
                "--memory-swap=1g", "--ulimit", "core=0", "--read-only",
                "--tmpfs", "/tmp:rw,noexec,nosuid,size=128m",
                "-p", "127.0.0.1::5000", "-v", f"{directory if bind else volume}:/data",
                "-e", "REDDB_NO_AUTH=true", "-e", "REDDB_AUTH=false",
                "-e", "REDDB_REQUIRE_AUTH=false", "-e", "REDDB_MEMORY_BUDGET=256MiB",
                *user, *env, image, *args,
            )
            config = json.loads(docker("inspect", name))[0]
            assert config["Config"]["User"] not in ("", "0", "0:0", "root", "root:root")
            base = wait_ready(name)
            query(base, "CREATE TABLE container_path_probe (id INT PRIMARY KEY, value TEXT)")
            query(base, "INSERT INTO container_path_probe (id, value) VALUES (1, 'retained')")
            files = data_files(name)
            relative = expected.removeprefix("/data/")
            assert files[relative][0] > 0 and files[relative][1] != 0, files
            assert {path for path in files if path.endswith(".rdb")} == {relative}, files
            docker("stop", "--time", "30", name)
            docker("start", name)
            result = query(wait_ready(name), "SELECT id, value FROM container_path_probe")
            rows = [record["values"] for record in result["result"]["records"]]
            assert rows == [{"id": 1, "value": "retained"}], rows
            state = json.loads(docker("inspect", name))[0]["State"]
            assert state["Running"] and not state["OOMKilled"], state
            print(json.dumps({"case": case, "path": expected, "restartPreservedWrite": True}), flush=True)
        finally:
            subprocess.run(["docker", "stop", "--time", "30", name], capture_output=True)
            subprocess.run(["docker", "rm", "-v", name], capture_output=True, check=True)
            if not bind:
                docker("volume", "rm", volume)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="Server image built from docker/Dockerfile.release or Dockerfile")
    image = parser.parse_args().image
    check_case(image, "default-volume", "/data/red/database.rdb")
    check_case(image, "default-empty-bind", "/data/red/database.rdb", bind=True)
    check_case(image, "legacy-env", "/data/data.rdb", env=("-e", "REDDB_DATA_PATH=/data/data.rdb"))
    check_case(image, "explicit-cli", "/data/custom.rdb", args=("server", "--path", "/data/custom.rdb"))
