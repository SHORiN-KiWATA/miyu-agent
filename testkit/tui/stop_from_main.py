#!/usr/bin/env python3
"""在主会话里停掉后台子代理：它名下的孙代理一起停，停完不再被叫醒（09-26）。

一、主会话派一个后台子代理（它又派两个后台孙代理、自己跑一条慢命令），主会话这一轮说完就闲着。
这时在主会话里按 Ctrl+C（输入框空、没有回复在跑：第三级「停后台任务」）：
- 子代理那一行从任务条上撤掉；
- 库里子代理和两个孙代理都进了终态（原来只停了子代理的镜像任务，孙代理接着跑，跑完还把
  子代理叫醒再跑一轮）；
- 过一会儿子代理会话里不再多出新的一轮（没被孙代理的汇报叫醒）；
- 回到输入框还能接着打字。

二、用户 09-26 的原话照做：派子代理、切进去、Ctrl+C 打断它、再在它会话里让它派一个跑 sleep 的
孙代理、切回主会话按 Ctrl+C。子代理的镜像任务在第一次 Ctrl+C 时就收了，原来主会话的 Ctrl+C 只停
主会话自己名下的任务，一个都停不到；现在整棵树一起停。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-stop-main/home MIYU_TUI_PORT=18995 STUB_PORT=18996 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-stop-main/rt OUT=~/.cache/miyu-stop-main/out \\
      python3 testkit/tui/stop_from_main.py
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
import strip_tree as st  # noqa: E402
import subagent_visit as sv  # noqa: E402
import visit_back_probe as vb  # noqa: E402

PENDING = ("running", "waiting")
# 孙代理的命令跑 10 秒就完：没被一起停的话，它们跑完就会把子代理叫醒再跑一轮，下面的观察窗口
# （14 秒）里看得见。
STUB = dict(st.STUB, STUB_GRANDCHILD_COMMAND="sleep 10")
WATCH_SECONDS = 14.0


def subagent_turns(home):
    """沙箱库里每条子代理会话有几轮（只读打开）。"""
    found = {}
    for db in Path(home).rglob("conversation.db"):
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            for name, count in conn.execute(
                "SELECT s.name, COUNT(t.turn_id) FROM sessions s"
                " LEFT JOIN turns t ON t.session_id = s.session_id"
                " WHERE s.kind = 'subagent' GROUP BY s.session_id"
            ):
                found[name] = count
        finally:
            conn.close()
    return found


def main():
    report = {}
    only = os.environ.get("ONLY", "")
    if only in ("", "fresh"):
        scenario_fresh(report)
    if only in ("", "revisited"):
        scenario_revisited(report)
    return report


def scenario_fresh(report):
    stub, daemon, tui, master, sink = r.start(STUB)
    home = os.environ["MIYU_HOME"]
    try:
        os.write(master, (h.PROMPT + " STUB_SUBBG").encode())
        h.drain_until(master, sink, "STUB_SUBBG", 3.0)
        os.write(master, b"\r")
        ready = r.wait_screen(
            master, sink,
            lambda s: "（+2）" in (st.row_with(s, st.CHILD) or "") and "好的,收到" in "\n".join(s),
            60.0,
        )
        r.save("stop-main-ready", st.now(sink))
        report["child_and_grandchildren_running"] = ready is not None
        if ready is None:
            return report
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        states = st.task_states(home)
        report["_states_before"] = states
        # 子代理会话这会儿有几轮：停完之后不该再多（没被孙代理的汇报叫醒）。
        turns = subagent_turns(home)
        # 第三级：输入框是空的、主会话没有回复在跑，Ctrl+C 停这个会话名下的后台任务。
        os.write(master, b"\x03")
        gone = r.wait_screen(master, sink, lambda s: st.row_with(s, st.CHILD) is None, 20.0)
        r.save("stop-main-stopped", st.now(sink))
        report["child_row_leaves_the_strip"] = gone is not None
        for _ in range(60):
            states = st.task_states(home)
            if states and all(state not in PENDING for state in states.values()):
                break
            time.sleep(0.25)
        report["_states_after"] = states
        report["child_is_settled"] = states.get(st.CHILD) not in (None, *PENDING)
        # 是跟着一起停的（被打断），不是自己跑完的。
        report["grandchildren_are_stopped_with_it"] = all(
            states.get(name) == "interrupted" for name in st.GRANDCHILDREN
        )
        time.sleep(WATCH_SECONDS)
        later = subagent_turns(home)
        report["_turns"] = [turns, later]
        report["child_is_not_woken_again"] = later.get(st.CHILD) == turns.get(st.CHILD)
        report["back_raw_and_typing"] = vb.typing_works(master, sink, "zq")
    finally:
        r.stop(tui, daemon, stub)


# 场景二：子代理只跑一条慢命令，不预先派孙代理；孙代理是切进去之后让它派的。
REVISIT_STUB = dict(
    st.STUB,
    STUB_GRANDCHILDREN="0",
    STUB_SUBAGENT_BG_COMMAND="sleep 120",
    STUB_GRANDCHILD_COMMAND="sleep 120",
)
AGAIN = "走查孙代理又一个"


def scenario_revisited(report):
    stub, daemon, tui, master, sink = r.start(REVISIT_STUB)
    home = os.environ["MIYU_HOME"]
    try:
        os.write(master, (h.PROMPT + " STUB_SUBBG").encode())
        h.drain_until(master, sink, "STUB_SUBBG", 3.0)
        os.write(master, b"\r")
        ready = r.wait_screen(master, sink, lambda s: st.row_with(s, st.CHILD) is not None, 60.0)
        report["r_child_running"] = ready is not None
        if ready is None:
            return
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        # 切进子代理，Ctrl+C 打断它这一轮。
        h.click(master, sink, 6, sv.strip_row(st.now(sink), st.CHILD), quiet=0.3, timeout=1.5)
        inside = r.wait_screen(master, sink, sv.inside_child, 15.0)
        report["r_entered_child"] = inside is not None
        if inside is None:
            return
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        os.write(master, b"\x03")
        for _ in range(60):
            if st.task_states(home).get(st.CHILD) not in PENDING:
                break
            time.sleep(0.25)
        report["_r_after_first_stop"] = st.task_states(home)
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        # 在它会话里让它再派一个跑 sleep 的孙代理。
        os.write(master, "STUB_GC_AGAIN 再派一个".encode())
        h.drain_until(master, sink, "STUB_GC_AGAIN", 3.0)
        os.write(master, b"\r")
        for _ in range(120):
            if st.task_states(home).get(AGAIN) == "running":
                break
            time.sleep(0.25)
        states = st.task_states(home)
        report["_r_states_with_again"] = states
        report["r_grandchild_running"] = states.get(AGAIN) == "running"
        r.wait_screen(master, sink, lambda s: "好的,收到" in "\n".join(s[-20:]), 20.0)
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        # 回主会话。
        h.click(master, sink, 4, sv.strip_row(st.now(sink), sv.UP), quiet=0.3, timeout=1.5)
        back = r.wait_screen(
            master, sink,
            lambda s: sv.back_in_parent(s) and st.row_with(s, st.CHILD) is not None,
            15.0,
        )
        r.save("stop-main-revisited-back", st.now(sink))
        report["r_back_in_main_with_child_row"] = back is not None
        if back is None:
            return
        h.settle(master, sink, quiet=0.8, timeout=5.0)
        # 主会话里 Ctrl+C：第三级停后台任务。
        os.write(master, b"\x03")
        gone = r.wait_screen(master, sink, lambda s: st.row_with(s, st.CHILD) is None, 20.0)
        r.save("stop-main-revisited-stopped", st.now(sink))
        report["r_child_row_leaves_the_strip"] = gone is not None
        for _ in range(60):
            states = st.task_states(home)
            if all(state not in PENDING for state in states.values()):
                break
            time.sleep(0.25)
        report["_r_states_after"] = states
        report["r_grandchild_stopped"] = states.get(AGAIN) == "interrupted"
        report["r_child_settled"] = states.get(st.CHILD) not in (None, *PENDING)
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
