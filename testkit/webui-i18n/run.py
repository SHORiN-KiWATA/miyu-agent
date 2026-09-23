#!/usr/bin/env python3
"""WebUI 双语走查(2026-09-23):沙箱 daemon + Playwright(Chromium)。

     BIN=<miyu 二进制> python3 testkit/webui-i18n/run.py

判定项(每条一行 ✅/❌,最后 n/m passed):
  payload_zh / payload_en     /i18n.js 注入的 MIYU_LANG 与浏览器语言一致
  dict_stub_zh / dict_full_en 中文界面不下发英文词典(空壳),英文界面下发真词典
  zh_default                  zh 浏览器 + auto:界面中文,<html lang>=zh-CN
  en_default                  en 浏览器 + auto:界面英文,<html lang>=en-US
  en_no_han                   英文界面主视图可见文本没有汉字
  server_board_en             看板标题/输入框占位符(服务端下发)跟请求语言走
  intl_locale                 数字/日期的 Intl 区域跟语言(zh-CN / en-US)
  api_error_en                服务端 API 报错跟请求语言(拿 display.language 校验试)
  config_en_wins              config=en 压过 zh 浏览器
  config_zh_wins              config=zh 压过 en 浏览器
  settings_switch             设置页把「界面语言」改成英语保存 → 整页重载成英文
  console_en                  英文界面下控制台(用量/账号/设置)可见文本没有汉字
产物:~/.cache/miyu-webui-i18n/<TAG>/{daemon.log,*.png}
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

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"])
TAG = os.environ.get("TAG", "run")
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-webui-i18n")).expanduser() / TAG
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18496"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")

HAN = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]")
ZH_HEADERS = {"Accept-Language": "zh-CN,zh;q=0.9"}
EN_HEADERS = {"Accept-Language": "en-US,en;q=0.9"}

results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    results.append((name, bool(ok), detail))
    print(f"{'✅' if ok else '❌'} {name}{('  ' + detail) if detail else ''}")


def write_config(language: str = "auto") -> None:
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "",
        "providers": [],
        "display": {"language": language},
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), "utf-8"
    )


def wait_http(url: str, timeout: float = 40) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def fetch(path: str, headers: dict) -> str:
    request = urllib.request.Request(BASE + path, headers=headers)
    with urllib.request.urlopen(request, timeout=10) as response:
        return response.read().decode("utf-8")


def api(method: str, path: str, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        BASE + path, data=data, method=method, headers={"content-type": "application/json"}
    )
    with authlib.OPENER.open(request, timeout=30) as response:
        raw = response.read()
    return json.loads(raw) if raw else None


def set_config_language(language: str) -> None:
    payload = api("GET", "/api/config")
    config = payload["config"]
    config.setdefault("display", {})["language"] = language
    api(
        "PUT",
        "/api/config",
        {"config": config, "secrets": {}, "prompts": payload.get("prompts") or {}},
    )


def open_page(browser, context, path: str = "/"):
    page = context.new_page()
    page.goto(BASE + path, wait_until="domcontentloaded")
    authlib.ui_login(page)
    page.wait_for_selector("#composerInput", timeout=15000)
    page.wait_for_timeout(700)
    return page


def main() -> int:
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config("auto")
    daemon = None
    try:
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=ENV,
            cwd=str(HOME),
            stdout=(OUT / "daemon.log").open("w"),
            stderr=subprocess.STDOUT,
        )
        assert wait_http(f"{BASE}/"), "daemon 没起来(看 daemon.log)"
        authlib.bootstrap(BASE)

        # ── 静态资源:语言注入与词典按语言分发 ──
        zh_js = fetch("/i18n.js", ZH_HEADERS)
        en_js = fetch("/i18n.js", EN_HEADERS)
        check("payload_zh", 'window.MIYU_LANG="zh"' in zh_js, "Accept-Language: zh-CN")
        check("payload_en", 'window.MIYU_LANG="en"' in en_js, "Accept-Language: en-US")
        zh_dict = fetch("/i18n-en.js", ZH_HEADERS)
        en_dict = fetch("/i18n-en.js", EN_HEADERS)
        check("dict_stub_zh", "New chat" not in zh_dict and len(zh_dict) < 200)
        check("dict_full_en", "New chat" in en_dict, f"{len(en_dict)} 字节")

        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            zh_ctx = browser.new_context(
                locale="zh-CN", extra_http_headers=ZH_HEADERS, viewport={"width": 1280, "height": 900}
            )
            en_ctx = browser.new_context(
                locale="en-US", extra_http_headers=EN_HEADERS, viewport={"width": 1280, "height": 900}
            )

            # ── auto + zh 浏览器 ──
            page = open_page(browser, zh_ctx)
            zh_title = page.get_attribute("#newChatButton", "title")
            zh_lang = page.evaluate("() => document.documentElement.lang")
            zh_board = page.text_content("#emptyTitle")
            zh_placeholder = page.get_attribute("#composerInput", "placeholder")
            zh_intl = page.evaluate("() => window.MiyuI18n?.intlLocale")
            check("zh_default", zh_title == "新对话" and zh_lang == "zh-CN", f"{zh_title!r} {zh_lang}")
            page.close()

            # ── auto + en 浏览器 ──
            page = open_page(browser, en_ctx)
            en_title = page.get_attribute("#newChatButton", "title")
            en_lang = page.evaluate("() => document.documentElement.lang")
            check("en_default", en_title == "New chat" and en_lang == "en-US", f"{en_title!r} {en_lang}")
            body_text = page.evaluate("() => document.body.innerText")
            han_lines = [line for line in body_text.split("\n") if HAN.search(line)]
            check("en_no_han", not han_lines, "; ".join(han_lines[:3]))
            en_board = page.text_content("#emptyTitle")
            en_placeholder = page.get_attribute("#composerInput", "placeholder")
            check(
                "server_board_en",
                en_board and not HAN.search(en_board) and en_board != zh_board
                and en_placeholder and not HAN.search(en_placeholder) and en_placeholder != zh_placeholder,
                f"board={en_board!r} placeholder={en_placeholder!r}",
            )
            en_intl = page.evaluate("() => window.MiyuI18n?.intlLocale")
            check("intl_locale", zh_intl == "zh-CN" and en_intl == "en-US", f"{zh_intl} / {en_intl}")
            page.screenshot(path=str(OUT / "main-en.png"))
            # 这个 app 没有 hashchange 监听:只改 hash 的 goto 是同一文档导航,
            # 不会切视图。设完 hash 再 reload 才是"用户打开这个地址"的形态。
            page.evaluate("() => { window.location.hash = '#console/settings'; }")
            page.reload(wait_until="domcontentloaded")
            page.wait_for_selector("#consoleView:not([hidden])", timeout=15000)
            page.wait_for_timeout(1200)
            page.screenshot(path=str(OUT / "settings-interface-en.png"))
            page.click("#consoleBack")
            page.wait_for_selector("#composerInput", timeout=15000)
            page.wait_for_timeout(600)

            # ── 服务端 API 报错跟请求语言(用非法 display.language 试) ──
            message = page.evaluate(
                """async () => {
                    const current = await (await fetch('/api/config')).json();
                    const config = current.config;
                    config.display = { ...(config.display || {}), language: 'fr' };
                    const response = await fetch('/api/config', {
                        method: 'PUT',
                        headers: { 'content-type': 'application/json' },
                        body: JSON.stringify({ config, secrets: {}, prompts: current.prompts || {} }),
                    });
                    const payload = await response.json().catch(() => ({}));
                    return { status: response.status, message: payload?.error?.message || JSON.stringify(payload) };
                }"""
            )
            check(
                "api_error_en",
                message["status"] == 400
                and "display.language" in message["message"]
                and not HAN.search(message["message"]),
                message["message"][:90],
            )
            page.close()

            # ── config 明确 en/zh 时压过浏览器语言 ──
            set_config_language("en")
            page = open_page(browser, zh_ctx)
            check(
                "config_en_wins",
                page.get_attribute("#newChatButton", "title") == "New chat",
                "zh 浏览器 + config=en",
            )
            page.close()

            set_config_language("zh")
            page = open_page(browser, en_ctx)
            check(
                "config_zh_wins",
                page.get_attribute("#newChatButton", "title") == "新对话",
                "en 浏览器 + config=zh",
            )
            page.close()

            # ── 设置页切「界面语言」:保存后整页重载,当场变英文 ──
            set_config_language("auto")
            # 「界面语言」是「界面」视图里「显示 · 仅 WebUI」卡的第一行
            # (对话字号上面,用户 09-23 指定),分段控件不是 select。
            page = open_page(browser, zh_ctx, "/#console/settings")
            page.wait_for_selector("#saveConfigButton", timeout=15000)
            page.wait_for_timeout(800)
            page.click('[data-language="en"]')
            page.wait_for_function(
                "() => !document.getElementById('saveConfigButton').disabled", timeout=8000
            )
            page.click("#saveConfigButton")
            page.wait_for_function("() => window.MiyuI18n && window.MiyuI18n.lang === 'en'", timeout=20000)
            page.wait_for_selector("#composerInput", timeout=15000)
            page.wait_for_timeout(600)
            switched = page.get_attribute("#newChatButton", "title")
            check("settings_switch", switched == "New chat", f"切换后 title={switched!r}")

            # ── 控制台各面板:英文界面无残留中文 ──
            leftovers: list[str] = []
            for panel in ["usage", "account", "settings"]:
                # 点侧栏按钮切面板(真用户路径);只改 hash 的 goto 是同一文档
                # 导航,不会重载也不会切面板——三张截图会一模一样。
                page.click(f'.con-rail-item[data-console-panel="{panel}"]')
                page.wait_for_selector("#consoleView:not([hidden])", timeout=15000)
                page.wait_for_selector(
                    f'.con-panel[data-console-panel="{panel}"]:not([hidden])', timeout=15000
                )
                page.wait_for_timeout(1600)
                text = page.evaluate("() => document.getElementById('consoleView').innerText")
                leftovers += [f"{panel}: {line}" for line in text.split("\n") if HAN.search(line)]
                page.screenshot(path=str(OUT / f"console-{panel}-en.png"))
            check("console_en", not leftovers, "; ".join(leftovers[:4]))
            page.close()

            browser.close()
    finally:
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()

    passed = sum(1 for _, ok, _ in results if ok)
    print(f"\n{passed}/{len(results)} passed")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
