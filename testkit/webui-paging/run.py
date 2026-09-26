#!/usr/bin/env python3
"""会话项目第 2 段：网页按页取回合。

    BIN=<miyu> python3 testkit/webui-paging/run.py

沙箱 daemon + OpenAI 桩（MODE=plain，回得快，每轮报 42 token）+ Playwright（无头 Chromium）。
先经 /api/turns 连发 40 句攒出一个长会话，再开网页：
- 首屏只拉最近一页（30 轮）：最新那句在，最早那句不在；
- 最后一轮的「累计」从会话开头算起：40 × 42 = 1680，显示 1.7k；只从这一页算是 1.3k；
- 对话区滚到顶，更早的 10 轮补上来，滚动位置没跳到最上面；
- 再发一句，跑完后更早那几页还在（运行中每秒同步只取最近一页、合并进来）；
- 回合接口不给 limit 时照旧整段（刷新前就开着的老页面）。
"""
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

# 跑测具的进程多半坐在某个 herdr pane 里：HERDR_* 漏给被测的 miyu 会搅乱那个 pane（09-23）。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

KIT = Path(__file__).resolve().parent.parent / "webui-fixes"
sys.path.insert(0, str(KIT))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-webui-paging")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18591"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18592"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
TURNS = 40
PAGE = 30

RESULTS = []


def check(label, ok, detail=""):
    RESULTS.append(ok)
    print(f"{'✅' if ok else '❌'} {label}" + (f"  ({detail})" if detail else ""))


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]}],
        "memory": {"enabled": False},
        "display": {"language": "zh"},
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


def api(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"content-type": "application/json", "Origin": BASE})
    with authlib.OPENER.open(req, timeout=30) as resp:
        raw = resp.read()
    return json.loads(raw) if raw else None


def settled_turns(session_id):
    """整段取（不给 limit），只数跑完的。"""
    turns = api("GET", f"/api/sessions/{session_id}/turns").get("turns") or []
    return [turn for turn in turns if turn.get("status") == "completed"]


def post_turn(body):
    """发一句。上一轮在库里记成 completed 之后，daemon 还要收一会儿尾（记账、断缓存计数），这时发
    会拿到 409「会话正忙」——等它收完再发（红绿账 09-26 两轮都是第一遍红、复跑绿）。"""
    for _ in range(50):
        try:
            return api("POST", "/api/turns", body)
        except urllib.error.HTTPError as error:
            if error.code != 409:
                raise
            time.sleep(0.2)
    return api("POST", "/api/turns", body)


def wait_until(check_fn, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if check_fn():
            return True
        time.sleep(0.3)
    return False


def user_messages(page):
    return page.locator("#timeline .user-message").count()


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    stub_env = dict(os.environ, STUB_PORT=str(STUB_PORT), MODE="plain", STUB_CHUNK_SLEEP="0")
    stub = subprocess.Popen([sys.executable, str(KIT / "stub_reasoning.py")], env=stub_env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = None
    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL,
                                  stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        created = api("POST", "/api/sessions", {"name": "长会话", "switch": True})
        session_id = (created.get("session") or {}).get("session_id")
        assert session_id, created

        for index in range(1, TURNS + 1):
            post_turn({"content": f"第 {index} 句", "session_id": session_id})
            assert wait_until(lambda: len(settled_turns(session_id)) == index, 60), f"第 {index} 轮没跑完"

        whole = api("GET", f"/api/sessions/{session_id}/turns")
        check("不给 limit 照旧整段", len(whole.get("turns") or []) == TURNS and whole.get("older") is None,
              f"{len(whole.get('turns') or [])} 轮")
        paged = api("GET", f"/api/sessions/{session_id}/turns?limit={PAGE}")
        check("给 limit 只取一页、带游标", len(paged.get("turns") or []) == PAGE and paged.get("older") is not None,
              f"{len(paged.get('turns') or [])} 轮 older={paged.get('older')}")
        per_turn = [turn.get("token_total") or 0 for turn in whole.get("turns") or []]
        expected_base = sum(per_turn[:TURNS - PAGE])
        check("一页之前的用量合计对得上", (paged.get("tokens_before") or {}).get("total") == expected_base,
              f"{paged.get('tokens_before')} vs {expected_base}")

        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 860})
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_selector("#timeline .user-message", timeout=20000)
            page.wait_for_timeout(1500)
            first_screen = user_messages(page)
            timeline = page.text_content("#timeline") or ""
            check("首屏只画最近一页", first_screen == PAGE, f"{first_screen} 条用户消息")
            check("最新那句在、最早那句不在", f"第 {TURNS} 句" in timeline and "第 1 句" not in timeline)
            total = sum(per_turn)
            check("最后一轮的累计从会话开头算起", "累计 1.7k" in timeline,
                  f"整段合计 {total}；页面里: {[part for part in timeline.split('·') if '累计' in part][-1:]}")

            page.mouse.move(640, 300)
            for _ in range(40):
                page.mouse.wheel(0, -4000)
                page.wait_for_timeout(100)
                if user_messages(page) >= TURNS:
                    break
            page.wait_for_timeout(800)
            after = user_messages(page)
            scroll_top = page.evaluate("() => document.getElementById('chatScroll').scrollTop")
            check("滚到顶把更早的补上来了", after == TURNS, f"{after} 条用户消息")
            check("最早那句也在了", "第 1 句" in (page.text_content("#timeline") or ""))
            check("补完之后没跳到最上面", scroll_top > 0, f"scrollTop={scroll_top}")
            page.screenshot(path=str(OUT / "after-older.png"))

            post_turn({"content": "第 41 句", "session_id": session_id})
            assert wait_until(lambda: len(settled_turns(session_id)) == TURNS + 1, 60), "第 41 轮没跑完"
            page.wait_for_timeout(2500)
            final = user_messages(page)
            check("再发一句之后更早那几页还在", final == TURNS + 1, f"{final} 条用户消息")
            browser.close()
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
        stub.terminate()
    print(f"{sum(RESULTS)}/{len(RESULTS)} passed")
    return 0 if all(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
