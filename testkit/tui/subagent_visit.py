#!/usr/bin/env python3
"""切进子代理会话看、再回来（会话项目第 3 段）。

子代理 09-18 起是一条会话。改之前点它只能开一个盖在正文上的浮层；现在点时间线上那一行、
点任务条上那一行、或者 `/subagent` 挑一条，都是切进那条会话：画面换成它自己的对话
（第一句画成「来自主会话的任务」），任务条第一行「○ 主会话」，footer 带「子代理 ↳1」。
`/back` 或点「○ 主会话」回去，主会话那一轮接着看。

    cargo build
    MIYU_HOME=~/.cache/miyu-subagent-visit/home MIYU_TUI_PORT=18661 STUB_PORT=18662 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-subagent-visit/rt OUT=~/.cache/miyu-subagent-visit/out \\
      python3 testkit/tui/subagent_visit.py

步骤：说一句 → 主线派一个子代理（09-26 起只在后台跑：派完这一轮就收，它跑一条 40 秒的命令）→
1. 任务条上它那一行露着它在干什么、秒数在走；点开时间线上收起来的那一段、点子代理那一步
   → 在子会话里 → `/back` 回来，任务条上的秒数接着走；
2. 点任务条上那一行 → 在子会话里 → 点「○ 主会话」回来；再用方向键 ↓ + 回车进去、
   ↓ + 回车回来；
3. 等它跑完、汇报叫醒主会话那一轮也收尾 → `/subagent` 挑它 → 在子会话里、看得到它的回复 →
   说一句，它接着回 →
   `/back`，主会话里没有这句；
4. `/session` 面板里没有子会话。
"""

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

ROW = "走查子代理"
TASK = "来自主会话的任务"
# 回主会话那一行。09-26 前是「↑ 主会话」：拿旧二进制量「改前」时用 `STRIP_UP` 换回去。
UP = os.environ.get("STRIP_UP", "○ 主会话")
BADGE = "子代理 ↳1"
REPLY_HEAD = "好的,收到"
FOLLOW_UP = "子会话里追问一句"
# 主回合收尾后那一段收成一行（`fold_timeline` 默认开）。
FOLD = "1 tool"
# 汇报叫醒主会话的那一轮开头那一行。
WAKE = "子代理完成"
STUB = {
    "STUB_SUBAGENT": "1",
    "STUB_SUBAGENT_COMMAND": "sleep 40; printf 'SUBOUT\\n'",
    "STUB_CHUNK_SLEEP": "0.03",
}


def timeline_row(screen):
    """正文里子代理那一步：`󰚩 子代理·走查子代理`（派完就收，那一步不转轮）。"""
    for index, line in enumerate(screen):
        if f"·{ROW}" in line:
            return index
    return None


def fold_row(screen):
    """主回合收尾后收起来的那一行：`› 用了 1 个工具`。"""
    for index, line in enumerate(screen):
        if FOLD in line and "›" in line:
            return index
    return None


def strip_seconds(screen):
    """任务条上子代理那一行报的秒数（行尾 `34s`、`1m 02s`）。"""
    index = strip_row(screen, ROW)
    if index is None:
        return None
    line = screen[index]
    minutes = re.search(r"(\d+)m (\d+)s\s*$", line)
    if minutes:
        return int(minutes.group(1)) * 60 + int(minutes.group(2))
    seconds = re.search(r"(\d+)s\s*$", line)
    return int(seconds.group(1)) if seconds else None


def strip_row(screen, text):
    """任务条上那一行：`○ 子代理 走查子代理` / `○ 主会话 …`，在屏幕最底下那一截。"""
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

        # 1. 主回合派完就收，子代理在后台跑：任务条上它那一行露着它在干什么（跑着 `sleep 40`
        #    那条命令）、秒数在走。
        screen = r.wait_screen(
            master, sink,
            lambda s: (fold_row(s) is not None or timeline_row(s) is not None)
            and (strip_seconds(s) or 0) >= 2,
            30.0,
        )
        if screen is None:
            r.save("visit-no-timeline-row", r.LAST["screen"] or [])
            report["subagent_row_appeared"] = False
            return report
        seconds_before = strip_seconds(screen)
        strip_line = screen[strip_row(screen, ROW)]
        report["strip_row_peeks_what_the_child_is_doing"] = "跑个命令" in strip_line or "运行命令" in strip_line
        report["_strip_row"] = strip_line.strip()
        # 时间线上那一段收起来了：先点开，再点子代理那一步切进它的会话。
        if timeline_row(screen) is None:
            h.click(master, sink, 4, fold_row(screen), quiet=0.3, timeout=1.5)
            screen = r.wait_screen(master, sink, lambda s: timeline_row(s) is not None, 5.0) or screen
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
        # 回来之后任务条上的秒数从它派出去那一刻接着算，不从回来那一刻重新数。
        screen = r.wait_screen(master, sink, lambda s: strip_seconds(s) is not None, 5.0) or screen
        seconds_after = strip_seconds(screen) if screen is not None else None
        report["strip_seconds_keep_counting"] = (
            seconds_before is not None and seconds_after is not None and seconds_after >= seconds_before
        )
        report["_strip_seconds"] = [seconds_before, seconds_after]

        # 2. 任务条上那一行（09-26 起子代理行首是空心 `○`，命令才是转轮）
        screen = r.wait_screen(
            master, sink, lambda s: strip_row(s, ROW) is not None and s[strip_row(s, ROW)].lstrip()[:1] == "○", 10.0
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

        # 2b. 方向键：↓ 停在子代理那一行、回车进去；光标留在任务条上、停在子代理那一行
        # （用户 09-26），↑ 挪到「○ 主会话」、回车回来。
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=1.5)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, inside_child, 10.0)
        report["keys_enter_child"] = screen is not None
        r.save("visit-keys-in", screen or r.LAST["screen"] or [])
        if screen is not None:
            h.settle(master, sink, quiet=0.4, timeout=2.0)
            focused = [line for line in h.render(bytes(sink))[-8:] if line.startswith("›")]
            report["keys_focus_stays_on_child"] = bool(focused) and ROW in focused[0]
            os.write(master, b"\x1b[A")
            h.settle(master, sink, quiet=0.3, timeout=1.5)
            os.write(master, b"\r")
            screen = r.wait_screen(master, sink, back_in_parent, 10.0)
            report["keys_return_to_parent"] = screen is not None
            r.save("visit-keys-out", screen or r.LAST["screen"] or [])

        # 3. 它跑完、汇报叫醒主会话的那一轮也收尾之后 `/subagent`：任务条上那一行没了，正文里有
        #    唤醒那一行。
        # 任务条上那一行认行首的 `○` / `›`：唤醒那一行「子代理完成 … · 走查子代理」也带着它的名字。
        def child_in_strip(screen):
            return any(
                ROW in line and WAKE not in line and line.lstrip()[:1] in ("○", "›")
                for line in screen[-8:]
            )

        finished = r.wait_screen(
            master, sink,
            lambda s: WAKE in "\n".join(s) and not child_in_strip(s),
            60.0,
        )
        report["report_wakes_the_parent_and_settles"] = finished is not None
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
        # 只看面板里的行：正文里唤醒那一行（「子代理完成 … · 走查子代理」）本来就带着它的名字。
        panel_rows = []
        if panel is not None:
            top = next(index for index, line in enumerate(panel) if "选择会话" in line)
            panel_rows = [line for line in panel[top:] if line.startswith("┃")]
        report["session_picker_hides_child"] = panel is not None and not any(ROW in line for line in panel_rows)
        r.save("visit-session-picker", panel or r.LAST["screen"] or [])
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.4, timeout=3.0)
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
