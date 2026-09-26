#!/usr/bin/env python3
"""任务条的树（09-26 照 Claude Code 改，用户拍板）。

主会话派一个后台子代理（它又派两个后台孙代理、自己跑一条慢命令）和一条后台命令：
- 主会话的任务条第一层只有子代理那一行（空心 `○`，名下两个孙代理收成「（+2）」）和那条命令
  （转轮），孙代理不在第一层；有子代理在跑，`● 主会话` 那一行就在最上面（用户 09-26：不用
  等切进子代理才出现）；子代理那一行标题后面窥视它此刻在干什么，右边先报词元再报时长
  （09-26：子代理的 token 计数没了）；
- 点进子代理：`○ 主会话` 钉在最上面，子代理这一行实心 `●`，两个孙代理用 `├`/`└` 挂在它下面，
  主会话自己的命令还在第一层；
- 方向键停到孙代理那一行回车切进去：还是同一棵树（主会话、子代理、两个孙代理），只是实心圆挪到
  孙代理上，光标还停在它那一行（用户 09-26）；↑ 回车回子代理，光标跟着停在子代理那一行；
- 在子代理里按 Ctrl+C：它这一轮连同两个孙代理一起停（用户 09-26：原来孙代理没停下），孙代理
  从任务条上撤掉、库里记成被打断；主会话的命令不受影响；
- 回主会话，还能接着打字（终端在 raw 里）；
- footer 上的 Σ 在停下那一下不往回掉（09-26：切进一条回合正跑着的会话时本回合算了两遍，
  跟着看的那一阵虚高，一停又掉回去）。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-strip-tree/home MIYU_TUI_PORT=18975 STUB_PORT=18976 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-strip-tree/rt OUT=~/.cache/miyu-strip-tree/out \\
      python3 testkit/tui/strip_tree.py
"""

import json
import os
import re
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
# 行尾那一栏：先词元（`≈1.2K`、`450`）再时长（`12s`、`1m 05s`）。
TOKENS_RE = re.compile(r"(?:≈)?\d[\d.]*[KM]?\s{2}\d+[hms]")


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


def focused(screen):
    """方向键停着的那一行（行首 `›`）。"""
    return next((line for line in strip(screen) if line.startswith("›")), None)


def focus_on(master, sink, text, key, tries=8):
    """按 `key` 一格格挪，直到光标停在带 `text` 的那一行。"""
    for _ in range(tries):
        row = focused(now(sink))
        if row is not None and text in row:
            return True
        os.write(master, key)
        h.settle(master, sink, quiet=0.3, timeout=2.0)
    row = focused(now(sink))
    return row is not None and text in row


def current_is(text):
    return lambda s: (row_with(s, text) or "").lstrip("› ").lstrip("├└│ ").startswith("●")


def sigma(screen):
    """footer 上的 Σ（`Σ900`、`Σ1.3k`），读不到是 None。"""
    for line in reversed(screen):
        found = re.search(r"Σ(\d+(?:\.\d+)?)([kM]?)", line)
        if found:
            scale = {"": 1, "k": 1_000, "M": 1_000_000}[found.group(2)]
            return float(found.group(1)) * scale
    return None


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
        report["_main_rows"] = rows
        report["main_root_row_on_top"] = bool(rows) and rows[0].lstrip("› ").startswith("● 主会话")
        # 窥视要等子代理那一轮真跑起来（派孙代理、跑慢命令），多等一会儿。
        peeked = r.wait_screen(master, sink, lambda s: f"{CHILD} · " in (row_with(s, CHILD) or ""), 20.0)
        child_row = row_with(peeked or now(sink), CHILD) or ""
        report["_main_child_row"] = child_row
        report["main_child_row_has_peek"] = peeked is not None
        report["main_child_row_has_tokens"] = bool(TOKENS_RE.search(child_row))
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
        # 孙代理那两行右边也要有词元（用户 09-26 截图：孙代理的 token 计数没了）。
        counted = r.wait_screen(
            master, sink,
            lambda s: all(TOKENS_RE.search(row_with(s, name) or "") for name in GRANDCHILDREN),
            20.0,
        )
        report["_grandchild_rows"] = [row_with(counted or now(sink), name) for name in GRANDCHILDREN]
        report["inside_grandchild_rows_have_tokens"] = counted is not None

        # 方向键进任务条，停到孙代理一那一行，回车切进去。
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=2.0)
        report["keys_reach_grandchild_row"] = focus_on(master, sink, GRANDCHILDREN[0], b"\x1b[B")
        os.write(master, b"\r")
        deeper = r.wait_screen(master, sink, current_is(GRANDCHILDREN[0]), 15.0)
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        deeper = now(sink)
        r.save("tree-grandchild", deeper)
        report["grandchild_is_current"] = current_is(GRANDCHILDREN[0])(deeper)
        rows = strip(deeper)
        report["grandchild_keeps_the_same_tree"] = (
            bool(rows)
            and rows[0].lstrip("› ").startswith("○ 主会话")
            and (row_with(deeper, CHILD) or "").lstrip("› ").startswith("○")
            and all(row_with(deeper, name) is not None for name in GRANDCHILDREN)
        )
        report["focus_stays_on_the_grandchild"] = GRANDCHILDREN[0] in (focused(deeper) or "")
        # ↑ 回到子代理那一行，回车切回去：光标跟着停在子代理那一行。
        report["keys_reach_child_row"] = focus_on(master, sink, CHILD, b"\x1b[A")
        os.write(master, b"\r")
        back_up = r.wait_screen(master, sink, current_is(CHILD), 15.0)
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        r.save("tree-child-again", now(sink))
        report["back_to_child_by_keys"] = back_up is not None
        report["focus_follows_back_to_child"] = CHILD in (focused(now(sink)) or "")
        # 光标还在任务条上：Esc 回输入框，接下来的 Ctrl+C 交给回合。
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.5, timeout=3.0)

        before = sigma(now(sink))
        report["_sigma_before_stop"] = before
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
        h.settle(master, sink, quiet=2.0, timeout=6.0)
        after = sigma(now(sink))
        report["_sigma_after_stop"] = after
        report["sigma_does_not_drop_on_stop"] = (
            before is not None and after is not None and after >= before
        )

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
