#!/usr/bin/env python3
"""BUG-06 走查：回合进行中 PgUp 翻到顶、再 PgDn 翻回底，之后视口要继续跟着输出走，
到底之后再按 PgDn 不该整屏重画，画面上也不该留下重影（用户截图：同一行
「已思考 · 1251 词元 · 11.1s」重复十几遍）。

用户的配置是 `expand_reasoning: true`（思考块自动展开、边想边长），照着来。

    cargo build
    python3 testkit/tui/page_follow.py

产物：~/.cache/miyu-tui-smoke/round26-pagefollow-*.txt，trace 在 /tmp/miyu-screen-trace.log。
"""

import json
import os
import re
import sys
import time
from pathlib import Path

os.environ["MIYU_SCREEN_TRACE"] = "1"
sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

TRACE = Path("/tmp/miyu-screen-trace.log")
STUB = {
    "STUB_REASONING": "1",
    "STUB_TOOL": "1",
    "STUB_EDIT": "1",
    # 一段里想十二次、跑十二条命令（用户截图那种长回合），每条命令慢一点好在回合中翻页。
    "STUB_TOOL_ROUNDS": "4",
    "STUB_TOOL_COMMAND": "sleep 0.5; printf '走查用的命令输出\\n'",
    "STUB_CHUNK_SLEEP": "0.02",
    "STUB_REASONING_TEXT": "先看一眼需求,再决定怎么下手。这段是思考正文,折叠时看不到,点开才有。" * 6,
    # 正文流得久一点（三十来秒），翻页动作都落在回合里。
    "STUB_REPLY": ("这是一段用于走查的回复，分块吐出来好让 footer 量得出每秒 token。\n" * 3 + "\n") * 40,
}


def ghost_rows(rows):
    """重影：一行步骤（思考/命令抬头）和它上面 1~3 行里的某一行一字不差。"""
    ghosts = 0
    for i, row in enumerate(rows):
        text = row.strip()
        if not text or text in ("│",) or "已思考" not in text and "运行命令" not in text:
            continue
        if any(rows[j].strip() == text for j in range(max(0, i - 3), i)):
            ghosts += 1
    return ghosts


def trace_frames(since):
    if not TRACE.exists():
        return []
    out = []
    for line in TRACE.read_text(encoding="utf-8", errors="replace").splitlines():
        m = re.match(r"(\d+) paint body=(\d+) total=(\d+) scroll=(\d+) follow=(\w+)", line)
        if m and int(m.group(1)) >= since:
            out.append({"t": int(m.group(1)), "body": int(m.group(2)), "total": int(m.group(3)),
                        "scroll": int(m.group(4)), "follow": m.group(5) == "true"})
    return out


def main():
    report = {}
    if TRACE.exists():
        TRACE.unlink()
    stub, daemon, tui, master, sink = r.start(
        STUB, config_extra={"display": {"expand_reasoning": True}}
    )
    try:
        # 先灌两轮把正文撑过一屏。
        for prompt in (h.PROMPT, h.PROMPT):
            os.write(master, prompt.encode())
            h.drain_until(master, sink, prompt, 3.0)
            os.write(master, b"\r")
            h.settle(master, sink, quiet=1.2, timeout=60)
        # 第三轮：回合进行中翻页。
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        # 等正文开始流（那一段过程已收成 Worked for，正文一行行往下长）再翻。
        r.wait_screen(
            master, sink,
            lambda rows: sum(1 for l in rows if "分块吐出来" in l) >= 3, timeout=60,
        )
        # 按到顶为止（最多 40 下）：前两轮加时间线三百多行，写死按 8 下（约 350 行）
        # 差一点点翻不到顶，`reached_top` 于是一直是假的（09-24 查实不是翻页坏了）。
        presses = 0
        top = h.render(bytes(sink))
        while presses < 40 and not any("走查一句" in l for l in top[:12]):
            os.write(master, b"\x1b[5~")
            h.settle(master, sink, quiet=0.08, timeout=1)
            presses += 1
            top = h.render(bytes(sink))
        r.save("pagefollow-top", top)
        report["reached_top"] = any("走查一句" in l for l in top[:12])
        report["_pgup_presses"] = presses
        # 翻回底：比翻上去多按几下，保证真到底。
        for _ in range(presses + 8):
            os.write(master, b"\x1b[6~")
            h.settle(master, sink, quiet=0.08, timeout=1)
        t_bottom = int(time.time() * 1000)
        # 已经在底了，再按两下：量这两下各重写了多少行。
        rewrites = []
        for _ in range(2):
            mark = len(sink)
            os.write(master, b"\x1b[6~")
            h.settle(master, sink, quiet=0.3, timeout=2)
            chunk = bytes(sink)[mark:].decode("utf-8", "replace")
            rewrites.append(len(re.findall(r"\x1b\[\d+;1H\x1b\[K", chunk)))
        report["pgdn_at_bottom_rewrites_rows"] = rewrites
        # 回合还在跑：翻回底之后过两秒抓一屏，看有没有重影（这时那一段还没收成 Worked for）。
        time.sleep(2.0)
        h.settle(master, sink, quiet=0.15, timeout=1)
        t_mid = int(time.time() * 1000)
        mid = h.render(bytes(sink))
        r.save("pagefollow-mid", mid)
        report["ghost_rows_mid"] = ghost_rows(mid)
        # 回合继续跑完。
        h.settle(master, sink, quiet=1.5, timeout=120)
        final = h.render(bytes(sink))
        r.save("pagefollow-final", final)
        report["ghost_rows_final"] = ghost_rows(final)
        # 回合结束、视口在底：再按两下 PgDn 不该重写任何一行（原来每按一次整屏 44 行）。
        idle = []
        for _ in range(2):
            mark = len(sink)
            os.write(master, b"\x1b[6~")
            h.settle(master, sink, quiet=0.3, timeout=2)
            chunk = bytes(sink)[mark:].decode("utf-8", "replace")
            idle.append(len(re.findall(r"\x1b\[\d+;1H\x1b\[K", chunk)))
        report["pgdn_idle_at_bottom_rewrites_rows"] = idle
        report["pgdn_idle_at_bottom_is_free"] = all(n == 0 for n in idle)
        frames = trace_frames(t_bottom)
        report["frames_after_bottom"] = len(frames)
        tail = frames[-8:]
        report["follow_true_at_end"] = bool(tail) and all(f["follow"] for f in tail)
        report["scroll_tracks_total_at_end"] = bool(tail) and all(
            f["scroll"] == max(0, f["total"] - f["body"]) for f in tail
        )
        # 回合还在跑的那几帧里，视口要跟着输出走（scroll 贴着 total-body）。
        mid_frames = [f for f in frames if f["t"] < t_mid]
        report["frames_mid"] = len(mid_frames)
        report["follow_true_mid"] = bool(mid_frames) and all(f["follow"] for f in mid_frames[-5:])
        # 收尾那句正文要在屏上（视口跟着输出走）。
        report["reply_visible_at_end"] = any("分块吐出来" in l for l in final[-12:])
        # 重影：同一行「已思考 · N 词元 · Xs」在屏上出现的次数不该超过一屏里真有的步数
        #（三轮 × 每轮四段思考 = 12 是上限，重影会翻倍）。
        thoughts = [l.strip() for l in final if "已思考" in l]
        dup = len(thoughts) - len(set(thoughts))
        report["thought_rows"] = len(thoughts)
        report["duplicate_thought_rows"] = dup
        report["no_ghost_rows"] = dup <= 1
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    print("产物：", h.OUT)
