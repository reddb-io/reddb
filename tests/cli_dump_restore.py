"""CLI regressions against an explicitly built binary.

Run: REDDB_BINARY_PATH=/path/to/red python3 tests/cli_dump_restore.py
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class DumpRestore(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="reddb-dump-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.binary = os.environ["REDDB_BINARY_PATH"]

    def cli(self, *args, success=True):
        result = subprocess.run([self.binary, *map(str, args)], capture_output=True,
                                text=True, timeout=60)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def dump(self, database, name):
        output = self.root / name
        self.cli("dump", "--path", database, "-o", output)
        return output, [json.loads(line) for line in output.read_text().splitlines()]

    def test_values_survive_dump_restore_and_reopen(self):
        source = self.root / "source.rdb"
        self.cli("query", "--path", source,
                 "CREATE TABLE roundtrip (id INTEGER, payload TEXT, enabled BOOLEAN, absent TEXT)")
        payload = "aspas ' e \"; barra \\n; ação\nlinha"
        self.cli("query", "--path", source,
                 "INSERT INTO roundtrip (id,payload,enabled,absent) VALUES ($1,$2,$3,$4)",
                 "-p", "9223372036854775807", "--param-type", "int",
                 "-p", payload, "--param-type", "text", "-p", "true", "-p", "null")
        dump, records = self.dump(source, "source.jsonl")
        row = next(record["fields"] for record in records if record.get("collection") == "roundtrip")
        self.assertEqual(row, {"id": {"$int": "9223372036854775807"},
                               "payload": payload, "enabled": True, "absent": None})
        self.assertFalse(any(record.get("collection", "").startswith(("red.", "__red_schema_"))
                             or record.get("collection") == "red" for record in records))
        target = self.root / "target.rdb"
        self.cli("restore", "--path", target, "-i", dump)
        _, restored = self.dump(target, "restored.jsonl")
        self.assertEqual([r for r in restored if r.get("collection") == "roundtrip"],
                         [r for r in records if r.get("collection") == "roundtrip"])

    def test_partial_restore_fails_in_text_and_json_modes(self):
        source = self.root / "invalid.jsonl"
        source.write_text('{"collection":"accepted","fields":{"id":1}}\nnot JSON\n')
        for flags in [[], ["--json"]]:
            result = self.cli("restore", "--path", self.root / f"partial{len(flags)}.rdb",
                              "-i", source, *flags, success=False)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("line 2", result.stderr)
            if flags:
                self.assertFalse(json.loads(result.stderr.splitlines()[-1])["ok"])


if __name__ == "__main__":
    unittest.main()
