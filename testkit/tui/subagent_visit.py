#!/usr/bin/env python3
"""切进子代理会话看、再回来（会话项目第 3 段）。

子代理 09-18 起是一条会话。改之前点它只能开一个盖在正文上的浮层；现在点时间线上那一行、
点任务条上那一行、或者 `/subagent` 挑一条，都是切进那条会话：画面换成它自己的对话
（第一句画成「来自主会话的任务」），任务条第一行「↑ 主会话」，footer 带「子代理 ↳1」。
`/back` 或点「↑ 主会话」回去，主会话那一轮接着看。

    cargo build
    MIYU_HOME=~/.cache/miyu-subagent-visit/home MIYU_TUI_PORT=18661 STUB_PORT=18662 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-subagent-visit/rt OUT=~/.cache/miyu-subagent-visit/out \\
      python3 testkit/tui/subagent_visit.py

步骤：说一句 → 主线派一个前台子代理（它跑一条 40 秒的命令）→
1. 点时间线上子代理那一行 → 在子会话里 → `/back` 回来；
2. 点任务条上那一行 → 在子会话里 → 点「↑ 主会话」回来；再用方向键 ↓ + 回车进去、
   ↓ + 回车回来；
3. 等整轮跑完 → `/subagent` 挑它 → 在子会话里、看得到它的回复 → 说一句，它接着回 →
   `/back`，主会话里没有这句；
4. `/session` 面板里没有子会话。
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

ROW = "走查子代理"
TASK = "来自主会话的任务"
UP = "↑ 主会话"
BADGE = "子代理 ↳1"
REPLY_HEAD = "好的,收到"
FOLLOW_UP = "子会话里追问一句"
STUB = {
    "STUB_SUBAGENT": "1",
    "STUB_SUBAGENT_COMMAND": "sleep 40; printf 'SUBOUT\\n'",
    "STUB_CHUNK_SLEEP": "0.03",
}


def timeline_row(screen):
    """正文里子代理那一步：`⠏ 󰚩 子代理·走查子代理 · 150 · 1.9s · …`（转轮打头）。"""
    for index, line in enumerate(screen):
        if f"·{ROW}" in line and r.is_running_row(line, ROW):
            return index
    return None


def strip_row(screen, text):
    """任务条上那一行：`⠸ 子代理 走查子代理` / `↑ 主会话 …`，在屏幕最底下那一截。"""
    for index in range(len(screen) - 1, -1, -1):
        line = screen[index]
        if text in line and f"·{text}" not in line:
            return index
    return None


def inside_child(screen):
    joined = "\n".join(screen)
    return TASK in joined and BADGE in joined and strip_row(screen, UP) is not None


def back_in_parent(screen):
    joined = "\n".join(screen)
    return h.PROMPT in joined and BADGE not in joined and strip_row(screen, UP) is None


def command(master, sink, text, quiet=0.4, timeout=4.0):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")

        # 1. 时间线上那一行
        screen = r.wait_screen(master, sink, lambda s: timeline_row(s) is not None, 30.0)
        if screen is None:
            r.save("visit-no-timeline-row", r.LAST["screen"] or [])
            report["subagent_row_appeared"] = False
            return report
        h.click(master, sink, 6, timeline_row(screen), quiet=0.3, timeout=1.5)
        screen = r.wait_screen(master, sink, inside_child, 10.0)
        report["timeline_click_enters_child"] = screen is not None
        r.save("visit-timeline", screen or r.LAST["screen"] or [])
        if screen is not None:
            task_line = next((line for line in screen if "子代理走查任务" in line), "")
            report["task_is_not_a_user_bubble"] = bool(task_line) and h.BAR not in task_line
        command(master, sink, "/back", timeout=1.5)
        screen = r.wait_screen(master, sink, back_in_parent, 10.0)
        report["back_command_returns"] = screen is not None
        r.save("visit-back", screen or r.LAST["screen"] or [])

        # 2. 任务条上那一行
        screen = r.wait_screen(
            master, sink, lambda s: strip_row(s, ROW) is not None and s[strip_row(s, ROW)][:1] in r.BRAILLE, 10.0
        )
        report["strip_lists_running_child"] = screen is not None
        if screen is not None:
            h.click(master, sink, 4, strip_row(screen, ROW), quiet=0.3, timeout=1.5)
            screen = r.wait_screen(master, sink, inside_child, 10.0)
            report["strip_click_enters_child"] = screen is not None
            r.save("visit-strip", screen or r.LAST["screen"] or [])
            if screen is not None:
                h.click(master, sink, 4, strip_row(screen, UP), quiet=0.3, timeout=1.5)
                screen = r.wait_screen(master, sink, back_in_parent, 10.0)
                report["up_row_returns"] = screen is not None
                r.save("visit-up-row", screen or r.LAST["screen"] or [])

        # 2b. 方向键：↓ 停在子代理那一行、回车进去；再 ↓ 停在「↑ 主会话」、回车回来。
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=1.5)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, inside_child, 10.0)
        report["keys_enter_child"] = screen is not None
        r.save("visit-keys-in", screen or r.LAST["screen"] or [])
        if screen is not None:
            os.write(master, b"\x1b[B")
            h.settle(master, sink, quiet=0.3, timeout=1.5)
            os.write(master, b"\r")
            screen = r.wait_screen(master, sink, back_in_parent, 10.0)
            report["keys_return_to_parent"] = screen is not None
            r.save("visit-keys-out", screen or r.LAST["screen"] or [])

        # 3. 整轮跑完之后 `/subagent`
        finished = r.wait_screen(
            master, sink,
            lambda s: REPLY_HEAD in "\n".join(s) and timeline_row(s) is None and strip_row(s, ROW) is None,
            60.0,
        )
        report["parent_turn_finishes_after_returning"] = finished is not None
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        command(master, sink, "/subagent", timeout=3.0)
        picker = r.wait_screen(master, sink, lambda s: any(ROW in line and "完成" in line for line in s), 8.0)
        report["subagent_picker_lists_child"] = picker is not None
        r.save("visit-picker", picker or r.LAST["screen"] or [])
        if picker is not None:
            os.write(master, b"\r")
            screen = r.wait_screen(
                master, sink, lambda s: inside_child(s) and REPLY_HEAD in "\n".join(s), 10.0
            )
            report["picker_enters_child_with_its_reply"] = screen is not None
            r.save("visit-picked", screen or r.LAST["screen"] or [])
            # 在子会话里说一句：发给这个子代理，起它的下一轮。
            command(master, sink, FOLLOW_UP, timeout=3.0)
            screen = r.wait_screen(
                master, sink,
                lambda s: FOLLOW_UP in "\n".join(s) and "\n".join(s).count(REPLY_HEAD) >= 2 and BADGE in "\n".join(s),
                30.0,
            )
            report["typing_in_child_talks_to_the_subagent"] = screen is not None
            r.save("visit-child-follow-up", screen or r.LAST["screen"] or [])
            h.settle(master, sink, quiet=0.6, timeout=5.0)
            command(master, sink, "/back", timeout=3.0)
            screen = r.wait_screen(master, sink, back_in_parent, 10.0)
            report["back_again"] = screen is not None
            report["parent_did_not_get_the_follow_up"] = screen is not None and not any(
                FOLLOW_UP in line for line in screen
            )

        # 4. `/session` 只列主会话
        command(master, sink, "/session", timeout=3.0)
        panel = r.wait_screen(master, sink, lambda s: any("选择会话" in line for line in s), 8.0)
        report["session_picker_hides_child"] = panel is not None and not any(ROW in line for line in panel)
        r.save("visit-session-picker", panel or r.LAST["screen"] or [])
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.4, timeout=3.0)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    passed = sum(1 for ok in report.values() if ok)
    print(f"{passed}/{len(report)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if report and passed == len(report) else 1)
