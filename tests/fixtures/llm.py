"""Deterministic OpenAI-compatible fixture for full Pod smoke tests; no paid API."""
import json
import signal
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
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
            target = json.loads(messages[-1]["content"])["target"]["content"]
            memories = []
            if "我住在成都" in target:
                memories.append({"key": "user.residence", "content": "用户住在成都。", "kind": "fact",
                                 "importance": 0.8, "confidence": 0.99, "entities": ["成都"],
                                 "source_quote": "我住在成都", "valid_until": None,
                                 "sensitive": False, "equivalent_to": None})
            data = json.dumps({"choices": [{"message": {"content": json.dumps({"memories": memories}, ensure_ascii=False)},
                                            "finish_reason": "stop"}]}).encode()
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)


signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
with ThreadingHTTPServer(("127.0.0.1", 18000), Handler) as server:
    server.serve_forever()
