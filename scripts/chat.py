#!/usr/bin/env python3
"""Minimal UTF-8 SSE terminal client using only the Python standard library."""
import argparse
import json
import os
import urllib.error
import urllib.request
import uuid

parser = argparse.ArgumentParser(description="Mneme 中文终端对话")
parser.add_argument("--url", default="http://127.0.0.1:8080")
parser.add_argument("--session", help="继续已有会话 UUID；不传则创建新会话")
args = parser.parse_args()
token = os.environ.get("MNEME_API_TOKEN")
if not token:
    raise SystemExit("请设置 MNEME_API_TOKEN")


def request(path, body):
    return urllib.request.urlopen(urllib.request.Request(
        args.url.rstrip("/") + path, data=json.dumps(body, ensure_ascii=False).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
    ), timeout=180)


session = args.session
if session is None:
    with request("/v1/sessions", {}) as response:
        session = json.load(response)["id"]
print(f"会话：{session}（/quit 退出）")
while True:
    try:
        text = input("你：").strip()
    except (EOFError, KeyboardInterrupt):
        print()
        break
    if text == "/quit":
        break
    if not text:
        continue
    turn = str(uuid.uuid4())
    committed = False
    print("Mneme：", end="", flush=True)
    try:
        with request(f"/v1/sessions/{session}/chat", {"request_id": turn, "message": text}) as response:
            event = ""
            for raw in response:
                line = raw.decode("utf-8").rstrip("\r\n")
                if line.startswith("event:"):
                    event = line[6:].strip()
                elif line.startswith("data:"):
                    data = json.loads(line[5:].strip())
                    if event == "delta":
                        print(data["content"], end="", flush=True)
                    elif event == "done":
                        committed = True
                    elif event == "error":
                        print(f"\n[本轮失败：{data['error']}；上面的部分输出未保存。]", end="")
        print()
        if not committed:
            print(f"未确认保存，可查询 GET /v1/turns/{turn}")
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        print(f"\n连接失败：{error}；可查询 GET /v1/turns/{turn}")
