import asyncio
import contextlib
import itertools
from contextlib import asynccontextmanager
from typing import Annotated

from fastapi import FastAPI, HTTPException, Request
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
        _, _, operation, future = await app.state.queue.get()
        try:
            if not future.cancelled():
                result = await asyncio.to_thread(operation)
                if not future.cancelled():
                    future.set_result(result)
        except Exception:
            if not future.done():
                future.set_exception(RuntimeError("model inference failed"))
        finally:
            app.state.queue.task_done()


@asynccontextmanager
async def lifespan(app):
    app.state.engine = await asyncio.to_thread(Engine)
    app.state.queue = asyncio.PriorityQueue(maxsize=16)
    app.state.counter = itertools.count()
    worker = asyncio.create_task(process_queue(app))
    yield
    worker.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await worker


app = FastAPI(lifespan=lifespan, docs_url=None, redoc_url=None, openapi_url=None)


@app.middleware("http")
async def body_limit(request: Request, call_next):
    # Internal service only; bound both chunked and Content-Length bodies.
    length = 0
    parts = []
    async for part in request.stream():
        length += len(part)
        if length > 1024 * 1024:
            from fastapi.responses import JSONResponse
            return JSONResponse({"error": "body_too_large"}, status_code=413)
        parts.append(part)
    request._body = b"".join(parts)
    return await call_next(request)


async def submit(priority, operation):
    future = asyncio.get_running_loop().create_future()
    try:
        app.state.queue.put_nowait((priority, next(app.state.counter), operation, future))
    except asyncio.QueueFull:
        raise HTTPException(429, "inference_queue_full") from None
    try:
        return await asyncio.wait_for(future, timeout=40)
    except TimeoutError:
        raise HTTPException(503, "inference_timeout") from None
    except RuntimeError:
        raise HTTPException(503, "inference_failed") from None


@app.get("/health")
async def health():
    return {"ready": True, "device": "cpu", "dimensions": 1024,
            "embedding": EMBED_MODEL, "embedding_revision": EMBED_REVISION,
            "reranker": RERANK_MODEL, "reranker_revision": RERANK_REVISION,
            "code_revision": CODE_REVISION, "queued": app.state.queue.qsize()}


@app.post("/embed")
async def embed(body: EmbedRequest):
    vectors = await submit(0 if body.query else 1, lambda: app.state.engine.embed(body.texts, body.query))
    return {"embeddings": vectors}


@app.post("/rerank")
async def rerank(body: RerankRequest):
    scores = await submit(0, lambda: app.state.engine.rerank(body.query, body.documents))
    return {"scores": scores}
