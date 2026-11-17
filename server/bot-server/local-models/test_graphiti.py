"""Opt-in integration check against a disposable Graphiti backend and models.

Run with ESPBOT_GRAPH_TEST_CONFIG pointing at a test-only graph config and use
the same Python environment as graphiti_worker.py. The test creates random
households and synthetic text; it does not touch the application's SQLite.
"""
import os
import time
import unittest
import uuid

from graphiti_worker import Adapter


@unittest.skipUnless(os.environ.get("ESPBOT_GRAPH_TEST_CONFIG"), "requires a disposable real graph backend")
class GraphitiIntegration(unittest.IsolatedAsyncioTestCase):
    async def test_ingest_recall_isolation_delete_and_rebuild(self):
        adapter = Adapter(os.environ["ESPBOT_GRAPH_TEST_CONFIG"])
        self.assertIsNone(adapter.error)
        suffix = uuid.uuid4().hex
        household = "test_a_" + suffix
        other = "test_b_" + suffix
        episode = str(uuid.uuid4())

        def request(op, group=household, **kwargs):
            return {"op": op, "household_id": group, "engine_signature": adapter.engine_signature,
                    "allow_cloud": False, **kwargs}

        def ingest():
            return request("ingest", graph_uuid=episode, episode_id="episode_test",
                           daily_conversation_id="day_test", input_version="v1",
                           reference_time_ms=int(time.time() * 1000),
                           items=[{"kind": "fact", "content": "青禾喜欢在周六早上喝桂花豆浆。",
                                   "sources": [{"conversation_turn_id": "source_1", "source_revision": 1}],
                                   "allow_cloud": False}])

        async def search(group=household):
            return await adapter.handle(request("search", group, query="青禾周六早上喜欢喝什么"))

        try:
            added = await adapter.handle(ingest())
            self.assertEqual(added["graph_uuid"], episode)
            found = await search()
            self.assertTrue(any("桂花豆浆" in edge["fact"] and episode in edge["episodes"]
                                for edge in found["edges"]), found)
            self.assertFalse((await search(other))["edges"])
            await adapter.handle(request("delete", other, graph_uuid=episode))
            self.assertTrue((await search())["edges"])
            await adapter.handle(request("delete", graph_uuid=episode))
            self.assertFalse((await search())["edges"])
            rebuilt = await adapter.handle(ingest())
            self.assertEqual(rebuilt["graph_uuid"], episode)
            self.assertTrue((await search())["edges"])
        finally:
            try:
                await adapter.handle(request("delete", graph_uuid=episode))
            finally:
                if adapter.graph is not None:
                    await adapter.graph.close()
                elif adapter.driver is not None:
                    await adapter.driver.close()


if __name__ == "__main__":
    unittest.main()
