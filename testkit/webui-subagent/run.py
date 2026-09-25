#!/usr/bin/env python3
"""会话项目第 4 段：网页里点子代理卡片进它的会话，「↑ 主会话」回来。

    cargo build
    BIN=target/debug/miyu python3 testkit/webui-subagent/run.py
    # 对照改前：BIN=~/.local/bin/miyu …

沙箱 daemon + repl-smoke 的桩（主线派一个前台子代理，子代理跑一条 15 秒的命令）+ Playwright：
- 接口：子会话的回合、上下文、待办都能取（改前 404），回合接口报它是子会话、父会话是谁；
  `/api/sessions/{id}/subagents` 列得出它；
- 网页：子代理那张卡片认出子会话（实时那条标记），卡片上的词元小标有数，点抬头换到子会话，
  输入框上方挂「↑ 主会话」，第一句画成「来自主会话的任务」；点「↑ 主会话」回来；
- 这一轮跑完、刷新之后（卡片是回看画的）照样点得进去，看得到子代理的回复；
- 老标记中继的标记（`__subagent_session__`、`__subagent_metric__` 等）一条都不漏到页面上。
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

ROOT = Path(__file__).resolve().parents[2]
KIT = ROOT / "testkit" / "webui-fixes"
sys.path.insert(0, str(KIT))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"]).expanduser().resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-webui-subagent")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18595"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18596"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
TASK_TEXT = "子代理走查任务"
REPLY_HEAD = "好的,收到"
# 老标记中继的那几种（`__subagent_session__`、`__subagent_metric__`、`__subtool_preparing__`
# ……）：哪一种漏到页面上都不对。
MARKER = re.compile(r"__sub[a-z_]*__")
# 漏出来的标记只在窥视那一行上闪一下就被下一条盖掉，取一次样抓不稳（09-25 修复前也绿过一次）：
# 页面里挂一个观察者，出现过就记下。
WATCH_LEAKS = """
() => {
  window.__markerLeaks = [];
  const scan = (text) => {
    const hits = String(text || "").match(/__sub[a-z_]*__/g);
    if (hits) window.__markerLeaks.push(...hits);
  };
  new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      if (mutation.type === "characterData") scan(mutation.target.textContent);
      for (const node of mutation.addedNodes) scan(node.textContent);
    }
  }).observe(document.body, { subtree: true, childList: true, characterData: true });
}
"""


def leaks(page):
    seen = set(page.evaluate("() => window.__markerLeaks || []"))
    seen.update(MARKER.findall(page.text_content("body") or ""))
    return sorted(seen)

RESULTS = []


def check(label, ok, detail=""):
    RESULTS.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {label}" + (f"  ({detail})" if detail else ""), flush=True)


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


def status_of(path):
    try:
        api("GET", path)
        return 200
    except urllib.error.HTTPError as error:
        return error.code


def wait_until(check_fn, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = check_fn()
        if value:
            return value
        time.sleep(0.4)
    return None


def children(parent_id):
    try:
        return api("GET", f"/api/sessions/{parent_id}/subagents").get("sessions") or []
    except urllib.error.HTTPError:
        return []


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    stub_env = dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_SUBAGENT="1",
                    STUB_SUBAGENT_COMMAND="sleep 15; printf 'SUBOUT\\n'", STUB_CHUNK_SLEEP="0.01")
    stub = subprocess.Popen([sys.executable, str(ROOT / "testkit" / "repl-smoke" / "stub_llm.py")],
                            env=stub_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = None
    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL,
                                  stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        created = api("POST", "/api/sessions", {"name": "网页走查", "switch": True})
        parent_id = (created.get("session") or {}).get("session_id")
        assert parent_id, created

        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 900})
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_timeout(1500)
            page.evaluate(WATCH_LEAKS)
            api("POST", "/api/turns", {"content": "走查一句", "session_id": parent_id})

            child = wait_until(lambda: (children(parent_id) or [None])[0], 30)
            check("父会话名下列得出子会话", child is not None)
            if child is None:
                return 1
            child_id = child["session_id"]
            turns = api("GET", f"/api/sessions/{child_id}/turns?limit=30")
            check("子会话的回合取得到，报它是子会话",
                  turns.get("session_kind") == "subagent"
                  and (turns.get("parent") or {}).get("session_id") == parent_id,
                  f"kind={turns.get('session_kind')} parent={turns.get('parent')}")
            check("子会话的上下文、待办取得到",
                  status_of(f"/api/sessions/{child_id}/context") == 200
                  and status_of(f"/api/sessions/{child_id}/todos") == 200)

            card = page.locator(".tool-card.is-task.has-child-session").first
            try:
                card.wait_for(timeout=20000)
                linked = True
            except Exception:
                linked = False
            check("实时那张子代理卡片认出了子会话", linked)
            # daemon 发的词元标记（`__subagent_metric__`）网页原来不认：小标一直空着，标记本身
            # 漏进窥视那一行。
            token = page.locator(".tool-card.is-task .tool-task-token").first
            try:
                page.wait_for_function(
                    "el => el && el.textContent.trim().length > 0", arg=token.element_handle(), timeout=15000
                )
                token_text = token.text_content().strip()
            except Exception:
                token_text = ""
            check("实时卡片上有子代理的词元数", bool(token_text), token_text)
            leaked = leaks(page)
            check("标记没漏到页面上", not leaked, ", ".join(leaked))
            page.screenshot(path=str(OUT / "01-parent-live.png"))
            if linked:
                card.locator(":scope > .tool-head").click()
                bar = page.locator("#subagentParentBar")
                try:
                    bar.wait_for(state="visible", timeout=10000)
                    entered = TASK_TEXT in (page.text_content("#timeline") or "")
                except Exception:
                    entered = False
                check("点卡片换到子会话，顶上挂「↑ 主会话」", entered,
                      (page.text_content("#subagentParentBar") or "").strip())
                task_block = page.locator(".xs-message").filter(has_text="来自主会话的任务")
                bubble_has_task = page.locator(".user-message").filter(has_text=TASK_TEXT).count() > 0
                check("子会话第一句画成「来自主会话的任务」，不是用户气泡",
                      task_block.count() > 0 and not bubble_has_task)
                page.screenshot(path=str(OUT / "02-child-live.png"))
                if entered:
                    bar.click()
                    page.wait_for_timeout(1500)
                    back = bar.is_hidden() and "走查一句" in (page.text_content("#timeline") or "")
                    check("点「↑ 主会话」回来", back)

            done = wait_until(
                lambda: all(t.get("status") == "completed"
                            for t in api("GET", f"/api/sessions/{parent_id}/turns").get("turns") or [{}])
                and api("GET", f"/api/sessions/{parent_id}/turns").get("turns"),
                90,
            )
            check("这一轮跑完了", bool(done))
            page.reload()
            page.wait_for_timeout(2500)
            saved = page.locator(".tool-card.is-task.has-child-session").first
            try:
                saved.wait_for(timeout=10000)
                saved_linked = True
            except Exception:
                saved_linked = False
            check("回看画的卡片也认得子会话", saved_linked)
            if saved_linked:
                # 跑完的过程收成了一行「Ran … / Worked for …」，卡片在里面：先点开那一行。
                folded = page.locator(".proc-line:not(.is-open)").filter(has=saved)
                if folded.count():
                    # 只有图标和那串字接点击（抬头本身 pointer-events: none，空白处不算）。
                    folded.first.locator(":scope > .proc-head .proc-summary").click()
                    page.wait_for_timeout(600)
                saved.locator(":scope > .tool-head").click()
                try:
                    page.locator("#subagentParentBar").wait_for(state="visible", timeout=10000)
                    page.wait_for_timeout(1000)
                    timeline = page.text_content("#timeline") or ""
                    ok = TASK_TEXT in timeline and REPLY_HEAD in timeline
                except Exception:
                    ok = False
                check("刷新后点进子会话，看得到它的回复", ok)
                page.screenshot(path=str(OUT / "03-child-saved.png"))
            # 刷新之后观察者没了，重挂一个也只看得到回看那一截：这一项查的是回看画出来的样子。
            check("回看的页面上也没有标记", not MARKER.search(page.text_content("body") or ""))
            browser.close()
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
        stub.terminate()
    print(f"{sum(RESULTS)}/{len(RESULTS)} passed", flush=True)
    return 0 if all(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
