#!/usr/bin/env python3
"""从子代理会话回主会话之后 TUI 还能不能用（09-25 用户实测：切回来之后输入框里只剩
`^[[27u`，打字没反应；切回来那一下任务条消失一瞬间）。

主会话一轮里同时挂着一个前台子代理（sleep 60）和一条后台命令（`STUB_BG`，睡很久），
和用户截图里「子代理 随机小任务」+「命令 1fcc91 · 睡600秒」同一个形状。三种回法各试一次：
点任务条「○ 主会话」、方向键 ↓ 回车、`/back`；每次回来都打几个字看输入框有没有跟上，并逐帧
看任务条有没有空掉过一帧。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-visit-back/home MIYU_TUI_PORT=18971 STUB_PORT=18972 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-visit-back/rt OUT=~/.cache/miyu-visit-back/out \\
      python3 testkit/tui/visit_back_probe.py

`ENTER_MIDTURN=1` 是卡死的那条路（09-26 定位）：主会话**自己这一轮还在说**时点进后台子代理
（回合循环把 raw 交出去），子代理那一轮由事件泵接着看（它不认交接、又开了一把 raw），主会话
这一轮在人不在的时候说完，再点「○ 主会话」回到空闲的主会话——输入循环认领了一个早已失效的
交接，终端停在回显模式，打字只有回显、TUI 收不到。
"""

import json
import os
import sys
import termios
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402
import subagent_visit as sv  # noqa: E402

STUB = {
    "STUB_ASK": "1",
    "STUB_SUBAGENT": "1",
    "STUB_SUBAGENT_COMMAND": "sleep 60; printf 'SUBOUT\\n'",
    # 后台命令一直挂着：默认那条十秒就跑完，任务条空掉是真空，查不出「空了一帧」。
    "STUB_BACKGROUND_COMMAND": "sleep 600",
    "STUB_CHUNK_SLEEP": "0.02",
}
# `IDLE_PARENT=1`：主会话这一轮只派一个后台子代理、一条后台命令就说完（用户截图的形状：主会话
# 闲着，状态行上挂着子代理和「睡600秒」），从后台子代理切回来。
IDLE_STUB = {
    "STUB_SUBAGENT_BG_COMMAND": os.environ.get("CHILD_COMMAND", "sleep 90; printf 'BGOUT\\n'"),
    "STUB_SUBAGENT_BG_ROUNDS": "1",
    "STUB_BACKGROUND_COMMAND": "sleep 600",
    "STUB_CHUNK_SLEEP": "0.02",
}
IDLE = os.environ.get("IDLE_PARENT") == "1"
# 主会话这一轮最后那段话说得慢（几秒），好趁它还在说的时候点进子代理；子代理自己那一轮
# 跑一条慢命令，回来的时候它还在跑。
MIDTURN = os.environ.get("ENTER_MIDTURN") == "1"
MIDTURN_STUB = {
    **IDLE_STUB,
    "STUB_SUBAGENT_BG_COMMAND": "sleep 60; printf 'BGOUT\\n'",
    "STUB_REPLY": "主会话这一轮最后这段话说得很慢，" * 12,
    "STUB_SUBAGENT_REPLY": "子代理说完了。",
    "STUB_CHUNK_SLEEP": "0.05",
}
JOB_ROW = "走查后台任务二"
CHILD_ROW = "走查后台子代理" if (IDLE or MIDTURN) else sv.ROW
END = b"\x1b[?2026l"


def now(sink):
    return h.render(bytes(sink))


def strip_rows(screen):
    """屏幕最底下那几行里的任务条行（子代理、命令）。"""
    return [line for line in screen[-8:] if "子代理 走查" in line or JOB_ROW in line or sv.UP in line]


def child_row(screen):
    for index in range(len(screen) - 1, -1, -1):
        if CHILD_ROW in screen[index] and f"·{CHILD_ROW}" not in screen[index]:
            return index
    return None


def frames_since(sink, mark):
    """`mark` 之后每一个同步块画完时的屏幕。"""
    frames, seen = [], mark
    while True:
        at = bytes(sink).find(END, seen)
        if at < 0:
            return frames
        seen = at + len(END)
        frames.append(h.render(bytes(sink[:seen])))


def raw_mode(master):
    """终端这会儿在不在 raw 里：PTY 主端上的 TCGETS 读的是从端（TUI 那一端）的标志。

    「打几个字看得见」分不出真输入和回显——cooked 模式下终端自己就把字回显出来了，
    卡死那次用户看到的 `^[[27u` 就是回显。"""
    lflag = termios.tcgetattr(master)[3]
    return not (lflag & (termios.ICANON | termios.ECHO))


def typing_works(master, sink, text):
    """打几个字：终端在 raw 里、输入框里看得到它，才算 TUI 还活着。打完删掉。"""
    raw = raw_mode(master)
    os.write(master, text.encode())
    ok = h.drain_until(master, sink, text, 4.0)
    os.write(master, b"\x7f" * len(text))
    h.settle(master, sink, quiet=0.4, timeout=3.0)
    return raw and bool(ok) and not any("^[[" in line for line in now(sink))


def enter_child(master, sink):
    screen = now(sink)
    row = child_row(screen)
    if row is None:
        return None
    h.click(master, sink, 6, row, quiet=0.3, timeout=1.5)
    return r.wait_screen(master, sink, sv.inside_child, 10.0)


def midturn(report, master, sink):
    """主会话自己这一轮还在说时点进子代理，等主会话说完，再点「○ 主会话」回来。"""
    os.write(master, (h.PROMPT + " STUB_SUBBG STUB_BG").encode())
    h.drain_until(master, sink, "STUB_BG", 3.0)
    os.write(master, b"\r")
    ready = r.wait_screen(master, sink, lambda s: child_row(s) is not None, 40.0)
    report["child_on_the_strip_while_parent_talks"] = ready is not None
    if ready is None:
        r.save("vb-midturn-no-strip", now(sink))
        return
    inside = enter_child(master, sink)
    report["entered_child_midturn"] = inside is not None
    if inside is None:
        r.save("vb-midturn-no-child", now(sink))
        return
    # 主会话那段慢话说完（约 12 秒），子代理的慢命令还在跑。
    h.drain(master, float(os.environ.get("MAIN_DONE_WAIT", "16")), sink)
    h.settle(master, sink, quiet=1.0, timeout=5.0)
    r.save("vb-midturn-child", now(sink))
    report["child_still_running"] = sv.inside_child(now(sink))
    row = sv.strip_row(now(sink), sv.UP)
    h.click(master, sink, 4, row, quiet=0.2, timeout=1.0)
    back = r.wait_screen(master, sink, sv.back_in_parent, 10.0)
    report["midturn_back_in_parent"] = back is not None
    h.settle(master, sink, quiet=1.0, timeout=5.0)
    r.save("vb-midturn-back", now(sink))
    report["midturn_raw_after_back"] = raw_mode(master)
    report["midturn_typing_works_after_back"] = typing_works(master, sink, "zq")
    r.save("vb-midturn-typed", now(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(MIDTURN_STUB if MIDTURN else IDLE_STUB if IDLE else STUB)
    try:
        if MIDTURN:
            midturn(report, master, sink)
            return report
        prompt = h.PROMPT + (" STUB_SUBBG STUB_BG" if IDLE else " STUB_BG")
        os.write(master, prompt.encode())
        h.drain_until(master, sink, "STUB_BG", 3.0)
        os.write(master, b"\r")
        ready = r.wait_screen(
            master, sink,
            lambda s: child_row(s) is not None and any(JOB_ROW in l for l in s[-8:]),
            40.0,
        )
        report["child_and_job_on_the_strip"] = ready is not None
        if ready is None:
            r.save("vb-no-strip", now(sink))
            return report
        h.settle(master, sink, quiet=1.0, timeout=4.0)
        # Ctrl+C 放最后：09-26 起它真的把子代理停了，停了之后任务条上就没有它，别的回法进不去。
        ways = os.environ.get("WAYS", "click,keys,command,interrupt").split(",")
        for way in ways:
            inside = enter_child(master, sink)
            report[f"{way}_entered_child"] = inside is not None
            if inside is None:
                r.save(f"vb-{way}-no-child", now(sink))
                continue
            h.settle(master, sink, quiet=0.6, timeout=4.0)
            # `CHILD_DONE_WAIT`：在子会话里等它这一轮跑完再回（子会话停在空闲输入循环里）。
            if os.environ.get("CHILD_DONE_WAIT"):
                h.drain(master, float(os.environ["CHILD_DONE_WAIT"]), sink)
                h.settle(master, sink, quiet=1.0, timeout=5.0)
                r.save(f"vb-{way}-child-done", now(sink))
            if way == "interrupt":
                # 用户 09-25：在子会话里 Ctrl+C 停了子代理，再切回来。
                os.write(master, b"\x03")
                h.settle(master, sink, quiet=1.0, timeout=8.0)
                r.save("vb-interrupt-child", now(sink))
            mark = len(sink)
            if way in ("click", "interrupt"):
                row = sv.strip_row(now(sink), sv.UP)
                h.click(master, sink, 4, row, quiet=0.2, timeout=1.0)
            elif way == "keys":
                os.write(master, b"\x1b[B")
                h.settle(master, sink, quiet=0.3, timeout=1.5)
                os.write(master, b"\r")
            else:
                os.write(master, b"/back")
                h.drain_until(master, sink, "/back", 3.0)
                os.write(master, b"\r")
            back = r.wait_screen(master, sink, sv.back_in_parent, 10.0)
            report[f"{way}_back_in_parent"] = back is not None
            h.settle(master, sink, quiet=1.0, timeout=5.0)
            r.save(f"vb-{way}-back", now(sink))
            # 回来之后任务条有没有空掉过一帧（用户：底下的状态行会消失一瞬间）。
            frames = [f for f in frames_since(sink, mark) if not sv.inside_child(f)]
            report[f"{way}_strip_never_blank"] = all(strip_rows(f) for f in frames) if frames else None
            report[f"_{way}_frames"] = len(frames)
            report[f"{way}_raw_after_back"] = raw_mode(master)
            report[f"{way}_typing_works_after_back"] = typing_works(master, sink, "zq")
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
