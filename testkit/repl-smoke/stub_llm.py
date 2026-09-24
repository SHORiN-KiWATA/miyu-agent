#!/usr/bin/env python3
"""REPL 走查用的桩 LLM:流式吐一小段回复,带真实 usage。

分块是为了让回合层量得出「每秒 token」——单块请求不计入速度。

用法:STUB_PORT=18498 python3 stub_llm.py
"""

import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("STUB_PORT", "18498"))
CHUNK_CHARS = int(os.environ.get("STUB_CHUNK_CHARS", "3"))
CHUNK_SLEEP = float(os.environ.get("STUB_CHUNK_SLEEP", "0.02"))
# 置 STUB_REPLY 换正文：走查链接/markdown 渲染时要一段带链接的。
REPLY = os.environ.get(
    "STUB_REPLY",
    "好的,收到。这是一段用于走查的回复,分块吐出来好让 footer 量得出每秒 token。",
)
# 默认不发思考:老的走查脚本按「回复就是全部输出」断言。置 STUB_REASONING=1
# 才多吐一段 reasoning_content,给全屏 TUI 的「点击展开」测具用。
REASONING = os.environ.get("STUB_REASONING")
# 置 STUB_TOOL_GAP=<秒>:想完之后静默这么久才吐工具调用。有的线路把整段工具调用
# 扣着、模型写完才放(09-24 实测 opencodego 的 deepseek 1.5–3.9s、bigmodel 的 glm
# 12s),这段静默里屏上该只剩转轮(`timeline/stall.rs`)。
TOOL_GAP = float(os.environ.get("STUB_TOOL_GAP", "0"))
# 置 STUB_TOOL=1:第一次请求先要一次 run_command,拿到结果再正常作答。
# 全屏 TUI 的时间线、命令窥视、点开看完整输出都得有真工具才验得了。
TOOL = os.environ.get("STUB_TOOL")
# 置 STUB_TODO=1:先写一份任务清单。全屏下表格排在时间线之后,
# 「表被 Worked for 截断 / 表要等整条时间线跑完才出现」只能这么验。
TODO = os.environ.get("STUB_TODO")
TODO_ITEMS = int(os.environ.get("STUB_TODO_ITEMS", "4"))
# 置 STUB_STAGE_PREFACE=1:第一段之后每次调工具前先吐一句正文。正文是时间线的
# 分段点,于是一轮里能出好几个 `Worked for` —— 「表被下一段收缩行截断」要两段
# 才撞得上。
STAGE_PREFACE = os.environ.get("STUB_STAGE_PREFACE")
# 置 STUB_ASK=1:第一次请求先问一个问题。全屏下提问面板是盖上去的,一退场
# 就没了,「问了什么答了什么有没有进正文」只能这么验。
ASK = os.environ.get("STUB_ASK")
# 置 STUB_SUBAGENT=1:主线先派一个子代理。子代理自己会想一段、跑一条命令,
# 于是全屏下能验「点开子代理 → 覆盖层里是它自己的时间线」。
SUBAGENT = os.environ.get("STUB_SUBAGENT")
SUBAGENT_MARK = "子代理走查任务"
# 主线用户消息里必然有、子对话里必然没有的一段。
MAIN_MARK = os.environ.get("STUB_MAIN_MARK", "走查一句")
TOOL_COMMAND = os.environ.get("STUB_TOOL_COMMAND", "printf '走查用的命令输出\\n第二行\\n'")
# 子代理内层跑的那条。默认和主线同一条；走查里会换成一条**慢的**——面板、
# 状态行上的那些量只有在它还跑着的时候才看得见，瞬间跑完就什么都测不到。
SUBAGENT_COMMAND = os.environ.get("STUB_SUBAGENT_COMMAND", TOOL_COMMAND)
# 后台子代理内层那条。它要多跑几轮——后台任务一收工，状态行和面板就跟着没了，
# 只跑一条的话"趁它还活着点开看看"这件事根本来不及做。
SUBAGENT_BG_COMMAND = os.environ.get("STUB_SUBAGENT_BG_COMMAND", SUBAGENT_COMMAND)
SUBAGENT_BG_ROUNDS = int(os.environ.get("STUB_SUBAGENT_BG_ROUNDS", "3"))
# 置 STUB_EXTRA_CALLS='[{"name":"load_tools","arguments":{"names":["x"]}},{"name":"x","arguments":{}}]'：
# 按顺序再调这几个工具（走查脚本工具的显示名之类，阶段表里没有的都从这儿来）。
EXTRA_CALLS = json.loads(os.environ.get("STUB_EXTRA_CALLS", "[]"))
# 置 STUB_BACKGROUND=1:再派一条后台命令。后台任务的状态行点开是日志面板,
# 只有真有后台任务在跑才验得了。
BACKGROUND = os.environ.get("STUB_BACKGROUND")
# 置 STUB_EDIT=1:改一次文件。全屏下"编辑文件"那一步点开该是**补丁 diff**,
# 而且路径只出现一次(时间线那行已经写过了)。
EDIT = os.environ.get("STUB_EDIT")
EDIT_PATH = os.environ.get("STUB_EDIT_PATH", "/tmp/miyu-tui-smoke/walk.txt")
# 置 STUB_FAIL=1:跑一条必定失败的命令。失败的那一步要渲染成红的。
FAIL = os.environ.get("STUB_FAIL")
FAIL_COMMAND = os.environ.get(
    "STUB_FAIL_COMMAND", "printf '走查用的报错\\n' >&2; exit 3"
)
BACKGROUND_COMMAND = os.environ.get(
    "STUB_BACKGROUND_COMMAND",
    "for i in 1 2 3 4 5 6 7 8 9 10; do echo \"后台第 $i 行\"; sleep 1; done",
)
REASONING_TEXT = os.environ.get(
    "STUB_REASONING_TEXT",
    "先看一眼需求,再决定怎么下手。这段是思考正文,折叠时看不到,点开才有。",
)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _sse(self, payload):
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.flush()

    def do_POST(self):
        # 置 STUB_HTTP_STATUS=500:直接回一个错误状态,用来验「回合报错时前端怎么
        # 收场」。不带流,内容也无所谓——调用方要的就是失败。
        status = os.environ.get("STUB_HTTP_STATUS")
        if status and status != "200":
            body = json.dumps({"error": {"message": "stub 故意失败"}}).encode()
            self.send_response(int(status))
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        # 模型请求的往返延迟:两批工具之间 live 区空着的那个窗口靠它撑开。
        time.sleep(float(os.environ.get("STUB_RESPONSE_DELAY", "0")))
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length) if length else b""
        # 置 STUB_REQUEST_LOG=<文件>:把每次请求的消息列表落一行 JSON。
        # 「模型到底收到了什么」只有这儿看得见——会话历史是 daemon 现拼的,
        # 库里那份不等于送进去的那份。
        log_path = os.environ.get("STUB_REQUEST_LOG")
        if log_path:
            try:
                payload = json.loads(body or b"{}")
                with open(log_path, "a", encoding="utf-8") as log:
                    log.write(json.dumps({
                        "at": time.time(),
                        "messages": [
                            {"role": message.get("role"),
                             # 全长也记下来：只记前缀的话，「完整回答」被我
                             # 自己截短会看成「模型收到半截」（09-19 踩过）。
                             "len": len(str(message.get("content"))),
                             "content": str(message.get("content"))[:4000]}
                            for message in payload.get("messages", [])
                        ],
                    }, ensure_ascii=False) + "\n")
            except Exception:
                pass
        # 按请求里已有几条 tool 结果决定这一轮要什么,免得来回死循环。
        done = body.count(b'"role": "tool"') + body.count(b'"role":"tool"')
        # 子代理的子对话是独立的一份消息列表,按任务标记认出来:它只跑一条命令
        # 就收工,不然会和主线的阶段表打架。
        # 只看标记不行：派子代理之后，**主线**的消息里也留着那段 prompt（在
        # assistant 的 tool_call 参数里）。用「有没有主线那条用户消息」来分。
        # 走查里要「再开一条后台任务」时用：消息里带这个记号就当场派一条，
        # 不看阶段表。（阶段表是按"这一轮已经有几条工具结果"走的，同一个会话里
        # 过了 background 那一格就再也派不出来了。）
        # `done` 数的是**整个会话**的工具结果，不是这一轮的——拿它当"还没派过"
        # 的判据，跑到后面永远为假。改成看这条命令自己有没有出现在历史里。
        wants_background = b"STUB_BG" in body and b"BG2" not in body
        # 后台子代理：派出去那条的 prompt 里带 `BGSUB-SENT`，历史里认得出来，
        # 免得每轮再派一条。
        wants_bg_subagent = b"STUB_SUBBG" in body and b"BGSUB-SENT" not in body
        # 消息里带 `STUB_USAGE` 就去查一次本会话用量，不看阶段表——阶段表是
        # 一轮内跑完的，而「这个会话烧了多少」要等**上一轮**落库才有数。
        wants_usage = b"STUB_USAGE" in body and b"Token \xe6\xb6\x88\xe8\x80\x97" not in body
        inside_subagent = (
            SUBAGENT_MARK.encode() in body and MAIN_MARK.encode() not in body
        )
        inside_bg_subagent = inside_subagent and b"BGSUB-SENT" in body
        if inside_subagent:
            rounds = SUBAGENT_BG_ROUNDS if inside_bg_subagent else 1
            stage = "tool" if done < rounds else None
        elif wants_bg_subagent:
            stage = "background_subagent"
        elif wants_usage:
            stage = "usage"
        elif wants_background:
            stage = "background2"
        else:
            stages = []
            if ASK:
                stages.append("ask")
            if SUBAGENT:
                stages.append("subagent")
            if TODO:
                stages.append("todo")
            if TOOL:
                for round_index in range(int(os.environ.get("STUB_TOOL_ROUNDS", "1"))):
                    stages.append("tool")
                    # 清单反复写是真实用法(每做完一件就推进一格),而每一次都会
                    # 切一段——「思考行被下一步顶掉」只在第二次之后才看得见。
                    if TODO and os.environ.get("STUB_TODO_REPEAT"):
                        stages.append("todo")
            for index in range(len(EXTRA_CALLS)):
                stages.append(f"extra:{index}")
            if EDIT:
                stages.append("edit")
            if FAIL:
                stages.append("fail")
            if BACKGROUND:
                stages.append("background")
            stage = stages[done] if done < len(stages) else None
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        if stage is not None:
            # 调工具之前先想一句。真模型都是这样的，而且这一段思考**只**存在于
            # 中间回合——`turns.assistant_reasoning` 那一列只留得住最后一回合
            # 那份，所以它正好把「重开之后思考行没了」这条钉住。
            if REASONING:
                for chunk in ("先想一句，", "再动手。"):
                    self._sse({"choices": [{"index": 0,
                                            "delta": {"reasoning_content": chunk},
                                            "finish_reason": None}]})
                    time.sleep(CHUNK_SLEEP)
            if STAGE_PREFACE and done > 0:
                for chunk in (f"第 {done + 1} 段开始,", "接着干。\n\n"):
                    self._sse({"choices": [{"index": 0,
                                            "delta": {"content": chunk},
                                            "finish_reason": None}]})
                    time.sleep(CHUNK_SLEEP)
            if TOOL_GAP > 0:
                time.sleep(TOOL_GAP)
            if stage == "ask":
                for line in os.environ.get("STUB_ASK_PREFACE", "").splitlines(keepends=True):
                    self._sse({"choices": [{"index": 0,
                                            "delta": {"content": line},
                                            "finish_reason": None}]})
                    time.sleep(CHUNK_SLEEP)
                name = "ask_question"
                arguments = json.dumps({"questions": [{
                    "header": "走查",
                    "question": "走查用的问题：选一个",
                    "options": [
                        {"label": "甲选项", "description": "第一个"},
                        {"label": "乙选项", "description": "第二个"},
                    ],
                }]}, ensure_ascii=False)
                if os.environ.get("STUB_ASK_QUESTIONS"):
                    arguments = json.dumps({"questions": json.loads(os.environ["STUB_ASK_QUESTIONS"])},
                                           ensure_ascii=False)
            elif stage == "subagent":
                name = "subagent"
                arguments = json.dumps({
                    "description": "走查子代理",
                    "prompt": f"{SUBAGENT_MARK}：跑一条命令看看，然后简单说一句。",
                }, ensure_ascii=False)
            elif stage == "background_subagent":
                name = "subagent"
                arguments = json.dumps({
                    "description": "走查后台子代理",
                    "prompt": f"{SUBAGENT_MARK}BGSUB-SENT：跑一条命令看看，然后简单说一句。",
                    "background": True,
                }, ensure_ascii=False)
            elif stage == "todo":
                name = "todowrite"
                todos = [
                    {"content": f"走查任务 {i + 1}:这一条长到足以铺满一行表格",
                     "status": "completed" if i == 0 else
                               ("in_progress" if i == 1 else "pending"),
                     "priority": "medium"}
                    for i in range(TODO_ITEMS)
                ]
                arguments = json.dumps({"todos": todos}, ensure_ascii=False)
            elif stage.startswith("extra:"):
                call = EXTRA_CALLS[int(stage.split(":", 1)[1])]
                name = call["name"]
                arguments = json.dumps(call.get("arguments", {}), ensure_ascii=False)
            elif stage == "edit":
                name = "edit"
                patch = (
                    "*** Begin Patch\n"
                    f"*** Add File: {EDIT_PATH}\n"
                    "+走查用的第一行\n"
                    "+走查用的第二行\n"
                    "*** End Patch\n"
                )
                arguments = json.dumps({"patchText": patch}, ensure_ascii=False)
            elif stage == "fail":
                name = "run_command"
                arguments = json.dumps({"command": FAIL_COMMAND}, ensure_ascii=False)
            elif stage == "background":
                name = "run_command"
                arguments = json.dumps({
                    "command": BACKGROUND_COMMAND,
                    "background": True,
                    "title": "走查后台任务",
                }, ensure_ascii=False)
            elif stage == "usage":
                name = "query_session_token_usage"
                arguments = "{}"
            elif stage == "background2":
                # 第二条：命令里带 `BG2` 当"已经派过"的记号，免得每轮再派一条。
                name = "run_command"
                arguments = json.dumps({
                    "command": "echo BG2; " + BACKGROUND_COMMAND,
                    "background": True,
                    "title": "走查后台任务二",
                }, ensure_ascii=False)
            else:
                name = "run_command"
                # 子代理内层跑的那条要慢一点：面板标题上的工具次数与词元、状态行
                # 上那串量，都只有在它还跑着的时候才看得见。
                if inside_bg_subagent:
                    command = SUBAGENT_BG_COMMAND
                elif inside_subagent:
                    command = SUBAGENT_COMMAND
                else:
                    command = TOOL_COMMAND
                arguments = json.dumps(
                    {"command": command,
                     "title": os.environ.get("STUB_TOOL_TITLE", "跑个命令")},
                    ensure_ascii=False,
                )
            # 参数分片吐（`STUB_TOOL_ARG_CHUNK=<每片字符数>`）：真供应商是先把
            # 工具名解码出来、参数随后才流完，中间那段窗口才是「准备执行 /
            # 准备问题」的显示。默认仍一次吐完，免得改动现有走查的时序。
            arg_chunk = int(os.environ.get("STUB_TOOL_ARG_CHUNK", "0"))
            if arg_chunk > 0:
                self._sse({"choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": f"call_stub_{done}",
                    "type": "function",
                    "function": {"name": name},
                }]}, "finish_reason": None}]})
                for start in range(0, len(arguments), arg_chunk):
                    time.sleep(CHUNK_SLEEP)
                    self._sse({"choices": [{"index": 0, "delta": {"tool_calls": [{
                        "index": 0,
                        "function": {"arguments": arguments[start:start + arg_chunk]},
                    }]}, "finish_reason": None}]})
            else:
                self._sse({"choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": f"call_stub_{done}",
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }]}, "finish_reason": None}]})
            # 带工具调用的那一轮也报 usage。真供应商都报，而子代理跑到一半时
            # 面板标题与状态行上的词元数就是从这儿来的——不报的话那两个数一路
            # 是 0，测具看着"有数"其实什么都没验到。
            tool_prompt = 120
            if os.environ.get("STUB_USAGE_BY_SIZE"):
                tool_prompt += len(body) // 4
            self._sse({"choices": [{"index": 0, "delta": {},
                                    "finish_reason": "tool_calls"}],
                       "usage": {"prompt_tokens": tool_prompt, "completion_tokens": 30,
                                 "total_tokens": tool_prompt + 30}})
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            return
        if REASONING:
            for start in range(0, len(REASONING_TEXT), CHUNK_CHARS):
                self._sse({"choices": [{"index": 0,
                                        "delta": {"reasoning_content":
                                                  REASONING_TEXT[start:start + CHUNK_CHARS]},
                                        "finish_reason": None}]})
                time.sleep(CHUNK_SLEEP)
        for start in range(0, len(REPLY), CHUNK_CHARS):
            self._sse({"choices": [{"index": 0,
                                    "delta": {"content": REPLY[start:start + CHUNK_CHARS]},
                                    "finish_reason": None}]})
            time.sleep(CHUNK_SLEEP)
        completion = max(1, len(REPLY) // 2)
        # prompt 用量随对话长度涨（每条 user 消息算 5 个）：撤销/弹出之后 footer 的
        # 上下文读数才有得变，走查看得出「即时刷新」。
        user_turns = body.count(b'"role":"user"') + body.count(b'"role": "user"')
        prompt_tokens = 12 + 5 * user_turns
        # 置 STUB_USAGE_BY_SIZE=1：prompt 用量改按**请求体积**算（≈4 字节 1 个
        # token），像真供应商那样随上下文一起涨。默认关着——别的走查有按现在
        # 这个小数目写的断言。
        if os.environ.get("STUB_USAGE_BY_SIZE"):
            prompt_tokens += len(body) // 4
        self._sse({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion,
                             "total_tokens": prompt_tokens + completion}})
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
