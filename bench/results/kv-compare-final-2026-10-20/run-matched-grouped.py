"""Append grouped writes with the same background key population as Strict.

Run after run.py finishes. Preserves its fresh-1k grouped diagnostic separately.
"""
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
SERVER = "reddb-kv-final-red-grouped-matched-20261020"
CLIENT = "reddb-kv-final-client-20261020"
manifest = json.loads((ROOT / "manifest.json").read_text())
assert manifest["status"] == "valid-diagnostic"
assert not any("grouped-matched" in case["file"] for case in manifest["cases"])


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def ready():
    probe = ("import sys;sys.path.insert(0,'/bench');"
             f"from compare import RedClient;c=RedClient('{SERVER}',5000,'probe');"
             "assert c.request('GET','/health/live');c.close()")
    for _ in range(30):
        result = subprocess.run(["docker", "exec", CLIENT, "python", "-c", probe],
                                capture_output=True, text=True)
        if result.returncode == 0:
            return
        time.sleep(1)
    raise RuntimeError("matched grouped server did not become ready")


docker("create", "--name", SERVER, "--network", "reddb-kv-20261020", "--cpus", "4",
       "--cpuset-cpus", "0-3", "--memory", "2g",
       "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
       "--mount", "type=volume,src=reddb-kv-final-data-grouped-matched-20261020,dst=/data",
       "-e", "REDDB_MEMORY_BUDGET=1073741824", "-e", "REDDB_RUNTIME_RESULT_CACHE_ENABLED=false",
       "-e", "RUST_MIN_STACK=8388608", "-e", "REDDB_DURABILITY=wal_durable_grouped",
       "-e", "REDDB_DURABILITY_MODE=wal_durable_grouped", "-e", "REDDB_GROUP_COMMIT_WINDOW_US=200",
       "reddb-query-perf-tools:20261020", "/build/architecture-pr-fae39eb6-red", "server",
       "--path", "/data/bench.rdb", "--http-bind", "0.0.0.0:5000", "--workers", "4",
       "--http-max-handlers", "64", "--no-auth")
try:
    docker("start", CLIENT, SERVER)
    ready()
    for keys in [1000, 10000]:
        subprocess.run(["docker", "exec", CLIENT, "python", "/bench/compare.py", "--engine", "reddb-rest",
                        "--host", SERVER, "--port", "5000", "--collection", f"kv_diag_{keys}",
                        "--keys", str(keys), "--prepare-only",
                        "--out", f"/results/grouped-matched-background-{keys}.json"], check=True)
    docker("restart", SERVER)
    ready()
    for keys in [1000, 10000]:
        subprocess.run(["docker", "exec", CLIENT, "python", "/bench/compare.py", "--engine", "reddb-rest",
                        "--host", SERVER, "--port", "5000", "--collection", f"kv_diag_{keys}",
                        "--keys", str(keys), "--verify-only",
                        "--out", f"/results/grouped-matched-background-{keys}-reopen.json"], check=True)
    subprocess.run([sys.executable, str(REPO / "bench/kv/local_matrix.py"), "--client", CLIENT,
                    "--redis", "reddb-kv-final-redis-20261020", "--kividb", "reddb-kv-final-kivi-20261020",
                    "--reddb", SERVER, "--out", str(ROOT),
                    "--engine-revision", manifest["red_engine_revision"],
                    "--engine-sha256", manifest["red_binary_sha256"],
                    "--resume", "--only", "reddb-rest", "--only-put", "--label", "grouped-matched"], check=True)
finally:
    docker("stop", SERVER, CLIENT)
for path in ROOT.rglob("*"):
    if path.is_file() and "__pycache__" not in path.parts:
        path.write_bytes(path.read_bytes().replace(b"\r\n", b"\n"))
subprocess.run([sys.executable, str(REPO / "bench/kv/summarize.py"), str(ROOT)], check=True)
assert json.loads((ROOT / "manifest.json").read_text())["status"] == "valid-diagnostic"
