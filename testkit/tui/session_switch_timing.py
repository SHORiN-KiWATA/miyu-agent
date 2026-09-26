#!/usr/bin/env python3
"""切进一条大子会话、再回来：从按键到画面稳定要多久、写了多少字节（09-25，只量不修）。

后台子代理跑 30 轮、每轮一条约 6KB 输出的命令（转录约 180KB）；前台子代理 sleep 120 把主线挂住。
等后台那条跑完，从主会话切进它、`/back` 回来，各量三样：到画面认得出来的耗时、到画面静下来
（0.5 秒没有新输出）的耗时、这段时间终端收到的字节数。

    MIYU_BIN=... MIYU_HOME=~/.cache/miyu-session-ux/home MIYU_TUI_PORT=18961 STUB_PORT=18962 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-session-ux/rt OUT=~/.cache/miyu-session-ux/out \\
      python3 testkit/tui/session_switch_timing.py
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

BIG_CHILD = dict(
    ux.ENV_B,
    STUB_SUBAGENT_COMMAND="sleep 120; printf 'SUBOUT\\n'",
    STUB_SUBAGENT_BG_COMMAND="python3 -c \"print(('输出行 ' * 400 + chr(10)) * 3)\"; sleep 0.2",
    STUB_SUBAGENT_BG_ROUNDS="30",
)


def measure(master, sink, keys, predicate):
    """按键 → 认得出（predicate）→ 静下来：两个耗时和这段时间收到的字节数。"""
    mark = len(sink)
    start = time.monotonic()
    os.write(master, keys)
    screen = r.wait_screen(master, sink, predicate, 20.0)
    recognised = time.monotonic() - start if screen else None
    h.settle(master, sink, quiet=0.5, timeout=20.0)
    settled = time.monotonic() - start
    return {
        "recognised_s": round(recognised, 3) if recognised is not None else None,
        "settled_s": round(settled - 0.5, 3),
        "bytes": len(sink) - mark,
        "full_clears": bytes(sink[mark:]).count(b"\x1b[2J"),
    }


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(ux.ENV_A)
    try:
        ux.say(master, sink, h.PROMPT + " 一")
        r.wait_screen(master, sink, lambda s: "撑大" in "\n".join(s), 30.0)
        h.settle(master, sink, quiet=1.5, timeout=20.0)
        stub = ux.swap_stub(stub, BIG_CHILD)
        ux.say(master, sink, h.PROMPT + " STUB_SUBBG")
        # 后台那条跑完：任务条上它那一行不再转（前台那条还在 sleep 120）。
        done = r.wait_screen(
            master, sink,
            lambda s: any(ux.BG_ROW in line and line[:1] not in r.BRAILLE for line in s[-8:])
            and sv.strip_row(s, sv.ROW) is not None,
            90.0,
        )
        report["background_child_finished"] = done is not None
        h.settle(master, sink, quiet=1.0, timeout=5.0)
        r.save("timing-before", ux.now(sink))
        rows = [line for line in ux.now(sink)[-8:] if ux.BG_ROW in line]
        report["_bg_row"] = rows[0].strip()[:100] if rows else None
        os.write(master, b"\x1b[B")
        h.settle(master, sink, quiet=0.3, timeout=1.5)
        report["enter_big_child"] = measure(master, sink, b"\r", sv.inside_child)
        r.save("timing-in-child", ux.now(sink))
        os.write(master, b"/back")
        h.drain_until(master, sink, "/back", 3.0)
        report["back_to_parent"] = measure(master, sink, b"\r", sv.back_in_parent)
        r.save("timing-back", ux.now(sink))
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    print(json.dumps(main(), ensure_ascii=False, indent=2))
    print("产物：", h.OUT)
