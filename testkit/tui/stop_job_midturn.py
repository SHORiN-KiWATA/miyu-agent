#!/usr/bin/env python3
"""AI 还在回复时，点状态行开后台任务的浮层、按 x：任务当场停，这一轮照常说完。

用户 09-24：「后台命令我没法在打开浮层后 x 关掉了」——点的是底部状态行，按的时候
AI 在跑。浮层只记下要停哪个任务、关掉自己，真去停原来只有空闲的输入循环会做，回合
循环不看，任务要等这一轮说完才停。

判据看 daemon 的任务表（`jobs_overview`），不看状态行：状态行按 x 之后会先被压住，
任务还在跑也可能暂时看不见。

    cargo build
    python3 testkit/tui/stop_job_midturn.py
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
import latency_probe as lp  # noqa: E402

TITLE = "走查后台任务"
REPLY_END = "回复到这里结束"
STUB = {
    "STUB_BACKGROUND": "1",
    "STUB_BACKGROUND_COMMAND": 'for i in $(seq 1 120); do echo "后台第 $i 行"; sleep 1; done',
    # 回复慢慢吐十几二十秒：按 x 的时候这一轮还没说完。
    "STUB_REPLY": "这一轮还在慢慢说。\n" * 120 + REPLY_END,
    "STUB_CHUNK_CHARS": "8",
    "STUB_CHUNK_SLEEP": "0.12",
}


def running_jobs():
    """daemon 任务表里还在跑的、标题是走查那条的任务。"""
    _, frame = lp.ipc({"command": "jobs_overview"})
    jobs = frame.get("data", {}).get("jobs", []) if isinstance(frame, dict) else []
    return [job for job in jobs if job.get("running") and TITLE in json.dumps(job, ensure_ascii=False)]


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, "派一条后台任务再慢慢说".encode())
        h.drain_until(master, sink, "慢慢说", 3.0)
        os.write(master, b"\r")
        # 认**状态行**自己的样子（`命令 <任务号> · 走查后台任务`）：正文时间线上那一步
        #（「运行命令 · 走查后台任务」）先写出这几个字，而状态行要等下一次轮询（约 1 秒）
        # 才冒出来——只认标题的话会去点时间线那一步。
        strip_line = re.compile(r"命令 [0-9a-f]{4,} · " + TITLE)
        screen = r.wait_screen(master, sink, lambda s: any(strip_line.search(l) for l in s), 30.0)
        report["strip_visible"] = screen is not None
        screen = screen or h.render(bytes(sink))
        strip = max((i for i, line in enumerate(screen) if strip_line.search(line)), default=None)
        report["job_running_before"] = bool(running_jobs())
        report["turn_running_when_opened"] = not any(REPLY_END in line for line in screen)
        if strip is None:
            return report
        # 按下和松开一次发出去：回合正吐字时画面静不下来，`h.click` 在两者之间等
        # 「安静」要等满超时，按住两秒就被当成拖动了（09-19 的老坑）。
        os.write(master, f"\x1b[<0;5;{strip + 1}M\x1b[<0;5;{strip + 1}m".encode())
        report["overlay_opened"] = h.drain_until(master, sink, "Esc 关闭", 10.0)
        r.save("stop-job-midturn-opened", h.render(bytes(sink)))
        pressed = time.time()
        os.write(master, b"x")
        # 停下来要多久：daemon 的任务表里它不再是「在跑」。
        stopped_ms = None
        while time.time() - pressed < 8.0:
            h.drain(master, 0.1, sink)
            if not running_jobs():
                stopped_ms = round((time.time() - pressed) * 1000)
                break
        screen = h.render(bytes(sink))
        r.save("stop-job-midturn", screen)
        report["stopped_ms"] = stopped_ms
        report["job_stopped_mid_turn"] = stopped_ms is not None
        report["turn_still_running_after_x"] = not any(REPLY_END in line for line in screen)
        report["overlay_closed"] = not any("Esc 关闭" in line for line in screen)
        # 这一轮照常说完，不被停任务那一下打断。
        report["turn_finishes"] = h.drain_until(master, sink, REPLY_END, 60.0)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
