#!/usr/bin/env python3
"""Package a release-profile qualifier without running it or accessing a database."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


def prepare(messages, destination):
    candidates = []
    for line in messages.read_text().splitlines():
        item = json.loads(line)
        if (
            item.get("reason") == "compiler-artifact"
            and item.get("target", {}).get("name") == "reddb_server"
            and item.get("profile", {}).get("test")
            and item.get("executable")
        ):
            candidates.append(item)
    if len(candidates) != 1:
        raise RuntimeError("expected exactly one server library test executable")
    artifact = candidates[0]
    profile = artifact["profile"]
    if profile["opt_level"] != "3" or profile["debug_assertions"]:
        raise RuntimeError("qualification requires release opt-level 3 without debug assertions")

    root = Path(__file__).resolve().parent.parent
    target = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    if not target.is_absolute():
        target = root / target
    executable = Path(artifact["executable"]).resolve()
    if not executable.is_relative_to((target / "release" / "deps").resolve()):
        raise RuntimeError("the executable must come from the release dependency directory")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=root):
        raise RuntimeError("qualification artifact requires a clean committed source tree")
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if os.environ.get("GITHUB_SHA", commit) != commit:
        raise RuntimeError("checkout differs from the workflow source commit")

    destination.mkdir(parents=True, exist_ok=False)
    binary = destination / "qualifier"
    shutil.copyfile(executable, binary)
    binary.chmod(0o755)
    subprocess.run(["strip", "--strip-unneeded", str(binary)], check=True)
    listing = subprocess.check_output([str(binary), "--list"], text=True)
    required = (
        "presentation::entity_json::historical_copy_qualification::"
        "complete_runtime_historical_copy_preserves_sql_results: test"
    )
    if required not in listing:
        raise RuntimeError("the executable lacks the real historical runtime qualification")

    compiler = subprocess.check_output(["rustc", "+1.95.0", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in compiler.splitlines()
                if line.startswith("host: "))
    if host != "x86_64-unknown-linux-gnu":
        raise RuntimeError("this qualification artifact requires the Linux x86_64 GNU target")
    digest = hashlib.file_digest(binary.open("rb"), "sha256").hexdigest()
    module = root / "crates/reddb-server/src/presentation/entity_json/historical_copy_qualification.rs"
    manifest = {
        "sourceCommit": commit,
        "profile": "release",
        "cargoProfile": profile,
        "features": artifact["features"],
        "rustc": compiler,
        "target": host,
        "binarySha256": digest,
        "binaryBytes": binary.stat().st_size,
        "qualificationModuleSha256": hashlib.sha256(module.read_bytes()).hexdigest(),
        "cargoMessagesSha256": hashlib.sha256(messages.read_bytes()).hexdigest(),
        "testHarness": True,
        "databaseAccessDuringBuild": False,
        "productionFactoryActivation": False,
    }
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (destination / "qualifier.sha256").write_text(digest + "  qualifier\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo-messages", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    prepare(args.cargo_messages, args.destination)
