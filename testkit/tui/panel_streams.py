#!/usr/bin/env python3
"""回合跑着时开 `/models` / `/session` 面板，正文还在往下流（会话项目第 3 段，B4）。

待办 BUG 第一条：AI 输出途中打开面板，渲染停在打开面板的那一刻。改前面板自己跑一个同步
事件循环，回合的事件在 socket 里排队，面板收掉才接着画。

判据照 `midturn_panels.py` 留下的那句：看得见的最大填充编号有没有往上走——「屏幕有没有
变化」会被转轮和面板重画骗过去。

    cargo build
    MIYU_HOME=~/.cache/miyu-panel-streams/home MIYU_TUI_PORT=18691 STUB_PORT=18692 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-panel-streams/rt OUT=~/.cache/miyu-panel-streams/out \\
      python3 testkit/tui/panel_streams.py
    # 对照改前：MIYU_BIN=~/.local/bin/miyu …（同上）

每场：说一句（回复是带编号的长正文，慢慢吐）→ 流一会儿 → 开面板 → 面板开着等 4 秒，
看最大编号涨没涨 → Esc 收掉 → 输入框回来、这一轮跑完。
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

FILLER = 400


def stub_for(marker):
    """一行一个编号：正文按整行落屏（半行要等「正文活尾巴」那一项），编号得各占一行才看得出
    它在往下走。每场换一个前缀，上一场留在屏上的编号不会混进来。"""
    reply = "\n".join(f"{marker}{index:04d}" for index in range(FILLER)) + "\n全文完。"
    return {"STUB_REPLY": reply, "STUB_CHUNK_SLEEP": "0.03", "STUB_CHUNK_CHARS": "4"}


def squash(screen):
    return "".join(line.strip() for line in screen)


def max_filler(screen, marker):
    found = re.findall(marker + r"(\d{4})", squash(screen))
    return max((int(index) for index in found), default=-1)


def panel_open(screen):
    return any(("选择模型" in line or "选择会话" in line) for line in screen)


def input_back(screen):
    return any(line.startswith("┃ 普通 ·") for line in screen)


def scenario(report, label, command, marker):
    """一场一个沙箱：开 TUI、说一句、流一会儿、开面板、看编号还涨不涨、Esc、等这一轮跑完。"""
    h.reset_view()
    stub, daemon, tui, master, sink = r.start(stub_for(marker))
    try:
        run_scenario(master, sink, report, label, command, marker)
    finally:
        r.stop(tui, daemon, stub)


def run_scenario(master, sink, report, label, command, marker):
    prompt = f"面板走查{label}"
    os.write(master, prompt.encode())
    h.drain_until(master, sink, prompt, 3.0)
    os.write(master, b"\r")
    started = r.wait_screen(master, sink, lambda s: max_filler(s, marker) >= 20, 30.0)
    report[f"{label}_turn_streams"] = started is not None
    if started is None:
        r.save(f"streams-{label}-nostream", r.LAST["screen"] or [])
        return
    os.write(master, command.encode())
    h.drain_until(master, sink, command, 3.0)
    os.write(master, b"\r")
    opened = r.wait_screen(master, sink, panel_open, 5.0)
    report[f"{label}_panel_opens_mid_turn"] = opened is not None
    if opened is None:
        r.save(f"streams-{label}-noopen", r.LAST["screen"] or [])
        return
    before = max_filler(opened, marker)
    h.drain(master, 4.0, sink)
    during = h.render(bytes(sink))
    r.save(f"streams-{label}-open", during)
    after = max_filler(during, marker)
    report[f"{label}_text_keeps_streaming_under_the_panel"] = (
        panel_open(during) and before < FILLER - 30 and after > before + 10
    )
    report[f"_{label}_filler"] = [before, after]
    os.write(master, b"\x1b")
    closed = r.wait_screen(master, sink, lambda s: not panel_open(s) and input_back(s), 5.0)
    report[f"{label}_esc_brings_the_input_back"] = closed is not None
    finished = r.wait_screen(master, sink, lambda s: "全文完。" in squash(s), 120.0)
    report[f"{label}_turn_finishes"] = finished is not None
    r.save(f"streams-{label}-done", finished or r.LAST["screen"] or [])


def main():
    report = {}
    scenario(report, "models", "/models", "甲")
    scenario(report, "session", "/session", "乙")
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    checks = {key: ok for key, ok in report.items() if not key.startswith("_")}
    passed = sum(1 for ok in checks.values() if ok)
    print(f"{passed}/{len(checks)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if checks and passed == len(checks) else 1)
