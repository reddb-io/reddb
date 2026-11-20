#!/usr/bin/env python3
"""Dead-code ratchet: suppressed dead code may only shrink.

Counts `allow(dead_code)` and `#[deprecated` in the Rust sources and compares
them with `.github/dead-code-baseline.json`. A higher count fails. A lower
count also fails until the baseline is lowered in the same change, so the
ratchet never slackens.
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
BASELINE = ROOT / ".github" / "dead-code-baseline.json"
SOURCE_ROOTS = ("crates", "src", "tests", "examples", "bench")
PATTERNS = {
    "allow_dead_code": re.compile(r"allow\s*\(\s*[^)]*\bdead_code\b"),
    "deprecated": re.compile(r"#\s*\[\s*deprecated\b"),
}


def count(root: pathlib.Path = ROOT) -> dict:
    totals = {name: 0 for name in PATTERNS}
    for source_root in SOURCE_ROOTS:
        base = root / source_root
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            if "target" in path.parts:
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            for name, pattern in PATTERNS.items():
                totals[name] += len(pattern.findall(text))
    return totals


def verdict(current: dict, baseline: dict) -> list:
    problems = []
    for name in PATTERNS:
        have, allowed = current[name], baseline[name]
        if have > allowed:
            problems.append(
                f"{name}: {have} > baseline {allowed}. Remove the dead code instead of suppressing it."
            )
        elif have < allowed:
            problems.append(
                f"{name}: {have} < baseline {allowed}. Lower {BASELINE.name} to {have} in this change."
            )
    return problems


def main() -> int:
    current = count()
    if "--print" in sys.argv:
        print(json.dumps(current, indent=2, sort_keys=True))
        return 0
    problems = verdict(current, json.loads(BASELINE.read_text()))
    for problem in problems:
        print(f"::error::dead-code ratchet: {problem}")
    if not problems:
        print(f"dead-code ratchet ok: {current}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
