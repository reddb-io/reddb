import json
import re
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import read_paths


class ReadPathTests(unittest.TestCase):
    def args(self, **overrides):
        values = dict(host="unused", port=5000, kv_collection="kv_diag_1000", keys=1000,
                      value_bytes=100, workers=3, seconds=.03)
        values.update(overrides)
        return SimpleNamespace(**values)

    def sql_result(self, indices):
        return {"result": {"records": [{"values": {"seq": index, "item": read_paths.payload(index, 100)}}
                                        for index in indices]}}

    def test_sql_point_checks_the_bound_key_and_payload(self):
        args = self.args()
        read_paths.validate(self.sql_result([42]), "sql-point", 42, args)
        with self.assertRaises(read_paths.ProtocolError):
            read_paths.validate(self.sql_result([43]), "sql-point", 42, args)
        wrong = self.sql_result([42])
        wrong["result"]["records"][0]["values"]["item"] = "wrong"
        with self.assertRaises(read_paths.ProtocolError):
            read_paths.validate(wrong, "sql-point", 42, args)

    def test_multirow_checks_count_order_and_every_value(self):
        args = self.args()
        read_paths.validate(self.sql_result(range(100)), "sql-100", 0, args)
        for indices in (range(99), list(reversed(range(100)))):
            with self.assertRaises(read_paths.ProtocolError):
                read_paths.validate(self.sql_result(indices), "sql-100", 0, args)
        wrong = self.sql_result(range(100))
        wrong["result"]["records"][99]["values"]["item"] = "wrong"
        with self.assertRaises(read_paths.ProtocolError):
            read_paths.validate(wrong, "sql-100", 0, args)

    def test_kv_uses_the_actual_query_envelope(self):
        result = {"result": {"records": [{"values": {"value": read_paths.payload(4, 100)}}]}}
        read_paths.validate(result, "kv", 1004, self.args())
        with self.assertRaises(read_paths.ProtocolError):
            read_paths.validate(result, "kv", 5, self.args())

    def test_fixed_duration_counts_completed_checked_queries_and_closes_connections(self):
        closed = []

        class Connection:
            def __init__(self, *args):
                pass

            def request(self, method, path, body):
                index = int(re.search(r"\.k(\d+)$", body["query"]).group(1))
                return {"result": {"records": [{"values": {"value": read_paths.payload(index, 100)}}]}}

            def close(self):
                closed.append(True)

        with patch.object(read_paths, "RedClient", Connection):
            result = read_paths.sample(self.args(), "kv", 1)
        self.assertGreater(result["queries"], 0)
        self.assertGreaterEqual(result["seconds"], .03)
        self.assertEqual(result["errors"], 0)
        self.assertEqual(len(closed), 3)

    def test_invalid_response_is_saved_and_cannot_become_a_success(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "invalid.json"
            argv = ["read_paths.py", "--host", "unused", "--out", str(output)]
            with patch("sys.argv", argv), patch.object(read_paths, "sample", side_effect=read_paths.ProtocolError("wrong value")):
                with self.assertRaises(read_paths.ProtocolError):
                    read_paths.main()
            result = json.loads(output.read_text())
            self.assertEqual(result["status"], "invalid")
            self.assertEqual(result["rounds"], [])
            self.assertNotIn("median_queries_s", result)


if __name__ == "__main__":
    unittest.main()
