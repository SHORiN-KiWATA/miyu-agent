#!/usr/bin/env python3
"""子代理会话化黑盒(09-18)的桩 LLM:OpenAI 兼容 SSE,按「谁在问」分流。

身份靠系统提示词认:通用子代理提示词开头是「你是通用任务子代理」,其余当主会话。
子/孙代理的任务写在它会话的第一条 user 消息里(父派的 prompt),暗号:

    CHILD plain            直接交结论 CHILD_RESULT ok
    CHILD run-bg-cmd       起后台命令(sleep 3)就结束回合;被唤醒后交 CHILD_AFTER_BG done
    CHILD run-bg-cmd-long  同上但 sleep 60(给「重启标中断」用)
    CHILD spawn-gc         前台开孙代理 GRANDCHILD plain,拿到结果后交 CHILD_GC_RESULT …
    CHILD spawn-gc-bg      后台开孙代理,结束回合;被唤醒后交 CHILD_AFTER_GC done
    CHILD spawn-gc-depth   前台开孙代理 GRANDCHILD try-spawn(孙代理试图再开一层)
    CHILD slow             前台跑 sleep 40(给「父被停级联」用)
    CHILD nap              前台跑 sleep 2.5 再交 CHILD_RESULT ok:比主回合收尾慢,报告一定另起一轮
    CHILD quick-nap        前台跑 sleep 1 再交 CHILD_QUICK ok(和 nap 配对:同一轮派出去、先后跑完)
    GRANDCHILD plain       交 GC_RESULT ok
    GRANDCHILD try-spawn   工具面里有 subagent 就调(应被拒),没有就交 GC_NO_SUBAGENT_TOOL

主会话的暗号在最后一条 user 消息里:TK fg-plain / fg-gc / fg-gc-bg / fg-bgcmd / fg-slow /
gc-depth / bg-plain / bg-wait / nap / pair(一次并排派 nap 与 quick-nap 两个)。被后台任务唤醒
(<background-job-report>)时回 `WOKEN: reports=<这一轮带了几份> ` + 报告。
每个请求都记一行 JSONL(系统提示词开头、工具面、最后一条消息的角色与开头),取证用。
"""
import json
import os
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18547"))
LOG = os.environ.get("STUB_LOG", "")


def log_line(obj):
    if not LOG:
        return
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")


def content_text(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def tool_call(name, arguments, call_id="call_1", index=0):
    return {
        "index": index,
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)},
    }


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
        systems = [content_text(m) for m in messages if m.get("role") == "system"]
        system_head = (systems[0] if systems else "")[:200]
        tools = sorted(t.get("function", {}).get("name") for t in body.get("tools") or [])
        users = [content_text(m) for m in messages if m.get("role") == "user"]
        last = messages[-1] if messages else {}
        last_text = content_text(last)
        task = users[0] if users else ""
        # 身份按任务暗号认(dev 子代理用的是 dev 提示词,不带「通用任务子代理」字样)。
        role = "subagent" if task.startswith(("CHILD", "GRANDCHILD", "GREATGRANDCHILD")) else "main"
        # 主会话的暗号在最后一条真实 user 消息里;子代理的任务在第一条 user 消息里。
        directive = ""
        if role == "main":
            for text in reversed(users):
                if "TK " in text:
                    directive = text
                    break
        else:
            directive = task
        # 后台汇报叫醒的那一轮：本地会话里汇报就是最后一条消息；QQ 那边是「请求上下文 / 汇报 /
        # 系统说明」几条 user 消息连着，所以数这一轮末尾那一串 user 消息里有几份汇报外壳。
        tail = []
        for message in reversed(messages):
            if message.get("role") != "user":
                break
            tail.append(content_text(message))
        tail_text = "\n".join(reversed(tail))
        reports = tail_text.count("<background-job-report>")
        log_line({
            "t": time.time(),
            "role": role,
            "reports": reports,
            "system_head": system_head[:80],
            "tools": tools,
            "last_role": last.get("role"),
            "last_head": last_text[:120],
            "directive": directive[:80],
        })

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        base = {"id": "stub", "object": "chat.completion.chunk", "model": body.get("model")}

        def sse(payload):
            self.wfile.write(b"data: " + json.dumps(payload, ensure_ascii=False).encode() + b"\n\n")
            self.wfile.flush()

        def done():
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def text_reply(text):
            half = max(1, len(text) // 2)
            for chunk in (text[:half], text[half:]):
                sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": chunk}, "finish_reason": None}]})
            sse({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                 "usage": {"prompt_tokens": 40, "completion_tokens": 10, "total_tokens": 50}})
            done()

        def call(name, arguments):
            calls([(name, arguments)])

        def calls(pairs):
            batch = [tool_call(name, arguments, f"call_{i + 1}", i) for i, (name, arguments) in enumerate(pairs)]
            sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": batch}, "finish_reason": None}]})
            sse({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                 "usage": {"prompt_tokens": 40, "completion_tokens": 10, "total_tokens": 50}})
            done()

        # ── 工具结果回来了:按刚才调的是什么工具、任务是什么来交结论 ──
        if last.get("role") == "tool":
            output = last_text
            called = ""
            for m in reversed(messages):
                if m.get("role") == "assistant" and m.get("tool_calls"):
                    called = m["tool_calls"][-1].get("function", {}).get("name", "")
                    break
            if role == "main":
                if called == "subagent" and "background_subagent" in output:
                    text_reply("PARENT_STARTED_BG " + output)
                else:
                    text_reply("PARENT_DONE " + output)
                return
            if "run-bg-cmd" in directive and called == "run_command":
                text_reply("CHILD_STARTED_BG")
                return
            if "spawn-gc-bg" in directive and called == "subagent":
                text_reply("CHILD_STARTED_GC")
                return
            if "spawn-gc" in directive and called == "subagent":
                text_reply("CHILD_GC_RESULT " + output)
                return
            if "slow" in directive:
                text_reply("CHILD_SLOW_DONE")
                return
            if directive.startswith("CHILD quick-nap"):
                text_reply("CHILD_QUICK ok")
                return
            if directive.startswith("CHILD nap"):
                text_reply("CHILD_RESULT ok")
                return
            if "GRANDCHILD" in directive and called == "subagent":
                text_reply("GC_SPAWN_RESULT " + output)
                return
            text_reply("TOOL_DONE " + output[:200])
            return

        # ── daemon 合成的唤醒轮(后台任务/子代理完成) ──
        if reports:
            if role == "main":
                # 回话里别原样带汇报外壳：QQ 群历史会把这句话带进后面的轮，外壳一在就又被认成唤醒。
                echoed = tail_text[-4000:].replace("<background-job-report>", "[report]")
                echoed = echoed.replace("</background-job-report>", "[/report]")
                text_reply(f"WOKEN: reports={reports} " + echoed)
            elif "run-bg-cmd" in directive:
                text_reply("CHILD_AFTER_BG done")
            elif "spawn-gc-bg" in directive:
                text_reply("CHILD_AFTER_GC done")
            else:
                text_reply("CHILD_WOKEN " + last_text[:200])
            return

        if last.get("role") != "user":
            text_reply("ok")
            return

        # ── 主会话:暗号按整词认(fg-gc 与 fg-gc-bg 不能互相串),取最后一条带 TK 的
        # user 消息——最后一条消息可能是回合的瞬态尾巴,不一定是用户那句。
        if role == "main":
            spawn = {
                "fg-plain": ("plain child", "CHILD plain", False),
                "fg-gc": ("gc child", "CHILD spawn-gc", False),
                "fg-gc-bg": ("gc-bg child", "CHILD spawn-gc-bg", False),
                "fg-bgcmd": ("bgcmd child", "CHILD run-bg-cmd", False),
                "fg-slow": ("slow child", "CHILD slow", False),
                "nap": ("nap child", "CHILD nap", False),
                "gc-depth": ("depth child", "CHILD spawn-gc-depth", False),
                "bg-plain": ("bg plain child", "CHILD plain", True),
                "bg-wait": ("bg wait child", "CHILD run-bg-cmd-long", True),
            }
            token = re.search(r"TK (\S+)", directive)
            key = token.group(1) if token else ""
            if key == "pair" and "subagent" in tools:
                calls([("subagent", {"description": "pair nap", "prompt": "CHILD nap"}),
                       ("subagent", {"description": "pair quick", "prompt": "CHILD quick-nap"})])
                return
            if key in spawn and "subagent" in tools:
                description, prompt, background = spawn[key]
                args = {"description": description, "prompt": prompt}
                if background:
                    args["background"] = True
                call("subagent", args)
                return
            text_reply("ok")
            return

        # ── 子代理 / 孙代理 ──
        if "TK continue" in last_text:
            text_reply("CHILD_CONTINUED")
            return
        if directive.startswith("CHILD plain"):
            text_reply("CHILD_RESULT ok")
            return
        if directive.startswith("CHILD run-bg-cmd"):
            seconds = 60 if "long" in directive else 3
            call("run_command", {"command": f"sleep {seconds}; echo BGDONE # miyu-subagent-test", "background": True, "title": "bg probe"})
            return
        if directive.startswith("CHILD spawn-gc-bg"):
            call("subagent", {"description": "bg grandchild", "prompt": "GRANDCHILD plain", "background": True})
            return
        if directive.startswith("CHILD spawn-gc-depth"):
            call("subagent", {"description": "depth grandchild", "prompt": "GRANDCHILD try-spawn"})
            return
        if directive.startswith("CHILD spawn-gc"):
            call("subagent", {"description": "grandchild", "prompt": "GRANDCHILD plain"})
            return
        if directive.startswith("CHILD quick-nap"):
            call("run_command", {"command": "sleep 1", "timeout_seconds": 60})
            return
        if directive.startswith("CHILD nap"):
            call("run_command", {"command": "sleep 2.5", "timeout_seconds": 60})
            return
        if directive.startswith("CHILD slow"):
            # sh 会把单条命令 exec 成 sleep 本体,注释标记会丢;用独特的时长当标记。
            call("run_command", {"command": "sleep 40.5", "timeout_seconds": 120})
            return
        if directive.startswith("GRANDCHILD try-spawn"):
            if "subagent" in tools:
                call("subagent", {"description": "great-grandchild", "prompt": "GREATGRANDCHILD plain"})
            else:
                text_reply("GC_NO_SUBAGENT_TOOL")
            return
        if directive.startswith("GRANDCHILD"):
            text_reply("GC_RESULT ok")
            return
        text_reply("SUB_OK " + directive[:60])


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
