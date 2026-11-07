import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock

from queue_compare import RedQueue


class QueueAckBatchTests(unittest.IsolatedAsyncioTestCase):
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
