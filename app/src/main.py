from __future__ import annotations

import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import Depends, FastAPI, Header, HTTPException, Query, Request, status
from fastapi.responses import FileResponse

from .agent import MemoryAgent
from .config import Settings, get_settings
from .embedding import EmbeddingClient
from .llm import LLMClient, LLMError
from .mcp_tools import MCPManager
from .memory_store import MemoryStore
from .schemas import (
    ChatRequest,
    ChatResponse,
    CreateMemoryRequest,
    LinkMemoryRequest,
    MemoryView,
)

settings = get_settings()
logging.basicConfig(
    level=getattr(logging, settings.log_level.upper(), logging.INFO),
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
# Neo4j 把 db.index.vector.queryNodes 的弃用提醒按 WARNING 刷屏，但功能正常，先压成 ERROR。
logging.getLogger("neo4j.notifications").setLevel(logging.ERROR)
# httpx/httpcore 在 DEBUG 下会打印完整请求 URL（含 MCP key），兜底不低于 WARNING，避免泄露。
for _noisy in ("httpx", "httpcore"):
    logging.getLogger(_noisy).setLevel(logging.WARNING)
logger = logging.getLogger(__name__)


@asynccontextmanager
async def lifespan(app: FastAPI):
    store = MemoryStore(settings)
    embedding = EmbeddingClient(settings)
    llm = LLMClient(settings)
    mcp = MCPManager(settings)
    await store.connect()
    await mcp.start()
    app.state.store = store
    app.state.embedding = embedding
    app.state.llm = llm
    app.state.mcp = mcp
    app.state.agent = MemoryAgent(settings, store, embedding, llm, mcp)
    yield
    await mcp.close()
    await llm.close()
    await embedding.close()
    await store.close()


app = FastAPI(
    title="Qwen + Neo4j Graph Memory Agent",
    version="0.1.0",
    description="本地 Embedding、Neo4j 图谱/向量记忆与双模型对话服务",
    lifespan=lifespan,
)


def require_api_key(authorization: str | None = Header(default=None)) -> None:
    if not settings.app_api_key:
        return
    if authorization != f"Bearer {settings.app_api_key}":
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="缺少或无效的 APP_API_KEY",
        )


def get_agent(request: Request) -> MemoryAgent:
    return request.app.state.agent


def get_store(request: Request) -> MemoryStore:
    return request.app.state.store


def get_embedding(request: Request) -> EmbeddingClient:
    return request.app.state.embedding


@app.get("/", include_in_schema=False)
async def index() -> FileResponse:
    return FileResponse(Path("static/index.html"))


@app.get("/health/live")
async def live() -> dict[str, str]:
    return {"status": "ok"}


@app.get("/health")
async def health(request: Request) -> dict[str, object]:
    neo4j_ok = await request.app.state.store.ping()
    embedding_ok = await request.app.state.embedding.health()
    llm_configured = bool(settings.ai_base_url and settings.chat_model and settings.memory_model)
    mcp_tools = len(request.app.state.mcp.openai_tools())
    return {
        "status": "ok" if neo4j_ok and embedding_ok and llm_configured else "degraded",
        "neo4j": neo4j_ok,
        "embedding": embedding_ok,
        "llm_configured": llm_configured,
        "mcp_tools": mcp_tools,
        "config": settings.safe_summary,
    }


@app.get("/v1/config", dependencies=[Depends(require_api_key)])
async def config() -> dict[str, object]:
    return settings.safe_summary


@app.post("/v1/chat", response_model=ChatResponse, dependencies=[Depends(require_api_key)])
async def chat(body: ChatRequest, agent: MemoryAgent = Depends(get_agent)) -> ChatResponse:
    try:
        result = await agent.chat(
            user_id=body.user_id,
            message=body.message,
            conversation_id=body.conversation_id,
            custom_system_prompt=body.system_prompt,
        )
    except LLMError as exc:
        raise HTTPException(status_code=502, detail=str(exc)) from exc
    except Exception as exc:
        logger.exception("Chat request failed")
        raise HTTPException(status_code=500, detail=str(exc)) from exc
    return ChatResponse(
        conversation_id=result.conversation_id,
        message=result.content,
        retrieved_memories=[MemoryView(**item) for item in result.retrieved],
        saved_memories=[MemoryView(**item) for item in result.saved],
        tool_events=result.tool_events,
        warnings=result.warnings,
    )


@app.get(
    "/v1/memories/search",
    response_model=list[MemoryView],
    dependencies=[Depends(require_api_key)],
)
async def search_memories(
    request: Request,
    user_id: str = Query(min_length=1, max_length=128),
    q: str = Query(min_length=1, max_length=50_000),
    limit: int = Query(default=8, ge=1, le=50),
) -> list[MemoryView]:
    vector = (await request.app.state.embedding.embed([q], is_query=True))[0]
    items = await request.app.state.store.search_memories(user_id, vector, limit=limit)
    return [MemoryView(**item) for item in items]


@app.get(
    "/v1/memories/recent",
    response_model=list[MemoryView],
    dependencies=[Depends(require_api_key)],
)
async def recent_memories(
    user_id: str = Query(min_length=1, max_length=128),
    limit: int = Query(default=10, ge=1, le=100),
    store: MemoryStore = Depends(get_store),
) -> list[MemoryView]:
    return [MemoryView(**item) for item in await store.recent_memories(user_id, limit)]


@app.post(
    "/v1/memories",
    response_model=MemoryView,
    dependencies=[Depends(require_api_key)],
)
async def create_memory(
    body: CreateMemoryRequest,
    store: MemoryStore = Depends(get_store),
    embedding: EmbeddingClient = Depends(get_embedding),
) -> MemoryView:
    vector = (await embedding.embed([body.text]))[0]
    item = await store.create_memory(
        user_id=body.user_id,
        text=body.text,
        kind=body.kind,
        importance=body.importance,
        entities=[entity.model_dump() for entity in body.entities],
        embedding=vector,
        source="manual_api",
    )
    return MemoryView(**item)


@app.delete("/v1/memories/{memory_id}", dependencies=[Depends(require_api_key)])
async def forget_memory(
    memory_id: str,
    user_id: str = Query(min_length=1, max_length=128),
    store: MemoryStore = Depends(get_store),
) -> dict[str, bool]:
    changed = await store.forget_memory(user_id, memory_id)
    if not changed:
        raise HTTPException(status_code=404, detail="没有找到该用户的有效记忆")
    return {"forgotten": True}


@app.post("/v1/memories/link", dependencies=[Depends(require_api_key)])
async def link_memories(
    body: LinkMemoryRequest, store: MemoryStore = Depends(get_store)
) -> dict[str, bool]:
    linked = await store.link_memories(
        body.user_id, body.from_memory_id, body.to_memory_id, body.relation
    )
    if not linked:
        raise HTTPException(status_code=404, detail="两条记忆必须存在且属于同一用户")
    return {"linked": True}


@app.get("/v1/graph/{user_id}", dependencies=[Depends(require_api_key)])
async def graph(
    user_id: str,
    limit: int = Query(default=100, ge=1, le=500),
    store: MemoryStore = Depends(get_store),
) -> dict[str, object]:
    return await store.graph_snapshot(user_id, limit)

