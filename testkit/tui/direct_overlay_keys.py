#!/usr/bin/env python3
"""直连模式（`MIYU_DIRECT=1`）回合进行中的浮层与状态行，要和远端那条路一样。

09-24 顺手发现：直连回合的循环里按键直接进输入框、不经浮层，状态行也一次都不刷。
于是回合进行中：浮层上按 Esc 关不掉（连按两下反倒把这一轮打断了）、按 x 停不了任务，
这一轮里新开的后台任务要等说完才上状态行。远端那条路（`remote/one_shot.rs`）早就是
先给浮层、转轮 tick 里刷状态行。

- 子代理那一步：回合进行中点开它的浮层，按 Esc 只关浮层，这一轮照常说完。
- 后台任务：回合进行中就上状态行；点开按 x，任务当场停（直连模式没有 daemon 可问，
  看任务日志停没停着长），这一轮照常说完。

「这一轮还在说」看原始输出流里有没有收尾那句，不看屏幕——浮层会把正文盖住。

    cargo build
    python3 testkit/tui/direct_overlay_keys.py
"""

import json
import os
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

REPLY_END = "回复到这里结束"
SLOW_REPLY = {
    # 回复慢慢吐二十来秒：开浮层、按键都在这一轮里。
    "STUB_REPLY": "这一轮还在慢慢说。\n" * 120 + REPLY_END,
    "STUB_CHUNK_CHARS": "8",
    "STUB_CHUNK_SLEEP": "0.12",
}
JOB_TITLE = "走查后台任务"
STRIP = re.compile(r"命令 [0-9a-f]{4,} · " + JOB_TITLE)


def said_it_all(sink):
    return REPLY_END in bytes(sink).decode("utf-8", "replace")


def click(master, row, column=5):
    """按下松开一次发出去：回合正吐字时画面静不下来，分开发会被当成拖动。"""
    os.write(master, f"\x1b[<0;{column};{row + 1}M\x1b[<0;{column};{row + 1}m".encode())


def job_log_size():
    """沙箱里最新那份任务日志的大小（直连模式任务跑在 TUI 自己的进程里）。"""
    logs = [path for path in h.HOME.rglob("*.log") if "jobs" in path.parts]
    logs.sort(key=lambda path: path.stat().st_mtime)
    return logs[-1].stat().st_size if logs else None


def scenario_esc_closes_the_overlay(report):
    stub, daemon, tui, master, sink = r.start(dict(SLOW_REPLY, STUB_SUBAGENT="1"), direct=True)
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        # 子代理跑完、主回合开始慢慢说之后，点子代理那一步。
        h.drain_until(master, sink, "这一轮还在慢慢说", 40.0)
        screen = h.render(bytes(sink))
        row = next((i for i, line in enumerate(screen) if "走查子代理" in line), None)
        report["esc_subagent_step_seen"] = row is not None
        if row is None:
            return
        click(master, row)
        report["esc_overlay_opened"] = h.drain_until(master, sink, "Esc 关闭", 5.0)
        report["esc_turn_running_when_opened"] = not said_it_all(sink)
        os.write(master, b"\x1b")
        h.drain(master, 1.0, sink)
        screen = h.render(bytes(sink))
        r.save("direct-esc", screen)
        report["esc_closes_the_overlay"] = not any("Esc 关闭" in line for line in screen)
        report["esc_turn_finishes"] = h.drain_until(master, sink, REPLY_END, 60.0)
        report["esc_turn_not_interrupted"] = not any(
            "中断" in line for line in h.render(bytes(sink))
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_x_stops_the_job(report):
    stub_env = dict(
        SLOW_REPLY,
        STUB_BACKGROUND="1",
        STUB_BACKGROUND_COMMAND='for i in $(seq 1 200); do echo "后台第 $i 行"; sleep 0.3; done',
    )
    stub, daemon, tui, master, sink = r.start(stub_env, direct=True)
    try:
        os.write(master, "派一条后台任务再慢慢说".encode())
        h.drain_until(master, sink, "慢慢说", 3.0)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lambda s: any(STRIP.search(l) for l in s), 30.0)
        report["x_strip_shows_while_the_turn_runs"] = screen is not None and not said_it_all(sink)
        if screen is None:
            return
        strip = max(i for i, line in enumerate(screen) if STRIP.search(line))
        click(master, strip)
        report["x_overlay_opened"] = h.drain_until(master, sink, "Esc 关闭", 5.0)
        os.write(master, b"x")
        h.drain(master, 1.0, sink)
        size = job_log_size()
        h.drain(master, 3.0, sink)
        grown = job_log_size()
        report["_x_log_size"] = [size, grown]
        report["x_turn_still_running"] = not said_it_all(sink)
        report["x_stops_the_job"] = size is not None and grown == size
        r.save("direct-x", h.render(bytes(sink)))
        report["x_turn_finishes"] = h.drain_until(master, sink, REPLY_END, 60.0)
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    saved_env = h.ENV
    h.ENV = dict(h.ENV, MIYU_DIRECT="1")
    try:
        scenario_esc_closes_the_overlay(report)
        scenario_x_stops_the_job(report)
    finally:
        h.ENV = saved_env
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
