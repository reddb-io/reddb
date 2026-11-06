#!/usr/bin/env python3
"""Run Hunter/Red Router regression contracts, reporting every failing lane.

Uses only temporary databases and loopback listeners. No running application,
external server, credential, or real customer database is required.
"""
from pathlib import Path
import subprocess
import sys


def main():
    root = Path(__file__).resolve().parents[1]
    commands = [
        ["cargo", "test", "--locked", "-p", "reddb-io-client", "--features", "redwire",
         "--test", "embedded_result_contract", "--test", "redwire_cancellation_contract",
         "--no-fail-fast", "--", "--nocapture"],
        ["cargo", "test", "--locked", "-p", "reddb-io-server", "--test", "consumer_incidents",
         "--", "--nocapture"],
        ["cargo", "test", "--locked", "-p", "reddb-io-server", "--lib",
         "runtime::statement_frame::tests::", "--", "--nocapture"],
        ["cargo", "test", "--locked", "-p", "reddb-io-engine", "--test", "portable",
         "portable_snapshot_conformance", "--", "--nocapture"],
    ]
    failed = []
    for command in commands:
        print("\nRunning: " + " ".join(command), flush=True)
        try:
            code = subprocess.run(command, cwd=root, timeout=1800).returncode
        except subprocess.TimeoutExpired:
            code = 124
        if code:
            failed.append((command, code))
    for command, code in failed:
        print(f"FAILED ({code}): {' '.join(command)}", flush=True)
    print(f"{len(commands) - len(failed)}/{len(commands)} lanes passed", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
