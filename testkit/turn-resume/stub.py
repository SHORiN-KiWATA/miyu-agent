#!/usr/bin/env python3
"""断点续跑黑盒(09-24)的桩 LLM:OpenAI 兼容 SSE,按对话里的暗号分流。

    TK slow <记号> [秒]   调 run_command:往 counter.txt 追加一行记号,再睡几秒(默认 30;
                          测具趁它睡着的时候杀 daemon 或让它关停)
    TK crashloop          同上,记号固定为 L;而且续跑那一轮也照样再跑一次,
                          用来看续跑次数一直往上数、没有上限
    TK sub <记号>         前台派一个子代理,任务是 `TK slow <记号>`(子会话里睡着的时候杀 daemon)
    TK goal ...           `/goal` 的目标原文带它:第一轮跑慢工具 GR,之后每轮记一行 GRn 就收

收到续跑消息(`<service-restart attempt="N">`)时:crashloop 那条会话再跑一次慢工具;
目标续轮(`goal-round="true"`)记一行 GR-resumed(这一轮得干点活,驱动器才接着开下一轮);
其余回 `RESUMED attempt=N`,续跑消息里列了接着跑的子代理就再带上 `subagents=<个数>`。
后台任务汇报(子代理跑完)回 `GOT REPORT`。工具结果回来就收尾 `DONE`。

每个请求记一行 JSONL:最后那条真用户消息的开头、请求里有没有「被打断的工具调用」
那句、有没有 `<interrupted-turn-recovery>`,取证用。
"""
import json
import os
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18582"))
LOG = os.environ.get("STUB_LOG", "")
RESTART = re.compile(r'<service-restart attempt="(\d+)"')
REPORT = "<background-job-report"
INTERRUPTED_TOOL = "tool execution was interrupted"


def log_line(obj):
    if LOG:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj, ensure_ascii=False) + "\n")


def text_of(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def call(name, arguments):
    return {"tool_calls": [{
        "index": 0,
        "id": f"call_{int(time.time() * 1000) % 10_000_000}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)},
    }]}


def say(text):
    return {"content": text}


def meaningful(text):
    return "TK " in text or RESTART.search(text) is not None or text.startswith(REPORT)


def latest_user(messages):
    """最后一条真 user 消息。回合在它后面还会垫几条尾巴(宿主环境之类),要跳过。"""
    for index in range(len(messages) - 1, -1, -1):
        message = messages[index]
        if message.get("role") == "user" and meaningful(text_of(message)):
            return text_of(message)
    return ""


def slow(marker, seconds=30):
    return call("run_command", {
        "command": f"echo {marker} >> counter.txt; sleep {seconds}; echo woke",
        "description": "slow step",
    })


def quick(marker):
    return call("run_command", {"command": f"echo {marker} >> counter.txt", "description": "quick step"})


def decide(messages):
    last = messages[-1] if messages else {}
    if last.get("role") == "tool":
        return say("DONE")
    text = latest_user(messages)
    users = [text_of(m) for m in messages if m.get("role") == "user"]
    crashloop = any("TK crashloop" in u for u in users)
    if text.startswith(REPORT):
        return say("GOT REPORT")
    restart = RESTART.search(text)
    if restart:
        if crashloop:
            return slow("L")
        if 'goal-round="true"' in text:
            return quick("GR-resumed")
        subagents = sum(1 for line in text.splitlines() if line.startswith("- job "))
        tail = f" subagents={subagents}" if subagents else ""
        return say(f"RESUMED attempt={restart.group(1)}{tail}")
    if "<goal_round>" in text and "TK goal" in text:
        rounds = sum(1 for u in users if "<goal_round>" in u)
        return slow("GR") if rounds <= 1 else quick(f"GR{rounds}")
    at = text.find("TK ")
    task = text[at:] if at >= 0 else text
    if task.startswith("TK crashloop"):
        return slow("L")
    if task.startswith("TK sub"):
        parts = task.split()
        marker = parts[2] if len(parts) > 2 else "SUB"
        return call("subagent", {"description": f"child {marker}", "prompt": f"TK slow {marker}"})
    if task.startswith("TK slow"):
        parts = task.split()
        seconds = int(parts[3]) if len(parts) > 3 and parts[3].isdigit() else 30
        return slow(parts[2] if len(parts) > 2 else "X", seconds)
    return say("OK")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        payload = json.dumps({"object": "list", "data": [{"id": "stub-model", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        messages = body.get("messages", [])
        reply = decide(messages)
        raw = json.dumps(messages, ensure_ascii=False)
        log_line({
            "last_role": (messages[-1] if messages else {}).get("role"),
            "user_head": latest_user(messages)[:120],
            "interrupted_tool": INTERRUPTED_TOOL in raw,
            "recovery_note": "<interrupted-turn-recovery>" in raw,
            "reply": reply,
        })
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        chunk = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-model",
                 "choices": [{"index": 0, "delta": {"role": "assistant", **reply}, "finish_reason": None}]}
        done = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-model",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if "tool_calls" in reply else "stop"}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}}
        for event in (chunk, done):
            self.wfile.write(f"data: {json.dumps(event, ensure_ascii=False)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
