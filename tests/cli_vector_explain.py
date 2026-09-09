"""Live CLI checks: REDDB_BINARY_PATH=/path/to/red python3 tests/cli_vector_explain.py."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class VectorExplain(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="reddb-vector-explain-")
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "db.rdb"
        self.binary = os.environ["REDDB_BINARY_PATH"]
        self.query("CREATE VECTOR places DIM 2 METRIC cosine")
        for name, vector, category in [
            ("a", "[0.8,0.6]", "yes"),
            ("b", "[0.6,0.8]", "yes"),
            ("outside", "[1.0,0.0]", "no"),
        ]:
            self.query(f"INSERT INTO places VECTOR (dense) VALUES ({vector}) "
                       f"WITH METADATA (name='{name}', category='{category}')")

    def query(self, sql):
        result = subprocess.run([self.binary, "query", "--path", str(self.path),
                                 "--json", sql], capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)["data"]

    def test_filtered_turbo_execution_reports_observed_work(self):
        sql = "VECTOR SEARCH places SIMILAR TO [1.0,0.0] WHERE category = 'yes' LIMIT 1"
        result = self.query("EXPLAIN ANALYZE " + sql)
        row = result["rows"][0]
        self.assertEqual(row["op"], "vector_turbo_search")
        self.assertTrue(row["index_used"])
        self.assertEqual(row["candidates_examined"], 3)
        self.assertEqual(row["metadata_rejected"], 1)
        self.assertEqual(row["exact_distance_evaluations"], 2)
        self.assertEqual(row["actual_rows"], 1)
        self.assertEqual(row["metrics_scope"], "vector_pipeline")
        self.assertGreaterEqual(row["actual_ms"], 0)
        self.assertEqual(len(self.query(sql)["rows"]), 1)

    def test_empty_filter_counts_work_without_fabricated_results(self):
        row = self.query("EXPLAIN ANALYZE VECTOR SEARCH places SIMILAR TO [1.0,0.0] "
                         "WHERE category = 'absent' LIMIT 2")["rows"][0]
        self.assertEqual(row["candidates_examined"], 3)
        self.assertEqual(row["metadata_rejected"], 3)
        self.assertEqual(row["exact_distance_evaluations"], 0)
        self.assertEqual(row["actual_rows"], 0)


if __name__ == "__main__":
    unittest.main()
