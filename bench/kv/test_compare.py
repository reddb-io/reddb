import io
import json
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import compare
import local_matrix


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
