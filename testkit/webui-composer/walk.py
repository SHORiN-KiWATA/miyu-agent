#!/usr/bin/env python3
"""输入框与侧栏的三件小事(09-24):这一轮的计时、`/clear`、批量删除会话。

沙箱 daemon + 桩模型(testkit/turn-resume/stub.py)+ Playwright(Chromium)。判据:

    timer_ticks_while_running    跑着的那一轮:输入框那排最右边出现「用时」,几秒后数字变大
    timer_hidden_when_done       这一轮跑完:「用时」不再显示(跑完就不显示,和终端一个规矩)
    timer_hidden_on_other_session  换到别的会话:也不显示
    clear_menu_shows_alias       敲 /cl:补全菜单里有 /clear,说明写着「= /reset」
    clear_resets_conversation    /clear 回车执行:这条会话的对话被清空(和 /reset 一样)
    select_mode_from_menu        「…」菜单点「多选…」:顶上出现操作栏「已选 1 个」,行首是勾选框
    select_click_toggles         选择模式下点另一行是勾上(已选 2 个),不是打开它
    batch_delete_removes         删除 → 确认 → 两条都没了(侧栏与接口都没了),操作栏收起
    batch_delete_viewed_falls_back  正在看的那条也被删了:切到剩下的某一条

截图(侧栏选择模式、输入框那排)落在 OUT(默认 /tmp/miyu-composer-webui),全过就删。

    python3 testkit/webui-composer/walk.py <miyu 二进制>
"""
import os
import re
import shutil
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from sandbox import WebSandbox  # noqa: E402

OUT = Path(os.environ.get("OUT", "/tmp/miyu-composer-webui"))


def seconds_of(text):
    """`12s` / `1m 05s` / `1h 02m 05s` → 秒。"""
    total = 0
    for value, unit in re.findall(r"(\d+)([hms])", text or ""):
        total += int(value) * {"h": 3600, "m": 60, "s": 1}[unit]
    return total


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    box = WebSandbox(Path(sys.argv[1]).resolve(), OUT)
    results = {}

    def check(name, ok, detail=""):
        results[name] = bool(ok)
        print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:240]}", flush=True)

    try:
        box.start()
        work = box.create_session("计时")
        other = box.create_session("另一条")
        c = box.create_session("删我一")
        d = box.create_session("删我二")
        with sync_playwright() as playwright:
            browser, page = box.open_page(playwright)
            box.view(page, work)

            # —— 这一轮的计时 ——
            page.fill("#composerInput", "TK slow T1 4")
            page.keyboard.press("Enter")
            page.wait_for_selector("#composerTimer:not([hidden])", timeout=15000)
            first = page.inner_text("#composerTimerValue")
            page.wait_for_timeout(2200)
            second = page.inner_text("#composerTimerValue")
            check("timer_ticks_while_running", seconds_of(second) > seconds_of(first), f"{first} → {second}")
            page.screenshot(path=str(OUT / "timer-running.png"))
            deadline = time.time() + 30
            while time.time() < deadline and not any(
                turn.get("status") == "completed" for turn in box.turns(work)
            ):
                time.sleep(0.5)
            page.wait_for_timeout(1500)
            visible = page.is_visible("#composerTimer")
            check("timer_hidden_when_done", not visible, f"visible={visible} last={second}")
            page.screenshot(path=str(OUT / "timer-done.png"))
            box.view(page, other)
            check("timer_hidden_on_other_session", not page.is_visible("#composerTimer"))

            # —— /clear 就是 /reset ——
            page.fill("#composerInput", "hello")
            page.keyboard.press("Enter")
            deadline = time.time() + 20
            while time.time() < deadline and not box.turns(other):
                time.sleep(0.5)
            page.wait_for_timeout(1200)
            page.fill("#composerInput", "")
            page.type("#composerInput", "/cl")
            page.wait_for_timeout(400)
            menu = page.locator(".commandMenuItem")
            rows = [menu.nth(i).inner_text() for i in range(menu.count())]
            check("clear_menu_shows_alias",
                  any("/clear" in row and "= /reset" in row for row in rows), rows)
            page.fill("#composerInput", "/clear")
            page.wait_for_timeout(300)
            page.keyboard.press("Escape")
            page.keyboard.press("Enter")
            deadline = time.time() + 15
            while time.time() < deadline and box.turns(other):
                time.sleep(0.5)
            check("clear_resets_conversation", not box.turns(other), len(box.turns(other)))

            # —— 批量删除 ——
            box.view(page, c)
            item = page.locator(f'.session-item[data-session-id="{c}"]')
            item.hover()
            item.locator(".session-menu-button").click()
            page.get_by_role("menuitem", name="多选…").click()
            page.wait_for_selector(".session-bulk-bar", timeout=5000)
            count = page.inner_text(".session-bulk-count")
            boxes = page.locator(".session-item.is-selecting .session-select-box").count()
            check("select_mode_from_menu", "已选 1 个" in count and boxes >= 3, f"{count} boxes={boxes}")
            viewed_before = page.evaluate("() => document.querySelector('.session-item.active')?.dataset.sessionId")
            page.click(f'.session-item[data-session-id="{d}"] .session-item-main')
            page.wait_for_timeout(400)
            count = page.inner_text(".session-bulk-count")
            viewed_after = page.evaluate("() => document.querySelector('.session-item.active')?.dataset.sessionId")
            check("select_click_toggles", "已选 2 个" in count and viewed_after == viewed_before,
                  f"{count} viewed {viewed_before} → {viewed_after}")
            page.locator(".session-bulk-bar").screenshot(path=str(OUT / "bulk-bar.png"))
            page.locator("#sidebar").screenshot(path=str(OUT / "sidebar-selecting.png"))
            page.locator(".session-bulk-button.is-danger").click()
            deadline = time.time() + 20
            while time.time() < deadline and page.locator(".session-bulk-bar").count():
                time.sleep(0.3)
            page.wait_for_timeout(1500)
            listed = page.evaluate(
                "() => [...document.querySelectorAll('.session-item')].map((n) => n.dataset.sessionId)")
            remaining = {s["session_id"] for s in box.sessions()}
            check("batch_delete_removes",
                  c not in listed and d not in listed and c not in remaining and d not in remaining
                  and page.locator(".session-bulk-bar").count() == 0,
                  f"listed={listed}")
            viewed = page.evaluate("() => document.querySelector('.session-item.active')?.dataset.sessionId")
            check("batch_delete_viewed_falls_back", viewed in (work, other), viewed)
            page.locator("#sidebar").screenshot(path=str(OUT / "sidebar-after.png"))
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
