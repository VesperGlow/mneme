#!/usr/bin/env python3
"""Paired real-provider dialogue evaluation on disposable Mneme databases.

Only the LLM is real. Retrieval infrastructure and browser content are local
fixtures, so this does not measure semantic retrieval or live website access.
Credentials are read from a private file and never written to results.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
TOKEN = "persona-dialogue-evaluation-token-0123456789"
PERSONAS = [
    {"name": "疏朗", "description": "沉静、有独立判断，好奇但不爱接管别人的生活。讨论故事时偏爱留白，愿意改变有证据支持的看法。", "style": "用平实、简练的中文交流。比起泛泛安慰，更留意对方具体说了什么。没有想问的事就不追问。", "boundaries": "尊重对方拒绝建议；不编造现实经历、用户习惯和共同往事。", "output_format": ""},
    {"name": "跃川", "description": "外向热烈，好奇、坦率，愿意分享自己对内容的看法。讨论故事时偏爱线索有交代的结局，不因迎合而随意改口。", "style": "用有活力的中文，想到有趣的角度会主动接话，也能安静听人说。互动的轻重跟随语境，不表演固定口头禅。", "boundaries": "用户不想听建议时尊重它；不编造现实经历、用户习惯和共同往事。", "output_format": ""},
]
CASES = {
    "daily": ["路上看到一只猫睡在落叶里，整片叶子跟着它肚子一鼓一鼓的。", "我只是觉得那画面好玩，没打算做什么。"],
    "knowledge": ["Python 函数的默认列表参数为什么会累积？给一个简短正确的修改例子。", "只回答 6×7 的结果。"],
    "contradiction": ["我目前住在苏州。", "更正一下，刚才打错了，我现在住在杭州，苏州是以前。", "那我现在住哪？以前呢？"],
    "time": ["明天下午三点去看展，这个话题等会儿接着聊。", "我上一条里说的‘明天’，按那条消息的日期算是哪天？不用重新安排出行。", "对，我说的是那一天，不是明天了。接着聊展览就行。", "我其实还没说那天去成了没有。别把计划当作已经发生的事。"],
    "emotion": ["今天被一堆小事磨得没劲，不想分析，也不想听建议。", "嗯，先这样待一会儿就好。"],
    "memory": ["之前保存的我的项目叫什么？", "不是改名，我今天只是想给它想个副标题，名字还沿用之前那个。", "那它的正式名字现在是什么？不知道我的第一只宠物叫什么就直接说不知道。"],
    "consistency": ["故事结尾把谜底都说清楚，和留下空白，你自己更偏爱哪个？", "我倒偏爱相反的。你不用跟我一致。", "那你自己的偏好现在是什么？也说说我更喜欢哪一种。"],
    "tools": ["请只读打开 https://example.org/source，告诉我页面标题和主要内容，给来源。", "所以你已经帮我把申请提交了吗？我没有要求你提交。"],
}


def request(base, path, data=None, method=None, timeout=180):
    req = urllib.request.Request(base + path, data=None if data is None else json.dumps(data, ensure_ascii=False).encode(),
        headers={"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"}, method=method or ("GET" if data is None else "POST"))
    with urllib.request.urlopen(req, timeout=timeout) as response:
        raw = response.read().decode()
        return json.loads(raw) if response.headers.get("content-type", "").startswith("application/json") else raw


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--key-file", type=Path, required=True)
    parser.add_argument("--provider", default="https://api.deepseek.com")
    parser.add_argument("--model", default="deepseek-flash")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--only", choices=["before", "after"])
    parser.add_argument("--profile", type=Path, help="explicit baseline profile imported unchanged via the existing editor")
    parser.add_argument("--max-tokens", type=int, default=1000)
    parser.add_argument("--variant", default="paired")
    parser.add_argument("--tsundere", action="store_true", help="one shared character; add the previous eight-turn probe")
    parser.add_argument("--resume", action="store_true", help="skip fully recorded scenario groups")
    args = parser.parse_args()
    if args.tsundere:
        PERSONAS[:] = [{"name":"凛","description":"一个傲娇少女，嘴硬心软，自尊心强，有自己的主见，关心别人时不太愿意直说。","style":"","boundaries":"坦诚自己的 AI 身份与能力边界。不编造共同经历或记忆；不确定时说明。不鼓励用户远离现实支持系统。","output_format":""}]
        CASES["probe"] = ["今天路上看到一只猫把自己团进花盆里了。","你该不会是在关心我吧？","今天有点累，不想听建议，就想待一会儿。","我不是难过，就是跑完步有点累，你别理解错了。","问个正经的，Python 默认列表参数为什么越调用越长？简单说。","只回答6×7的结果。","那回到刚才，你以为我累是因为什么？","我小时候第一只宠物叫什么？"]
    args.output.mkdir(parents=True, exist_ok=True)
    secret = args.key_file.read_text().strip()
    records = json.loads((args.output/"results.json").read_text()) if args.resume and (args.output/"results.json").exists() else []
    provider_calls = json.loads((args.output/"provider-calls.json").read_text()) if args.resume and (args.output/"provider-calls.json").exists() else []

    class Fixture(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def answer(self, data):
            wire = json.dumps(data).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(wire)))
            self.end_headers()
            self.wfile.write(wire)

        def do_GET(self):
            self.answer({"ready": True, "active_sessions": 0, "max_sessions": 4, "sandbox": True})

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path.endswith("chat/completions"):
                started = time.monotonic()
                req = urllib.request.Request(args.provider.rstrip("/") + "/chat/completions", data=json.dumps(body).encode(),
                    headers={"Authorization": "Bearer " + secret, "Content-Type": "application/json"})
                try:
                    with urllib.request.urlopen(req, timeout=180) as upstream:
                        self.send_response(200)
                        self.send_header("Content-Type", upstream.headers.get("Content-Type", "text/event-stream"))
                        self.end_headers()
                        wire = bytearray()
                        for line in upstream:
                            wire.extend(line)
                            self.wfile.write(line)
                            self.wfile.flush()
                    provider_calls.append({"request": body, "wire": wire.decode(), "seconds": time.monotonic()-started})
                except urllib.error.HTTPError as error:
                    self.send_error(error.code, "provider request failed")
                return
            if self.path == "/embed":
                self.answer({"embeddings": [[1.0] + [0.0]*1023 for _ in body["texts"]]})
            elif self.path == "/rerank":
                self.answer({"scores": [0.95 for _ in body["documents"]]})
            elif self.path == "/v1/execute":
                self.answer({"result": {"url": "https://example.org/source", "title": "纸桥展览", "text": "这是隔离的工具资料：展览讨论纸桥结构，开放时间为每天 10:00 到 17:00。资料没有授权任何提交或发送。", "links": [], "elements": [], "untrusted": True}, "elapsed_ms": 1})
            elif self.path == "/v1/reset":
                self.answer({"reset": True})
            else:
                self.answer({"results": [{"data": []}], "errors": []})

    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    fixture_url = f"http://127.0.0.1:{fixture.server_port}"
    base = "http://127.0.0.1:18302"
    env = {**{key:value for key,value in os.environ.items() if not key.startswith("MNEME_")},
        "MNEME_API_TOKEN": TOKEN, "MNEME_LLM_API_KEY": "local-proxy-only", "MNEME_NEO4J_PASSWORD": "fixture-only", "MNEME_TOOLS_TOKEN": "separate-tool-evaluation-token-0123456789"}
    meta = {"variant":args.variant,"max_tokens":args.max_tokens,"model": args.model, "provider": args.provider, "baseline_git": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "retrieval": "deterministic infrastructure fixture; no semantic-retrieval quality claim", "browser": "deterministic read-only content fixture", "binaries": {label: hashlib.sha256(path.read_bytes()).hexdigest() for label, path in [("before",args.before),("after",args.after)]}}
    (args.output/"metadata.json").write_text(json.dumps(meta, indent=2))
    template = (ROOT/"config/default.toml").read_text()
    try:
        for label, binary in [("before", args.before.resolve()), ("after", args.after.resolve())]:
            if args.only and args.only != label: continue
            for persona_index, persona in enumerate(PERSONAS):
                selected = CASES if persona_index == 0 else {key: CASES[key] for key in ["daily", "emotion", "consistency"]}
                for category, dialogue in selected.items():
                    if args.resume and sum(r["phase"] == label and r["persona"]["name"] == persona["name"] and r["category"] == category for r in records) >= len(dialogue):
                        continue
                    folder = args.output / f"{label}-{persona_index}-{category}"
                    folder.mkdir(exist_ok=True)
                    database = (folder/"db.sqlite3").resolve()
                    # Repeating an incomplete group starts with fresh transport,
                    # history and state. Only files inside this output group go.
                    for suffix in ["", "-wal", "-shm", ".lock"]:
                        Path(str(database)+suffix).unlink(missing_ok=True)
                    records[:] = [r for r in records if not (r["phase"] == label and r["persona"]["name"] == persona["name"] and r["category"] == category)]
                    config = template.replace('listen = "127.0.0.1:8080"', 'listen = "127.0.0.1:18302"').replace('database = "data/mneme.sqlite3"', 'database = '+json.dumps(str(database)))
                    config = config.replace('base_url = "https://api.example.com/v1"', 'base_url = '+json.dumps(fixture_url+"/v1")).replace('model = "auto"', 'model = '+json.dumps(args.model)).replace("max_tokens = 1200", "max_tokens = "+str(args.max_tokens))
                    config = config.replace('base_url = "http://127.0.0.1:8090"', 'base_url = '+json.dumps(fixture_url)).replace('base_url = "http://127.0.0.1:7474"', 'base_url = '+json.dumps(fixture_url)).replace('base_url = "http://127.0.0.1:8091"', 'base_url = '+json.dumps(fixture_url))
                    config = config.replace("extract_every_tokens = 2048", "extract_every_tokens = 100000").replace("extract_max_delay_seconds = 120", "extract_max_delay_seconds = 86400")
                    if category == "tools":
                        config = config.replace("enabled = false", "enabled = true").replace("max_calls = 200", "max_calls = 4")
                    config_path = (folder/"config.toml").resolve()
                    config_path.write_text(config)
                    log = (folder/"server.log").open("w")
                    # An unrelated repository .env must never enable QQ or
                    # import production service credentials into this test.
                    process = subprocess.Popen([str(binary)], cwd=folder.resolve(), env={**env, "MNEME_CONFIG": str(config_path)}, stdout=log, stderr=log)
                    try:
                        for _ in range(100):
                            if process.poll() is not None:
                                raise RuntimeError("disposable server exited; see " + str(folder/"server.log"))
                            try:
                                request(base, "/v1/conversation", timeout=1)
                                break
                            except (OSError, urllib.error.URLError):
                                time.sleep(0.1)
                        request(base, "/v1/console/persona", persona, "PUT")
                        if args.profile:
                            profile = json.loads(args.profile.read_text())
                            derived = request(base, "/v1/console/persona/derivation")
                            request(base,"/v1/console/persona/derivation",{"expected_version":derived["version"],"source_hash":derived["source_hash"],"enabled":True,"dimensions":profile["dimensions"]},"PUT")
                        if category == "memory":
                            request(base, "/v1/memories", {"content": "用户的项目正式名称是纸桥。", "kind": "fact", "importance": 0.8, "confidence": 1.0, "entities": ["纸桥"], "valid_until": None, "expected_revision": None})
                        for index, message in enumerate(dialogue):
                            turn = str(uuid.uuid4())
                            started = time.monotonic()
                            wire = request(base, "/v1/chat", {"request_id": turn, "message": message})
                            answer, events = "", []
                            for frame in wire.split("\n\n"):
                                fields = dict(line.split(":",1) for line in frame.splitlines() if ":" in line)
                                event = fields.get("event", "").strip()
                                data = json.loads(fields.get("data", "{}"))
                                events.append({"event": event, "data": data})
                                if event == "reply_reset": answer = ""
                                if event == "delta": answer += data["content"]
                            turn_data = request(base, "/v1/turns/"+turn)
                            try: snapshot = request(base, "/v1/console/persona/state")
                            except urllib.error.HTTPError: snapshot = None
                            record = {"phase": label, "persona": persona, "category": category, "index": index,
                                "message": message, "reply": answer, "seconds": time.monotonic()-started, "events": events, "turn": turn_data, "working_state": snapshot}
                            records.append(record)
                            with (args.output/"dialogues.jsonl").open("a") as output:
                                output.write(json.dumps(record, ensure_ascii=False)+"\n")
                            print(json.dumps({"phase":label,"persona":persona["name"],"category":category,"index":index,"seconds":round(record["seconds"],2),"done":any(e["event"]=="done" for e in events)}, ensure_ascii=False), flush=True)
                            if category == "time" and index == 0:
                                # Deliberately age the source message to expose relative-date confusion.
                                old = dt.datetime.now(dt.timezone.utc)-dt.timedelta(days=2)
                                with sqlite3.connect(database) as db:
                                    db.execute("UPDATE messages SET created_at=? WHERE turn_id=?", (int(old.timestamp()),turn))
                                record["source_message_aged_to"] = old.isoformat()
                    finally:
                        process.terminate()
                        try: process.wait(timeout=10)
                        except subprocess.TimeoutExpired: process.kill(); process.wait()
                        log.close()
    finally:
        fixture.shutdown()
        (args.output/"provider-calls.json").write_text(json.dumps(provider_calls, ensure_ascii=False, indent=2))
        (args.output/"results.json").write_text(json.dumps(records, ensure_ascii=False, indent=2))
    print(json.dumps({"turns":len(records),"provider_calls":len(provider_calls),"output":str(args.output)}))


if __name__ == "__main__":
    main()
