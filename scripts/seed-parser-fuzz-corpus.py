#!/usr/bin/env python3
"""Seed cargo-fuzz parser corpora from conformance TOML cases."""

from __future__ import annotations

import hashlib
import pathlib
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[1]
CONFORMANCE_DIR = ROOT / "crates" / "reddb-server" / "tests" / "conformance"
CORPUS_DIR = ROOT / "fuzz" / "corpus"

MIGRATION_PREFIXES = (
    "CREATE MIGRATION ",
    "APPLY MIGRATION ",
    "ROLLBACK MIGRATION ",
    "EXPLAIN MIGRATION ",
)

CONN_STRING_SEEDS = [
    "red://localhost:5050",
    "red://admin:secret@localhost:5050/default",
    "red://primary.svc:5050?tls=true&tenant=acme",
    "red://host-a:5050,host-b:5050/app?mode=cluster",
]


def expanded_input(case: dict[str, object]) -> str:
    raw = str(case["input"])
    repeat_count = int(case.get("input_repeat_count", 1))
    prefix = str(case.get("input_prefix", ""))
    suffix = str(case.get("input_suffix", ""))
    return prefix + (raw * repeat_count) + suffix


def write_seed(target: str, data: str) -> None:
    target_dir = CORPUS_DIR / target
    target_dir.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256(data.encode("utf-8")).hexdigest()[:24]
    path = target_dir / digest
    if not path.exists():
        path.write_text(data, encoding="utf-8")


def main() -> int:
    sql_count = 0
    migration_count = 0

    for case_path in sorted(CONFORMANCE_DIR.rglob("*.toml")):
        with case_path.open("rb") as handle:
            case = tomllib.load(handle)
        if case.get("kind") != "positive":
            continue
        data = expanded_input(case)
        write_seed("sql_parser", data)
        sql_count += 1
        if data.upper().startswith(MIGRATION_PREFIXES):
            write_seed("migration_parser", data)
            migration_count += 1

    for seed in CONN_STRING_SEEDS:
        write_seed("conn_string_parser", seed)

    print(
        "seeded parser fuzz corpus: "
        f"sql_parser={sql_count}, "
        f"migration_parser={migration_count}, "
        f"conn_string_parser={len(CONN_STRING_SEEDS)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
