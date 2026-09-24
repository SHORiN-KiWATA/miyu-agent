#!/usr/bin/env python3
"""「指针停在边缘就把轮询放快到 20ms」会不会把 CPU 吃上去。

用户 09-22 问的。设计上那一档是**瞬态**的：判据是「指针停在最外一圈 **且** 还有
提亮亮着」，而熄灭那一步会把记录清空，于是条件立刻不再成立——最多跑 100ms
就回落到常态的 80ms。这份走查用 `/proc/<pid>/stat` 把它量出来，而不是嘴上讲。

三段各 15 秒，量 TUI 进程用掉的 CPU 毫秒：

1. 指针停在正文**当中**不动——常态 80ms 轮询，这是基线；
2. 指针停在**边缘**不动——触发一次 20ms 档，熄灭后该回落；
3. 指针在边缘**反复**进出（每 150ms 动一次，让提亮不停亮起又熄灭）——人为
   最坏情况，真人不会这么用。

    cargo build
    python3 testkit/tui/hover_cpu.py

TUI 走查只能一个一个跑。
"""

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

WINDOW = 15.0


def move(master, sink, column, row):
    os.write(master, f"\x1b[<35;{column + 1};{row + 1}M".encode())
    h.drain(master, 0.05, sink)


def measure(pid, master, sink, seconds, wiggle=None):
    """量这段时间里进程用掉多少 CPU 毫秒。`wiggle` 给了就按它周期性动指针。"""
    start = h.cpu_ms(pid)
    deadline = time.time() + seconds
    while time.time() < deadline:
        if wiggle is not None:
            for column, row in wiggle:
                move(master, sink, column, row)
                time.sleep(0.15)
        else:
            h.drain(master, 0.3, sink)
    return h.cpu_ms(pid) - start


def main():
    report = {}
    h.ENV["MIYU_LOG"] = "info"
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        "STUB_REASONING_TEXT": "想一下。",
        "STUB_TOOL": "1",
        "STUB_TOOL_COMMAND": "printf 'CPU-PROBE\\n'",
        "STUB_CHUNK_SLEEP": "0.03",
    })
    numbers = {}
    try:
        h.drain_until(master, sink, "A G E N T", 15.0)
        h.drain(master, 1.0, sink)
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 5.0)
        os.write(master, b"\r")
        screen = r.wait_screen(
            master, sink, lambda s: any(h.is_fold_summary(l) for l in s), 90.0
        )
        report["cpu01_turn_finished"] = screen is not None
        screen = screen or r.LAST["screen"] or []
        target = next((i for i, l in enumerate(screen) if h.is_fold_summary(l)), None)
        report["cpu02_found_a_row"] = target is not None
        if target is None:
            return summary(report, numbers)
        # 大厅动画会自己吃 CPU，这里已经有正文了，banner 早就撤了。
        h.drain(master, 1.0, sink)

        move(master, sink, 8, target)
        numbers["A 指针停正文当中(基线)"] = measure(tui.pid, master, sink, WINDOW)

        move(master, sink, 0, target)
        numbers["B 指针停边缘"] = measure(tui.pid, master, sink, WINDOW)

        numbers["C 边缘反复进出(最坏)"] = measure(
            tui.pid, master, sink, WINDOW, wiggle=[(0, target), (8, target)]
        )
    finally:
        r.stop(tui, daemon, stub)

    base = numbers.get("A 指针停正文当中(基线)")
    edge = numbers.get("B 指针停边缘")
    worst = numbers.get("C 边缘反复进出(最坏)")
    # 停在边缘不动只该多出「一次 100ms 的 20ms 档」，也就是几毫秒的量级。
    report["cpu03_resting_on_edge_costs_no_more"] = (
        base is not None and edge is not None and edge <= max(base * 2, base + 30)
    )
    # 人为最坏也不该把 15 秒里的 CPU 吃到 10%（1500ms）以上。
    report["cpu04_worst_case_stays_small"] = worst is not None and worst < 1500
    return summary(report, numbers)


def summary(report, numbers):
    for name, value in numbers.items():
        print(f"   {name}: {value} ms CPU / {WINDOW:.0f}s  = {value / WINDOW / 10:.2f}%")
    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
