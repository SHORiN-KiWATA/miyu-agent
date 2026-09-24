#!/usr/bin/env python3
"""网页上的思考档位是每个会话自己的(09-24:effort 做成会话级)。

沙箱 daemon + Playwright(Chromium)。配置里多挂一个 codex 协议的模型:它自带档位表,
不靠 models.dev 元数据,模型菜单里那一行才长档位小片。只点界面、不起回合。判据:

    pin_high_on_a          会话 A 里把那个模型的档位改成 high 并确认:接口里 A 钉着 high
    global_untouched       全局那份没动(GET /api/models/thinking-variants 里还是空)
    chip_shows_pin         A 的档位小片写 high,带「本会话」标记(is-pinned)
    other_session_follows  换到会话 B:小片是全局那一档、没有钉子标记
    model_default_pins     回到 A 选「模型默认」→ 确认:钉的是模型默认档(不是回到跟随全局),小片写 default 带钉子标记
    follow_global_unpins   档位菜单第一项「跟随全局（…）」→ 确认:A 的钉子没了

截图落在 OUT(默认 /tmp/miyu-effort-webui),全过就删(KEEP=1 留着)。

    python3 testkit/webui-composer/effort.py <miyu 二进制>
"""
import os
import re
import shutil
import sys
from pathlib import Path

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from sandbox import WebSandbox  # noqa: E402

OUT = Path(os.environ.get("OUT", "/tmp/miyu-effort-webui"))
CODEX = {
    "id": "cx", "display_name": "Codex", "enabled": True, "protocol": "codex",
    "base_url": "", "models": ["gpt-x"], "default_model": "gpt-x",
}


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    box = WebSandbox(Path(sys.argv[1]).resolve(), OUT, extra_providers=[CODEX])
    results = {}

    def check(name, ok, detail=""):
        results[name] = bool(ok)
        print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:240]}", flush=True)

    def pins(session):
        return box.api("GET", f"/api/sessions/{session}/thinking-variants").get("pinned", [])

    def chip(page):
        row = page.locator(".model-menu-row", has_text="gpt-x")
        return row.locator(".model-level-chip")

    def open_menu(page):
        page.click("#modelButton")
        page.wait_for_selector("#modelMenu:not([hidden])", timeout=5000)
        page.wait_for_timeout(600)

    def pick_level(page, label):
        chip(page).click()
        page.wait_for_selector("#modelLevelMenu:not([hidden])", timeout=5000)
        # 按整行认:「跟随全局（模型默认）」里也有「模型默认」四个字。
        exact = re.compile(rf"^{re.escape(label)}")
        page.locator("#modelLevelMenu .model-level-option").filter(has_text=exact).first.click()
        page.wait_for_timeout(200)
        page.click("#modelMenu .model-confirm")
        page.wait_for_selector("#modelMenu", state="hidden", timeout=10000)
        page.wait_for_timeout(800)

    try:
        box.start()
        a = box.create_session("甲")
        b = box.create_session("乙")
        with sync_playwright() as playwright:
            browser, page = box.open_page(playwright)
            box.view(page, a)
            open_menu(page)
            pick_level(page, "high")
            pinned = pins(a)
            check("pin_high_on_a", any(p.get("model") == "gpt-x" and p.get("selected") == "high" for p in pinned),
                  pinned)
            options = box.api("GET", "/api/models/thinking-variants").get("options", [])
            global_cx = [o for o in options if o.get("model") == "gpt-x"]
            check("global_untouched", global_cx and global_cx[0].get("selected") in (None, ""), global_cx)
            open_menu(page)
            text, classes = chip(page).inner_text(), chip(page).get_attribute("class") or ""
            page.screenshot(path=str(OUT / "a-pinned.png"))
            check("chip_shows_pin", "high" in text and "is-pinned" in classes, f"{text!r} {classes!r}")
            page.keyboard.press("Escape")
            page.wait_for_timeout(300)

            box.view(page, b)
            open_menu(page)
            text, classes = chip(page).inner_text(), chip(page).get_attribute("class") or ""
            check("other_session_follows", "high" not in text and "is-pinned" not in classes,
                  f"{text!r} {classes!r}")
            page.keyboard.press("Escape")
            page.wait_for_timeout(300)

            box.view(page, a)
            open_menu(page)
            pick_level(page, "模型默认")
            pinned = pins(a)
            open_menu(page)
            text, classes = chip(page).inner_text(), chip(page).get_attribute("class") or ""
            page.screenshot(path=str(OUT / "a-model-default.png"))
            check("model_default_pins",
                  any(p.get("model") == "gpt-x" and p.get("selected") == "@model-default" for p in pinned)
                  and text.strip().startswith("default") and "is-pinned" in classes,
                  f"{pinned} {text!r} {classes!r}")
            page.keyboard.press("Escape")
            page.wait_for_timeout(300)
            open_menu(page)
            pick_level(page, "跟随全局")
            check("follow_global_unpins", not [p for p in pins(a) if p.get("model") == "gpt-x"], pins(a))
            browser.close()
    except Exception as error:  # noqa: BLE001 — 走查中途出错也要记成失败
        check("aborted", False, repr(error))
    finally:
        box.stop()
    failed = [name for name, ok in results.items() if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} passed  截图在 {OUT}")
    if not failed and not os.environ.get("KEEP"):
        shutil.rmtree(OUT, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
