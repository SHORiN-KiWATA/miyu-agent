#!/usr/bin/env python3
"""`smooth_probe.py` 专用的桩模型：每一轮都能按提示词里的记号调几次工具、慢慢吐字。

`testkit/repl-smoke/stub_llm.py` 的阶段表按**整个会话**里已有几条工具结果走，
第一轮用完之后后面的轮次就只剩说话——撑不出「每轮都有一条时间线」的长会话。
这里按**这一轮**（最后一条 user 消息之后）数工具结果：

- 提示词带 `TOOLS:<n>`：这一轮先调 n 次 `run_command`，每次之前想一句；
- 提示词带 `SLOW`：正文 3 字一块、20ms 一块地吐（≈ 真模型的节奏）；否则一口气吐完；
- 正文是一段带标题、列表、代码块的长 markdown。

用法：STUB_PORT=18499 python3 smooth_stub.py
"""

import json
import os
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18499"))

REASONING = "先想一想这一步要做什么，再决定怎么下手。" * 6
REPLY = (
    "## 小结\n\n"
    + "".join(
        f"- 第 {i} 条：这是一段用来撑长会话的正文，带一点 `代码` 和 **强调**，"
        "长到在终端里会折一次行才对，否则量不出折行的开销。\n"
        for i in range(1, 13)
    )
    + "\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n最后一句。\n"
)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _sse(self, payload):
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.flush()

    def _delta(self, key, text):
        self._sse({"choices": [{"index": 0, "delta": {key: text}, "finish_reason": None}]})

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length) if length else b"{}"
        try:
            messages = json.loads(body).get("messages", [])
        except Exception:
            messages = []
        log_path = os.environ.get("SMOOTH_STUB_LOG")
        if log_path:
            with open(log_path, "a", encoding="utf-8") as log:
                log.write(json.dumps([(m.get("role"), str(m.get("content"))[:80]) for m in messages],
                                     ensure_ascii=False) + "\n")
        # 用户那句后面还跟着一条 `<runtime …>` 瞬时尾巴（也是 user 角色），跳过这类注入。
        last_user = max(
            (i for i, m in enumerate(messages)
             if m.get("role") == "user" and not str(m.get("content")).lstrip().startswith("<")),
            default=-1,
        )
        prompt = str(messages[last_user].get("content")) if last_user >= 0 else ""
        done = sum(1 for m in messages[last_user + 1:] if m.get("role") == "tool")
        match = re.search(r"TOOLS:(\d+)", prompt)
        tools = int(match.group(1)) if match else 0
        slow = "SLOW" in prompt
        step, pause = (3, 0.02) if slow else (400, 0.0)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        for start in range(0, len(REASONING), step):
            self._delta("reasoning_content", REASONING[start:start + step])
            if pause:
                time.sleep(pause)
        if done < tools:
            lines = "\\n".join(f"第 {k} 行输出" for k in range(1, 7))
            arguments = json.dumps(
                {"command": f"printf '{lines}\\n'", "title": f"第 {done + 1} 条命令"},
                ensure_ascii=False,
            )
            self._sse({"choices": [{"index": 0, "delta": {"tool_calls": [{
                "index": 0,
                "id": f"call_{done}_{time.time_ns()}",
                "type": "function",
                "function": {"name": "run_command", "arguments": arguments},
            }]}, "finish_reason": None}]})
            self._sse({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                       "usage": {"prompt_tokens": 1000 + len(body) // 4, "completion_tokens": 30,
                                 "total_tokens": 1030 + len(body) // 4}})
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            return
        for start in range(0, len(REPLY), step):
            self._delta("content", REPLY[start:start + step])
            if pause:
                time.sleep(pause)
        self._sse({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 1000 + len(body) // 4, "completion_tokens": 400,
                             "total_tokens": 1400 + len(body) // 4}})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        payload = json.dumps({"data": [{"id": "stub-model"}]}).encode()
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
