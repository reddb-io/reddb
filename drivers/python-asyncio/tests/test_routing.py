import json
from pathlib import Path
import unittest

from reddb_asyncio.routing import RoutingClient, decode_topology, sql_policy
from reddb_asyncio.errors import RedDBError


class TopologyFixtures(unittest.TestCase):
    def test_shared_contract(self):
        manifest = json.loads((Path(__file__).parents[3] /
            "testdata/conformance/topology/manifest.json").read_text())
        for case in manifest["cases"]:
            with self.subTest(case=case["name"]):
                if case.get("error"):
                    with self.assertRaises(RedDBError):
                        decode_topology(bytes.fromhex(case["hex"]))
                else:
                    self.assertEqual(decode_topology(bytes.fromhex(case["hex"])), case["expected"])


class RoutingTests(unittest.IsolatedAsyncioTestCase):
    async def test_routing_promotion_transaction_and_no_write_replay(self):
        primary, replica = "http://primary:5000", "http://replica:5000"
        advertised = primary
        calls = []

        class Transport:
            def __init__(self, address):
                self.address = address

            async def topology(self, protocol):
                return {"version": 1, "primary": {"addr": advertised},
                        "replicas": [{"addr": replica, "healthy": True, "lag_ms": 10}]}

            async def query(self, sql, params=None):
                calls.append((self.address, sql))
                if sql == "UPDATE lost":
                    raise OSError("lost commit reply")
                return self.address

            async def ping(self):
                pass

            async def close(self):
                pass

        async def dial(address):
            return Transport(address)

        client = await RoutingClient(primary, await dial(primary), dial, "http").initialize()
        self.assertEqual(await client.query("SELECT 1"), primary)
        self.assertEqual(await client.query_with_options("SELECT 1", {"consistency": "local"}), replica)
        self.assertEqual(await client.query_with_options("SELECT 1",
            {"consistency": "bounded_staleness", "max_lag_ms": 1}), primary)
        with self.assertRaises(RedDBError):
            await client.query_with_options("SELECT 1", {"consistency": "causal"})
        await client.query("/* txn */ BEGIN")
        advertised = "http://promoted:5000"
        client.refresh_at = 0
        self.assertEqual(await client.query_with_options("SELECT 1", {"consistency": "local"}), primary)
        await client.query("ROLLBACK TO SAVEPOINT nested")
        self.assertEqual(await client.query("SELECT 1"), primary)
        await client.query("ROLLBACK")
        self.assertEqual(await client.query("UPDATE users SET id = 1"), advertised)
        with self.assertRaises(OSError):
            await client.query("UPDATE lost")
        self.assertEqual(sum(sql == "UPDATE lost" for _, sql in calls), 1)
        await client.close()
        with self.assertRaises(RedDBError):
            await client.query("SELECT 1")

    def test_sql_sessions(self):
        self.assertEqual(sql_policy("SET search_path = tenant")[0], "session")
        self.assertEqual(sql_policy("COMMIT AND CHAIN")[0], "begin")
        self.assertFalse(sql_policy("SELECT effectful()")[1])
        self.assertFalse(sql_policy("SELECT 1; DELETE FROM users")[1])
