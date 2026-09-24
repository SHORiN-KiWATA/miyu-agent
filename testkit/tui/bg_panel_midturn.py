#!/usr/bin/env python3
"""主回合还在说话时点开后台子代理的浮层：浮层要跟着子代理往下走。

用户 09-24 验收时记下的老问题：她还在回答时打开后台子代理的浮层，浮层一直停在
「提示词」那一行不动，要等她这一轮说完才正常。纯 main 上也这样。

桩模型：主回合先派一条后台子代理，再慢慢吐一大段回复（这段时间里主回合一直在跑）；
子代理跑两轮命令（每轮先想一句），最后也说一段。在主回合说完之前点开状态行上那条
子代理，之后每秒看一眼浮层：它里面出现「运行命令」这一步，才算跟上了。

    cargo build
    python3 testkit/tui/bg_panel_midturn.py
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

TITLE = "走查后台子代理"
REPLY_END = "主回合说到这里结束"
STUB = {
    "STUB_SUBAGENT": "1",
    "STUB_SUBBG": "1",
    "STUB_REASONING": "1",
    "STUB_SUBAGENT_BG_COMMAND": "sleep 2; printf 'BGOUT走查输出\\n'",
    "STUB_SUBAGENT_BG_ROUNDS": "2",
    # 主回合的回复慢慢吐三四十秒：点开浮层、看它走，都在这段时间里。
    "STUB_REPLY": "主回合还在慢慢说。\n" * 60 + REPLY_END,
    "STUB_CHUNK_CHARS": "6",
    "STUB_CHUNK_SLEEP": "0.08",
}
# `--thinking`：主回合在**想**（一大段不空行的思考）而不是在说。
THINKING = "--thinking" in sys.argv
if THINKING:
    STUB["STUB_REASONING_TEXT"] = "主回合先把这件事想清楚再说，" * 60
    STUB["STUB_REPLY"] = REPLY_END
# `--sub-thinking`：子代理一上来就想一大段（不空行，日志要攒够一段才落盘），主回合
# 同时也在想。这时浮层只有订标记流才跟得上。
SUB_THINKING = "--sub-thinking" in sys.argv
THOUGHT_MARK = "先把这件事想清楚"
if SUB_THINKING:
    STUB["STUB_SUBAGENT_BG_ROUNDS"] = "0"
    STUB["STUB_REASONING_TEXT"] = f"{THOUGHT_MARK}再说，" * 80
    STUB["STUB_REPLY"] = REPLY_END


def panel_rows(screen):
    """浮层那一块：从带任务标题的那行到「Esc 关闭」那行。"""
    top = next((i for i, line in enumerate(screen) if TITLE in line and "running" in line), None)
    foot = next((i for i, line in enumerate(screen) if "Esc" in line and "关闭" in line), None)
    if top is None or foot is None or foot <= top:
        return []
    return screen[top:foot + 1]


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        os.write(master, "STUB_SUBBG 开条后台子代理".encode())
        h.drain_until(master, sink, "后台子代理", 3.0)
        os.write(master, b"\r")
        # 状态行上那条子代理（最下面那行；正文时间线上也有一行写着它的标题）。
        strip_line = re.compile(TITLE)
        screen = r.wait_screen(
            master, sink,
            lambda s: sum(1 for line in s if strip_line.search(line)) >= 2, 40.0,
        )
        screen = screen or h.render(bytes(sink))
        strip = max((i for i, line in enumerate(screen) if strip_line.search(line)), default=None)
        report["strip_visible"] = strip is not None
        report["turn_running_when_opened"] = not any(REPLY_END in line for line in screen)
        if strip is None:
            return report
        # 按下和松开一次发出去：回合正吐字时画面静不下来，分开发会被当成拖动。
        os.write(master, f"\x1b[<0;4;{strip + 1}M\x1b[<0;4;{strip + 1}m".encode())
        report["overlay_opened"] = h.drain_until(master, sink, "Esc 关闭", 10.0)
        # 主回合说完之前，每秒看一眼浮层里有什么。
        samples = []
        followed_at = None
        opened = time.time()
        while time.time() - opened < 25.0:
            h.drain(master, 1.0, sink)
            screen = h.render(bytes(sink))
            rows = panel_rows(screen)
            turn_running = not any(REPLY_END in line for line in screen)
            has_command = any(
                "运行命令" in line or "BGOUT" in line or (SUB_THINKING and THOUGHT_MARK in line)
                for line in rows[1:-1]
            )
            samples.append({
                "t": round(time.time() - opened, 1),
                "turn_running": turn_running,
                "panel_rows": len(rows),
                "has_command": has_command,
                "last": (rows[-2].strip()[:50] if len(rows) >= 2 else ""),
            })
            if has_command and turn_running and followed_at is None:
                followed_at = samples[-1]["t"]
            if not turn_running:
                break
        r.save("bg-panel-midturn", h.render(bytes(sink)))
        report["samples"] = samples
        report["panel_followed_while_turn_ran"] = followed_at is not None
        report["_followed_at_s"] = followed_at
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
