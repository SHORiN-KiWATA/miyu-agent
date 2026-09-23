#!/usr/bin/env python3
"""BUG-14 黑盒:QQ 里管理员让她「把这个发到 xxx 群」——沙箱 daemon + 会调工具的
OpenAI 桩 + 假 NapCat(反向 WS)。

    BIN=<miyu> python3 testkit/qq-outreach/run.py

链路:管理员私聊 → daemon 起回合 → 桩 LLM 先调 qq_contacts 查「交流群」→ daemon 问
假 NapCat get_friend_list / get_group_list → 桩拿到群号后调 send_qq_message(群名)→
daemon 解析成群号、经 send_group_msg 发到假 NapCat → 桩最后回一句话到私聊。

判定:
  terminal_before_ws    ws 没连上时终端会话的工具面里没有 send_qq_message / qq_contacts
  terminal_normal       ws 连上后终端普通模式两个都有,send_qq_message 的 to 自由填(无枚举)
  terminal_dev          终端开发模式只有 send_qq_message,to 是管理员枚举,没有 qq_contacts
  admin_send_group      假 NapCat 收到 send_group_msg(交流群号, "开饭了")
  admin_contacts        桩看到的 qq_contacts 结果里有交流群的号
  admin_tools           管理员的工具面里有 send_qq_message + qq_contacts
  guest_can_send        白名单私聊用户也有这两个工具,同样能发到交流群(用户裁定:发消息是基础能力)
  ambiguous_asks        收件人「花」撞两个好友:工具结果报候选、没发出去
  number_send_friend    用号码发好友:send_private_msg(10004)
  group_guest_calls     非白名单群友在群里 @ 她「把阿花叫来」:查到通讯录、私聊发给阿花(10003)
  terminal_after_ws     ws 断开后终端会话的工具面里两个又没了
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-qq-outreach")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18533"))
QQ_PORT = int(os.environ.get("QQ_PORT", "18534"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18535"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))

spec = importlib.util.spec_from_file_location("fake", REPO / "testkit" / "fake-onebot" / "run.py")
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
fake.PORT = QQ_PORT
ADMIN, WHITE = 1, 2
# 地址簿:两个好友撞「花」,一个交流群。
FRIENDS = [
    {"user_id": ADMIN, "nickname": "老板", "remark": ""},
    {"user_id": WHITE, "nickname": "白名单", "remark": ""},
    {"user_id": 10003, "nickname": "阿花", "remark": ""},
    {"user_id": 10004, "nickname": "花花", "remark": "同事花花"},
]
GROUPS = [
    {"group_id": 20001, "group_name": "Miyu 交流群", "member_count": 42},
    {"group_id": fake.GROUP_ID, "group_name": "假群(测具)", "member_count": 3},
]
EXCHANGE_GROUP = 20001

CALLS = []          # (action, group_id, user_id, text)
STUB_LOG = []       # 每次请求:{"tools": [...], "user": ..., "tool_results": [...]}


# ---------------- 桩 LLM:按用户那句话决定调什么工具 ----------------
class Stub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _sse(self, delta, finish=None):
        payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-a",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
        if finish:
            payload["usage"] = {"prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20}
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.flush()

    def _done(self):
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def _tool_call(self, name, arguments):
        self._sse({"tool_calls": [{"index": 0, "id": f"call_{int(time.time() * 1000) % 100000}", "type": "function",
                                   "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)}}]})
        self._sse({}, "tool_calls")
        self._done()

    def _say(self, text):
        self._sse({"content": text})
        self._sse({}, "stop")
        self._done()

    def do_GET(self):
        payload = json.dumps({"data": [{"id": "stub-a"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        messages = body.get("messages", [])

        def text_of(m):
            c = m.get("content")
            if isinstance(c, list):
                return "".join(p.get("text", "") for p in c if isinstance(p, dict))
            return c or ""

        # 私聊会话带历史:上一场景的工具调用/结果也在 messages 里。只看本轮
        # (最后一条带 TK 的用户消息之后)的调用与结果,否则第二场景起会误判「已调过」。
        turn_start = max((i for i, m in enumerate(messages) if m.get("role") == "user" and "TK" in text_of(m)), default=-1)
        user = text_of(messages[turn_start]) if turn_start >= 0 else ""
        this_turn = messages[turn_start + 1:] if turn_start >= 0 else []
        tool_results = [text_of(m) for m in this_turn if m.get("role") == "tool"]
        tools = sorted(t.get("function", {}).get("name") for t in body.get("tools") or [])
        called = [tc.get("function", {}).get("name") for m in this_turn if m.get("role") == "assistant"
                  for tc in (m.get("tool_calls") or [])]
        send_spec = next((t.get("function", {}) for t in body.get("tools") or []
                          if t.get("function", {}).get("name") == "send_qq_message"), None)
        to_enum = bool(((send_spec or {}).get("parameters") or {}).get("properties", {}).get("to", {}).get("enum"))
        STUB_LOG.append({"user": user, "tools": tools, "tool_results": tool_results, "called": called,
                         "send_to_enum": to_enum})
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        if "send-group" in user and "send_qq_message" in tools:
            if "qq_contacts" not in called:
                return self._tool_call("qq_contacts", {"query": "交流群"})
            if "send_qq_message" not in called:
                return self._tool_call("send_qq_message", {"to": "Miyu 交流群", "kind": "group", "text": "开饭了"})
            return self._say("发好了。")
        if "ambiguous" in user and "send_qq_message" in tools:
            if "send_qq_message" not in called:
                return self._tool_call("send_qq_message", {"to": "花", "text": "在吗"})
            return self._say("有两个叫花的,你说哪个?")
        if "call-friend" in user and "send_qq_message" in tools:
            if "qq_contacts" not in called:
                return self._tool_call("qq_contacts", {"query": "阿花"})
            if "send_qq_message" not in called:
                return self._tool_call("send_qq_message", {"to": "阿花", "text": "群里有人找你,来一下"})
            return self._say("叫了。")
        if "number" in user and "send_qq_message" in tools:
            if "send_qq_message" not in called:
                return self._tool_call("send_qq_message", {"to": "10004", "text": "号码直发"})
            return self._say("发给花花了。")
        return self._say("好的。")


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-a"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat", "api_key": "stub", "models": ["stub-a"],
        }],
        "memory": {"enabled": False},
        "platforms": {"qq": {
            "enabled": True, "reverse_ws_port": QQ_PORT, "access_token": "",
            "admin_users": [ADMIN], "private_chats": {"whitelist": [WHITE]},
        }},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def pump(ws):
    while True:
        try:
            frame = ws.recv()
        except Exception as error:
            print(f"  [pump] end: {error}")
            return
        if not isinstance(frame, dict) or "action" not in frame:
            continue
        action, params = frame["action"], frame.get("params", {})
        if action in ("send_group_msg", "send_msg", "send_private_msg"):
            text = fake.render(params.get("message"))
            CALLS.append((action, params.get("group_id"), params.get("user_id"), text))
            print(f"  ← {action} group={params.get('group_id')} user={params.get('user_id')}: {text[:40]}")
        if action == "get_friend_list":
            data = FRIENDS
        elif action == "get_group_list":
            data = GROUPS
        else:
            data = fake.api_data(action, params)
        ws.send({"status": "ok", "retcode": 0, "data": data, "echo": frame.get("echo")})


def terminal_probe(tag, extra=()):
    """终端那条路(程序驱动 ask,阅后即焚会话)问一句,回桩看到的那次请求。"""
    before = len(STUB_LOG)
    proc = subprocess.run([str(BIN), "ask", "--output-format", "json", *extra, f"TK probe {tag}"],
                          env=ENV, capture_output=True, text=True, timeout=90)
    assert proc.returncode == 0, proc.stderr[-300:]
    entries = STUB_LOG[before:]
    assert entries, "stub saw no request from the terminal probe"
    return entries[0]


def wait_reply(sender, before, wait=25.0):
    """等到私聊 sender 收到一条回复(回合收尾)。"""
    deadline = time.time() + wait
    while time.time() < deadline:
        if any(c[0] == "send_private_msg" and c[2] == sender for c in CALLS[before:]):
            time.sleep(1.0)
            return True
        time.sleep(0.2)
    return False


results = {}


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:200]}", flush=True)


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    server = ThreadingHTTPServer(("127.0.0.1", STUB_PORT), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = None
    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        # /api/config 现在要登录(09-10 多用户),就绪探根路径(登录页)。
        assert wait_http(f"{BASE}/"), "daemon not up"
        time.sleep(1.0)
        # 0. ws 没连上:终端会话看不到这两个工具
        seen = terminal_probe("before-ws")
        check("terminal_before_ws", "send_qq_message" not in seen["tools"] and "qq_contacts" not in seen["tools"],
              [t for t in seen["tools"] if "qq" in t])
        ws = fake.WS.connect("")
        threading.Thread(target=pump, args=(ws,), daemon=True).start()
        time.sleep(2.0)
        # 0b. ws 连上:普通模式两个都有、to 自由填;开发模式只有发 QQ、to 是管理员枚举
        seen = terminal_probe("normal")
        check("terminal_normal", "send_qq_message" in seen["tools"] and "qq_contacts" in seen["tools"]
              and not seen["send_to_enum"], [t for t in seen["tools"] if "qq" in t] + [f"enum={seen['send_to_enum']}"])
        seen = terminal_probe("dev", ["--session", "devsess", "--create", "--mode", "dev"])
        check("terminal_dev", "send_qq_message" in seen["tools"] and "qq_contacts" not in seen["tools"]
              and seen["send_to_enum"], [t for t in seen["tools"] if "qq" in t] + [f"enum={seen['send_to_enum']}"])

        # 1. 管理员:发到交流群
        before, log_before = len(CALLS), len(STUB_LOG)
        fake.private_msg(ws, "TK send-group 把开饭了发到交流群", sender=ADMIN, name="老板")
        replied = wait_reply(ADMIN, before)
        group_sends = [c for c in CALLS[before:] if c[0] == "send_group_msg" and c[1] == EXCHANGE_GROUP]
        check("admin_send_group", replied and any("开饭了" in c[3] for c in group_sends), group_sends)
        first = STUB_LOG[log_before] if len(STUB_LOG) > log_before else {}
        check("admin_tools", "send_qq_message" in first.get("tools", []) and "qq_contacts" in first.get("tools", []),
              [t for t in first.get("tools", []) if "qq" in t or "send" in t])
        contacts_results = [r for e in STUB_LOG[log_before:] for r in e["tool_results"] if "group_id" in r]
        check("admin_contacts", any(str(EXCHANGE_GROUP) in r and "交流群" in r for r in contacts_results),
              (contacts_results[:1] or [""])[0][:160])

        # 2. 白名单私聊用户:同样有这两个工具,也能发到交流群
        before, log_before = len(CALLS), len(STUB_LOG)
        fake.private_msg(ws, "TK send-group 把开饭了发到交流群", sender=WHITE, name="白名单")
        replied = wait_reply(WHITE, before)
        seen = STUB_LOG[log_before] if len(STUB_LOG) > log_before else {}
        group_sends = [c for c in CALLS[before:] if c[0] == "send_group_msg" and c[1] == EXCHANGE_GROUP]
        check("guest_can_send", replied and "send_qq_message" in seen.get("tools", [])
              and "qq_contacts" in seen.get("tools", []) and any("开饭了" in c[3] for c in group_sends),
              group_sends)

        # 3. 管理员:收件人撞两个好友 → 报候选,不发
        before, log_before = len(CALLS), len(STUB_LOG)
        fake.private_msg(ws, "TK ambiguous 给花发在吗", sender=ADMIN, name="老板")
        replied = wait_reply(ADMIN, before)
        tool_results = [r for e in STUB_LOG[log_before:] for r in e["tool_results"]]
        check("ambiguous_asks", replied and any("several" in r and "10003" in r and "10004" in r for r in tool_results)
              and not any(c[0] == "send_private_msg" and c[2] in (10003, 10004) for c in CALLS[before:]),
              (tool_results[:1] or [""])[0][:160])

        # 4. 管理员:用号码发好友
        before, log_before = len(CALLS), len(STUB_LOG)
        fake.private_msg(ws, "TK number 给10004发", sender=ADMIN, name="老板")
        replied = wait_reply(ADMIN, before)
        direct = [c for c in CALLS[before:] if c[0] == "send_private_msg" and c[2] == 10004]
        check("number_send_friend", replied and any("号码直发" in c[3] for c in direct), direct)

        # 5. 非白名单群友在群里 @ 她「把阿花叫来」:群回合也有工具,查通讯录后私聊阿花
        GUEST = 3
        before, log_before = len(CALLS), len(STUB_LOG)
        fake.group_msg(ws, "TK call-friend 把阿花叫来", sender=GUEST, at_self=True, name="路人群友")
        deadline = time.time() + 25
        while time.time() < deadline and not any(c[0] == "send_group_msg" and c[1] == fake.GROUP_ID for c in CALLS[before:]):
            time.sleep(0.2)
        time.sleep(1.0)
        # 群回合前面还有判官/命名之类不带工具的请求,要挑带 TK 那句且带工具面的那次。
        seen = next((e for e in STUB_LOG[log_before:] if "call-friend" in e["user"] and e["tools"]), {})
        dm = [c for c in CALLS[before:] if c[0] == "send_private_msg" and c[2] == 10003]
        contacts_seen = [r for e in STUB_LOG[log_before:] for r in e["tool_results"] if "10003" in r]
        check("group_guest_calls", "send_qq_message" in seen.get("tools", []) and "qq_contacts" in seen.get("tools", [])
              and bool(contacts_seen) and any("来一下" in c[3] for c in dm),
              {"tools": [t for t in seen.get("tools", []) if "qq" in t], "dm": dm})

        # 6. ws 断开:终端会话的工具面里两个又没了
        # 光 close() 不行:另一个线程还阻塞在 recv 里,fd 要等它返回才真正释放,
        # 对端收不到 FIN,daemon 根本不知道断了。shutdown 才会立刻发 FIN。
        import socket as _socket
        ws.sock.shutdown(_socket.SHUT_RDWR)
        ws.sock.close()
        time.sleep(float(os.environ.get("AFTER_WS_WAIT", "2")))
        seen = terminal_probe("after-ws")
        check("terminal_after_ws", "send_qq_message" not in seen["tools"] and "qq_contacts" not in seen["tools"],
              [t for t in seen["tools"] if "qq" in t])
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
        server.shutdown()
        (OUT / "verdict.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), "utf-8")
        (OUT / "stub-log.json").write_text(json.dumps(STUB_LOG, ensure_ascii=False, indent=2), "utf-8")
    passed = sum(results.values())
    print(f"\n{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
