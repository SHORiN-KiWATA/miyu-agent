#!/usr/bin/env python3
"""切子会话、回主会话时终端上依次出现的每一帧（09-25，只量不修）。

每个同步块（`ESC[?2026h … l`）算一帧，记下时间、footer、时间线上看不看得到正在跑的子代理那一步、
非空行数。场景同 `session_ux_probe`（前台/后台子代理都 sleep 90，画面上只有转轮在动）。
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402
import subagent_visit as sv  # noqa: E402
import session_ux_probe as ux  # noqa: E402

END = b"\x1b[?2026l"


def frames_after(master, sink, keys, seconds=3.0):
    mark = len(sink)
    start = time.monotonic()
    os.write(master, keys)
    stamps = []
    deadline = start + seconds
    seen = mark
    while time.monotonic() < deadline:
        h.drain(master, 0.02, sink)
        while True:
            at = bytes(sink).find(END, seen)
            if at < 0:
                break
            seen = at + len(END)
            stamps.append((round(time.monotonic() - start, 3), seen))
    frames, last = [], None
    for stamp, upto in stamps:
        screen = h.render(bytes(sink[:upto]))
        footer = next((line.strip()[:70] for line in reversed(screen) if "stub-model" in line), "")
        summary = (footer, sv.timeline_row(screen) is not None, sv.inside_child(screen),
                   sum(1 for line in screen if line.strip()))
        if summary != last:
            frames.append({"t": stamp, "footer": footer, "turn_visible": summary[1],
                           "in_child": summary[2], "lines": summary[3]})
            last = summary
    return {"bytes": len(sink) - mark, "full_clears": bytes(sink[mark:]).count(b"\x1b[2J"), "frames": frames}


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(ux.ENV_A)
    try:
        ux.say(master, sink, h.PROMPT + " 一")
        r.wait_screen(master, sink, lambda s: "撑大" in "\n".join(s), 30.0)
        h.settle(master, sink, quiet=1.5, timeout=20.0)
        stub = ux.swap_stub(stub, ux.ENV_B)
        ux.say(master, sink, h.PROMPT + " STUB_SUBBG")
        r.wait_screen(master, sink, lambda s: sv.timeline_row(s) is not None, 40.0)
        h.settle(master, sink, quiet=1.0, timeout=4.0)
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=1.5)
        report["enter"] = frames_after(master, sink, b"\r")
        os.write(master, b"/back")
        h.drain_until(master, sink, "/back", 3.0)
        report["back"] = frames_after(master, sink, b"\r")
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    print(json.dumps(main(), ensure_ascii=False, indent=1))
