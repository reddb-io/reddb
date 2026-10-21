"""Untraced populated-store PUT comparison across repeated WAL rollovers.

Requires the pinned images/build volume and the stopped, verified 12k-key
fixture from kv-compare-final-2026-10-20. Refuses existing names/captures.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
sys.path.insert(0, str(REPO / "bench/kv"))
from local_matrix import command, inspect, Resources
from summarize import summarize_case

args_parser = argparse.ArgumentParser(description=__doc__)
args_parser.add_argument("--after-revision", required=True)
args_parser.add_argument("--after-binary", required=True, help="immutable filename in build volume")
args_parser.add_argument("--after-sha256", required=True)
args = args_parser.parse_args()
BEFORE_REVISION = "fae39eb6d07c32f8bc734973f10a9349a8e56a45"
BEFORE_BINARY = "architecture-pr-fae39eb6-red"
BEFORE_SHA = "3337efdfdd686fafca7d9d02fc82f53c18bc589824f391300946256c45ac1adf"
CLIENT_IMAGE = "sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258"
TOOLS_IMAGE = "reddb-query-perf-tools:20261020"
ORDER = ["before-strict", "after-strict", "after-grouped", "before-grouped"]
NAMES = {case: (f"reddb-wal-{case}-20261021", f"reddb-wal-client-{case}-20261021") for case in ORDER}
VOLUMES = {case: f"reddb-wal-data-{case}-20261021" for case in ORDER}
if (ROOT / "manifest.json").exists():
    raise RuntimeError("capture already exists; preserve completed and failed samples")
subprocess.run(["git", "diff", args.after_revision, "HEAD", "--exit-code", "--", "crates", "src",
                "Cargo.toml", "Cargo.lock"], cwd=REPO, check=True)
assert command("docker", "inspect", "reddb-kv-final-red-20261020", "--format", "{{.State.Running}}") == "false"
existing = set(command("docker", "ps", "-a", "--format", "{{.Names}}").splitlines())
assert not existing.intersection(name for pair in NAMES.values() for name in pair)
assert not set(command("docker", "volume", "ls", "--format", "{{.Name}}").splitlines()).intersection(VOLUMES.values())
manifest = {"schema": "reddb.wal-pressure.v1", "status": "running", "started_unix_ms": int(time.time() * 1000),
            "classification": "shared-host diagnostic; fixed case order; five rounds/process; no leadership interval",
            "primary_metric": "checked HTTP PUT p99 completion latency, pipeline 1, 8 connections",
            "conditions": {"background_keys": 11000, "write_keys": 1000, "value_bytes": 100,
                           "workers": 8, "rounds": 5, "operations_per_round": 10000,
                           "cache": "disabled", "group_window_us": 200, "server_cpus": "0-3 / 4 CPUs",
                           "client_cpus": "4-5 / 2 CPUs", "memory": "server 2GiB / budget 1GiB; client 1GiB",
                           "order": ORDER, "artifact_telemetry": "checksummed roots and snapshot SHA256 outside timed requests",
                           "recovery": "SIGKILL after each full cell; verify write keys and all background keys",
                           "fixture_source": "stopped reddb-kv-final-data-red-20261020/bench.rdb"},
            "cases": [], "containers": {}, "errors": []}


def save():
    (ROOT / "manifest.json").write_bytes((json.dumps(manifest, indent=2) + "\n").encode())


def ready(server, client):
    probe = ("import sys;sys.path.insert(0,'/bench');from compare import RedClient;"
             f"c=RedClient('{server}',5000,'probe');print(c.request('GET','/health/live'));c.close()")
    for _ in range(30):
        try:
            command("docker", "exec", client, "python", "-c", probe, timeout=30)
            return
        except RuntimeError:
            time.sleep(1)
    raise RuntimeError("server did not become ready")


def run(server, client, name, collection="kv_durable", keys=1000, *extra):
    parts = ["docker", "exec", client, "python", "/bench/compare.py", "--engine", "reddb-rest",
             "--host", server, "--port", "5000", "--collection", collection, "--keys", str(keys),
             "--value-bytes", "100", "--workers", "8", "--rounds", "5", "--warmup", "10",
             "--operations", "10000", "--profile", server, "--out", f"/results/{name}.json", *extra]
    with (ROOT / f"{name}.stdout.log").open("w") as output:
        process = subprocess.Popen(parts, stdout=output, stderr=subprocess.STDOUT, text=True)
        try:
            code = process.wait(timeout=1200)
        except BaseException:
            process.kill()
            process.wait()
            raise
    if code:
        raise RuntimeError(f"{name} failed; preserve its JSON and stdout log")
    result = json.loads((ROOT / f"{name}.json").read_text())
    assert result["status"] in ["prepared", "valid-diagnostic"], name
    print(name, result["status"], result.get("median_throughput_ops_s", ""), flush=True)
    return result


created = []
try:
    for version, binary, sha in [("before", BEFORE_BINARY, BEFORE_SHA),
                                  ("after", args.after_binary, args.after_sha256)]:
        observed = command("docker", "run", "--rm", "--name", f"reddb-wal-identity-{version}-20261021",
                           "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
                           TOOLS_IMAGE, "sha256sum", f"/build/{binary}")
        assert observed.split()[0] == sha, version
    for case in ORDER:
        server, client = NAMES[case]
        is_before = case.startswith("before")
        binary, revision, sha = (BEFORE_BINARY, BEFORE_REVISION, BEFORE_SHA) if is_before else (
            args.after_binary, args.after_revision, args.after_sha256)
        mode = "strict" if case.endswith("strict") else "wal_durable_grouped"
        clone_sha = command("docker", "run", "--rm", "--name", f"reddb-wal-copy-{case}-20261021",
                            "--mount", "type=volume,src=reddb-kv-final-data-red-20261020,dst=/source,readonly",
                            "--mount", f"type=volume,src={VOLUMES[case]},dst=/data", TOOLS_IMAGE,
                            "sh", "-c", "cp /source/bench.rdb /data/bench.rdb && sha256sum /data/bench.rdb").split()[0]
        if manifest["cases"]:
            assert clone_sha == manifest["cases"][0]["fixture_sha256"]
        common = ["--network", "reddb-kv-20261020", "--cpus", "4", "--cpuset-cpus", "0-3", "--memory", "2g"]
        command("docker", "create", "--name", server, *common,
                "--mount", "type=volume,src=reddb-kv-build-20261020,dst=/build,readonly",
                "--mount", f"type=volume,src={VOLUMES[case]},dst=/data",
                "-e", "REDDB_MEMORY_BUDGET=1073741824", "-e", "REDDB_RUNTIME_RESULT_CACHE_ENABLED=false",
                "-e", "RUST_MIN_STACK=8388608", "-e", f"REDDB_DURABILITY={mode}",
                "-e", f"REDDB_DURABILITY_MODE={mode}", "-e", "REDDB_GROUP_COMMIT_WINDOW_US=200",
                TOOLS_IMAGE, f"/build/{binary}", "server", "--path", "/data/bench.rdb",
                "--http-bind", "0.0.0.0:5000", "--workers", "4", "--http-max-handlers", "64", "--no-auth")
        created.append(server)
        command("docker", "create", "--name", client, "--network", "reddb-kv-20261020", "--cpus", "2",
                "--cpuset-cpus", "4-5", "--memory", "1g", "--mount", f"type=bind,src={REPO / 'bench/kv'},dst=/bench,readonly",
                "--mount", f"type=bind,src={ROOT},dst=/results",
                "--mount", f"type=volume,src={VOLUMES[case]},dst=/artifact,readonly", CLIENT_IMAGE, "sleep", "infinity")
        created.append(client)
        manifest["containers"].update({name: inspect(name) for name in [server, client]})
        manifest["cases"].append({"case": case, "revision": revision, "binary_sha256": sha,
                                  "server": server, "client": client, "file": f"{case}.json",
                                  "fixture_sha256": clone_sha})
        save()
    for case in ORDER:
        server, client = NAMES[case]
        monitor = None
        try:
            command("docker", "start", client, server)
            ready(server, client)
            for keys in [1000, 10000]:
                run(server, client, f"{case}-background-{keys}", f"kv_diag_{keys}", keys, "--verify-only")
            run(server, client, f"{case}-seed", "kv_durable", 1000, "--prepare-only")
            monitor = Resources(ROOT, server, client)
            result = run(server, client, case, "kv_durable", 1000, "--operation", "put",
                         "--artifact-state-path", "/artifact/bench.rdb")
            monitor.close()
            monitor = None
            assert len(result["rounds"]) == 5 and all(r["operations"] == 10000 for r in result["rounds"])
            changes = sum(r["artifact_before"]["snapshot_sha256"] != r["artifact_after"]["snapshot_sha256"] for r in result["rounds"])
            assert changes >= 3, f"{case}: insufficient measured checkpoint crossings"
            command("docker", "kill", server)
            command("docker", "start", server)
            ready(server, client)
            run(server, client, f"{case}-recovery", "kv_durable", 1000, "--verify-only", "--recovery-from", f"/results/{case}.json")
            for keys in [1000, 10000]:
                run(server, client, f"{case}-background-{keys}-recovery", f"kv_diag_{keys}", keys, "--verify-only")
        finally:
            if monitor:
                monitor.close()
            command("docker", "stop", server, client)
    manifest["status"] = "valid-diagnostic"
except BaseException as error:
    manifest["status"] = "invalid"
    manifest["errors"].append(str(error))
    raise
finally:
    manifest["finished_unix_ms"] = int(time.time() * 1000)
    save()
    if created:
        command("docker", "stop", *created)
    for path in ROOT.iterdir():
        if path.is_file():
            path.write_bytes(path.read_bytes().replace(b"\r\n", b"\n"))
    events = [json.loads(line) for p in ROOT.glob("resources-*.jsonl") for line in p.read_text().splitlines() if line]
    measurements = []
    for case in manifest["cases"]:
        if (ROOT / case["file"]).exists():
            result = json.loads((ROOT / case["file"]).read_text())
            value = summarize_case(result, events, case["server"], case["client"])
            value.update(case=case["case"], file=case["file"], binary_sha256=case["binary_sha256"])
            measurements.append(value)
    checksums = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(ROOT.iterdir())
                 if p.is_file() and p.name != "summary.json"}
    (ROOT / "summary.json").write_bytes((json.dumps({"schema": "reddb.wal-pressure-summary.v1",
        "status": manifest["status"], "aggregation": "median/range of five correlated round samples per case; no confidence interval",
        "measurements": measurements, "artifact_sha256": checksums}, indent=2) + "\n").encode())
