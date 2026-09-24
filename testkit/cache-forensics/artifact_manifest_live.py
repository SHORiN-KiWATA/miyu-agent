#!/usr/bin/env python3
"""网页会话的 artifact 清单没变就不重发(C3,09-24)的黑盒实测。

隔离家目录 + 独立端口 daemon,回合走 /api/turns:清单块只出现在网页回合(External 听众、
普通模式、非平台)的回合尾巴里,终端回合看不到它。

桩模式(默认):脚本内置 OpenAI 兼容桩,逐条记下请求体。剧本九轮:

    t1 你好        清单 A(还没有文件)          → 发
    t2 建文件      A 没变 → 不发;这轮里桩调 artifact 工具建 plan.md
    t3 删文件      清单 B → 发;这轮里桩调工具删 plan.md(尾巴非空的工具轮)
    t4 看看        回到 A,模型最近看到的却是 B → 发;上一轮是尾巴非空的工具轮
    (重启 daemon,之后的回放全从库里读)
    t5 继续        A 没变                       → 不发
    t6 建文件      A 没变 → 不发;这轮里桩调工具再建 plan.md
    t7 现在呢      清单 B                       → 发
    t8 最后一句    B 没变                       → 不发
    t9 问清单      B 没变 → 不发;真模型模式另看回答里有没有 plan.md(清单已隔了两轮)

每轮第一条请求另查三件:上一轮最后一条请求原样是它的前缀(纯追加);system 与 tools 不变;
请求里最近一份清单就是此刻的清单。`--baseline` 给修前的二进制用:期望每轮都发。

真模型模式(--real):供应商换成本机配置里的官方 deepseek(deepseek-flash),同一剧本,
建/删文件由模型调 artifact 工具(照没照做看清单有没有变);发没发清单读库里的回合尾巴
化石,缓存看 cache-usage.jsonl 每轮第一条请求的 cache_read 盖没盖住上一条请求。

用法: artifact_manifest_live.py <miyu 二进制> [标签] [--real] [--baseline]
产物: ~/.cache/miyu-cache-replay/artifact-<标签>.json
"""

import json
import shutil
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402
from replay_live import Sandbox, free_port, wait_for  # noqa: E402

OPEN = "<artifact-workspace>"
CLOSE = "</artifact-workspace>"
EMPTY = "(no managed artifacts yet)"
CREATE = "请用 artifact 工具新建一个文件 plan.md,内容只写一行 hello。做完只回一句话。"
DELETE = "请用 artifact 工具删掉 plan.md。做完只回一句话。"
ASK = "现在有哪些 artifact 文件?只列文件名。"
# (输入, 这一轮开头的清单状态, 这一轮之前重启 daemon)
SCRIPT = [
    ("你好,回一个字就行。", "A", False),
    (CREATE, "A", False),
    (DELETE, "B", False),
    ("看看,一个字就行。", "A", False),
    ("继续,一个字就行。", "A", True),
    (CREATE, "A", False),
    ("现在呢?一个字就行。", "B", False),
    ("最后一句,一个字就行。", "B", False),
    (ASK, "B", False),
]
OUT = Path.home() / ".cache/miyu-cache-replay"


def text_of(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def manifest_blocks(messages):
    """user 消息里的全部清单块(整块),按出现顺序。"""
    blocks = []
    for message in messages:
        if message.get("role") != "user":
            continue
        text = text_of(message)
        start = text.find(OPEN)
        while start >= 0:
            end = text.find(CLOSE, start)
            if end < 0:
                break
            blocks.append(text[start:end + len(CLOSE)])
            start = text.find(OPEN, end)
    return blocks


def state_of(block):
    return None if block is None else ("A" if EMPTY in block else "B" if "plan.md" in block else "?")


class Stub:
    """OpenAI 兼容桩:记下每条请求体;建/删文件那两句回 artifact 工具调用,其余回一个字。"""

    def __init__(self):
        self.requests = []
        self.port = free_port()
        stub = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_GET(self):
                self._json({"object": "list", "data": [{"id": "stub-model", "object": "model"}]})

            def do_POST(self):
                length = int(self.headers.get("content-length", "0"))
                body = json.loads(self.rfile.read(length) or b"{}") if length else {}
                stub.requests.append(body)
                delta, finish = stub.answer(body)
                if not body.get("stream"):
                    message = {"role": "assistant", "content": delta.get("content", ""),
                               **({"tool_calls": delta["tool_calls"]} if "tool_calls" in delta else {})}
                    self._json({"id": "stub", "object": "chat.completion", "model": "stub-model",
                                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                                "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}})
                    return
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("cache-control", "no-cache")
                self.end_headers()
                for chunk in ({"delta": delta, "finish_reason": None}, {"delta": {}, "finish_reason": finish}):
                    payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-model",
                               "choices": [{"index": 0, **chunk}]}
                    if chunk["finish_reason"]:
                        payload["usage"] = {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}
                    self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            def _json(self, value):
                raw = json.dumps(value, ensure_ascii=False).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        self.server = ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @staticmethod
    def answer(body):
        messages = body.get("messages", [])
        if messages and messages[-1].get("role") == "tool":
            return {"content": "好了。"}, "stop"
        # 用户消息后面挂着 <runtime …> / <artifact-workspace> 这类尾巴,取最近一条正文。
        users = [text_of(m) for m in messages if m.get("role") == "user" and not text_of(m).lstrip().startswith("<")]
        latest = users[-1] if users else ""
        patch = None
        if latest.startswith(CREATE):
            patch = "*** Begin Patch\n*** Add File: plan.md\n+hello\n*** End Patch"
        elif latest.startswith(DELETE):
            patch = "*** Begin Patch\n*** Delete File: plan.md\n*** End Patch"
        if patch is None:
            return {"content": "好。"}, "stop"
        call = {"index": 0, "id": f"call_{len(users)}", "type": "function",
                "function": {"name": "artifact", "arguments": json.dumps({"patchText": patch})}}
        return {"tool_calls": [call]}, "tool_calls"

    def chat_requests(self):
        """主对话请求(带工具表的那些);标题之类的辅助请求不算。"""
        return [body for body in self.requests if body.get("tools")]

    def stop(self):
        self.server.shutdown()


class WebBox(Sandbox):
    """replay_live 的沙箱 + 网页接口。桩模式把供应商换成本地桩。"""

    def __init__(self, miyu, stub):
        super().__init__(miyu)
        if stub is not None:
            config = json.loads((self.home / "config" / "config.jsonc").read_text(encoding="utf-8"))
            config["providers"] = [{
                "id": "stub", "display_name": "Stub", "enabled": True,
                "base_url": f"http://127.0.0.1:{stub.port}/v1", "protocol": "openai-chat",
                "api_key": "stub", "models": ["stub-model"], "default_model": "stub-model",
                "model_context_window": {"stub-model": 200000},
            }]
            config["active_provider"] = "stub"
            config["active_provider_models"] = [{"provider_id": "stub", "model": "stub-model"}]
            (self.home / "config" / "config.jsonc").write_text(json.dumps(config), encoding="utf-8")
        self.base = f"http://127.0.0.1:{self.port}"

    def start(self):
        super().start()
        if not wait_for(self.health, 40):
            raise SystemExit("网页接口没起来")
        authlib.bootstrap(self.base)

    def health(self):
        try:
            urllib.request.urlopen(self.base + "/api/health", timeout=2)
            return True
        except OSError:
            return False

    def restart(self):
        self.stop()
        self.daemon = None
        self.start()

    def api(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(self.base + path, data=data, method=method,
                                         headers={"content-type": "application/json"})
        with authlib.OPENER.open(request, timeout=30) as response:
            raw = response.read()
        return json.loads(raw) if raw else None

    def new_session(self, name):
        created = self.api("POST", "/api/sessions", {"name": name, "switch": True})
        session_id = created.get("session_id") or created.get("id") \
            or (created.get("session") or {}).get("session_id")
        assert session_id, f"no session id in {created}"
        return session_id

    def idle(self):
        return not (self.api("GET", "/api/bootstrap") or {}).get("runs")

    def send(self, session_id, text, timeout=300):
        count = len(self.turns(session_id)) + 1
        # 库里回合已落「完成」时运行槽可能还没释放,这时发下一句会撞 409「Miyu is busy」。
        if not wait_for(self.idle, 60):
            raise AssertionError("daemon never went idle")
        self.api("POST", "/api/turns", {"content": text, "session_id": session_id})

        def settled():
            turns = self.turns(session_id)
            return len(turns) >= count and all(turn["status"] != "running" for turn in turns) and self.idle()

        if not wait_for(settled, timeout):
            raise AssertionError(f"turn {count} never finished")
        return self.turns(session_id)[-1]["status"]

    def fossil_blocks(self, session_id):
        rows = self.query("SELECT turn_id, context_messages FROM turns WHERE session_id = ? ORDER BY seq",
                          (session_id,))
        return [manifest_blocks(json.loads(row["context_messages"] or "[]")) for row in rows]


def stub_checks(chat, boundaries):
    """每轮第一条请求:新增清单块、最近一份清单、纯追加、system/tools 不变。"""
    rows = []
    first = chat[boundaries[0][0]]
    for index, (start, end) in enumerate(boundaries):
        request = chat[start]
        messages = request["messages"]
        previous = chat[boundaries[index - 1][1] - 1]["messages"] if index else []
        appended = messages[len(previous):]
        blocks = manifest_blocks(messages)
        rows.append({
            "requests": end - start,
            "new_blocks": len(manifest_blocks(appended)),
            "latest": state_of(blocks[-1] if blocks else None),
            "pure_append": messages[:len(previous)] == previous,
            "system_same": messages[0] == first["messages"][0],
            "tools_same": request.get("tools") == first.get("tools"),
        })
    return rows


def real_checks(box, session_id):
    """每轮第一条请求的 cache_read 对上一条请求的 prompt;发没发清单读回合尾巴化石。"""
    requests = box.cache_rows(session_id)
    turn_ids = [row["turn_id"] for row in box.query(
        "SELECT turn_id FROM turns WHERE session_id = ? ORDER BY seq", (session_id,))]
    fossils = box.fossil_blocks(session_id)
    rows = []
    latest = None
    for index, turn_id in enumerate(turn_ids):
        own = [i for i, row in enumerate(requests) if row.get("turn") == turn_id]
        row = {"requests": len(own), "new_blocks": len(fossils[index])}
        if fossils[index]:
            latest = fossils[index][-1]
        row["latest"] = state_of(latest)
        if own:
            head = requests[own[0]]
            row.update(prompt=head.get("prompt"), cache_read=head.get("cache_read"),
                       pure_append=head.get("prev") is None or (head.get("same") or 0) >= head["prev"])
            if own[0] > 0:
                before = requests[own[0] - 1].get("prompt") or 0
                row["recomputed"] = max(0, before - (head.get("cache_read") or 0))
        rows.append(row)
    return rows


def main():
    args = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
    real = "--real" in sys.argv
    baseline = "--baseline" in sys.argv
    if not args:
        print(__doc__)
        return 2
    label = args[1] if len(args) > 1 else "run"
    stub = None if real else Stub()
    box = WebBox(Path(args[0]).resolve(), stub)
    rows, statuses, ok = [], [], False
    try:
        box.start()
        session_id = box.new_session("清单去重")
        boundaries = []
        for text, _, restart in SCRIPT:
            if restart:
                box.restart()
            start = len(stub.chat_requests()) if stub else 0
            statuses.append(box.send(session_id, text))
            boundaries.append((start, len(stub.chat_requests()) if stub else 0))
        rows = stub_checks(stub.chat_requests(), boundaries) if stub else real_checks(box, session_id)
        expected_new = [1] * len(SCRIPT) if baseline else \
            [int(i == 0 or SCRIPT[i][1] != SCRIPT[i - 1][1]) for i in range(len(SCRIPT))]
        for row, (text, state, restart), want in zip(rows, SCRIPT, expected_new):
            row.update(input=text[:14], expected_state=state, expected_new=want, restart_before=restart)
        print(f"== {label}{' (真模型)' if real else ' (桩)'}{' 修前期望' if baseline else ''}  回合状态 {statuses}")
        print(f"  {'轮':>2} {'请求':>4} {'新增清单块':>6} {'期望':>4} {'最近清单':>6} {'期望':>4} {'纯追加':>5}"
              + ("" if real else f" {'system':>6} {'tools':>5}")
              + ("  prompt  cache_read  已发过却重算" if real else ""))
        for index, row in enumerate(rows, 1):
            line = (f"  t{index:<2} {row['requests']:>4} {row['new_blocks']:>9} {row['expected_new']:>5}"
                    f" {str(row['latest']):>8} {row['expected_state']:>5} {str(row.get('pure_append')):>7}")
            if not real:
                line += f" {str(row['system_same']):>6} {str(row['tools_same']):>5}"
            else:
                line += f"  {row.get('prompt', '-'):>6}  {row.get('cache_read', '-'):>10}  {row.get('recomputed', '-'):>8}"
            print(line + ("   ← 这一轮之前重启了 daemon" if row["restart_before"] else ""))
        if real:
            # 清单隔了几轮没重发,模型还答得出现状吗(答之前调没调工具也记下来)。
            last = box.query("SELECT assistant_content, tool_flow FROM turns WHERE session_id = ? ORDER BY seq",
                             (session_id,))[-1]
            calls = [call["name"] for rnd in json.loads(last["tool_flow"] or "[]") for call in rnd.get("calls", [])]
            print(f"  t9 回答提到 plan.md: {'plan.md' in (last['assistant_content'] or '')}  调用的工具: {calls or '无'}")
            # 真模型不一定照剧本建/删文件:清单状态以它实际看到的为准,只核「没变不发、变了就发」。
            states = [row["latest"] for row in rows]
            ok = all(row.get("pure_append") for row in rows) and all(
                row["new_blocks"] == int(i == 0 or states[i] != states[i - 1] or baseline)
                for i, row in enumerate(rows))
        else:
            ok = all(row["pure_append"] and row["system_same"] and row["tools_same"]
                     and row["new_blocks"] == row["expected_new"] and row["latest"] == row["expected_state"]
                     for row in rows) and all(status == "completed" for status in statuses)
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败
        print(f"❌ aborted: {error!r}")
    finally:
        box.stop()
        if stub:
            stub.stop()
        OUT.mkdir(parents=True, exist_ok=True)
        log = box.home / "daemon.log"
        if log.exists() and not real:
            shutil.copy(log, OUT / f"artifact-{label}-daemon.log")
        (OUT / f"artifact-{label}.json").write_text(
            json.dumps({"statuses": statuses, "rows": rows}, ensure_ascii=False, indent=1), encoding="utf-8")
        # 家目录里有 API key:不论成败都删。
        shutil.rmtree(box.home, ignore_errors=True)
    print("✅ 通过" if ok else "❌ 未通过")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
