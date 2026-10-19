"""Deterministic OpenAI-compatible fixture for full Pod smoke tests; no paid API."""
import json
import signal
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    sent = []
    lock = threading.Lock()

    def json_response(self, value):
        data = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/qq-sent':
            with self.lock:
                self.json_response(self.sent)
        else:
            self.json_response({'data': [{'id': 'fixture'}]})

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == '/app/getAppAccessToken':
            assert body == {'appId': '12345678', 'clientSecret': 'mneme-qq-smoke-only-secret'}
            self.json_response({'access_token': 'fixture-qq-access-token', 'expires_in': '7200'})
            return
        if self.path.startswith('/v2/users/'):
            assert self.headers['Authorization'] == 'QQBot fixture-qq-access-token'
            assert self.path == '/v2/users/mneme-qq-owner/messages'
            with self.lock:
                if not any(m['msg_id'] == body['msg_id'] and m['msg_seq'] == body['msg_seq'] for m in self.sent):
                    self.sent.append(body)
            self.json_response({'id': 'fixture-qq-reply'})
            return
        messages = body["messages"]
        self.send_response(200)
        if body.get("stream"):
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            memory = "\n".join(m["content"] for m in messages[:-1])
            answer = "你住在成都。" if "用户住在成都" in memory else "我记下你告诉我的事情了。"
            for chunk in (
                {"choices": [{"delta": {"content": answer}, "finish_reason": None}]},
                {"choices": [{"delta": {}, "finish_reason": "stop"}]},
            ):
                self.wfile.write(("data: " + json.dumps(chunk, ensure_ascii=False) + "\n\n").encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            payload = json.loads(messages[-1]["content"])
            targets = payload.get("targets", [payload.get("target", {})])
            target = next((t for t in reversed(targets) if "我住在成都" in t.get("content", "")), {})
            memories = []
            if "我住在成都" in target.get("content", ""):
                memories.append({"key": "user.residence", "content": "用户住在成都。", "kind": "fact",
                                 "importance": 0.8, "confidence": 0.99, "entities": ["成都"],
                                 "source_quote": "我住在成都", "valid_until": None,
                                 "sensitive": False, "equivalent_to": None})
            if memories and "targets" in payload:
                memories[0]["source_id"] = target["id"]
            result = {"memories": memories}
            if "targets" in payload:
                result["processed_source_ids"] = [t["id"] for t in targets]
            if "max_tokens" in payload:
                text = payload.get("previous_summary", "") + "。".join(m["content"] for m in payload["messages"] if m["role"] == "user")
                result = {"summary": text[:payload["max_tokens"]] or "继续此前的话题。"}
            data = json.dumps({"choices": [{"message": {"content": json.dumps(result, ensure_ascii=False)},
                                            "finish_reason": "stop"}]}).encode()
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)


signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
with ThreadingHTTPServer(("127.0.0.1", 18000), Handler) as server:
    server.serve_forever()
