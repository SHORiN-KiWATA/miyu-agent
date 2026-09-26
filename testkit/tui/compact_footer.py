#!/usr/bin/env python3
"""`/compact` 期间 footer 的转轮要转（用户 09-18：正在压缩上下文应该有个 spinner），
压完 footer 的上下文读数当场刷新（原来要等下一轮结束才变）。

    cargo build
    python3 testkit/tui/compact_footer.py

桩模型的压缩摘要要跑一次完整调用，尾巴预算压到几十个词元好让三轮小对话压得动；
桩分块吐得慢一点（STUB_CHUNK_SLEEP）让「正在压缩」那段有几百毫秒可以观察转轮。
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

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import fold_summary as fs  # noqa: E402

WAVE = set("▁▂▃▄▅▆▇")
BRAILLE = set("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
READING = re.compile(r"(\d+(?:\.\d+)?k?)/(?:~)?\d+k")


def footer_row(screen):
    for line in reversed(screen):
        if line.startswith("┃") and "·" in line:
            return line
    return None


def reading_of(line):
    m = READING.search(line or "")
    return m.group(1) if m else None


def send_turn(master, sink, text, index):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    r.wait_screen(master, sink, lambda s, n=index: sum(1 for l in s if "走查的回复" in l) >= n, 40.0)
    h.settle(master, sink)


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(
        {"STUB_CHUNK_SLEEP": "0.05"},
        # 开着「显示 token 用量」,压完那行才带上下文读数,好和 footer 对数。
        # 会话池两个模型:回复末尾带「供应商 / 模型」那行,好验它和转轮行之间的空行
        # (用户 09-18 截图:少空行)。
        config_extra={
            "context": {"compact_tail_tokens": 40},
            "display": {"show_token_usage": True},
            "active_provider_models": [
                {"provider_id": "stub", "model": "stub-model"},
                {"provider_id": "stub", "model": "stub-b"},
            ],
            "providers": [{
                "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1",
                "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model", "stub-b"],
            }],
        },
    )
    try:
        for index in range(1, 4):
            send_turn(master, sink, f"{h.PROMPT} 第{index}遍", index)
        before = reading_of(footer_row(h.render(bytes(sink))))
        report["footer_reading_before"] = before is not None
        os.write(master, b"/compact\r")
        # 压缩期间：正文里有「正在压缩上下文」、结果还没出来时，footer 行上要有转轮。
        spinner_seen = False
        braille_seen = False
        gap_ok = None
        deadline = time.time() + 60
        done_marks = ("上下文已压缩", "没有可压缩的上下文")
        while time.time() < deadline:
            h.drain(master, 0.05, sink)
            screen = h.render(bytes(sink))
            if any(any(m in l for m in done_marks) for l in screen):
                break
            row = footer_row(screen)
            if row and any(ch in WAVE for ch in row):
                spinner_seen = True
            # 时间线状态行:logo 左侧的点阵转轮,文案是「正在压缩上下文」
            idx = next((i for i, l in enumerate(screen) if any(ch in BRAILLE for ch in l) and "正在压缩上下文" in l), None)
            if idx is not None:
                braille_seen = True
                # 转轮行上面要空一行,再上面是回复末尾那一行:09-26 起是 `✻ 模型 · … 完成`,混合模型池的
                # 「供应商 / 模型」在它上面(用户 09-18 截图:少空行)。
                if gap_ok is None:
                    above = screen[idx - 2] if idx >= 2 else ""
                    gap_ok = (
                        idx >= 2
                        and screen[idx - 1].strip() == ""
                        and (fs.is_turn_end(above) or "stub / stub-" in above)
                    )
        report["footer_wave_while_compacting"] = spinner_seen
        report["blank_line_between_model_line_and_spinner"] = bool(gap_ok)
        report["dot_spinner_with_compacting_text"] = braille_seen
        h.settle(master, sink, quiet=0.8, timeout=8.0)
        screen = h.render(bytes(sink))
        r.save("compact-footer", screen)
        row = footer_row(screen)
        after = reading_of(row)
        head = next((l for l in screen if "上下文已压缩" in l), "")
        head_reading = reading_of(head)
        report["compacted"] = bool(head)
        # 压完:footer 的读数和正文那行报的数一致(不再是压缩前的旧数)。
        report["footer_matches_result_line"] = head_reading is not None and after == head_reading
        report["footer_reading_changed"] = before is not None and after is not None and after != before
        report["footer_spinner_stopped"] = row is not None and not any(ch in WAVE for ch in row)
        # 转轮行自带文案,不再另写一行静态的「正在压缩」(用户 09-18:重复行);
        # 压完转轮撤掉,正文里不该剩下这句。
        report["no_duplicate_compacting_line"] = sum("正在压缩上下文" in l for l in screen) == 0
        print(json.dumps({"before": before, "after": after, "head": head.strip()[:80]}, ensure_ascii=False))

        # /session 列表里当前会话那行的上下文 = 压缩后的数(库里落好的那份),不是压缩前的。
        os.write(master, b"/session\r")
        r.wait_screen(master, sink, lambda s: any("选择会话" in l or "Select session" in l for l in s), 15.0)
        h.settle(master, sink, quiet=0.6, timeout=5.0)
        rows = h.render(bytes(sink))
        r.save("compact-session-rows", rows)
        current = next((l for l in rows if "* " in l and " · " in l and ("普通" in l or "开发" in l)), "")
        row_reading = re.search(r"(普通|开发) · (\S+) · ", current)
        report["session_row_shows_compacted_context"] = bool(row_reading) and row_reading.group(2) == after
        os.write(master, b"\x1b")
        h.settle(master, sink, quiet=0.6, timeout=5.0)

        # /undo 撤销压缩:提示「已撤销上下文压缩」,屏上前一轮对话还在,footer 读数回到压缩前。
        replies_before_undo = sum(1 for l in h.render(bytes(sink)) if "走查的回复" in l)
        os.write(master, b"/undo\r")
        r.wait_screen(master, sink, lambda s: any("已撤销上下文压缩" in l or "已撤销消息数" in l for l in s), 20.0)
        h.settle(master, sink, quiet=0.8, timeout=8.0)
        undone = h.render(bytes(sink))
        r.save("compact-undo", undone)
        report["undo_reports_compaction_undone"] = any("已撤销上下文压缩" in l for l in undone)
        report["undo_keeps_last_turn_on_screen"] = sum(1 for l in undone if "走查的回复" in l) >= replies_before_undo
        # 那块「上下文已压缩」要从屏上撤掉(用户 09-18:撤了它还在,还能点开)
        report["undo_removes_compact_block"] = not any("上下文已压缩" in l for l in undone)
        report["undo_footer_back_to_before"] = reading_of(footer_row(undone)) == before
    finally:
        r.stop(tui, daemon, stub)
    for key, ok in report.items():
        print(f"{'✅' if ok else '❌'} {key}")
    passed = sum(1 for v in report.values() if v)
    print(f"\n{passed}/{len(report)} passed")
    sys.exit(0 if passed == len(report) else 1)


if __name__ == "__main__":
    main()
