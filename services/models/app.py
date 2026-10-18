import asyncio
import contextlib
import itertools
from contextlib import asynccontextmanager
from typing import Annotated

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel, ConfigDict, Field, StringConstraints

from engine import Engine, EMBED_MODEL, EMBED_REVISION, RERANK_MODEL, RERANK_REVISION, CODE_REVISION

Text = Annotated[str, StringConstraints(min_length=1, max_length=4000)]


class EmbedRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    texts: list[Text] = Field(min_length=1, max_length=8)
    query: bool = False


class RerankRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    query: Text
    documents: list[Text] = Field(min_length=1, max_length=64)


async def process_queue(app):
    while True:
        _, _, operation, future, enqueued = await app.state.queue.get()
        stats = app.state.inference_stats
        started = asyncio.get_running_loop().time()
        try:
            if not future.cancelled():
                stats["running"] = 1
                stats["started"] += 1
                stats["queue_wait_ms"] += (started - enqueued) * 1000
                result = await asyncio.to_thread(operation)
                stats["completed"] += 1
                if not future.cancelled():
                    future.set_result(result)
                else:
                    stats["abandoned_running"] += 1
            else:
                stats["cancelled_queued"] += 1
        except Exception:
            stats["failed"] += 1
            if not future.done():
                future.set_exception(RuntimeError("model inference failed"))
        finally:
            if stats["running"]:
                stats["inference_ms"] += (asyncio.get_running_loop().time() - started) * 1000
            stats["running"] = 0
            app.state.queue.task_done()


def inference_stats():
    return dict(running=0, started=0, completed=0, failed=0, cancelled_queued=0,
                abandoned_running=0, queue_full=0, queue_wait_ms=0.0, inference_ms=0.0)


@asynccontextmanager
async def lifespan(app):
    app.state.engine = await asyncio.to_thread(Engine)
    app.state.queue = asyncio.PriorityQueue(maxsize=16)
    app.state.counter = itertools.count()
    app.state.inference_stats = inference_stats()
    worker = asyncio.create_task(process_queue(app))
    yield
    worker.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await worker


class BodyLimitMiddleware:
    """Bound request bodies without hiding http.disconnect behind a task stream."""
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.app(scope, receive, send)
        length = 0
        parts = []
        while True:
            message = await receive()
            if message["type"] == "http.disconnect":
                return
            part = message.get("body", b"")
            length += len(part)
            if length > 1024 * 1024:
                response = JSONResponse({"error": "body_too_large"}, status_code=413)
                return await response(scope, receive, send)
            if part:
                parts.append(part)
            if not message.get("more_body", False):
                break
        pending = True
        async def replay():
            nonlocal pending
            if pending:
                pending = False
                return {"type": "http.request", "body": b"".join(parts), "more_body": False}
            return await receive()
        await self.app(scope, replay, send)


app = FastAPI(lifespan=lifespan, docs_url=None, redoc_url=None, openapi_url=None)
app.add_middleware(BodyLimitMiddleware)


async def wait_disconnect(request):
    # FastAPI has already consumed the body. Read the ASGI disconnect directly:
    # Request.is_disconnected() uses an immediately cancelled AnyIO scope which
    # can swallow our task cancellation. Awaiting that task in submit's finally
    # then holds an already completed inference response until the client times out.
    while True:
        message = await request.receive()
        if message["type"] == "http.disconnect":
            return


async def submit(priority, operation, request=None):
    future = asyncio.get_running_loop().create_future()
    try:
        app.state.queue.put_nowait((priority, next(app.state.counter), operation, future,
                                   asyncio.get_running_loop().time()))
    except asyncio.QueueFull:
        app.state.inference_stats["queue_full"] += 1
        raise HTTPException(429, "inference_queue_full") from None
    disconnected = asyncio.create_task(wait_disconnect(request)) if request is not None else None
    try:
        if disconnected is None:
            return await asyncio.wait_for(future, timeout=40)
        done, _ = await asyncio.wait((future, disconnected), timeout=40,
                                     return_when=asyncio.FIRST_COMPLETED)
        if future in done:
            return future.result()
        if disconnected in done:
            raise HTTPException(499, "inference_client_disconnected")
        raise HTTPException(503, "inference_timeout")
    except TimeoutError:
        raise HTTPException(503, "inference_timeout") from None
    except RuntimeError:
        raise HTTPException(503, "inference_failed") from None
    finally:
        # Rust retrieval deadlines are shorter than the service's inference
        # timeout. Do not spend CPU on their abandoned queued requests.
        if not future.done():
            future.cancel()
        if disconnected is not None:
            disconnected.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await disconnected


@app.get("/health")
async def health():
    return {"ready": True, "device": "cpu", "dimensions": 1024,
            "dtype": getattr(app.state.engine, "dtype_name", "float32"),
            "rerank_dtype": getattr(app.state.engine, "rerank_dtype_name", "float32"),
            "embedding": EMBED_MODEL, "embedding_revision": EMBED_REVISION,
            "reranker": RERANK_MODEL, "reranker_revision": RERANK_REVISION,
            "code_revision": CODE_REVISION, "queued": app.state.queue.qsize(),
            "inference": dict(app.state.inference_stats)}


@app.post("/embed")
async def embed(body: EmbedRequest, request: Request):
    vectors = await submit(0 if body.query else 1, lambda: app.state.engine.embed(body.texts, body.query), request)
    return {"embeddings": vectors}


@app.post("/rerank")
async def rerank(body: RerankRequest, request: Request):
    scores = await submit(0, lambda: app.state.engine.rerank(body.query, body.documents), request)
    return {"scores": scores}
