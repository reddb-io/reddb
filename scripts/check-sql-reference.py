#!/usr/bin/env python3
"""Keep generated parts of docs/reference/sql-1-0-x.md in sync."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOC = ROOT / "docs/reference/sql-1-0-x.md"
LEXER = ROOT / "crates/reddb-server/src/storage/query/lexer.rs"
SQL = ROOT / "crates/reddb-server/src/storage/query/sql.rs"


def rust_strings(text: str) -> list[str]:
    return re.findall(r'"([A-Z][A-Z0-9_]*)"', text)


def lexer_keywords() -> list[str]:
    text = LEXER.read_text()
    match = re.search(
        r"match value\.to_uppercase\(\)\.as_str\(\) \{(?P<body>.*?)_\s*=>\s*Token::Ident",
        text,
        re.S,
    )
    if not match:
        raise SystemExit("could not find lexer keyword table")
    return sorted(set(rust_strings(match.group("body"))))


def top_level_sql_commands() -> list[str]:
    text = SQL.read_text()
    matches = list(
        re.finditer(
            r"other\s*=>\s*Err\(ParseError::expected\(\s*vec!\[(?P<body>.*?)\],\s*other,",
            text,
            re.S,
        )
    )
    if not matches:
        raise SystemExit("could not find parse_sql_command top-level expected list")
    return rust_strings(matches[-1].group("body"))


def render_inline_code_list(values: list[str]) -> str:
    return ", ".join(f"`{value}`" for value in values)


def replace_block(doc: str, name: str, body: str) -> str:
    start = f"<!-- generated:{name} begin -->"
    end = f"<!-- generated:{name} end -->"
    pattern = re.compile(
        rf"{re.escape(start)}\n.*?\n{re.escape(end)}",
        re.S,
    )
    replacement = f"{start}\n{body}\n{end}"
    new_doc, count = pattern.subn(replacement, doc)
    if count != 1:
        raise SystemExit(f"could not replace generated block {name!r}")
    return new_doc


def rendered_doc() -> str:
    doc = DOC.read_text()
    doc = replace_block(
        doc,
        "lexer-keywords",
        render_inline_code_list(lexer_keywords()),
    )
    doc = replace_block(
        doc,
        "top-level-sql",
        render_inline_code_list(top_level_sql_commands()),
    )
    return doc


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="rewrite generated doc blocks")
    args = parser.parse_args()

    expected = rendered_doc()
    current = DOC.read_text()
    if args.write:
        DOC.write_text(expected)
        return 0
    if current != expected:
        print("docs/reference/sql-1-0-x.md generated blocks are stale")
        print("run: python3 scripts/check-sql-reference.py --write")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
