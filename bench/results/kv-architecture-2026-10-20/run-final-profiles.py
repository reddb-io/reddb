"""Repeat write diagnostics for the session-fence revision in fresh containers.

Requires the documented immutable binary in the existing build volume. Existing
2584 captures are preserved. Run from this repository with Docker Desktop.
"""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
REVISION = "fae39eb6d07c32f8bc734973f10a9349a8e56a45"
BINARY = "/work/architecture-pr-fae39eb6-red"


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


sha = docker("exec", "reddb-arch-build-20261020", "sha256sum", BINARY).split()[0]
for mode, durability in [("strict", "strict"), ("grouped", "wal_durable_grouped")]:
    server = f"reddb-arch-final-{mode}-20261020"
    client = f"reddb-arch-final-client-{mode}-20261020"
    out = ROOT / "writes-final" / mode
    out.mkdir(parents=True, exist_ok=False)
    common = ["--network", "reddb-kv-20261020"]
    docker("create", "--name", server, *common, "--cpus", "4", "--cpuset-cpus", "0-3",
           "--memory", "2g", "--cap-add", "SYS_PTRACE", "--security-opt", "seccomp=unconfined",
           "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
           "--mount", f"type=volume,src=reddb-arch-final-data-{mode}-20261020,dst=/data",
           "--mount", f"type=bind,src={out},dst=/results",
           "-e", "REDDB_MEMORY_BUDGET=1073741824", "-e", "REDDB_RUNTIME_RESULT_CACHE_ENABLED=false",
           "-e", "RUST_MIN_STACK=8388608", "-e", f"REDDB_DURABILITY={durability}",
           "-e", f"REDDB_DURABILITY_MODE={durability}", "-e", "REDDB_GROUP_COMMIT_WINDOW_US=200",
           "reddb-query-perf-tools:20261020", BINARY.replace("/work/", "/build/"),
           "server", "--path", "/data/bench.rdb", "--http-bind", "0.0.0.0:5000",
           "--workers", "4", "--http-max-handlers", "64", "--no-auth")
    docker("create", "--name", client, *common, "--cpus", "2", "--cpuset-cpus", "4-5",
           "--memory", "1g", "--mount", f"type=bind,src={REPO / 'bench'},dst=/bench,readonly",
           "--mount", f"type=bind,src={out},dst=/results",
           "sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258",
           "sleep", "infinity")
    try:
        subprocess.run([sys.executable, str(REPO / "bench/query/write_profile.py"),
                        "--server", server, "--client", client, "--out", str(out),
                        "--engine-revision", REVISION, "--binary-sha256", sha], check=True)
    finally:
        docker("stop", server, client)
