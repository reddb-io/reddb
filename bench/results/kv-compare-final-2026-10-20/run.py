"""Repeat the checked Redis/KiviDB comparison against the final Rust revision.

Requires the existing immutable RedDB build volume, pinned cached images and
internal network. Uses fresh containers/volumes; refuses existing captures.
"""
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
REVISION = "fae39eb6d07c32f8bc734973f10a9349a8e56a45"
BINARY = "/build/architecture-pr-fae39eb6-red"
SHA256 = "3337efdfdd686fafca7d9d02fc82f53c18bc589824f391300946256c45ac1adf"
CLIENT_IMAGE = "sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258"
REDIS_IMAGE = "sha256:c22af04bb576503bf16b3e34a1fd2fd82de0f765afd866d2e380145e0af30d78"
KIVI_IMAGE = "sha256:9c7cda22f6aa7924e2c28a7605e765469249c389f5388b7dff9cd56a35d18c2d"
PREFIX = "reddb-kv-final"
NAMES = {key: f"{PREFIX}-{key}-20261020" for key in ["redis", "kivi", "red", "red-grouped", "client"]}


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


if (ROOT / "manifest.json").exists():
    raise RuntimeError("capture exists; never overwrite completed or failed samples")
subprocess.run(["git", "diff", REVISION, "HEAD", "--exit-code", "--", "crates", "src",
                "Cargo.toml", "Cargo.lock"], cwd=REPO, check=True)
observed = docker("run", "--rm", "--name", f"{PREFIX}-identity-20261020",
                  "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
                  CLIENT_IMAGE, "python", "-c",
                  f"import hashlib;print(hashlib.sha256(open('{BINARY}','rb').read()).hexdigest())")
assert observed == SHA256, "immutable engine identity differs"
common = ["--network", "reddb-kv-20261020", "--cpus", "4", "--cpuset-cpus", "0-3", "--memory", "2g"]
docker("create", "--name", NAMES["redis"], *common,
       "--mount", f"type=volume,src={PREFIX}-data-redis-20261020,dst=/data", REDIS_IMAGE,
       "redis-server", "--save", "", "--appendonly", "yes", "--appendfsync", "always",
       "--protected-mode", "no", "--io-threads", "4", "--maxmemory", "1073741824",
       "--maxmemory-policy", "noeviction")
docker("create", "--name", NAMES["kivi"], *common,
       "--mount", f"type=volume,src={PREFIX}-data-kivi-20261020,dst=/home/kividb", KIVI_IMAGE,
       "--aof", "--threads", "4", "--io-model", "tokio", "--protected-mode", "no",
       "--update-check", "no", "--maxmemory", "1073741824")
for key, mode in [("red", "strict"), ("red-grouped", "wal_durable_grouped")]:
    docker("create", "--name", NAMES[key], *common,
           "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
           "--mount", f"type=volume,src={PREFIX}-data-{key}-20261020,dst=/data",
           "-e", "REDDB_MEMORY_BUDGET=1073741824", "-e", "REDDB_RUNTIME_RESULT_CACHE_ENABLED=false",
           "-e", "RUST_MIN_STACK=8388608", "-e", f"REDDB_DURABILITY={mode}",
           "-e", f"REDDB_DURABILITY_MODE={mode}", "-e", "REDDB_GROUP_COMMIT_WINDOW_US=200",
           "reddb-query-perf-tools:20261020", BINARY, "server", "--path", "/data/bench.rdb",
           "--http-bind", "0.0.0.0:5000", "--workers", "4", "--http-max-handlers", "64", "--no-auth")
docker("create", "--name", NAMES["client"], "--network", "reddb-kv-20261020",
       "--cpus", "2", "--cpuset-cpus", "4-5", "--memory", "1g",
       "--mount", f"type=bind,src={REPO / 'bench/kv'},dst=/bench,readonly",
       "--mount", f"type=bind,src={ROOT},dst=/results", CLIENT_IMAGE, "sleep", "infinity")
provenance = {"binary_revision": REVISION, "binary_sha256": SHA256,
              "checkout_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
              "rust_source_equivalence": "git diff of crates/src and Cargo manifests/lock is empty",
              "classification": "shared-host product-path diagnostic; one process matrix per engine, five rounds/cell",
              "versions": {"redis": "8.4.0", "kividb": "1.1.0"},
              "grouped_continuation": {"window_us": 200, "scope": "durable overwrite only, fresh volume, both sync barriers"},
              "no_instrumentation": "No strace/perf attached to timed workloads"}
(ROOT / "provenance.json").write_bytes((json.dumps(provenance, indent=2) + "\n").encode())
base = [sys.executable, str(REPO / "bench/kv/local_matrix.py"), "--client", NAMES["client"],
        "--redis", NAMES["redis"], "--kividb", NAMES["kivi"], "--out", str(ROOT),
        "--engine-revision", REVISION, "--engine-sha256", SHA256]
try:
    docker("start", NAMES["client"])
    subprocess.run([*base, "--reddb", NAMES["red"]], check=True)
    docker("start", NAMES["client"])
    subprocess.run([*base, "--reddb", NAMES["red-grouped"], "--resume", "--only", "reddb-rest",
                    "--only-put", "--label", "grouped"], check=True)
finally:
    docker("stop", *NAMES.values())
for path in ROOT.rglob("*"):
    if path.is_file() and "__pycache__" not in path.parts:
        path.write_bytes(path.read_bytes().replace(b"\r\n", b"\n"))
subprocess.run([sys.executable, str(REPO / "bench/kv/summarize.py"), str(ROOT)], check=True)
manifest = json.loads((ROOT / "manifest.json").read_text())
assert manifest["status"] == "valid-diagnostic", manifest["errors"]
for case in manifest["cases"]:
    result = json.loads((ROOT / case["file"]).read_text())
    assert result["status"] in ["prepared", "valid-diagnostic", "unsupported"], case
    if "recovery" in case["file"]:
        assert result["verification"]["keys_checked"] == result["config"]["keys"], case
print("Closed matrix: all measured responses and recovery verified", flush=True)
