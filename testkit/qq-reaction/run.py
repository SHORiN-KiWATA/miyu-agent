#!/usr/bin/env python3
"""QQ 主动回复黑盒(09-24):贴表情、观察窗口加分、冷静机制、短消息与判官提示词。

沙箱 daemon + 进程内判官桩(OpenAI SSE)+ 假 NapCat(反向 WS)。走的是真路径:
群消息 → 触发条件 → 判官模型放行 → 主回合 → 投递 → 摘表情/记账。

    BIN=<miyu> python3 testkit/qq-reaction/run.py

场景(抽样概率开到 1.0,判官桩永远给高分):
  1 群友 A 不 @ 说话      → 抽样触发       → 回了,不贴表情
  2 群友 B 紧接着说三个字 → 刚说过话触发   → 回了,不贴表情,加分 +0.100,短消息不再抬门槛
  3 群友 C @ 她           → 直接触发       → 回了,贴过表情且回完摘掉,冷静不压
  4 C 十五秒内叫着她接着说 → 续聊触发      → 回了,贴过表情,判官说冲她来 → 冷静不压
  5 A 又抛一句开放话题    → 刚说过话触发   → 判官说不冲她来 → 冷静照压,发言量比场景 2 高
  冷静机制:插嘴那两条日志「阈值 +x（近期发言量 y）」且 x = 0.35·y³/(y³+2.5³);
  判官提示词里没有「程序加减分」那行数字(09-24 删掉,判官会拿它重复计分)。
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里:HERDR_* 漏给被测的 miyu 会去认领那个 pane(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-qq-reaction")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18531"))
QQ_PORT = int(os.environ.get("QQ_PORT", "18532"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18533"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")

spec = importlib.util.spec_from_file_location("fake", REPO / "testkit" / "fake-onebot" / "run.py")
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
fake.PORT = QQ_PORT
A, B, C = 800000011, 800000012, 800000013

JUDGE_MARK = "Decide whether the current bot persona should reply"
JUDGE_PROMPTS = []
VERDICT = {
    "should_reply": True, "relevance": 9, "willingness": 9, "social": 9, "timing": 9,
    "continuity": 9, "reasoning": "stub",
    "moderation": {"violation": False, "severity": 0, "category": "", "evidence": "",
                   "rule_basis": "", "reasoning": "", "related_user_ids": [],
                   "related_message_ids": []},
}


class Stub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _chunk(self, delta, finish=None):
        payload = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-a",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
        if finish:
            payload["usage"] = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        text = ""
        for message in body.get("messages", []):
            content = message.get("content")
            if isinstance(content, list):
                content = "".join(p.get("text", "") for p in content if isinstance(p, dict))
            text += (content or "") + "\n"
        if JUDGE_MARK in text:
            JUDGE_PROMPTS.append(text)
            # 桩判官:当前消息里叫了 miyu 就算冲她来(to_bot),否则算插嘴。
            current = text.split("Current message content", 1)[-1].lower()
            answer = json.dumps(dict(VERDICT, to_bot="miyu" in current))
        else:
            answer = "嗯嗯"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        self._chunk({"role": "assistant", "content": answer})
        self._chunk({}, finish="stop")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_GET(self):
        payload = json.dumps({"data": [{"id": "stub-a"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


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
            "admin_users": [1],
            "plugins": {"real_context": {"enabled": True, "settings": {
                "active_judge_probability": 1.0,
                "text_models": "inherit",
            }}},
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


SENDS = []
REACTIONS = []


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
        if action in ("send_group_msg", "send_msg"):
            SENDS.append(fake.render(params.get("message")))
        elif action == "set_msg_emoji_like":
            REACTIONS.append((str(params.get("message_id")), bool(params.get("set"))))
        ws.send({"status": "ok", "retcode": 0, "data": fake.api_data(action, params), "echo": frame.get("echo")})


def say(ws, text, sender, at=False, wait=25.0):
    """发一条群消息,等她回;回了再多等一会让摘表情、记账跑完。"""
    before = len(SENDS)
    mid = fake.group_msg(ws, text, sender=sender, at_self=at, name=f"群友{sender % 100}")
    deadline = time.time() + wait
    while time.time() < deadline and len(SENDS) == before:
        time.sleep(0.2)
    replied = len(SENDS) > before
    time.sleep(2.0)
    return str(mid), replied


RESULTS = []


def check(name, ok, detail=""):
    RESULTS.append(ok)
    print(f"{'✅' if ok else '❌'} {name}" + (f"  ({detail})" if detail else ""))


def decision_blocks(log_text):
    """daemon 日志里的判断块:[(触发, 结果, 整块文本)]。"""
    blocks = []
    for match in re.finditer(r"(【(?:主动回复判断|续聊窗口判断)：[^】]+】|\[(?:Active reply decision|Continuation decision): [^\]]+\])(.*?)(?=\n\d{4}-\d\d-\d\dT|\Z)", log_text, re.S):
        body = match.group(0)
        trigger = re.search(r"\((probability|after_speaking|direct|continuation|supersede|moderation)\)", body)
        blocks.append((trigger.group(1) if trigger else None, body))
    return blocks


def restraint_of(body):
    match = re.search(r"(?:冷静机制调整：阈值 \+([\d.]+)（近期发言量 ([\d.]+)）|Restraint adjustment: threshold \+([\d.]+) \(recent replies ([\d.]+)\))", body)
    if not match:
        return None
    groups = [g for g in match.groups() if g is not None]
    return float(groups[0]), float(groups[1])


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    server = ThreadingHTTPServer(("127.0.0.1", STUB_PORT), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = None
    log_path = OUT / "daemon.log"
    try:
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL, stdout=log_path.open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/"), "daemon not up"
        time.sleep(1.5)
        ws = fake.WS.connect("")
        threading.Thread(target=pump, args=(ws,), daemon=True).start()
        ws.send({"post_type": "meta_event", "meta_event_type": "lifecycle",
                 "sub_type": "connect", "self_id": fake.SELF_ID, "time": int(time.time())})
        time.sleep(1.0)

        m1, r1 = say(ws, "今天晚饭吃什么好呢", A)
        m2, r2 = say(ws, "我也是", B)
        m3, r3 = say(ws, "你觉得呢", C, at=True)
        m4, r4 = say(ws, "miyu 你说说看嘛", C)
        m5, r5 = say(ws, "这周末干啥好呢", A)
        time.sleep(1.0)

        marked = lambda mid: (mid, True) in REACTIONS
        unmarked = lambda mid: (mid, False) in REACTIONS
        check("抽样触发:回了", r1)
        check("抽样触发:没贴表情", not marked(m1), str([r for r in REACTIONS if r[0] == m1]))
        check("刚说过话触发:回了", r2)
        check("刚说过话触发:没贴表情", not marked(m2), str([r for r in REACTIONS if r[0] == m2]))
        check("被 @:回了", r3)
        check("被 @:贴过表情且回完摘掉", marked(m3) and unmarked(m3), str([r for r in REACTIONS if r[0] == m3]))
        check("续聊:回了", r4)
        check("续聊:贴过表情", marked(m4), str([r for r in REACTIONS if r[0] == m4]))

        # 判断块写在 daemon 自己的日志文件里,stdout 只有启动那几行。
        log_text = "".join(p.read_text("utf-8", errors="replace")
                           for p in sorted((HOME / "cache" / "logs").glob("miyu.*.log")))
        blocks = [b for b in decision_blocks(log_text) if b[0]]
        triggers = [t for t, _ in blocks]
        check("判断日志触发顺序", triggers[:5] == ["probability", "after_speaking", "direct", "continuation", "after_speaking"], str(triggers))
        after = next((body for t, body in blocks if t == "after_speaking"), "")
        check("刚说过话加分 +0.100", "刚说过话加分：+0.100" in after or "After-speaking bonus: +0.100" in after)
        first = next((body for t, body in blocks if t == "probability"), "")
        check("第一句之前没说过话:无冷静行", first != "" and restraint_of(first) is None)
        second, direct, cont, fifth = (body for _, body in blocks[1:5])
        curve = lambda p: 0.35 * p**3 / (p**3 + 2.5**3)
        values = [restraint_of(second), restraint_of(fifth)]
        ok = all(v is not None for v in values)
        if ok:
            ok = values[1][1] > values[0][1] > 0.85 and all(abs(t - curve(p)) < 0.0015 for t, p in values)
        check("插嘴照压:阈值 = 0.35·p³/(p³+2.5³),发言量随回复上升", ok, str(values))
        check("被 @ 豁免(门槛就是 0.800),日志写「豁免（直接触发」", restraint_of(direct) is None and "豁免（直接触发" in direct and "阈值 0.800" in direct)
        check("判官说指向机器人的豁免", "指向机器人：是" in cont and "豁免（指向机器人" in cont and restraint_of(cont) is None)
        check("判官说没指向机器人的记一笔「指向机器人：否」", "指向机器人：否" in second and "指向机器人：否" in fifth)
        after = second
        # 三个字的短消息:门槛只该是 0.800 + 冷静,不再多出「短句阈值调整」。
        threshold = re.search(r"阈值 ([\d.]+)）|threshold ([\d.]+)\)", after)
        restraint = restraint_of(after)
        short_ok = "短句阈值调整" not in after and "Short-message" not in after and threshold and restraint
        if short_ok:
            shown = float(next(g for g in threshold.groups() if g))
            short_ok = abs(shown - (0.8 + restraint[0])) < 0.0015
        check("短消息不再抬门槛(门槛 = 0.800 + 冷静)", bool(short_ok))
        check("判官提示词带 to_bot 字段与说明", bool(JUDGE_PROMPTS) and all(
            '"to_bot":false' in p and "to_bot is true when" in p for p in JUDGE_PROMPTS))
        check("判官提示词里没有程序加减分那行", bool(JUDGE_PROMPTS) and all(
            not any(mark in p for mark in ("program adjustments", "restraint threshold", "reply heat", "short-message threshold"))
            for p in JUDGE_PROMPTS))
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(5)
            except Exception:
                daemon.kill()
        server.shutdown()
    print(f"{sum(RESULTS)}/{len(RESULTS)} passed")
    sys.exit(0 if RESULTS and all(RESULTS) else 1)


if __name__ == "__main__":
    main()
