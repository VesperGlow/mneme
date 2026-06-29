from __future__ import annotations

import json
import os
import re
from functools import lru_cache
from typing import Any, Literal

from pydantic import Field, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

# 支持在 MCP 配置里用 ${NAME} 或 $NAME 引用环境变量，方便“只在 env 填 key”。
_ENV_REF = re.compile(r"\$\{(\w+)\}|\$(\w+)")


def _expand_env(value: str) -> str:
    def replace(match: re.Match[str]) -> str:
        name = match.group(1) or match.group(2)
        resolved = os.environ.get(name)
        if resolved is None:
            raise ValueError(f"MCP_SERVERS_JSON 引用了未设置的环境变量 {name}")
        return resolved

    return _ENV_REF.sub(replace, value)


def _string_list(value: Any, index: int, field_name: str) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError(f"MCP_SERVERS_JSON[{index}] 的 {field_name} 必须是字符串数组")
    return [str(item).strip() for item in value if str(item).strip()]


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env", env_file_encoding="utf-8", case_sensitive=False, extra="ignore"
    )

    app_api_key: str = ""
    log_level: str = "INFO"

    ai_base_url: str = ""
    ai_api_key: str = ""
    memory_model: str = ""
    chat_model: str = ""
    ai_timeout_seconds: float = 120
    ai_max_retries: int = 2
    ai_extra_headers_json: str = "{}"
    chat_max_output_tokens: int = 2048
    memory_max_output_tokens: int = 800
    max_tool_rounds: int = 6

    # 远程 MCP 工具服务器。JSON 数组，每项形如：
    # {"name":"tavily","url":"https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-xxx"}
    # 可选字段：transport（streamable_http|sse，默认 streamable_http）、headers（对象）、enabled（默认 true）。
    mcp_servers_json: str = "[]"
    mcp_timeout_seconds: float = 60
    mcp_result_max_chars: int = 12000

    embedding_base_url: str = "http://embedding:80"
    embedding_api_key: str = ""
    embedding_api_style: Literal["tei", "openai"] = "tei"
    embedding_model: str = "Qwen/Qwen3-Embedding-0.6B"
    embedding_dimensions: int = Field(default=1024, ge=32, le=4096)
    embedding_context_size: int = Field(default=32768, ge=128, le=131072)
    embedding_query_instruction: str = (
        "Given a user's message, retrieve memories that are useful for personalizing the response"
    )
    embedding_timeout_seconds: float = 180

    neo4j_uri: str = "bolt://neo4j:7687"
    neo4j_user: str = "neo4j"
    neo4j_password: str = ""
    neo4j_database: str = "neo4j"

    memory_search_limit: int = Field(default=8, ge=1, le=50)
    memory_min_score: float = Field(default=0.30, ge=-1, le=1)
    memory_history_messages: int = Field(default=16, ge=0, le=100)
    memory_duplicate_threshold: float = Field(default=0.995, ge=0.8, le=1)

    # 时序加权检索：在向量相似度之上叠加新近度、重要性与访问强化。
    # 相似度仍是主导，其余为小幅加成；全设 0 即退回纯相似度排序。
    memory_similarity_weight: float = Field(default=1.0, ge=0)
    memory_recency_weight: float = Field(default=0.15, ge=0)
    memory_importance_weight: float = Field(default=0.10, ge=0)
    memory_recency_halflife_days: float = Field(default=30.0, gt=0)

    @field_validator("ai_base_url", "embedding_base_url", "neo4j_uri")
    @classmethod
    def trim_url(cls, value: str) -> str:
        return value.strip().rstrip("/")

    @property
    def mcp_servers(self) -> list[dict[str, Any]]:
        try:
            raw = json.loads(self.mcp_servers_json or "[]")
        except json.JSONDecodeError as exc:
            raise ValueError("MCP_SERVERS_JSON 必须是合法 JSON") from exc
        if not isinstance(raw, list):
            raise ValueError("MCP_SERVERS_JSON 必须是 JSON 数组")
        servers: list[dict[str, Any]] = []
        seen: set[str] = set()
        for index, item in enumerate(raw):
            if not isinstance(item, dict):
                raise ValueError("MCP_SERVERS_JSON 的每一项必须是对象")
            if not item.get("enabled", True):
                continue
            name = str(item.get("name", "")).strip()
            url = _expand_env(str(item.get("url", "")).strip())
            if not name or not url:
                raise ValueError(f"MCP_SERVERS_JSON[{index}] 缺少 name 或 url")
            if name in seen:
                raise ValueError(f"MCP_SERVERS_JSON 出现重复的 name：{name}")
            seen.add(name)
            transport = str(item.get("transport", "streamable_http")).strip().lower()
            if transport not in {"streamable_http", "sse"}:
                raise ValueError(f"MCP_SERVERS_JSON[{index}] 的 transport 只能是 streamable_http 或 sse")
            headers = item.get("headers") or {}
            if not isinstance(headers, dict):
                raise ValueError(f"MCP_SERVERS_JSON[{index}] 的 headers 必须是对象")
            include = _string_list(item.get("tools"), index, "tools")
            exclude = _string_list(item.get("exclude"), index, "exclude")
            servers.append(
                {
                    "name": name,
                    "url": url,
                    "transport": transport,
                    "headers": {str(k): _expand_env(str(v)) for k, v in headers.items()},
                    "include": include,
                    "exclude": exclude,
                }
            )
        return servers

    @property
    def ai_extra_headers(self) -> dict[str, str]:
        try:
            raw = json.loads(self.ai_extra_headers_json or "{}")
        except json.JSONDecodeError as exc:
            raise ValueError("AI_EXTRA_HEADERS_JSON 必须是合法 JSON") from exc
        if not isinstance(raw, dict):
            raise ValueError("AI_EXTRA_HEADERS_JSON 必须是 JSON 对象")
        return {str(k): str(v) for k, v in raw.items()}

    @property
    def safe_summary(self) -> dict[str, object]:
        return {
            "ai_base_url": self.ai_base_url,
            "memory_model": self.memory_model,
            "chat_model": self.chat_model,
            "embedding_base_url": self.embedding_base_url,
            "embedding_api_style": self.embedding_api_style,
            "embedding_model": self.embedding_model,
            "embedding_dimensions": self.embedding_dimensions,
            "embedding_context_size": self.embedding_context_size,
            "neo4j_uri": self.neo4j_uri,
            "neo4j_database": self.neo4j_database,
            "mcp_servers": [server["name"] for server in self.mcp_servers],
        }


@lru_cache
def get_settings() -> Settings:
    return Settings()

