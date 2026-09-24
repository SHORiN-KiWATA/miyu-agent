#!/usr/bin/env python3
"""全屏完整回放（会话项目第 2 段）：切回一条长会话，先画最近一屏，往上翻到顶再往前补，
一直翻得到第一句。改前固定回放 3 轮，往上翻到第 12 句就到头了。

    cargo build
    MIYU_HOME=~/.cache/miyu-replay-pages/home MIYU_TUI_PORT=18651 STUB_PORT=18652 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-replay-pages/rt OUT=~/.cache/miyu-replay-pages/out \\
      python3 testkit/tui/replay_pages.py

步骤：说 14 句（每句回 8 段）→ `/dev` 切走 → `/normal` 切回来（换会话会擦掉画布、
按库回放）→ 看最新那句在屏上 → 一直按 PageUp，第一句要翻得出来。
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

TURNS = 14
REPLY = "\n\n".join(f"回放正文第 {index} 段。" for index in range(1, 9))
STUB = {"STUB_CHUNK_SLEEP": "0.005", "STUB_REPLY": REPLY}


def prompt(index):
    return f"回放第 {index} 句走查"


def send(master, sink, text, quiet=1.0, timeout=40):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def on_screen(screen, text):
    return any(text in line for line in screen)


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        for index in range(1, TURNS + 1):
            send(master, sink, prompt(index))
        send(master, sink, "/dev", quiet=1.5, timeout=20)
        screen = send(master, sink, "/normal", quiet=2.0, timeout=30)
        r.save("replay-pages-switched-back", screen)
        report["latest_turn_on_screen"] = on_screen(screen, prompt(TURNS))
        report["first_turn_not_drawn_yet"] = not on_screen(screen, prompt(1))

        seen = set()
        reached_first = False
        for _ in range(120):
            os.write(master, b"\x1b[5~")
            h.settle(master, sink, quiet=0.3, timeout=5)
            paged = h.render(bytes(sink))
            for index in range(1, TURNS + 1):
                if on_screen(paged, prompt(index)):
                    seen.add(index)
            if on_screen(paged, prompt(1)):
                reached_first = True
                r.save("replay-pages-top", paged)
                break
        report["pages_reached_first_turn"] = reached_first
        report["every_turn_seen_on_the_way_up"] = seen == set(range(1, TURNS + 1))
        report["_seen"] = sorted(seen)
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    keys = ["latest_turn_on_screen", "first_turn_not_drawn_yet", "pages_reached_first_turn",
            "every_turn_seen_on_the_way_up"]
    bad = [key for key in keys if not report.get(key)]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
