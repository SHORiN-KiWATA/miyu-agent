#!/usr/bin/env python3
"""OpenAI 兼容桩(成员工具走查用):第一幕按 STUB_CALLS(JSON 数组 [{name, args}])把工具
全叫一遍,第二幕说一句话收尾。给 member_tools_probe.py 用:看成员回合里脚本工具、
print_image、read/edit 的显示名与沙盒表现。"""
import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18497"))
CALLS = json.loads(os.environ.get("STUB_CALLS", "[]"))
# 每个请求的第一条 system 消息追加一行 JSON 到这个文件(验环境块里的 sandbox 属性)。
DUMP = os.environ.get("STUB_DUMP_SYSTEM")


def content_text(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _sse(self, delta):
        payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-tools",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.flush()
        time.sleep(0.02)

    def _finish(self, reason="stop"):
        payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-tools",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
                   "usage": {"prompt_tokens": 40, "completion_tokens": 30, "total_tokens": 70}}
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}") if length else {}
        messages = body.get("messages", [])
        if DUMP:
            system = next((content_text(m) for m in messages if m.get("role") == "system"), "")
            # 沙盒说明 09-23 起不在系统提示词里，是请求尾部「变了才追加」的一条
            # `<sandbox …/>`（1069b7d0）：记最近那一条，就是这次请求时的沙盒状态。
            sandbox = ([t for t in map(content_text, messages) if t.startswith("<sandbox")] or [""])[-1]
            with open(DUMP, "a", encoding="utf-8") as f:
                f.write(json.dumps({"system": system, "sandbox": sandbox}, ensure_ascii=False) + "\n")
        # 一轮的第一个请求以用户消息收尾 → 把工具全叫一遍;带着工具结果回来的第二个
        # 请求 → 说一句话收尾。按「最后一条是不是 user」判,同一会话连跑几轮都成立
        # (09-13 沙盒走查要在同一会话上绑定→解绑各跑一轮)。
        last_is_user = bool(messages) and messages[-1].get("role") == "user"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        if last_is_user and CALLS:
            for index, call in enumerate(CALLS):
                self._sse({"tool_calls": [{"index": index, "id": f"call_{index}", "type": "function",
                                           "function": {"name": call["name"], "arguments": json.dumps(call.get("args", {}), ensure_ascii=False)}}]})
            self._finish("tool_calls")
        else:
            self._sse({"content": "工具都试过了。"})
            self._finish()

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        payload = json.dumps({"data": [{"id": "stub-tools"}]}).encode()
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
