#!/usr/bin/env python3
"""WebUI 换会话后,输入框下方信息行(上下文、累计)有没有换成新会话的数。

todolist 09-23「webui 删除会话触发的会话切换不会刷新 footer」。两条会话一大一小
(桩模型按「用量 N」报用量):先看大会话,点侧栏切到小会话(对照),再切回大会话把
它删掉——自动切到小会话后,信息行必须是小会话的数。

沙箱 daemon + stub_usage.py + Playwright(Chromium)。网页资源从本地 web/ 读
(WEB=… 指到别的目录就能对照修前修后),改前端不用重编二进制。

    BIN=/path/to/miyu python3 testkit/webui-delete-footer/run.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"]).expanduser()
WEB = Path(os.environ.get("WEB", HERE.parent.parent / "web")).resolve()
# 放 /tmp 下的短路径:unix socket 有 SUN_LEN 上限。
OUT = Path(os.environ.get("OUT", "/tmp/miyu-delete-footer"))
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18510"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18511"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
BIG, SMALL = 50000, 2000


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-usage"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat", "api_key": "stub", "models": ["stub-usage"],
            "model_context_window": {"stub-usage": 100000},
        }],
        "memory": {"enabled": False},
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


def api(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method, headers={"content-type": "application/json"})
    with authlib.OPENER.open(req, timeout=10) as resp:
        raw = resp.read()
        return json.loads(raw) if raw else {}


def serve_local(route):
    url = route.request.url
    name = url.split("?")[0].rsplit("/", 1)[-1] or "index.html"
    local = WEB / name
    if local.exists():
        ctype = {"html": "text/html; charset=utf-8", "js": "application/javascript; charset=utf-8",
                 "css": "text/css; charset=utf-8"}[name.rsplit(".", 1)[-1]]
        route.fulfill(status=200, body=local.read_bytes(), headers={"content-type": ctype, "cache-control": "no-store"})
    else:
        route.continue_()


def tokens(text):
    """「50k / 100k」「2k」这类文本里的第一个数(带单位)。"""
    found = re.search(r"([\d.,]+)\s*([kKmM万]?)", text or "")
    if not found:
        return None
    value = float(found.group(1).replace(",", ""))
    return value * {"k": 1e3, "K": 1e3, "m": 1e6, "M": 1e6, "万": 1e4}.get(found.group(2), 1)


FOOTER_JS = """() => ({
  ctx: document.getElementById('contextNumbers')?.textContent || '',
  cum: document.getElementById('composerCumulative')?.hidden ? null
       : (document.getElementById('composerCumulativeValue')?.textContent || ''),
  chat: document.getElementById('chatScroll')?.innerText || '',
})"""


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True)
    write_config()
    stub = subprocess.Popen([sys.executable, str(HERE / "stub_usage.py")], env=dict(os.environ, STUB_PORT=str(STUB_PORT)))
    daemon = None
    results = []

    def check(name, ok, detail=""):
        results.append(ok)
        print(f"{'✅' if ok else '❌'} {name}  {detail}")

    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        big = api("POST", "/api/sessions", {"name": "大会话"})["session"]["session_id"]
        small = api("POST", "/api/sessions", {"name": "小会话"})["session"]["session_id"]
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 860})
            page.on("dialog", lambda dialog: dialog.accept())
            page.route(lambda u: u.startswith(BASE) and (u.rstrip("/") == BASE or any(k in u for k in ("/app.js", "/styles.css", "/index.html"))), serve_local)
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)

            def open_session(session_id):
                page.click(f'.session-item[data-session-id="{session_id}"] .session-item-main')
                page.wait_for_timeout(1500)

            def send(amount):
                replies = page.evaluate("() => (document.getElementById('chatScroll')?.innerText.match(/收到/g) || []).length")
                page.fill("#composerInput", f"用量 {amount}")
                page.click("#sendButton")
                page.wait_for_function(
                    f"() => (document.getElementById('chatScroll')?.innerText.match(/收到/g) || []).length > {replies}",
                    timeout=30000)
                page.wait_for_timeout(2000)

            def footer():
                return page.evaluate(FOOTER_JS)

            open_session(small)
            send(SMALL)
            open_session(big)
            send(BIG)

            on_big = footer()
            print("看大会话:", {k: on_big[k] for k in ("ctx", "cum")})
            check("看大会话时,上下文是大会话的", (tokens(on_big["ctx"]) or 0) >= 40000, on_big["ctx"])
            check("看大会话时,累计是大会话的", (tokens(on_big["cum"]) or 0) >= 40000, str(on_big["cum"]))

            open_session(small)
            clicked = footer()
            print("点到小会话:", {k: clicked[k] for k in ("ctx", "cum")})
            check("点到小会话,上下文换成小会话的", (tokens(clicked["ctx"]) or 1e9) <= 10000, clicked["ctx"])
            check("点到小会话,累计换成小会话的", (tokens(clicked["cum"]) or 1e9) <= 10000, str(clicked["cum"]))

            open_session(big)
            page.click(f'.session-item[data-session-id="{big}"] .session-menu-button')
            page.wait_for_selector(".session-menu button.is-danger")
            page.click(".session-menu button.is-danger")
            page.wait_for_timeout(3000)
            after = footer()
            print("删掉大会话后:", {k: after[k] for k in ("ctx", "cum")})
            check("删掉大会话后,看的是小会话", f"用量 {SMALL}" in after["chat"] and f"用量 {BIG}" not in after["chat"])
            check("删掉大会话后,上下文是小会话的", (tokens(after["ctx"]) or 1e9) <= 10000, after["ctx"])
            check("删掉大会话后,累计是小会话的", (tokens(after["cum"]) or 1e9) <= 10000, str(after["cum"]))
            page.screenshot(path=str(OUT / "after-delete.png"))
            browser.close()
    finally:
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(10)
            except Exception:
                daemon.kill()
        stub.terminate()
    print(f"{sum(results)}/{len(results)} passed")
    return 0 if results and all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
