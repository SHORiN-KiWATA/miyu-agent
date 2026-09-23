#!/usr/bin/env python3
"""WebUI 设置页 → 通用 → 工具:「默认开启沙盒模式」开关(09-23)。

     BIN=<miyu 二进制> python3 testkit/settings-ui/sandbox_default_toggle.py

沙箱 daemon(自己的端口,不碰 8300)+ Playwright(Chromium)。沙箱配置不写版本号,
迁移会把它当老配置、把这一项刷成关——正好验「升级上来的机器默认关着」。拨开、
保存、从接口读回;英文界面看标签。一条判定一行 ✅/❌,最后 n/m passed。
产物:~/.cache/miyu-sandbox-default-toggle/{daemon.log,*.png}
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

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-sandbox-default-toggle")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18498"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
LABEL = "默认开启沙盒模式"

results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + detail) if detail and not ok else ''}")


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def api_config():
    with authlib.OPENER.open(BASE + "/api/config", timeout=10) as response:
        return json.loads(response.read())


def default_enabled():
    return api_config()["config"].get("tools", {}).get("sandbox", {}).get("default_enabled")


def open_general(page, synced):
    page.evaluate("() => { window.location.hash = '#console/settings'; }")
    page.reload(wait_until="domcontentloaded")
    authlib.ui_login(page)
    page.wait_for_function(
        f"() => document.getElementById('settingsStatus')?.textContent?.includes('{synced}')",
        timeout=15000)
    page.click('[data-settings-view="general"]')
    page.wait_for_timeout(400)


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    (HOME / "config").mkdir(parents=True)
    (HOME / "config" / "config.jsonc").write_text(json.dumps({
        "active_provider": "",
        "providers": [],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }))
    daemon = subprocess.Popen(
        [str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
        stdin=subprocess.DEVNULL, stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
    )
    try:
        assert wait_http(f"{BASE}/"), "daemon 没起来(看 daemon.log)"
        authlib.bootstrap(BASE)
        check("upgraded_config_starts_off", default_enabled() is False, str(default_enabled()))
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            context = browser.new_context(locale="zh-CN", viewport={"width": 1280, "height": 900},
                                          extra_http_headers={"Accept-Language": "zh-CN,zh;q=0.9"})
            page = context.new_page()
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(BASE + "/", wait_until="domcontentloaded")
            authlib.ui_login(page)
            open_general(page, "配置已同步")
            row = page.locator("#settings-general .st-row", has_text=LABEL)
            check("toggle_row_present", row.count() == 1)
            row.first.scroll_into_view_if_needed()
            page.screenshot(path=str(OUT / "01-general.png"))
            row.first.locator("button, input").first.click()
            page.wait_for_timeout(300)
            page.click("#saveConfigButton")
            page.wait_for_function(
                "() => document.getElementById('settingsStatus')?.textContent?.includes('配置已同步')",
                timeout=15000)
            check("saved_as_on", default_enabled() is True, str(default_enabled()))
            check("no_page_errors", not errors, "; ".join(errors[:3]))

            en = browser.new_context(locale="en-US", viewport={"width": 1280, "height": 900},
                                     extra_http_headers={"Accept-Language": "en-US,en;q=0.9"})
            current = api_config()
            current["config"].setdefault("display", {})["language"] = "en"
            request = urllib.request.Request(
                BASE + "/api/config",
                data=json.dumps({"config": current["config"], "secrets": {},
                                 "prompts": current.get("prompts") or {}}).encode(),
                method="PUT", headers={"content-type": "application/json"})
            authlib.OPENER.open(request, timeout=30).read()
            page = en.new_page()
            page.goto(BASE + "/", wait_until="domcontentloaded")
            authlib.ui_login(page)
            open_general(page, "in sync")
            row = page.locator("#settings-general .st-row", has_text="Sandbox mode on by default")
            check("english_label", row.count() == 1)
            browser.close()
    finally:
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
