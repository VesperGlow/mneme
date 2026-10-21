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
args = parser.parse_args()
token = os.environ.get("MNEME_API_TOKEN")
if not token:
    raise SystemExit("请设置 MNEME_API_TOKEN")


def request(path, body):
    return urllib.request.urlopen(urllib.request.Request(
        args.url.rstrip("/") + path, data=json.dumps(body, ensure_ascii=False).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
    ), timeout=180)


print("持续对话（/quit 退出，再次启动会继续当前对话）")
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
    answer = ""
    print("Mneme：", end="", flush=True)
    try:
        with request("/v1/chat", {"request_id": turn, "message": text}) as response:
            event = ""
            for raw in response:
                line = raw.decode("utf-8").rstrip("\r\n")
                if line.startswith("event:"):
                    event = line[6:].strip()
                elif line.startswith("data:"):
                    data = json.loads(line[5:].strip())
                    if event == "delta":
                        answer += data["content"]
                    elif event == "reply_reset":
                        answer = ""
                    elif event == "done":
                        committed = True
                        # A plain terminal cannot reliably retract wrapped text.
                        # Render only the committed answer, without tool preambles.
                        print(answer.strip(), end="", flush=True)
                    elif event == "error":
                        print(f"\n[本轮失败：{data['error']}；临时输出未保存。]", end="")
        print()
        if not committed:
            print(f"未确认保存，可查询 GET /v1/turns/{turn}")
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        print(f"\n连接失败：{error}；可查询 GET /v1/turns/{turn}")
