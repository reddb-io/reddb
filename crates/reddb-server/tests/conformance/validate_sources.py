#!/usr/bin/env python3
"""Validate conformance case source=file:line references."""

from __future__ import annotations

import pathlib
import sys
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[4]
CASE_DIR = pathlib.Path(__file__).resolve().parent


def validate_source(source: str) -> str | None:
    if source.startswith("proptest-regression:"):
        return None

    if ":" not in source:
        return "expected source in file:line form"

    file_part, line_part = source.rsplit(":", 1)
    try:
        line = int(line_part)
    except ValueError:
        return f"source line is not numeric: {line_part!r}"

    if line <= 0:
        return "source line must be 1-based"

    path = ROOT / file_part
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        return f"cannot read {path}: {exc}"

    line_count = len(text.splitlines())
    if line > line_count:
        return f"source line {line} is past end of {path} ({line_count} lines)"

    return None


def main() -> int:
    failures: list[str] = []
    case_paths = sorted(CASE_DIR.rglob("*.toml"))
    for case_path in case_paths:
        with case_path.open("rb") as handle:
            case = tomllib.load(handle)
        error = validate_source(str(case.get("source", "")))
        if error is not None:
            failures.append(f"{case_path.name}: {case.get('source')!r}: {error}")

    if failures:
        print("conformance source reference failures:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(f"validated {len(case_paths)} conformance source references")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
