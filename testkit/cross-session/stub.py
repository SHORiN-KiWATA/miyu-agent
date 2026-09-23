#!/usr/bin/env python3
"""跨会话消息黑盒(09-23)的桩 LLM:OpenAI 兼容 SSE,按最后一条消息分流。

暗号写在 `miyu ask` 发出去的那句话里:

    TK list                      调 send_to_other_running_session(list),把结果原样回出来
    TK send <会话id> <正文>       调 send,回 SENT: + 工具结果
    TK slow                      跑 `sleep 6` 再收尾;收尾时看这一轮里有没有插进来的跨会话消息

收到跨会话消息(`<cross-session-message`)时:
    正文含 PING  → 用 send 回一句 PONG 给发件会话(外壳里的 session="…"),然后收尾
    正文含 PONG  → 收尾 GOT_PONG,不再回(不然两边互相回个没完)
    其余         → 收尾 GOT: + 正文

每个请求记一行 JSONL(最后一条消息的角色与开头、工具面里有没有这件工具),取证用。
"""
import json
import os
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18581"))
LOG = os.environ.get("STUB_LOG", "")
TOOL = "send_to_other_running_session"
ENVELOPE = re.compile(r'<cross-session-message from="(?P<name>[^"]*)" session="(?P<session>[^"]*)">\n(?P<rest>.*)</cross-session-message>', re.S)


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


def envelope_body(text):
    match = ENVELOPE.search(text)
    if not match:
        return None
    rest = match.group("rest")
    # 第一行是那句发件说明,正文在它后面。
    body = rest.split("\n", 1)[1] if "\n" in rest else rest
    return match.group("name"), match.group("session"), body.strip()


def directive(text):
    """暗号从「TK 」起算:网页发起的回合,用户消息前面还垫着别的内容。"""
    at = text.find("TK ")
    return text[at:] if at >= 0 else text


def meaningful(text):
    return "TK " in text or ENVELOPE.search(text) is not None


def latest_user(messages):
    """最后一条真 user 消息及其下标。网页回合会在它后面再垫一条尾巴(artifact
    工作区之类),那条要跳过。"""
    for index in range(len(messages) - 1, -1, -1):
        message = messages[index]
        if message.get("role") == "user" and meaningful(text_of(message)):
            return text_of(message), index
    return "", -1


def decide(messages):
    last = messages[-1] if messages else {}
    text, index = latest_user(messages)
    if last.get("role") == "tool":
        result = text_of(last)
        task = directive(text)
        if task.startswith("TK list"):
            return say("LIST_RESULT " + result)
        if task.startswith("TK send"):
            return say("SENT: " + result)
        if task.startswith("TK slow"):
            return say("SLOW_DONE_NO_FOLLOWUP")
        found = envelope_body(text)
        if found and "PING" in found[2]:
            return say("REPLIED_PONG " + result)
        return say("DONE " + result[:200])
    found = envelope_body(text)
    if found:
        name, session, body = found
        # 跑着的轮里插进来的(前面是工具结果),和新起的一轮分开记。
        midturn = index >= 1 and messages[index - 1].get("role") == "tool"
        if "MIDTURN" in body:
            return say(("SAW_MIDTURN " if midturn else "GOT_MIDTURN_AS_NEW_TURN ") + body)
        if "PING" in body:
            return call(TOOL, {"action": "send", "session_id": session, "message": f"PONG from the other side (to {name})"})
        if "PONG" in body:
            return say("GOT_PONG " + body)
        return say("GOT: " + body[:80])
    text = directive(text)
    if text.startswith("TK list"):
        return call(TOOL, {"action": "list"})
    if text.startswith("TK send "):
        _, _, target, body = text.split(" ", 3)
        return call(TOOL, {"action": "send", "session_id": target, "message": body})
    if text.startswith("TK slow"):
        return call("run_command", {"command": "sleep 6", "description": "wait"})
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
        tools = sorted(t.get("function", {}).get("name") for t in body.get("tools") or [])
        reply = decide(messages)
        last = messages[-1] if messages else {}
        log_line({
            "last_role": last.get("role"),
            "last_head": text_of(last)[:160],
            "has_tool": TOOL in tools,
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
