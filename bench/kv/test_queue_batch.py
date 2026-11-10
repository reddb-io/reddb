import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock

from queue_compare import RedQueue


class QueueAckBatchTests(unittest.IsolatedAsyncioTestCase):
    async def test_concurrent_reads_keep_separate_atomic_ack_batches(self):
        adapter = RedQueue()
        adapter.args = SimpleNamespace(name="jobs", red_ack_batch=True, red_read_concurrency=2)
        adapter.query = AsyncMock(side_effect=[
            [{"message_id": "1", "payload": "one"}, {"message_id": "2", "payload": "two"}],
            [{"message_id": "3", "payload": "three"}, {"message_id": "4", "payload": "four"}],
            [], [],
        ])
        self.assertEqual(await adapter.consume(4), ["one", "two", "three", "four"])
        self.assertEqual([call.args[0] for call in adapter.query.call_args_list[2:]], [
            "QUEUE ACK jobs BATCH GROUP workers ('1','2')",
            "QUEUE ACK jobs BATCH GROUP workers ('3','4')",
        ])

    async def test_read_requests_overlap_and_preserve_uneven_window(self):
        adapter = RedQueue()
        adapter.args = SimpleNamespace(name="jobs", red_ack_batch=False, red_read_concurrency=3)
        all_reading = asyncio.Event()
        counts = []
        acknowledgements = []

        async def query(sql):
            if sql.startswith("QUEUE ACK"):
                acknowledgements.append(sql)
                return []
            first = sum(counts)
            count = int(sql.rsplit(" ", 1)[1])
            counts.append(count)
            if len(counts) == 3:
                all_reading.set()
            await all_reading.wait()
            return [{"message_id": str(index), "payload": str(index)}
                    for index in range(first, first + count)]

        adapter.query = query
        values = await asyncio.wait_for(adapter.consume(5), timeout=1)
        self.assertEqual(counts, [2, 2, 1])
        self.assertEqual(sorted(values), ["0", "1", "2", "3", "4"])
        self.assertEqual(len(set(acknowledgements)), 5)

    async def test_batch_uses_one_ack_and_preserves_payloads(self):
        adapter = RedQueue()
        adapter.args = SimpleNamespace(name="jobs", red_ack_batch=True)
        adapter.query = AsyncMock(side_effect=[[
            {"message_id": "1", "payload": "first"},
            {"message_id": "2", "payload": "second"},
        ], []])
        self.assertEqual(await adapter.consume(2), ["first", "second"])
        self.assertEqual(adapter.query.call_count, 2)
        self.assertEqual(adapter.query.call_args.args[0],
                         "QUEUE ACK jobs BATCH GROUP workers ('1','2')")

    async def test_empty_read_does_not_issue_empty_ack(self):
        adapter = RedQueue()
        adapter.args = SimpleNamespace(name="jobs", red_ack_batch=True)
        adapter.query = AsyncMock(return_value=[])
        self.assertEqual(await adapter.consume(2), [])
        self.assertEqual(adapter.query.call_count, 1)

    async def test_baseline_retains_individual_acks(self):
        adapter = RedQueue()
        adapter.args = SimpleNamespace(name="jobs", red_ack_batch=False)
        adapter.query = AsyncMock(side_effect=[[
            {"message_id": "1", "payload": "first"},
            {"message_id": "2", "payload": "second"},
        ], [], []])
        self.assertEqual(await adapter.consume(2), ["first", "second"])
        self.assertEqual(adapter.query.call_count, 3)
        self.assertEqual([call.args[0] for call in adapter.query.call_args_list[1:]],
                         ["QUEUE ACK jobs GROUP workers '1'", "QUEUE ACK jobs GROUP workers '2'"])


if __name__ == "__main__":
    unittest.main()
