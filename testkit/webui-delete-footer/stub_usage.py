#!/usr/bin/env python3
"""OpenAI 兼容桩:主回合按用户消息里的「用量 N」报 prompt_tokens=N,回一句「收到」。

给 WebUI 信息行(上下文圆环、累计)的走查造出大小分明的两条会话。只认带 tools 的
主回合请求;标题之类的辅助请求一律报 1,免得把账算乱。
"""
import json
import os
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18511"))
USAGE = re.compile(r"用量\s*(\d+)")


def _text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict))
    return ""


def _usage_for(body):
    if not body.get("tools"):
        return 1
    for message in reversed(body.get("messages") or []):
        if message.get("role") != "user":
            continue
        found = USAGE.search(_text(message.get("content")))
        if found:
            return int(found.group(1))
    return 1


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({"object": "list", "data": [{"id": "stub-usage", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        prompt = _usage_for(body)
        usage = {"prompt_tokens": prompt, "completion_tokens": 20, "total_tokens": prompt + 20}
        if not body.get("stream"):
            reply = json.dumps({
                "id": "stub", "object": "chat.completion", "model": "stub-usage",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "收到"},
                             "finish_reason": "stop"}],
                "usage": usage,
            }, ensure_ascii=False).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        chunks = [
            {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "收到"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage},
        ]
        for chunk in chunks:
            payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-usage", **chunk}
            self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
