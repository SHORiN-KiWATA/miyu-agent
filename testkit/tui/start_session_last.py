#!/usr/bin/env python3
"""打开终端界面进「最近会话」（用户 09-26 加的开关 `tui_start_session = "last"`）真机走查。

默认（开新会话）那一面由 new_session.py 管；这里只验设成「最近会话」之后：

- 说过一句、退出、再开 → 屏幕上**看得到**上一句（接着上次那条聊）；
- 车道指针没换、这条车道的会话没多一条。

直连模式（`MIYU_DIRECT=1`，调试用）不跟这个开关、照旧开新会话：它启动时不回放历史，
接着上次那条会一句之前的话都看不见（09-26 定的口径，见 `cli/repl/direct.rs`）。

    cargo build
    python3 testkit/tui/start_session_last.py
"""

import json
import os
import re
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.append(str(Path(__file__).resolve().parent.parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402
import new_session as ns  # noqa: E402
import sandbox_dir  # noqa: E402
from config_visual import Driver, to_main  # noqa: E402

LAST = {"tui_start_session": "last"}


def settings_scenario(report):
    """`miyu config` → 全局设置最后一项「打开终端界面时进入」：默认「新会话」，改成「最近会话」
    后保存退出，配置里是 `"tui_start_session": "last"`。"""
    sandbox = sandbox_dir.make("miyu-start-session-")
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    runtime = Path(tempfile.mkdtemp(prefix="mx-ss-", dir="/tmp"))
    config_path = home / "config/config.jsonc"
    config_path.write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": "http://127.0.0.1:1/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]}],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }))
    driver = Driver(h.BIN.resolve(), home, runtime)
    try:
        driver.wait("供应商和模型", "保存并退出")
        to_main(driver)
        driver.send(b"j" * 7 + b"\r", "工具最大轮数", "界面语言")
        text = driver.send(b"j" * 19, "打开终端界面时进入") or ""
        row = next((line for line in text.split("\n") if "打开终端界面时进入" in line), "")
        report["设置里有「打开终端界面时进入」、默认新会话"] = "新会话" in row
        driver.send(b"\r", "最近会话")
        os.write(driver.master, b"j")
        driver.pump(0.3)
        text = driver.send(b"\r", "打开终端界面时进入") or ""
        row = next((line for line in text.split("\n") if "打开终端界面时进入" in line), "")
        report["挑成最近会话"] = "最近会话" in row
        driver.send(b"\x1b", "供应商和模型", "保存并退出")
        to_main(driver)
        os.write(driver.master, b"j" * 9 + b"\r")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and driver.process.poll() is None:
            driver.pump(0.1)
        saved = config_path.read_text()
        report["保存后配置里是 last"] = re.search(r'"tui_start_session"\s*:\s*"last"', saved) is not None
    finally:
        driver.close()


def scenario(report, label, direct):
    saved_env = h.ENV
    if direct:
        h.ENV = dict(h.ENV, MIYU_DIRECT="1")
    stub, daemon, tui, master, sink = r.start(ns.STUB, LAST, direct=direct)
    try:
        said = f"{label}第一次说的话"
        ns.ask(master, sink, said)
        # 直连模式起一轮要先在本进程里装好 Agent，回复来得晚：等到字出来，别光等安静。
        report[f"{label}：第一轮跑起来了"] = h.drain_until(master, sink, "知道了", 60.0)
        h.settle(master, sink, quiet=1.0, timeout=10)
        first_pointer = ns.lane_pointer()
        first_count = len(ns.lane_sessions())
        ns.quit_tui(tui, master, sink)

        tui, master = ns.reopen(sink)
        # 直连模式启动慢（本进程开库、装记忆整理），`reopen` 那 4 秒还画不出来：等字出来。
        h.drain_until(master, sink, said, 30.0)
        screen = h.render(bytes(sink))
        r.save(f"start-last-{'direct' if direct else 'remote'}", screen)
        report[f"{label}：再开看得到上一句"] = any(said in line for line in screen)
        report[f"{label}：车道指针没换"] = (
            first_pointer is not None and ns.lane_pointer() == first_pointer
        )
        report[f"{label}：会话没多一条"] = len(ns.lane_sessions()) == first_count
    finally:
        r.stop(tui, daemon, stub)
        h.ENV = saved_env


def main():
    report = {}
    settings_scenario(report)
    scenario(report, "远端", direct=False)
    return report


if __name__ == "__main__":
    report = main()
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{sum(1 for ok in report.values() if ok)}/{len(report)} passed")
    print("产物：", h.OUT)
