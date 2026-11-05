"""Run checked sync profiles only after the final read capture has stopped."""
import json
import subprocess
import sys
import time
from pathlib import Path

root = Path(__file__).resolve().parent
deadline = time.monotonic() + 1200
while True:
    try:
        manifest = json.loads((root / "final" / "manifest.json").read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        manifest = {"status": "running"}
    if manifest["status"] != "running":
        if manifest["status"] != "valid-diagnostic":
            raise RuntimeError("read capture failed; do not overlap or hide failed measurements")
        break
    if time.monotonic() >= deadline:
        raise RuntimeError("read capture did not finish")
    time.sleep(1)

engines = {
    "main": ("0eb3a89cf3559b9408de6a935bfbca57bcf504ae", "6afcf03b3cc7aa42d693f02bdf653f7ca1cda2b9e3e125940b8e4551c8626c56"),
    "pr": ("2584e4841ae5df19be09704f05cdc0f1824d6fb9", "9b015e9f6d51cd8a3e93ff2af75fa2487cfc7ce4da9a94df842b30562a20d0dd"),
}
for case in ["main-strict", "pr-strict", "main-grouped", "pr-grouped"]:
    revision, sha = engines[case.split("-")[0]]
    print("Profile", case, flush=True)
    subprocess.run([
        sys.executable, str(root.parents[1] / "query" / "write_profile.py"),
        "--server", f"reddb-arch-write-{case}-20261020",
        "--client", f"reddb-arch-write-client-{case}-20261020",
        "--out", str(root / "writes" / case),
        "--engine-revision", revision, "--binary-sha256", sha,
    ], check=True)
