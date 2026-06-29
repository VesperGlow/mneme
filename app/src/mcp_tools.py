from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field
from datetime import timedelta
from typing import Any

from .config import Settings

logger = logging.getLogger(__name__)

# 内置工具一律无前缀；MCP 工具统一加这个前缀，避免与内置工具或彼此重名。
MCP_TOOL_PREFIX = "mcp__"

# MCP 的 key 常嵌在 URL（Tavily 在查询串、Firecrawl 在路径），出错时异常文本会带出来。
# 写日志或回传错误前先脱敏，避免密钥落到日志、模型上下文或 API 响应里。
_REDACT_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"(?i)([?&][\w-]*(?:key|token|secret|password)=)[^&\s\"']+"), r"\1***"),
    (re.compile(r"\b((?:fc|tvly|sk)-)[A-Za-z0-9._-]{6,}"), r"\1***"),
    (re.compile(r"(?i)(bearer\s+)[A-Za-z0-9._\-]{6,}"), r"\1***"),
    (re.compile(r"(https?://)[^/@\s]+@"), r"\1***@"),
]


def redact_secret(text: str) -> str:
    for pattern, repl in _REDACT_PATTERNS:
        text = pattern.sub(repl, text)
    return text


@dataclass
class MCPServer:
    name: str
    url: str
    transport: str
    headers: dict[str, str] = field(default_factory=dict)
    # include 为白名单（非空时只注册这些工具）；exclude 为黑名单，进一步剔除。
    include: list[str] = field(default_factory=list)
    exclude: list[str] = field(default_factory=list)

    def accepts(self, tool_name: str) -> bool:
        if self.include and tool_name not in self.include:
            return False
        return tool_name not in self.exclude


class MCPManager:
    """通过 streamable-http / SSE 连接远程 MCP 服务器，把它们的工具暴露给对话模型。

    采用“每次调用新建连接”的策略：启动时各连一次抓取工具清单，之后每次工具调用
    临时建立连接、调用、关闭。这样可以彻底避开长连接会话跨 asyncio 任务复用时
    anyio cancel scope 报错的问题，对个人项目这点连接开销完全可以接受。
    """

    def __init__(self, settings: Settings):
        self.settings = settings
        self._servers: list[MCPServer] = [
            MCPServer(
                name=item["name"],
                url=item["url"],
                transport=item["transport"],
                headers=item["headers"],
                include=item.get("include", []),
                exclude=item.get("exclude", []),
            )
            for item in settings.mcp_servers
        ]
        # 限定名（mcp__server__tool） -> (服务器, 原始工具名)
        self._index: dict[str, tuple[MCPServer, str]] = {}
        self._openai_tools: list[dict[str, Any]] = []

    @property
    def enabled(self) -> bool:
        return bool(self._servers)

    def openai_tools(self) -> list[dict[str, Any]]:
        return self._openai_tools

    def owns(self, name: str) -> bool:
        return name in self._index

    async def start(self) -> None:
        """启动时抓取各服务器的工具清单。单个服务器失败只记日志并跳过（降级）。"""
        if not self._servers:
            return
        for server in self._servers:
            try:
                tools = await self._list_tools(server)
            except Exception as exc:  # noqa: BLE001 - 启动期降级，不让单个 MCP 拖垮服务
                logger.warning(
                    "加载 MCP 服务器 %s 的工具失败，已跳过：%s",
                    server.name,
                    redact_secret(str(exc)),
                )
                continue
            available = {tool.name for tool in tools}
            for missing in [name for name in server.include if name not in available]:
                logger.warning("MCP 服务器 %s 的 tools 白名单里的 %s 不存在", server.name, missing)
            registered = 0
            for tool in tools:
                if not server.accepts(tool.name):
                    continue
                qualified = f"{MCP_TOOL_PREFIX}{server.name}__{tool.name}"
                self._index[qualified] = (server, tool.name)
                self._openai_tools.append(
                    {
                        "type": "function",
                        "function": {
                            "name": qualified,
                            "description": (tool.description or "")[:1024],
                            "parameters": _normalize_schema(tool.inputSchema),
                        },
                    }
                )
                registered += 1
            logger.info(
                "MCP 服务器 %s 共 %d 个工具，注册 %d 个", server.name, len(tools), registered
            )

    async def close(self) -> None:
        # 每次调用都是独立连接，无常驻资源需要释放。
        self._index.clear()
        self._openai_tools.clear()

    async def call(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        entry = self._index.get(name)
        if entry is None:
            raise ValueError(f"未知的 MCP 工具：{name}")
        server, tool_name = entry
        try:
            result = await self._call_tool(server, tool_name, arguments)
        except Exception as exc:
            # 脱敏后再抛出：上层会把它写进 tool_events、回传给模型与 API 响应。
            raise RuntimeError(f"调用 MCP 工具 {name} 失败：{redact_secret(str(exc))}") from None
        return _serialize_result(result, self.settings.mcp_result_max_chars)

    # ---- 内部：每次新建连接 ----

    async def _open_session(self, server: MCPServer):
        from mcp.client.session import ClientSession

        if server.transport == "sse":
            from mcp.client.sse import sse_client

            client_cm = sse_client(server.url, headers=server.headers or None)
        else:
            from mcp.client.streamable_http import streamablehttp_client

            client_cm = streamablehttp_client(server.url, headers=server.headers or None)
        return client_cm, ClientSession

    async def _list_tools(self, server: MCPServer):
        client_cm, ClientSession = await self._open_session(server)
        async with client_cm as streams:
            read, write = streams[0], streams[1]
            async with ClientSession(read, write) as session:
                await session.initialize()
                response = await session.list_tools()
                return response.tools

    async def _call_tool(self, server: MCPServer, tool_name: str, arguments: dict[str, Any]):
        client_cm, ClientSession = await self._open_session(server)
        timeout = timedelta(seconds=self.settings.mcp_timeout_seconds)
        async with client_cm as streams:
            read, write = streams[0], streams[1]
            async with ClientSession(read, write) as session:
                await session.initialize()
                return await session.call_tool(
                    tool_name, arguments, read_timeout_seconds=timeout
                )


def _normalize_schema(schema: Any) -> dict[str, Any]:
    if isinstance(schema, dict) and schema.get("type") == "object":
        return schema
    # 部分 MCP 工具不带 inputSchema 或给的不是 object，补一个空对象 schema。
    return {"type": "object", "properties": {}}


def _serialize_result(result: Any, max_chars: int) -> dict[str, Any]:
    texts: list[str] = []
    for block in getattr(result, "content", None) or []:
        text = getattr(block, "text", None)
        if text:
            texts.append(str(text))
        else:
            block_type = getattr(block, "type", "content")
            texts.append(f"[{block_type} 内容已省略]")
    combined = "\n".join(texts)
    if len(combined) > max_chars:
        combined = combined[:max_chars] + "…（结果过长已截断）"
    payload: dict[str, Any] = {
        "is_error": bool(getattr(result, "isError", False)),
        "text": combined,
    }
    structured = getattr(result, "structuredContent", None)
    if structured:
        payload["structured"] = structured
    return payload
