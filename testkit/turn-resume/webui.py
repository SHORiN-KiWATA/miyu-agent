#!/usr/bin/env python3
"""断点续跑的网页走查(09-24):沙箱 daemon + stub.py + Playwright(Chromium)。

网页开着一条会话,里面跑着一轮慢工具(sleep 30);命令跑起来之后把 daemon SIGKILL 掉、
再起一个新的(相当于重启、换二进制)。判据:

    web01_notice_live        网页自己接上续跑那一轮:出现「↻ Miyu 重启了，接着上一轮继续」
    web02_reply_live         续跑那一轮的回话(RESUMED attempt=1)出现在同一页
    web03_notice_after_reload  刷新之后回看,还是那一行提示
    web04_no_envelope        页面上不露 <service-restart 外壳,也不画成用户气泡

截图落在 OUT(默认 /tmp/miyu-resume-webui),全过就删(KEEP=1 留着)。

    python3 testkit/turn-resume/webui.py <miyu 二进制>
"""
import os
import shutil
import signal
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-composer"))
from sandbox import WebSandbox  # noqa: E402

OUT = Path(os.environ.get("OUT", "/tmp/miyu-resume-webui"))
NOTICE = "Miyu 重启了，接着上一轮继续"


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
        session = box.create_session("续跑")
        with sync_playwright() as playwright:
            browser, page = box.open_page(playwright)
            box.view(page, session)
            page.fill("#composerInput", "TK slow W1")
            page.keyboard.press("Enter")
            page.wait_for_selector("text=sleep 30", timeout=20000)
            page.wait_for_timeout(1500)
            box.kill_daemon(signal.SIGKILL)
            page.wait_for_timeout(1500)
            box.start_daemon()
            deadline = time.time() + 60
            body = ""
            while time.time() < deadline:
                body = page.inner_text("#timeline") if page.locator("#timeline").count() else page.inner_text("body")
                if NOTICE in body and "RESUMED attempt=1" in body:
                    break
                time.sleep(0.5)
            page.screenshot(path=str(OUT / "live.png"))
            check("web01_notice_live", NOTICE in body, body[-300:])
            check("web02_reply_live", "RESUMED attempt=1" in body, body[-300:])
            page.reload()
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)
            box.view(page, session)
            page.wait_for_timeout(1500)
            body = page.inner_text("#timeline") if page.locator("#timeline").count() else page.inner_text("body")
            page.screenshot(path=str(OUT / "reload.png"))
            check("web03_notice_after_reload", NOTICE in body, body[-300:])
            bubbles = page.locator(".user-message .user-bubble").all_inner_texts()
            check("web04_no_envelope",
                  "<service-restart" not in body and not any("service-restart" in text for text in bubbles),
                  bubbles)
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
