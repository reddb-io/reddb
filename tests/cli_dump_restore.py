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

    def test_restore_rejects_identifiers_containing_sql(self):
        source = self.root / "identifiers.jsonl"
        source.write_text(json.dumps({"collection": "rows; DROP TABLE protected",
                                      "fields": {"id": 1}}) + "\n")
        result = self.cli("restore", "--path", self.root / "identifiers.rdb", "-i", source,
                          success=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported collection identifier", result.stderr)

    def test_restore_exact_number_envelopes(self):
        fields = {"signed": {"$int": "9223372036854775807"},
                  "unsigned": {"$uint": "18446744073709551615"},
                  "amount": {"$decimal": "1234567890.123456789"}}
        source = self.root / "numbers.jsonl"
        source.write_text(json.dumps({"collection": "exact_numbers", "fields": fields}) + "\n")
        target = self.root / "numbers.rdb"
        self.cli("restore", "--path", target, "-i", source)
        _, records = self.dump(target, "numbers-restored.jsonl")
        self.assertEqual([r["fields"] for r in records if r.get("collection") == "exact_numbers"],
                         [fields])

    def test_selected_collection_retains_config_and_override_preserves_it(self):
        source = self.root / "config-source.rdb"
        self.cli("query", "--path", source, "SET CONFIG red.config.demo.enabled = true")
        self.cli("query", "--path", source, "SET CONFIG red.config.demo.enabled = false")
        self.cli("query", "--path", source, "INSERT INTO original (id) VALUES (1)")
        dump = self.root / "selected.jsonl"
        self.cli("dump", "--path", source, "--collection", "original", "-o", dump)
        records = [json.loads(line) for line in dump.read_text().splitlines()]
        self.assertEqual({r.get("collection") for r in records}, {"original", "red_config"})
        target = self.root / "config-target.rdb"
        self.cli("restore", "--path", target, "--collection", "renamed", "-i", dump)
        _, restored = self.dump(target, "config-restored.jsonl")
        def custom_config(records):
            return [r for r in records if r.get("collection") == "red_config"
                    and r["fields"].get("key") == "red.config.demo.enabled"]
        self.assertEqual(custom_config(restored), custom_config(records))
        query = self.cli("query", "--path", target, "--json",
                         "SELECT $red.config.demo.enabled AS enabled")
        self.assertFalse(json.loads(query.stdout)["data"]["rows"][0]["enabled"])
        self.assertEqual([r["fields"] for r in restored if r.get("collection") == "renamed"],
                         [{"id": 1}])


if __name__ == "__main__":
    unittest.main()
