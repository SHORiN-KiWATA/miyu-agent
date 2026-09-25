#!/usr/bin/env python3
"""子代理状态行的悬浮提亮探针(用户 09-18:「鼠标悬浮子代理状态行的高亮没了」)。

跑着的子代理那一行是 dim 的,鼠标移上去整块去 dim(`hover_paint`)。判据看字节流:
鼠标移到那一行之后,那一行被重画且重画那段里标题前面没有 `\\x1b[2m`。

    cargo build
    python3 testkit/tui/subagent_hover.py            # 新二进制
    MIYU_BIN=~/.local/bin/miyu python3 testkit/tui/subagent_hover.py   # 对照
"""

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

ROW_TEXT = "走查子代理"


def row_segment(raw, text):
    """字节流里最后一次画到 `text` 的那一段(从上一个光标定位序列起)。"""
    at = raw.rfind(text)
    if at < 0:
        return ""
    start = max(raw.rfind("\x1b[", 0, at) - 80, 0)
    # 往前找最近的 CUP(ESC[row;colH),那是这一行重画的起点。
    cup = [m.start() for m in re.finditer(r"\x1b\[\d+;\d+H", raw[:at])]
    if cup:
        start = cup[-1]
    return raw[start:at + len(text)]


def main():
    """场景由 argv 选:fg(默认,前台子代理跑着)/bg(后台子代理,那一行已收工)/dev-fg/dev-bg。"""
    scenario = sys.argv[1] if len(sys.argv) > 1 else "fg"
    # 原来还有一档 panel：点开后台子代理的浮层、悬浮浮层里那一步。09-18 起点子代理是切进
    # 它的会话，浮层退役，那一档 09-25 删了（新行为见 `subagent_visit.py`）。
    background = scenario.endswith("bg")
    dev = scenario.startswith("dev")
    row_text = "走查后台子代理" if background else ROW_TEXT
    report = {}
    stub, daemon, tui, master, sink = r.start({
        "STUB_REASONING": "1",
        "STUB_SUBAGENT": "1",
        "STUB_SUBAGENT_COMMAND": "sleep 8; printf 'SUBOUT\\n'",
        "STUB_SUBAGENT_BG_COMMAND": "sleep 8; printf 'BGOUT\\n'",
        "STUB_REASONING_TEXT": "想一下。",
        "STUB_CHUNK_SLEEP": "0.05",
    }, {"terminal_session_mode": "dev"} if dev else None)
    try:
        prompt = h.PROMPT + (" STUB_SUBBG" if background else "")
        os.write(master, prompt.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        if background:
            # 后台子代理那一行立刻收工(返回 job_id),等它出现并静一下。
            screen = r.wait_screen(master, sink, lambda s: any(row_text in l for l in s), 30.0)
            h.drain(master, 1.0, sink)
            screen = h.render(bytes(sink))
        else:
            screen = r.wait_screen(master, sink, lambda s: any(r.is_running_row(l, row_text) for l in s), 30.0)
        report["row_running"] = screen is not None and any(row_text in l for l in screen)
        if not report["row_running"]:
            r.save("hover-timeout", r.LAST["screen"] or screen or [])
            return report
        row = next(i for i, l in enumerate(screen) if row_text in l)
        ROW = row_text
        before = bytes(sink).decode("utf-8", "replace")
        seg_before = row_segment(before, ROW)
        report["row_dim_before_hover"] = "\x1b[2m" in seg_before
        mark = len(sink)
        # SGR 鼠标移动(无按键):按钮码 35 = 32(动作) + 3(无键)。
        os.write(master, f"\x1b[<35;12;{row + 1}M".encode())
        h.drain(master, 0.6, sink)
        after = bytes(sink)[mark:].decode("utf-8", "replace")
        seg_after = row_segment(after, ROW)
        report["row_repainted_on_hover"] = bool(seg_after)
        report["row_undimmed_on_hover"] = bool(seg_after) and "\x1b[2m" not in seg_after
        r.save("hover", h.render(bytes(sink)))
        print(json.dumps({"before": seg_before[-160:], "after": seg_after[-160:]}, ensure_ascii=False))
        # 移开:又变回 dim
        mark = len(sink)
        os.write(master, f"\x1b[<35;12;{max(row - 1, 1)}M".encode())
        h.drain(master, 0.6, sink)
        away = bytes(sink)[mark:].decode("utf-8", "replace")
        seg_away = row_segment(away, ROW)
        report["row_redimmed_when_left"] = bool(seg_away) and "\x1b[2m" in seg_away
        os.write(master, b"\x1b")
        h.drain_until(master, sink, "走查的回复", 40.0)
        r.save(f"hover-{scenario}", h.render(bytes(sink)))
    finally:
        r.stop(tui, daemon, stub)
    return report


if __name__ == "__main__":
    report = main()
    for key, ok in report.items():
        print(f"{'✅' if ok else '❌'} {key}")
    passed = sum(1 for v in report.values() if v)
    print(f"\n{passed}/{len(report)} passed")
    sys.exit(0 if passed == len(report) else 1)
