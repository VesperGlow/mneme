"""Run in the models image; no weights, downloads or paid calls are needed."""
import asyncio
import importlib.util
import itertools
import os
import json
import threading
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch


@unittest.skipUnless(importlib.util.find_spec("fastapi"), "requires model-service dependencies")
class QueueTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        engine = types.ModuleType("engine")
        for name in ("Engine", "EMBED_MODEL", "EMBED_REVISION", "RERANK_MODEL", "RERANK_REVISION", "CODE_REVISION"):
            setattr(engine, name, object if name == "Engine" else "fixture")
        path = Path(os.environ.get("MNEME_MODEL_APP", Path(__file__).resolve().parents[1] / "services/models/app.py"))
        spec = importlib.util.spec_from_file_location("model_queue_test_app", path)
        self.module = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {"engine": engine}):
            spec.loader.exec_module(self.module)
        self.module.app.state.queue = asyncio.PriorityQueue(maxsize=16)
        self.module.app.state.counter = itertools.count()
        self.module.app.state.inference_stats = self.module.inference_stats()

    async def test_disconnected_pending_request_never_runs_inference(self):
        class Disconnected:
            async def receive(self):
                return {"type": "http.disconnect"}
        ran = []
        with self.assertRaises(self.module.HTTPException) as result:
            await self.module.submit(0, lambda: ran.append(True), Disconnected())
        self.assertEqual(result.exception.status_code, 499)
        worker = asyncio.create_task(self.module.process_queue(self.module.app))
        await asyncio.wait_for(self.module.app.state.queue.join(), 2)
        worker.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await worker
        self.assertEqual(ran, [])
        self.assertEqual(self.module.app.state.inference_stats["cancelled_queued"], 1)

    async def test_completed_inference_does_not_wait_for_a_cancellation_swallowing_disconnect_poll(self):
        entered = asyncio.Event()
        release = asyncio.Event()
        class Request:
            async def is_disconnected(self):
                entered.set()
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    return False  # Starlette's cancelled-scope race.
                return True
            async def receive(self):
                entered.set()
                await release.wait()
                return {"type": "http.disconnect"}
        pending = asyncio.create_task(self.module.submit(0, lambda: "ready", Request()))
        await asyncio.wait_for(entered.wait(), 1)
        worker = asyncio.create_task(self.module.process_queue(self.module.app))
        try:
            done, _ = await asyncio.wait((pending,), timeout=0.2)
            self.assertIn(pending, done, "inference completed but disconnect-task cleanup held the reply")
            self.assertEqual(pending.result(), "ready")
        finally:
            release.set()
            await asyncio.gather(pending, return_exceptions=True)
            worker.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await worker

    async def test_chunked_overflow_is_rejected_before_inference(self):
        messages = iter([{"type": "http.request", "body": b"x" * 524288, "more_body": True},
                         {"type": "http.request", "body": b"x" * 524289, "more_body": False}])
        sent = []
        async def receive():
            return next(messages)
        async def send(message):
            sent.append(message)
        async def downstream(*_):
            self.fail("oversized input must not reach parsing or inference")
        await self.module.BodyLimitMiddleware(downstream)({"type": "http"}, receive, send)
        self.assertEqual(sent[0]["status"], 413)

    async def test_overload_is_bounded_and_cancellation_releases_work(self):
        pending = [asyncio.create_task(self.module.submit(0, lambda: None)) for _ in range(16)]
        await asyncio.sleep(0)
        with self.assertRaises(self.module.HTTPException) as result:
            await self.module.submit(0, lambda: None)
        self.assertEqual(result.exception.status_code, 429)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        self.assertTrue(all(item[3].cancelled() for item in self.module.app.state.queue._queue))

    async def test_failure_does_not_kill_worker_and_query_priority_is_preserved(self):
        order = []
        def broken():
            raise ValueError("fixture")
        document = asyncio.create_task(self.module.submit(1, lambda: order.append("doc")))
        query = asyncio.create_task(self.module.submit(0, lambda: order.append("query")))
        failure = asyncio.create_task(self.module.submit(0, broken))
        await asyncio.sleep(0)
        worker = asyncio.create_task(self.module.process_queue(self.module.app))
        results = await asyncio.gather(document, query, failure, return_exceptions=True)
        self.assertEqual(order, ["query", "doc"])
        self.assertEqual(results[2].status_code, 503)
        worker.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await worker

    @unittest.skipUnless(importlib.util.find_spec("uvicorn"), "requires model HTTP runtime")
    async def test_real_http_disconnect_discards_queued_cpu_work(self):
        import uvicorn
        gate = threading.Event()
        calls = []
        class FakeEngine:
            def embed(self, texts, query, with_usage=False):
                calls.extend(texts)
                if texts == ["block"]:
                    gate.wait(3)
                return {"embeddings": [[1.0] for _ in texts]} if with_usage else [[1.0] for _ in texts]
        self.module.Engine = FakeEngine
        server = uvicorn.Server(uvicorn.Config(self.module.app, host="127.0.0.1", port=0,
                                               log_level="error", lifespan="on"))
        task = asyncio.create_task(server.serve())
        writers = []
        try:
            for _ in range(200):
                if server.started:
                    break
                await asyncio.sleep(0.01)
            self.assertTrue(server.started)
            port = server.servers[0].sockets[0].getsockname()[1]
            async def connect(path, body=None):
                reader, writer = await asyncio.open_connection("127.0.0.1", port)
                payload = b"" if body is None else json.dumps(body).encode()
                method = "GET" if body is None else "POST"
                writer.write(f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\nConnection: close\r\n\r\n".encode() + payload)
                await writer.drain()
                return reader, writer
            async def health():
                reader, writer = await connect("/health")
                wire = await reader.read()
                writer.close()
                await writer.wait_closed()
                return json.loads(wire.split(b"\r\n\r\n", 1)[1])
            reader, first = await connect("/embed", {"texts": ["block"], "query": True})
            writers.append(first)
            for _ in range(100):
                if calls:
                    break
                await asyncio.sleep(0.01)
            for i in range(8):
                _, writer = await connect("/embed", {"texts": [f"queued-{i}"], "query": True})
                writers.append(writer)
            for _ in range(100):
                if (await health())["queued"] == 8:
                    break
                await asyncio.sleep(0.01)
            self.assertEqual((await health())["queued"], 8)
            for writer in writers[1:]:
                writer.close()
                await writer.wait_closed()
            await asyncio.sleep(0.25)
            gate.set()
            await asyncio.wait_for(reader.read(), 3)
            for _ in range(100):
                state = await health()
                if state["queued"] == 0:
                    break
                await asyncio.sleep(0.01)
            self.assertEqual(calls, ["block"])
            self.assertEqual(state["inference"]["cancelled_queued"], 8)
        finally:
            gate.set()
            for writer in writers:
                writer.close()
            server.should_exit = True
            await asyncio.wait_for(task, 5)


if __name__ == "__main__":
    unittest.main()
