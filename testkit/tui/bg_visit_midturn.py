#!/usr/bin/env python3
"""主回合还在说话时点进后台子代理的会话：子代理那边要一直往下走（会话项目第 3 段）。

`bg_panel_midturn.py` 的新版。用户 09-24 验收时记下的老问题：她还在回答时点开后台子代理
的浮层，浮层一直停在「提示词」那一行不动，要等她这一轮说完才正常。子代理 09-18 起是一条
会话，点状态行上那一行不再开浮层，而是切进它的会话、看它自己的回合。判据照旧：主回合还
没说完的时候，子代理那边跑命令的那一步要出现。

    cargo build
    MIYU_HOME=~/.cache/miyu-bg-visit/home MIYU_TUI_PORT=18681 STUB_PORT=18682 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-bg-visit/rt OUT=~/.cache/miyu-bg-visit/out \\
      python3 testkit/tui/bg_visit_midturn.py [--thinking | --sub-thinking]

步骤：主回合派一条后台子代理（它跑两轮命令，每轮先想一句）→ 主回合慢慢吐一大段 → 主回合
说完之前点状态行上那条子代理 → 在子会话里，每秒看一眼：跑命令那一步出现、同时库里主回合
还在跑，才算跟上；一直看到主回合在库里跑完，子会话的画面上不许出现主回合的字、子代理自己
的回复要出来 → `/back` → 主回合这一轮接着画完。

子代理的回复另给一段（桩的 `STUB_SUBAGENT_REPLY`）：桩默认两边吐同一段，子代理说的话会被
认成主回合串进来（09-25 老走查 `bg_panel_midturn.py` 的「turn_running=false」就是这么来的）。
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

TITLE = "走查后台子代理"
TASK = "来自主会话的任务"
UP = "↑ 主会话"
BADGE = "子代理 ↳1"
# 带上桩认主线的那句（`STUB_MAIN_MARK`，默认「走查一句」）：派完子代理之后主线的消息里也有
# 子代理的任务（在工具调用的参数里），没有这句，桩会把主回合也当成子代理，让它跑命令、回
# 子代理那段话。
ASK = "走查一句 STUB_SUBBG 开条后台子代理"
REPLY_END = "主回合说到这里结束"
# 子代理自己的回复另给一段：桩默认两边吐同一段，分不清画面上是谁说的。
SUB_REPLY = "子代理这边说完了。"
STUB = {
    "STUB_SUBAGENT_REPLY": SUB_REPLY,
    "STUB_SUBBG": "1",
    "STUB_REASONING": "1",
    "STUB_SUBAGENT_BG_COMMAND": "sleep 2; printf 'BGOUT走查输出\\n'",
    "STUB_SUBAGENT_BG_ROUNDS": "2",
    # 主回合的回复慢慢吐半分钟：点进子会话、看它走，都在这段时间里。
    "STUB_REPLY": "主回合还在慢慢说。\n" * 250 + REPLY_END,
    "STUB_CHUNK_CHARS": "6",
    "STUB_CHUNK_SLEEP": "0.08",
}
# `--thinking`：主回合在**想**（一大段不空行的思考）而不是在说。
THINKING = "--thinking" in sys.argv
if THINKING:
    STUB["STUB_REASONING_TEXT"] = "主回合先把这件事想清楚再说，" * 200
    STUB["STUB_REPLY"] = REPLY_END
# `--sub-thinking`：子代理一上来就想一大段（不空行），主回合同时也在想。
SUB_THINKING = "--sub-thinking" in sys.argv
THOUGHT_MARK = "先把这件事想清楚"
if SUB_THINKING:
    STUB["STUB_SUBAGENT_BG_ROUNDS"] = "0"
    STUB["STUB_REASONING_TEXT"] = f"{THOUGHT_MARK}再说，" * 200
    STUB["STUB_REPLY"] = REPLY_END


def strip_row(screen, text):
    """任务条上那一行（`⠸ 子代理 走查后台子代理` / `↑ 主会话 …`），在屏幕最底下那一截；
    正文时间线上那一步写成 `子代理·走查后台子代理`，不算。"""
    for index in range(len(screen) - 1, -1, -1):
        line = screen[index]
        if text in line and f"·{text}" not in line:
            return index
    return None


def inside_child(screen):
    joined = "\n".join(screen)
    return TASK in joined and BADGE in joined and strip_row(screen, UP) is not None


def back_in_parent(screen):
    """回到主会话：没有「子代理 ↳1」、任务条上没有「↑ 主会话」，画面上是主回合的东西（默认档
    的回复有 250 行，开场白早滚出屏幕了，只认它会漏判）。"""
    joined = "\n".join(screen)
    mine = ASK in joined or "主回合" in joined
    return mine and BADGE not in joined and strip_row(screen, UP) is None


def child_moved(screen):
    """子会话里出现了跑命令那一步（或者 `--sub-thinking` 时它的思考）。"""
    joined = "\n".join(screen)
    if SUB_THINKING:
        return THOUGHT_MARK in joined
    return any(mark in joined for mark in ("跑个命令", "运行命令", "BGOUT"))


def parent_text_in_child(screen):
    """人还在子会话里（footer 带「子代理 ↳1」），画面上却有主回合的字。

    主回合的思考和子代理的思考是桩里同一段话，分不出来；认回复：默认档是一行行「主回合还在
    慢慢说」，三档都以「主回合说到这里结束」收尾。子代理的回复是另一段（`SUB_REPLY`）。"""
    joined = "\n".join(screen)
    if BADGE not in joined:
        return False
    return REPLY_END in joined or "主回合还在慢慢说" in joined


def parent_running():
    """库里主会话最新那一轮还在跑：屏幕上看不见主会话，只能问库。"""
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    if not candidates:
        return None
    connection = sqlite3.connect(f"file:{candidates[0]}?mode=ro", uri=True)
    try:
        row = connection.execute(
            "SELECT t.status FROM turns t JOIN sessions s ON s.session_id = t.session_id "
            "WHERE s.kind = 'user' ORDER BY t.seq DESC LIMIT 1"
        ).fetchone()
    finally:
        connection.close()
    return row is not None and row[0] == "running"


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, ASK.encode())
        h.drain_until(master, sink, "后台子代理", 3.0)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lambda s: strip_row(s, TITLE) is not None, 40.0)
        report["strip_lists_the_background_child"] = screen is not None
        if screen is None:
            r.save("bg-visit-no-strip", r.LAST["screen"] or [])
            return report
        report["parent_running_when_clicked"] = parent_running() is True
        h.click(master, sink, 4, strip_row(screen, TITLE), quiet=0.3, timeout=1.5)
        screen = r.wait_screen(master, sink, inside_child, 10.0)
        report["click_enters_the_child"] = screen is not None
        r.save("bg-visit-child", screen or r.LAST["screen"] or [])
        if screen is None:
            return report
        # 主回合说完之前，每秒看一眼子会话：跑命令那一步出现、同时主回合还在跑，才算跟上。
        # 一直看到主回合在库里跑完、再多看几秒：这段时间人还在子会话里，主回合那一轮在 daemon
        # 里照跑，它的字一个都不许画进来。
        samples = []
        # 进门那一屏就算数：子代理常常在点进去之后一两秒就跑完，步骤收成一行「Ran …」，
        # 第一拍（1 秒后）就看不见了。
        followed_at = 0.0 if child_moved(screen) and parent_running() else None
        leaked_at = None
        parent_done_at = None
        entered = time.time()
        while time.time() - entered < 90.0:
            h.drain(master, 1.0, sink)
            screen = h.render(bytes(sink))
            running = parent_running()
            moved = child_moved(screen)
            leaked = parent_text_in_child(screen)
            now = round(time.time() - entered, 1)
            samples.append({"t": now, "parent_running": running, "moved": moved, "leaked": leaked})
            if moved and running and followed_at is None:
                followed_at = now
                r.save("bg-visit-followed", screen)
            if leaked and leaked_at is None:
                leaked_at = now
                r.save("bg-visit-leaked", screen)
            if not running and parent_done_at is None:
                parent_done_at = now
            if parent_done_at is not None and now - parent_done_at >= 4.0:
                break
        r.save("bg-visit-watched", h.render(bytes(sink)))
        report["child_moves_while_the_parent_runs"] = followed_at is not None
        report["child_reply_shows_in_the_child"] = SUB_REPLY in "".join(
            line.strip() for line in h.render(bytes(sink))
        )
        report["parent_text_stays_out_of_the_child"] = leaked_at is None and parent_done_at is not None
        report["_followed_at_s"] = followed_at
        report["_leaked_at_s"] = leaked_at
        report["_parent_done_at_s"] = parent_done_at
        report["_samples"] = samples
        os.write(master, b"/back")
        h.drain_until(master, sink, "/back", 3.0)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, back_in_parent, 10.0)
        report["back_returns_to_the_parent"] = screen is not None
        finished = r.wait_screen(master, sink, lambda s: REPLY_END in "".join(line.strip() for line in s), 90.0)
        report["parent_turn_finishes_on_screen"] = finished is not None
        r.save("bg-visit-parent-done", finished or r.LAST["screen"] or [])
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
