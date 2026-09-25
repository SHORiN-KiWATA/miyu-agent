#!/usr/bin/env python3
"""方向键进出任务条、在命令候选里挑（会话项目第 3 段，用户口径第 8 条）。

用户原话：在输入框底部一行继续往下按方向键，如果下方有状态行就切换过去；状态行最多
显示 5 个，多了底下写「↓ 还有 x 个」，往下滚一个 x 减一个，反之亦然；滚到顶再往上回到
输入框；在状态行上回车相当于交互。命令列表开着时 ↑↓ 就在列表里。

    cargo build
    MIYU_HOME=~/.cache/miyu-strip-keys/home MIYU_TUI_PORT=18681 STUB_PORT=18682 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-strip-keys/rt OUT=~/.cache/miyu-strip-keys/out \\
      python3 testkit/tui/strip_keys.py

步骤：一轮里开 6 条后台命令 → 任务条露 5 条加「↓ 还有 1 个」→ ↑ 翻历史、↓ 翻回来（不进
任务条）→ 再 ↓ 进任务条，一路往下滚到底、再一路往上回输入框 → 停在一条上回车开日志
面板 → `/s` 的候选里 ↓ 挑第二条、Tab 补全 → `/us` 挑中回车就执行。
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

JOBS = 6
CALLS = [
    {"name": "run_command",
     "arguments": {"command": "sleep 300", "background": True, "title": f"走查后台{index}"}}
    for index in range(1, JOBS + 1)
]
STUB = {"STUB_EXTRA_CALLS": json.dumps(CALLS, ensure_ascii=False), "STUB_CHUNK_SLEEP": "0.01"}
DOWN, UP, ENTER, TAB, ESC = b"\x1b[B", b"\x1b[A", b"\r", b"\t", b"\x1b"
REPLY_HEAD = "好的,收到"


def strip_rows(screen):
    """任务条上的几条（`⠋ 命令 xxxxxx · 走查后台N`，停着的那条行首是 `›`）。"""
    return [(index, line) for index, line in enumerate(screen) if "走查后台" in line and " · " in line]


def more_line(screen):
    return next((line for line in screen if line.startswith("↓ 还有")), None)


def focused(screen):
    return [line for _, line in strip_rows(screen) if line.startswith("›")]


def input_box(screen):
    """输入框那几行：footer（模式那一行）上面紧挨着的三行。"""
    footer = next((index for index, line in enumerate(screen) if line.startswith("┃ 普通 ·")), None)
    if footer is None:
        return ""
    return "\n".join(screen[max(footer - 3, 0):footer])


def footer_row(screen):
    return next((index for index, line in enumerate(screen) if line.startswith("┃ 普通 ·")), None)


def key(master, sink, data, quiet=0.25):
    os.write(master, data)
    h.settle(master, sink, quiet=quiet, timeout=2.0)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, ENTER)
        screen = r.wait_screen(
            master, sink, lambda s: REPLY_HEAD in "\n".join(s) and more_line(s) is not None, 60.0
        )
        report["five_rows_and_one_more"] = (
            screen is not None and len(strip_rows(screen)) == 5 and "1" in (more_line(screen) or "")
        )
        r.save("keys-strip", screen or r.LAST["screen"] or [])
        if screen is None:
            return report
        h.settle(master, sink, quiet=0.8, timeout=5.0)

        # ↑ 翻出上一句，↓ 翻回来：翻历史的时候 ↓ 还是历史的。
        screen = key(master, sink, UP)
        report["up_recalls_history"] = h.PROMPT in input_box(screen)
        screen = key(master, sink, DOWN)
        report["down_while_browsing_stays_in_history"] = not focused(screen)

        # 再 ↓：输入框空着，进任务条。
        screen = key(master, sink, DOWN)
        footer_before = footer_row(screen)
        rows = focused(screen)
        report["down_enters_the_strip"] = len(rows) == 1 and "走查后台" in rows[0]
        r.save("keys-focus-first", screen)
        for _ in range(JOBS - 1):
            screen = key(master, sink, DOWN)
        report["scrolled_to_the_last_row"] = (
            more_line(screen) is None
            and len(strip_rows(screen)) == 5
            and len(focused(screen)) == 1
            and f"走查后台{JOBS}" in focused(screen)[0]
        )
        # 滚到底输入框不跳：footer 还在原来那一行。
        report["input_did_not_jump"] = footer_row(screen) == footer_before
        r.save("keys-focus-last", screen)
        for _ in range(JOBS - 1):
            screen = key(master, sink, UP)
        report["back_at_the_top_row"] = len(focused(screen)) == 1 and more_line(screen) is not None
        screen = key(master, sink, UP)
        report["up_from_the_top_returns_to_input"] = not focused(screen)

        # 停在一条上回车：后台命令开日志面板。
        key(master, sink, DOWN)
        screen = key(master, sink, ENTER, quiet=0.5)
        report["enter_opens_the_log_panel"] = any("Esc" in line and "关闭" in line for line in screen)
        r.save("keys-panel", screen)
        key(master, sink, ESC, quiet=0.5)

        # 命令候选：↓ 挑第二条，Tab 补全成它。
        os.write(master, b"/s")
        h.drain_until(master, sink, "/s", 3.0)
        screen = key(master, sink, DOWN)
        screen = key(master, sink, DOWN)
        raw = bytes(sink).decode("utf-8", "replace")
        report["picked_candidate_is_highlighted"] = "\x1b[1m\x1b[35m/" in raw
        r.save("keys-pick", screen)
        screen = key(master, sink, TAB)
        prompt_line = next((line for line in reversed(screen) if line.startswith("┃ /")), "")
        report["tab_takes_the_picked_candidate"] = prompt_line.strip("┃ ").startswith("/s") and prompt_line.strip("┃ ") != "/s"
        report["_tab_result"] = prompt_line
        for _ in range(12):
            os.write(master, b"\x7f")
        h.settle(master, sink, quiet=0.3, timeout=2.0)

        # `/us` 挑中回车：直接执行 /usage。
        os.write(master, b"/us")
        h.drain_until(master, sink, "/us", 3.0)
        key(master, sink, DOWN)
        os.write(master, ENTER)
        screen = r.wait_screen(master, sink, lambda s: any("Token" in line or "词元" in line for line in s), 10.0)
        report["enter_runs_the_picked_command"] = screen is not None
        r.save("keys-usage", screen or r.LAST["screen"] or [])
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    checks = {key: ok for key, ok in report.items() if not key.startswith("_")}
    passed = sum(1 for ok in checks.values() if ok)
    print(f"{passed}/{len(checks)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if checks and passed == len(checks) else 1)
