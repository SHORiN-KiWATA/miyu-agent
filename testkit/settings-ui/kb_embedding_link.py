#!/usr/bin/env python3
"""WebUI 设置页 → 插件 → 知识库：embedding 那一行跟全局同口径、能跳过去。

09-23 用户反馈：主页有 embedding 模型（用内置 bge），知识库设置里却说没配置。
那一行绑的是运行时不读的旧字段 `plugins.knowledge_base.embedding_*`。

     BIN=<miyu 二进制> python3 testkit/settings-ui/kb_embedding_link.py

判定项(每条一行 ✅/❌,最后 n/m passed):沙箱 daemon(自己的端口,不碰 8300)
+ Playwright(Chromium)。产物:~/.cache/miyu-kb-embedding-link/{daemon.log,*.png}
"""
import json
import os
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

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-kb-embedding-link")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18497"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")
LOCAL = "本地 · bge-small-zh-v1.5-int8"
REMOTE = "emb/emb-model"

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + detail) if detail and not ok else ''}")


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [
            {"id": "stub", "display_name": "Stub", "base_url": "http://127.0.0.1:1/v1",
             "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]},
            {"id": "emb", "display_name": "Emb", "base_url": "http://127.0.0.1:1/v1",
             "protocol": "openai-chat", "api_key": "stub", "models": ["emb-model"],
             "model_modalities": {"emb-model": ["embedding"]}},
        ],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2))


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def api_config_payload():
    with authlib.OPENER.open(BASE + "/api/config", timeout=10) as response:
        return json.loads(response.read())


def api_config():
    return api_config_payload()["config"]


def kb_row(page):
    """知识库抽屉里 embedding 那一行:(标签, 值文本)。"""
    row = page.locator(".st-drawer .st-row", has_text="Embedding 模型(全局)")
    if row.count() == 0:
        return None, ""
    return row.first.locator(".lbl strong").inner_text(), row.first.locator(".st-link-value").inner_text()


def open_kb_drawer(page):
    page.click('[data-settings-view="plugins"]')
    page.wait_for_timeout(300)
    page.click(".st-plugin-open:has-text('知识库')")
    page.wait_for_selector(".st-drawer", timeout=5000)
    page.wait_for_timeout(400)


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    daemon = None
    try:
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
            stdin=subprocess.DEVNULL, stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        assert wait_http(f"{BASE}/"), "daemon 没起来(看 daemon.log)"
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
                "() => document.getElementById('settingsStatus')?.textContent?.includes('配置已同步')",
                timeout=15000)

            # ── 知识库抽屉:显示全局值,死字段不摆 ──
            open_kb_drawer(page)
            row = page.locator(".st-drawer .st-row", has_text="Embedding 模型(全局)")
            if row.count():
                row.first.scroll_into_view_if_needed()
                page.wait_for_timeout(200)
            page.screenshot(path=str(OUT / "01-kb-drawer.png"))
            label, value = kb_row(page)
            check("kb_row_present", label is not None, "抽屉里没有「Embedding 模型(全局)」")
            check("kb_row_shows_global_local", value == LOCAL, repr(value))
            drawer_text = page.inner_text(".st-drawer")
            check("no_unconfigured_text", "未配置 Embedding" not in drawer_text)
            check("dead_rows_hidden", "语义最低分" not in drawer_text and "Embedding 超时秒数" not in drawer_text)

            # ── 「去设置」:关抽屉、切到通用页、Embedding 卡进视口 ──
            page.locator(".st-drawer .st-row", has_text="Embedding 模型(全局)").locator("button", has_text="去设置").click()
            page.wait_for_timeout(700)
            check("drawer_closed", page.locator(".st-drawer").count() == 0
                  or not page.locator(".st-drawer").first.is_visible())
            check("general_view_shown", page.is_visible("#settings-general"))
            in_view = page.evaluate("""() => {
                const card = document.querySelector('#settings-general [data-section="embedding"]');
                if (!card) return false;
                const box = card.getBoundingClientRect();
                return box.top >= 0 && box.top < window.innerHeight;
            }""")
            check("embedding_card_in_view", in_view)
            page.screenshot(path=str(OUT / "02-general-embedding.png"))

            # ── 在全局 Embedding 卡里换成远程模型,回知识库抽屉看同步 ──
            card = page.locator('#settings-general [data-section="embedding"]')
            card.locator(".st-row", has_text="远程 Embedding 供应商/模型").locator(".st-picker").click()
            page.wait_for_selector(".st-menu", timeout=5000)
            page.locator(".st-menu .st-menu-item", has_text="emb-model").click()
            page.wait_for_timeout(300)
            open_kb_drawer(page)
            page.locator(".st-drawer .st-row", has_text="Embedding 模型(全局)").first.scroll_into_view_if_needed()
            page.wait_for_timeout(200)
            _, value = kb_row(page)
            check("kb_row_follows_global", value == REMOTE, repr(value))
            page.screenshot(path=str(OUT / "03-kb-drawer-remote.png"))
            page.keyboard.press("Escape")
            page.wait_for_timeout(300)

            # ── 保存:写进全局,旧字段不碰 ──
            page.click("#saveConfigButton")
            page.wait_for_function(
                "() => document.getElementById('settingsStatus')?.textContent?.includes('配置已同步')",
                timeout=15000)
            saved = api_config()
            embedding = saved.get("embedding", {})
            kb = saved.get("plugins", {}).get("knowledge_base", {})
            check("saved_to_global", embedding.get("provider_id") == "emb" and embedding.get("model") == "emb-model",
                  json.dumps(embedding))
            check("legacy_untouched", not kb.get("embedding_provider_id") and not kb.get("embedding_model"),
                  json.dumps({k: kb.get(k) for k in ("embedding_provider_id", "embedding_model")}))
            check("no_page_errors", not errors, "; ".join(errors[:3]))

            # ── 英文界面:标签/按钮/值都没有汉字 ──
            en = browser.new_context(locale="en-US", viewport={"width": 1280, "height": 900},
                                     extra_http_headers={"Accept-Language": "en-US,en;q=0.9"})
            page = en.new_page()
            page.goto(BASE + "/", wait_until="domcontentloaded")
            authlib.ui_login(page)
            current = api_config_payload()
            current["config"].setdefault("display", {})["language"] = "en"
            payload = {"config": current["config"], "secrets": {}, "prompts": current.get("prompts") or {}}
            request = urllib.request.Request(BASE + "/api/config", data=json.dumps(payload).encode(),
                                             method="PUT", headers={"content-type": "application/json"})
            authlib.OPENER.open(request, timeout=30).read()
            page.evaluate("() => { window.location.hash = '#console/settings'; }")
            page.reload(wait_until="domcontentloaded")
            authlib.ui_login(page)
            page.wait_for_function(
                "() => document.getElementById('settingsStatus')?.textContent?.includes('in sync')",
                timeout=15000)
            page.click('[data-settings-view="plugins"]')
            page.wait_for_timeout(300)
            page.click(".st-plugin-open:has-text('Knowledge base')")
            page.wait_for_selector(".st-drawer", timeout=5000)
            page.wait_for_timeout(400)
            row = page.locator(".st-drawer .st-row", has_text="Embedding model (global)")
            en_text = row.inner_text() if row.count() else ""
            check("english_row", row.count() == 1 and "Open settings" in en_text and REMOTE in en_text,
                  repr(en_text))
            page.screenshot(path=str(OUT / "04-kb-drawer-en.png"))
            browser.close()
    finally:
        if daemon and daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({OUT})")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
