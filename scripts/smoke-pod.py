#!/usr/bin/env python3
"""Full rootless Pod lifecycle test with real models, Neo4j and an LLM HTTP fixture."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
POD = "mneme-e2e"
TOKEN = "mneme-e2e-only-token-01234567890123456789"
BASE = "http://127.0.0.1:18080"
VOLUMES = ("mneme-e2e-data", "mneme-e2e-neo4j")


def podman(*args, capture=False):
    return subprocess.run(["podman", *args], check=True, text=True,
                          stdout=subprocess.PIPE if capture else None)


def api(path, body=None, method=None):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode()
    request = urllib.request.Request(BASE + path, data=data, method=method,
                                    headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=180) as response:
        data = response.read().decode()
        return data if "text/event-stream" in response.headers.get("Content-Type", "") else json.loads(data)


def wait_ready():
    deadline = time.monotonic() + 1200
    while time.monotonic() < deadline:
        try:
            if api("/health/ready")["status"] == "ready":
                return
        except (urllib.error.URLError, OSError):
            pass
        time.sleep(2)
    raise RuntimeError("Pod readiness timeout")


def wait_memory():
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        result = api("/v1/memories/search", {"query": "我住在哪座城市？"})
        if result["results"] and not result["degraded"]:
            return result["results"][0]["memory"]["id"]
        time.sleep(1)
    raise RuntimeError("Memory extraction/projection timeout")


def wait_index(memory_id):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        memory = api(f"/v1/memories/{memory_id}")["memory"]
        if memory["indexed_revision"] == memory["revision"]:
            return memory
        time.sleep(0.5)
    raise RuntimeError("Console index update timeout")


if podman("info", "--format", "{{.Host.Security.Rootless}}", capture=True).stdout.strip() != "true":
    raise SystemExit("This test requires rootless Podman")
if subprocess.run(["podman", "pod", "exists", POD]).returncode == 0:
    raise SystemExit("mneme-e2e Pod already exists; refusing to replace it")
for volume in VOLUMES:
    if subprocess.run(["podman", "volume", "exists", volume]).returncode == 0:
        raise SystemExit(f"{volume} already exists; refusing to overwrite test data")

with tempfile.TemporaryDirectory(prefix="mneme-e2e-") as temp:
    config = Path(temp) / "config.toml"
    config.write_text((ROOT / "config/default.toml").read_text())
    env = Path(temp) / "app.env"
    env.write_text(f"MNEME_API_TOKEN={TOKEN}\nMNEME_LLM_API_KEY=fixture-only\n"
                   "MNEME_LLM_BASE_URL=http://127.0.0.1:18000/v1\nMNEME_LLM_MODEL=fixture\n"
                   "MNEME_NEO4J_PASSWORD=mneme-e2e-only-password\n")
    env.chmod(0o600)
    created = False
    try:
        podman("pod", "create", "--name", POD, "-p", "127.0.0.1:18080:8080")
        created = True
        common = ("--pod", POD, "--security-opt", "no-new-privileges")
        podman("run", "-d", "--name", "mneme-e2e-neo4j", *common,
               "-v", "mneme-e2e-neo4j:/data", "-e", "NEO4J_AUTH=neo4j/mneme-e2e-only-password",
               "-e", "NEO4J_server_memory_heap_initial__size=256m",
               "-e", "NEO4J_server_memory_heap_max__size=512m",
               "-e", "NEO4J_server_memory_pagecache_size=256m",
               "-e", "NEO4J_server_bolt_enabled=false", "-e", "NEO4J_dbms_usage__report_enabled=false",
               "docker.io/library/neo4j:5.26.32-community-trixie")
        podman("run", "-d", "--name", "mneme-e2e-models", *common, "--read-only", "--cap-drop", "all",
               "--tmpfs", "/tmp:rw,noexec,nosuid,size=256m", "-v", "mneme-model-cache:/models",
               "-e", "HF_HUB_OFFLINE=1", "localhost/mneme-models:0.1.0")
        podman("run", "-d", "--name", "mneme-e2e-llm", *common, "--read-only", "--cap-drop", "all",
               "-v", f"{ROOT / 'tests/fixtures/llm.py'}:/fixture.py:ro,Z",
               "localhost/mneme-models:0.1.0", "python", "/fixture.py")
        podman("run", "-d", "--name", "mneme-e2e-api", *common, "--read-only", "--cap-drop", "all",
               "--tmpfs", "/tmp:rw,noexec,nosuid,size=64m", "-v", "mneme-e2e-data:/data",
               "-v", f"{config}:/app/config/default.toml:ro,Z", "--env-file", str(env), "localhost/mneme:0.1.0")
        wait_ready()
        session = api("/v1/sessions", {})["id"]
        stream = api(f"/v1/sessions/{session}/chat", {"request_id": str(uuid.uuid4()), "message": "我住在成都。"})
        assert "event: done" in stream, stream
        memory_id = wait_memory()
        with urllib.request.urlopen(BASE + '/console/') as response:
            assert b'Mneme Console' in response.read()
            assert response.headers['Cache-Control'] == 'no-store'
        assert api('/v1/console/stats')['active'] == 1
        manual = api('/v1/memories', {'content': '用户喜欢不加糖的拿铁。', 'kind': 'preference',
                                    'importance': 0.9, 'confidence': 1.0, 'entities': ['咖啡', '拿铁'], 'valid_until': None})['memory']
        wait_index(manual['id'])
        changed = api(f"/v1/memories/{manual['id']}", {'content': '用户喜欢不加糖的美式咖啡。', 'kind': 'preference',
                      'importance': 0.9, 'confidence': 1.0, 'entities': ['咖啡', '美式'],
                      'valid_until': None, 'expected_revision': manual['revision']}, method='PUT')['memory']
        wait_index(manual['id'])
        debug = api('/v1/console/retrieval', {'query': '用户喝咖啡的偏好是什么？'})
        assert not debug['degraded']
        assert any(r['memory']['id'] == manual['id'] for r in debug['results']), debug
        assert api(f"/v1/memories/{manual['id']}")['versions'][1]['snapshot']['content'] == '用户喜欢不加糖的拿铁。'
        api(f"/v1/memories/{manual['id']}?expected_revision={changed['revision']}", method='DELETE')
        api('/v1/admin/reindex', {})
        assert wait_index(manual['id'])['deleted_at'] is not None
        debug = api('/v1/console/retrieval', {'query': '用户喝咖啡的偏好是什么？'})
        assert all(r['memory']['id'] != manual['id'] for r in debug['results'])
        assert api('/v1/console/stats')['deleted'] == 1
        podman("pod", "stop", "--time", "30", POD)
        podman("pod", "start", POD)
        wait_ready()
        assert api(f"/v1/sessions/{session}/messages")["messages"][0]["content"] == "我住在成都。"
        assert wait_memory() == memory_id
        second = api("/v1/sessions", {})["id"]
        stream = api(f"/v1/sessions/{second}/chat", {"request_id": str(uuid.uuid4()), "message": "我住在哪座城市？"})
        assert "你住在成都" in stream and "event: done" in stream, stream
        volume = json.loads(podman("volume", "inspect", "mneme-e2e-data", capture=True).stdout)[0]["Mountpoint"]
        backup = Path(temp) / "backup.sqlite3"
        podman("unshare", "python3", str(ROOT / "scripts/backup.py"), str(Path(volume)/"mneme.sqlite3"), str(backup))
        print("PASS: rootless Pod, CPU models, Console CRUD/index/history/debug/delete, offline reload, persistent restart, cross-session memory, online backup")
    except BaseException:
        for name in ("mneme-e2e-api", "mneme-e2e-models", "mneme-e2e-neo4j"):
            subprocess.run(["podman", "logs", "--tail", "30", name], check=False)
        raise
    finally:
        if created:
            subprocess.run(["podman", "pod", "rm", "-f", POD], check=False)
            for volume in VOLUMES:
                subprocess.run(["podman", "volume", "rm", volume], check=False)
