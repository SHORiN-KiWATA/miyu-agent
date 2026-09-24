#!/usr/bin/env python3
"""QQ 群聊专属限流覆盖(09-24):准入黑盒 + 网页设置走查。

沙箱 daemon(自己的端口,不碰 8300)+ 进程内 OpenAI 桩 + 假 NapCat(反向 WS)+ Playwright。

    BIN=<miyu> python3 testkit/qq-rate-override/run.py

准入(真实群消息走 dispatch 的限流闸):
  白名单群 999000001 平台档位是 20 条/60 秒,会话专属配置覆盖成 1 条/60 秒
    → 第 1 个 @ 回了,第 2 个 @ 收到「消息太频繁了」,第 3 个 @ 不再理
  非白名单群 999000002 没设覆盖(档位 5 条/300 秒)→ 连着两个 @ 都回
网页(设置 → QQ → 会话专属配置):
  群聊那条的摘要带「限流 1 条/60 秒」;抽屉里有「覆盖群聊限流」,切成私聊就藏起来
  取消勾选覆盖并保存 → 配置里这条会话没有 rate_limit 键(回到档位,不留 null)
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from playwright.sync_api import sync_playwright

# 跑测具的进程多半坐在某个 herdr pane 里:HERDR_* 漏给被测的 miyu 会去认领那个 pane(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "testkit" / "webui-fixes"))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-qq-rate-override")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18541"))
QQ_PORT = int(os.environ.get("QQ_PORT", "18542"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18543"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")

spec = importlib.util.spec_from_file_location("fake", REPO / "testkit" / "fake-onebot" / "run.py")
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
fake.PORT = QQ_PORT
LIMITED, FREE = 999000001, 999000002
GUEST = 800000011
REPLY = "嗯嗯"
NOTICE = "消息太频繁"

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}" + (f"  ({detail})" if detail else ""))


class Stub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", "0")))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for delta, finish in (({"role": "assistant", "content": REPLY}, None), ({}, "stop")):
            chunk = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-a",
                     "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode())
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
        "display": {"language": "zh"},
        "memory": {"enabled": False},
        "platforms": {"qq": {
            "enabled": True, "reverse_ws_port": QQ_PORT, "access_token": "",
            "admin_users": [1],
            "group_chats": {"whitelist": [LIMITED]},
            # 主动插话整个关掉:这里只验准入层的限流闸,别让判官掺进来。
            "plugins": {"real_context": {"enabled": False}},
            "conversations": [{
                "conversation": {"kind": "group", "id": str(LIMITED)},
                "rate_limit": {"max_messages": 1, "window_seconds": 60},
            }],
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


SENT = []  # (group_id, text)


def pump(ws):
    while True:
        try:
            frame = ws.recv()
        except Exception:
            return
        if not isinstance(frame, dict) or "action" not in frame:
            continue
        action, params = frame["action"], frame.get("params", {})
        if action in ("send_group_msg", "send_msg"):
            SENT.append((params.get("group_id"), fake.render(params.get("message"))))
        ws.send({"status": "ok", "retcode": 0, "data": fake.api_data(action, params), "echo": frame.get("echo")})


def at(ws, group_id, text, wait=20.0):
    """在指定群 @ 她,返回这之后发到该群的消息。"""
    before = len(SENT)
    fake.GROUP_ID = group_id
    fake.group_msg(ws, text, sender=GUEST, at_self=True, name="群友")
    deadline = time.time() + wait
    while time.time() < deadline and not any(g == group_id for g, _ in SENT[before:]):
        time.sleep(0.2)
    time.sleep(1.5)
    return [text for g, text in SENT[before:] if g == group_id]


def qq_checks():
    ws = fake.WS.connect("")
    threading.Thread(target=pump, args=(ws,), daemon=True).start()
    ws.send({"post_type": "meta_event", "meta_event_type": "lifecycle",
             "sub_type": "connect", "self_id": fake.SELF_ID, "time": int(time.time())})
    time.sleep(1.0)
    first = at(ws, LIMITED, "第一问")
    second = at(ws, LIMITED, "第二问")
    third = at(ws, LIMITED, "第三问", wait=6.0)
    check("覆盖群第 1 个 @ 回了", any(REPLY in text for text in first), str(first))
    check("覆盖群第 2 个 @ 被限流并提示", any(NOTICE in text for text in second) and not any(REPLY in text for text in second), str(second))
    check("覆盖群第 3 个 @ 不再理", third == [], str(third))
    free_one = at(ws, FREE, "别的群第一问")
    free_two = at(ws, FREE, "别的群第二问")
    check("没设覆盖的群按档位,连着两个 @ 都回",
          any(REPLY in t for t in free_one) and any(REPLY in t for t in free_two), f"{free_one} {free_two}")


def api_config():
    with authlib.OPENER.open(BASE + "/api/config", timeout=10) as response:
        return json.loads(response.read())["config"]


def web_checks():
    authlib.bootstrap(BASE)
    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        context = browser.new_context(locale="zh-CN", viewport={"width": 1280, "height": 900},
                                      extra_http_headers={"Accept-Language": "zh-CN,zh;q=0.9"})
        page = context.new_page()
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(BASE + "/", wait_until="domcontentloaded")
        authlib.ui_login(page)
        page.evaluate("() => { window.location.hash = '#console/settings'; }")
        page.reload(wait_until="domcontentloaded")
        authlib.ui_login(page)
        page.wait_for_function(
            "() => document.getElementById('settingsStatus')?.textContent?.includes('配置已同步')", timeout=15000)
        page.click('[data-settings-view="qq"]')
        page.wait_for_timeout(400)
        route_row = page.locator(".st-route-row", has_text=f"群聊 {LIMITED}")
        check("会话列表摘要带「限流 1 条/60 秒」", route_row.count() == 1 and "限流 1 条/60 秒" in route_row.first.inner_text(),
              route_row.first.inner_text() if route_row.count() else "没找到会话行")
        route_row.first.click()
        page.wait_for_selector(".st-drawer", timeout=5000)
        page.wait_for_timeout(300)
        field = page.locator(".st-drawer .st-row", has_text="覆盖群聊限流")
        check("群聊抽屉里有「覆盖群聊限流」且显示 1 条 / 60 秒",
              field.count() == 1 and field.first.is_visible() and "1 条" in field.first.inner_text(),
              field.first.inner_text() if field.count() else "没找到")
        page.screenshot(path=str(OUT / "01-group-drawer.png"))
        kind = page.locator('.st-drawer select[aria-label="会话类型"]')
        kind.select_option("private")
        page.wait_for_timeout(200)
        check("切成私聊后这一项藏起来", field.count() == 1 and not field.first.is_visible())
        kind.select_option("group")
        page.wait_for_timeout(200)
        check("切回群聊又出来", field.first.is_visible())
        # 取消覆盖:点开限流控件,去掉勾选,保存。
        field.first.locator("button.st-picker").click()
        page.wait_for_timeout(300)
        box = page.locator(".st-popover input[type=checkbox]")
        box.first.uncheck()
        page.wait_for_timeout(200)
        page.screenshot(path=str(OUT / "02-unchecked.png"))
        check("取消勾选后显示「继承」", "继承" in field.first.inner_text(), field.first.inner_text())
        page.keyboard.press("Escape")
        page.wait_for_timeout(200)
        page.keyboard.press("Escape")
        page.wait_for_timeout(300)
        page.click("#saveConfigButton")
        page.wait_for_timeout(1500)
        routes = api_config()["platforms"]["qq"].get("conversations", [])
        mine = [r for r in routes if r.get("conversation", {}).get("id") == str(LIMITED)]
        check("保存后这条会话没有 rate_limit 键", len(mine) == 1 and "rate_limit" not in mine[0], json.dumps(mine, ensure_ascii=False)[:200])
        check("页面没有脚本报错", not errors, "; ".join(errors[:3]))
        browser.close()


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    server = ThreadingHTTPServer(("127.0.0.1", STUB_PORT), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = None
    try:
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL, stdout=(OUT / "daemon.log").open("w"),
                                  stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/"), "daemon 没起来(看 daemon.log)"
        time.sleep(1.5)
        qq_checks()
        web_checks()
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(5)
            except Exception:
                daemon.kill()
        server.shutdown()
    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if results and all(results) else 1)


if __name__ == "__main__":
    main()
