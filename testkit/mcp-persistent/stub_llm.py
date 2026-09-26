#!/usr/bin/env python3
"""OpenAI Chat SSE 桩：最后一条消息是工具结果就收尾（把结果原样说出来），否则调一次
`mcp_counter_count`。每次请求往 $STUB_REQUEST_LOG 落一行 JSON（system 提示词、最后一条消息）。"""

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ["STUB_PORT"])
LOG = os.environ.get("STUB_REQUEST_LOG")


def sse(handler, payloads):
    handler.send_response(200)
    handler.send_header("content-type", "text/event-stream")
    handler.end_headers()
    for payload in payloads:
        handler.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
    handler.wfile.write(b"data: [DONE]\n\n")
    handler.wfile.flush()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data":[{"id":"stub-model"}]}')

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))) or b"{}")
        messages = body.get("messages", [])
        last = messages[-1] if messages else {}
        if LOG:
            with open(LOG, "a", encoding="utf-8") as log:
                log.write(json.dumps({
                    "system": next((str(m.get("content")) for m in messages if m.get("role") == "system"), ""),
                    "last_role": last.get("role"),
                    "last": str(last.get("content")),
                }, ensure_ascii=False) + "\n")
        if last.get("role") == "tool":
            sse(self, [
                {"choices": [{"delta": {"content": f"result: {last.get('content')}"}}]},
                {"choices": [{"delta": {}, "finish_reason": "stop"}]},
            ])
            return
        sse(self, [
            {"choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": "call_count", "type": "function",
                "function": {"name": "mcp_counter_count", "arguments": "{}"},
            }]}}]},
            {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
        ])


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
