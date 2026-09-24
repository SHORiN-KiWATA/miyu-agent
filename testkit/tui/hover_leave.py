#!/usr/bin/env python3
"""鼠标移出窗口之后，那一下悬浮提亮该灭。

用户 09-22：「鼠标悬浮在可以展开的内容上会高亮，这是正常行为，但是我在这种情况
下把鼠标直接移出窗口，高亮依旧保持」。

终端不报「指针离开」——09-22 用 `pointer_leave_probe.py` 在真 kitty 上实测：
移出窗口时焦点事件一条都没有，终端只是从此静默。可用的线索是移出去之前最后
那一下必然落在**边缘**（实测一路报到第 1 列），而窗口内连续移动之间最多隔
0.4 秒。所以判据是「最后停在边缘 + 随后静默」。

这份走查验三件事：

- 悬在可展开的那一行上会提亮（提亮本身没坏）；
- 把指针移到**边缘**再静默一会儿，提亮灭掉（模拟移出窗口）；
- 停在**正文当中**不动同样久，提亮**还在**（那是悬着看，不能误熄）。

    cargo build
    python3 testkit/tui/hover_leave.py

TUI 走查只能一个一个跑。
"""

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 提亮 = 那一行不再是 dim。判据看字节流里那一行被重画时带不带 `\x1b[2m`，
# 和 `subagent_hover.py` 一个路数；这里看渲染后的屏幕更直观：pyte 会把
# dim 记在属性上。
DIM = "\x1b[2m"


def move(master, sink, column, row, quiet=0.25):
    """把指针挪到 (column, row)，不按键。"""
    os.write(master, f"\x1b[<35;{column + 1};{row + 1}M".encode())
    h.drain(master, quiet, sink)


def row_is_dim(raw, marker):
    """最后一次重画 `marker` 那一行时，它是暗的吗。`marker` 可以是正则(收缩行按类写,
    09-24 起不一定以 `Worked for` 打头)。"""
    text = raw.decode("utf-8", "replace")
    if hasattr(marker, "finditer"):
        found = [match.start() for match in marker.finditer(text)]
        index = found[-1] if found else -1
    else:
        index = text.rfind(marker)
    if index < 0:
        return None
    head = text[max(0, index - 40):index]
    return DIM in head


def main():
    report = {}
    h.ENV["MIYU_LOG"] = "info"
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        "STUB_REASONING_TEXT": "想一下。",
        "STUB_TOOL": "1",
        "STUB_TOOL_COMMAND": "printf 'HOVER-OUT\\n'",
        "STUB_CHUNK_SLEEP": "0.03",
    })
    try:
        h.drain_until(master, sink, "A G E N T", 15.0)
        h.drain(master, 1.0, sink)
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 5.0)
        os.write(master, b"\r")
        # 等这一轮收完：屏幕上留下可点开的那几步。
        screen = r.wait_screen(
            master, sink, lambda s: any(h.is_fold_summary(l) for l in s), 90.0
        )
        report["hv01_turn_finished"] = screen is not None
        screen = screen or r.LAST["screen"] or []
        (h.OUT / "hover-idle.txt").write_text("\n".join(screen) + "\n", encoding="utf-8")
        target = next(
            (i for i, l in enumerate(screen) if h.is_fold_summary(l)), None
        )
        report["hv02_found_an_expandable_row"] = target is not None
        if target is None:
            return summary(report)

        # 1. 悬上去：那一行该提亮（不 dim）。
        sink.clear()
        move(master, sink, 8, target)
        report["hv03_hover_lights_the_row"] = row_is_dim(bytes(sink), h.FOLD_SUMMARY_RE) is False

        # 2. 停在正文当中不动：提亮要留着（悬着看）。
        sink.clear()
        time.sleep(1.5)
        h.drain(master, 0.5, sink)
        still = row_is_dim(bytes(sink), h.FOLD_SUMMARY_RE)
        report["hv04_holding_still_keeps_the_light"] = still is not False or still is None

        # 3. 移到最左列（模拟从左边移出窗口），再静默：该灭。
        sink.clear()
        move(master, sink, 0, target, quiet=0.0)
        # 灵敏度也钉住：判定 100ms + 轮询 20ms，200ms 内就该灭（用户 09-22
        # 连着两次要更灵敏：最早要等满 700ms+80ms）。
        time.sleep(0.2)
        h.drain(master, 0.1, sink)
        left = h.render(bytes(sink))
        (h.OUT / "hover-left.txt").write_text("\n".join(left) + "\n", encoding="utf-8")
        report["hv05_edge_then_silence_clears"] = row_is_dim(bytes(sink), h.FOLD_SUMMARY_RE) is True
    finally:
        r.stop(tui, daemon, stub)
    return summary(report)


def summary(report):
    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
