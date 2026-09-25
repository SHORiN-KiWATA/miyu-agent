#!/usr/bin/env python3
"""任务条的树（09-26 照 Claude Code 改，用户拍板）。

主会话派一个后台子代理（它又派两个后台孙代理、自己跑一条慢命令）和一条后台命令：
- 主会话的任务条第一层只有子代理那一行（空心 `○`，名下两个孙代理收成「（+2）」）和那条命令
  （转轮），孙代理不在第一层；
- 点进子代理：`○ 主会话` 钉在最上面，子代理这一行实心 `●`，两个孙代理用 `├`/`└` 挂在它下面，
  主会话自己的命令还在第一层；
- 在子代理里按 Ctrl+C：它这一轮连同两个孙代理一起停（用户 09-26：原来孙代理没停下），孙代理
  从任务条上撤掉、库里记成被打断；主会话的命令不受影响；
- 回主会话，还能接着打字（终端在 raw 里）。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-strip-tree/home MIYU_TUI_PORT=18975 STUB_PORT=18976 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-strip-tree/rt OUT=~/.cache/miyu-strip-tree/out \\
      python3 testkit/tui/strip_tree.py
"""

import json
import os
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402
import subagent_visit as sv  # noqa: E402
import visit_back_probe as vb  # noqa: E402

STUB = {
    "STUB_SUBAGENT_BG_COMMAND": "sleep 120; printf 'BGOUT\\n'",
    "STUB_SUBAGENT_BG_ROUNDS": "1",
    "STUB_GRANDCHILDREN": "2",
    "STUB_GRANDCHILD_COMMAND": "sleep 120",
    "STUB_BACKGROUND_COMMAND": "sleep 600",
    "STUB_CHUNK_SLEEP": "0.02",
}
CHILD = "走查后台子代理"
GRANDCHILDREN = ("走查孙代理1", "走查孙代理2")
COMMAND = "走查后台任务二"


def now(sink):
    return h.render(bytes(sink))


def strip(screen):
    """屏幕最底下那几行里的任务条行。"""
    return [line.rstrip() for line in screen[-9:] if any(
        mark in line for mark in ("○ ", "● ", "├ ", "└ ", COMMAND)
    )]


def row_with(screen, text):
    return next((line for line in strip(screen) if text in line), None)


def main_view_ready(screen):
    row = row_with(screen, CHILD)
    return row is not None and "（+2）" in row and row_with(screen, COMMAND) is not None


def tree_ready(screen):
    return (
        sv.inside_child(screen)
        and all(row_with(screen, name) is not None for name in GRANDCHILDREN)
    )


def task_states(home):
    """沙箱家里的会话库，只读打开：孙代理这会儿的任务状态。"""
    found = {}
    for db in Path(home).rglob("conversation.db"):
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            for name, state in conn.execute(
                "SELECT name, task_state FROM sessions WHERE kind = 'subagent'"
            ):
                found[name] = state
        finally:
            conn.close()
    return found


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, (h.PROMPT + " STUB_SUBBG STUB_BG").encode())
        h.drain_until(master, sink, "STUB_BG", 3.0)
        os.write(master, b"\r")
        ready = r.wait_screen(master, sink, main_view_ready, 60.0)
        r.save("tree-main", now(sink))
        report["main_folds_grandchildren_into_plus_two"] = ready is not None
        if ready is None:
            return report
        rows = strip(ready)
        report["main_lists_no_grandchild_rows"] = not any(
            name in line for line in rows for name in GRANDCHILDREN
        )
        report["main_child_row_is_hollow"] = row_with(ready, CHILD).lstrip().startswith("○")
        command_row = row_with(ready, COMMAND).lstrip()
        report["main_command_keeps_the_spinner"] = command_row[:1] not in ("○", "●")

        screen = now(sink)
        h.click(master, sink, 6, sv.strip_row(screen, CHILD), quiet=0.3, timeout=1.5)
        inside = r.wait_screen(master, sink, tree_ready, 15.0)
        r.save("tree-child", now(sink))
        report["inside_grandchildren_hang_under_the_child"] = inside is not None
        if inside is None:
            return report
        rows = strip(inside)
        report["inside_way_back_is_first"] = rows[0].lstrip().startswith("○ 主会话")
        current = row_with(inside, CHILD)
        report["inside_current_is_filled"] = current.lstrip().startswith("●") and "（+" not in current
        first, second = (row_with(inside, name).lstrip() for name in GRANDCHILDREN)
        report["inside_twigs"] = first.startswith("├ ○") and second.startswith("└ ○")
        report["inside_main_command_on_level_one"] = row_with(inside, COMMAND) is not None

        # 子代理这一轮还在跑（慢命令），Ctrl+C 停它，连两个孙代理一起。
        os.write(master, b"\x03")
        stopped = r.wait_screen(
            master, sink,
            lambda s: all(row_with(s, name) is None for name in GRANDCHILDREN),
            15.0,
        )
        r.save("tree-child-stopped", now(sink))
        report["ctrl_c_takes_the_grandchildren_off_the_strip"] = stopped is not None
        states = {}
        for _ in range(40):
            states = task_states(os.environ["MIYU_HOME"])
            if all(states.get(name) == "interrupted" for name in GRANDCHILDREN):
                break
            time.sleep(0.25)
        report["_task_states"] = states
        report["ctrl_c_interrupts_the_grandchildren"] = all(
            states.get(name) == "interrupted" for name in GRANDCHILDREN
        )
        report["ctrl_c_leaves_the_main_command"] = row_with(now(sink), COMMAND) is not None

        h.settle(master, sink, quiet=0.6, timeout=4.0)
        row = sv.strip_row(now(sink), sv.UP)
        h.click(master, sink, 4, row, quiet=0.2, timeout=1.0)
        back = r.wait_screen(master, sink, sv.back_in_parent, 10.0)
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        r.save("tree-back", now(sink))
        report["back_in_main"] = back is not None
        report["back_raw_and_typing"] = vb.typing_works(master, sink, "zq")
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    checks = {k: v for k, v in report.items() if not k.startswith("_") and v is not None}
    passed = sum(1 for v in checks.values() if v)
    print(f"{passed}/{len(checks)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if checks and passed == len(checks) else 1)
