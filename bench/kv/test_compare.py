import io
import json
import struct
import tempfile
import threading
import time
import unittest
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import compare
import local_matrix
import summarize


class ArtifactStateTests(unittest.TestCase):
    def test_torn_root_is_rejected_and_only_checksummed_generation_is_selected(self):
        def root(copy, generation):
            block = bytearray(4096)
            block[:8] = b"RDBSBLK1"
            struct.pack_into("<IBQ", block, 8, 2, copy, generation)
            struct.pack_into("<QQQQQQI", block, 45, 16384, 1048576, 16400, 16, 1064960, 4, generation)
            struct.pack_into("<I", block, 4092, zlib.crc32(block[:-4]))
            return block

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "data.rdb"
            def write(first, second, body=b"row0"):
                with path.open("wb") as output:
                    output.write(first + second)
                    output.seek(1064960)
                    output.write(body)
            first, second = root(0, 7), root(1, 8)
            write(first, second)
            self.assertEqual(compare.artifact_state(path)["generation"], 8)
            second[80] ^= 1
            write(first, second)
            state = compare.artifact_state(path)
            self.assertEqual(state["generation"], 7)
            self.assertEqual(state["wal_capacity_bytes"], 1048576)
            write(first, second, b"row1")
            self.assertNotEqual(state["snapshot_sha256"], compare.artifact_state(path)["snapshot_sha256"])
            first[80] ^= 1
            write(first, second)
            with self.assertRaises(compare.ProtocolError):
                compare.artifact_state(path)


def resp(raw, fence=False):
    connection = compare.RespClient.__new__(compare.RespClient)
    connection.reader = io.BytesIO(raw)
    connection.socket = Mock()
    connection.collection = "fixture"
    connection.fence_writes = fence
    return connection


def arguments(out=None, **changes):
    values = dict(engine="redis", host="fixture", port=6379, collection="fixture", keys=5,
                  value_bytes=100, operations=11, workers=3, warmup=2, rounds=2, pipeline=2,
                  operation="get", seconds=0, prepare_only=False, verify_only=False,
                  verification="full", fence_writes=False, recovery_from=None, out=out)
    values["verification_workers"] = 8
    values.update(changes)
    return SimpleNamespace(**values)


class FakeClient:
    def __init__(self, values, discard_writes=False):
        self.values = values
        self.discard_writes = discard_writes

    def close(self):
        pass

    def batch(self, operation, indices, size, generation=0):
        for index in indices:
            expected = compare.payload(index, size, generation)
            if operation == "put":
                if not self.discard_writes:
                    self.values[index] = expected
            elif self.values[index] != expected:
                raise compare.ProtocolError("readback mismatch")


class CompareTests(unittest.TestCase):
    def test_zero_only_cpu_telemetry_does_not_claim_zero_cpu_under_load(self):
        sample = dict(started_unix_ms=1000, finished_unix_ms=3000, operations=10000,
                      throughput_ops_s=5000, batch_p50_us=100, batch_p95_us=200, batch_p99_us=300)
        case = dict(engine="redis", operation="get", status="valid-diagnostic",
                    config=vars(arguments(rounds=1)), rounds=[sample])
        event = dict(unix_ms=2000, Name="server", CPUPerc="0.00%", MemUsage="1MiB / 2GiB",
                     BlockIO="0B / 0B", NetIO="1MB / 1MB")
        result = summarize.summarize_case(case, [event], "server", "client")
        self.assertIsNone(result["server_median_cpu_percent"])
        self.assertTrue(result["server_cpu_zero_only_samples"])
        self.assertEqual(result["server_peak_working_set_mib"], 1)

    def test_parallel_verification_checks_every_key_once(self):
        args = arguments(keys=31)
        values = {index: compare.payload(index, 100) for index in range(args.keys)}
        visited = []
        lock = threading.Lock()

        class TrackingClient(FakeClient):
            def batch(self, operation, indices, size, generation=0):
                super().batch(operation, indices, size, generation)
                with lock:
                    visited.extend(indices)

        with patch("compare.client", side_effect=lambda _: TrackingClient(values)):
            result = compare.verify(args)
        self.assertEqual(sorted(visited), list(range(args.keys)))
        self.assertEqual(result["keys_checked"], args.keys)
        self.assertEqual(result["verification_workers"], 8)

    def test_failed_partial_samples_cannot_be_aggregated_as_success(self):
        case = {"engine": "redis", "operation": "get", "status": "invalid",
                "config": vars(arguments()), "rounds": [{"throughput_ops_s": 999999}]}
        result = summarize.summarize_case(case, [], "server", "client")
        self.assertEqual(result["status"], "invalid")
        self.assertNotIn("median_throughput_ops_s", result)

    def test_resource_samples_accept_windows_docker_cursor_escapes(self):
        sample = local_matrix.resource_sample('\x1b[H{"CPUPerc":"12.5%"}\x1b[K\n')
        self.assertEqual(sample["CPUPerc"], "12.5%")

    def test_resp_errors_and_eof_are_failures(self):
        for raw in [b"-ERR unsupported\r\n", b"", b"$8\r\nshort\r\n", b"$-2\r\n"]:
            with self.subTest(raw=raw), self.assertRaises(compare.ProtocolError):
                resp(raw).read()

    def test_wrong_read_value_is_not_a_success(self):
        with self.assertRaises(compare.ProtocolError):
            resp(b"$5\r\nwrong\r\n").batch("get", [0], 100)

    def test_fenced_write_requires_local_fsync_confirmation(self):
        connection = resp(b"+OK\r\n*2\r\n:1\r\n:0\r\n", fence=True)
        connection.batch("put", [0], 100)
        self.assertIn(b"WAITAOF", connection.socket.sendall.call_args.args[0])
        with self.assertRaises(compare.ProtocolError):
            resp(b"+OK\r\n*2\r\n:0\r\n:0\r\n", fence=True).batch("put", [0], 100)

    def test_http_errors_and_connection_closure_are_failures(self):
        connection = compare.RedClient.__new__(compare.RedClient)
        connection.connection = Mock()
        response = connection.connection.getresponse.return_value
        response.status = 200
        response.will_close = False
        response.read.return_value = b'{"ok": false, "error": "denied"}'
        with self.assertRaises(compare.ProtocolError):
            connection.request("GET", "/fixture")
        response.will_close = True
        response.read.return_value = b'{"ok": true}'
        with self.assertRaises(compare.ProtocolError):
            connection.request("GET", "/fixture")

    def test_query_reads_use_the_actual_record_envelope(self):
        connection = compare.RedClient.__new__(compare.RedClient)
        connection.collection = "fixture"
        connection.query = True
        connection.request = Mock(return_value={"result": {"records": [
            {"values": {"value": compare.payload(0, 100)}}]}})
        connection.batch("get", [0], 100)
        connection.request.return_value["result"]["records"][0]["values"]["value"] = "wrong"
        with self.assertRaises(compare.ProtocolError):
            connection.batch("get", [0], 100)

    def test_uneven_worker_and_pipeline_quotas_count_actual_operations(self):
        args = arguments()
        values = {index: compare.payload(index, 100) for index in range(args.keys)}
        with patch("compare.client", side_effect=lambda _: FakeClient(values)):
            sample = compare.measure(args, 0, [0] * args.keys)
        self.assertEqual(sample["operations"], 11)
        self.assertGreater(sample["seconds"], 0)
        self.assertGreaterEqual(sample["batch_p99_us"], sample["batch_p50_us"])

    def test_duration_mode_is_not_truncated_to_the_operation_quota(self):
        class SlowClient(FakeClient):
            def batch(self, *args):
                time.sleep(.001)
                super().batch(*args)
        args = arguments(seconds=.05, operations=3)
        values = {index: compare.payload(index, 100) for index in range(args.keys)}
        with patch("compare.client", side_effect=lambda _: SlowClient(values)):
            sample = compare.measure(args, 0, [0] * args.keys)
        self.assertGreater(sample["operations"], 3)
        self.assertGreaterEqual(sample["seconds"], .05)

    def test_acknowledged_but_discarded_writes_invalidate_saved_run(self):
        with tempfile.TemporaryDirectory() as directory:
            args = arguments(Path(directory) / "result.json", operation="put")
            values = {index: compare.payload(index, 100) for index in range(args.keys)}
            with patch("compare.arguments", return_value=args), patch(
                "compare.client", side_effect=lambda _: FakeClient(values, discard_writes=True)
            ), self.assertRaises(compare.ProtocolError):
                compare.main()
            result = json.loads(args.out.read_text())
            self.assertEqual(result["status"], "invalid")
            self.assertIn("readback mismatch", result["error"])
            self.assertNotIn("median_throughput_ops_s", result)


if __name__ == "__main__":
    unittest.main()
