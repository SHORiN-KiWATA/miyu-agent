#!/usr/bin/env python3
"""长回复切走再切回来，回放要画全（会话项目第 2 段的补丁）。

跑完的轮把流水收成 `turns.replay_journal`，终端按它回放。原来正文每段截到 2048 字、整轮
8 KB：一段 3000 多字的回复切走再切回来，只剩前 2048 字加一个「…」，最后那句没了
（09-25 走查 `bg_visit_midturn.py` 时撞上）。

    cargo build
    MIYU_HOME=~/.cache/miyu-long-reply/home MIYU_TUI_PORT=18695 STUB_PORT=18696 \\
      MIYU_TUI_RUNTIME=~/.cache/miyu-long-reply/rt OUT=~/.cache/miyu-long-reply/out \\
      python3 testkit/tui/long_reply_replay.py
    # 对照改前：MIYU_BIN=~/.local/bin/miyu …（同上）

两场，各一个沙箱：
- plain：说一句，回一段 3000 多字的长文；
- tool：先跑一条命令，再回同样的长文（正文夹在工具后面，走的是流水里的另一段）。
每场都是说完之后 `/dev` 切走、`/normal` 切回来，看屏幕最下面是不是长文的最后一行。
"""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

END = "长回复到此结束"
LINES = 300
REPLY = "\n".join(f"长回复第{index:04d}行" for index in range(LINES)) + "\n" + END


def squash(screen):
    return "".join(line.strip() for line in screen)


def send(master, sink, text, quiet=1.0, timeout=60):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def scenario(report, label, stub_extra):
    h.reset_view()
    stub, daemon, tui, master, sink = r.start(dict({"STUB_CHUNK_SLEEP": "0.002", "STUB_CHUNK_CHARS": "40",
                                                    "STUB_REPLY": REPLY}, **stub_extra))
    try:
        send(master, sink, h.PROMPT)
        done = r.wait_screen(master, sink, lambda s: END in squash(s), 60.0)
        report[f"{label}_reply_finishes_live"] = done is not None
        send(master, sink, "/dev", quiet=1.5, timeout=20)
        screen = send(master, sink, "/normal", quiet=2.0, timeout=30)
        r.save(f"long-reply-{label}-switched-back", screen)
        report[f"{label}_replay_keeps_the_last_line"] = END in squash(screen)
        report[f"{label}_replay_not_clipped"] = not any(line.rstrip().endswith("…") for line in screen)
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    scenario(report, "plain", {})
    scenario(report, "tool", {"STUB_TOOL": "1"})
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    passed = sum(1 for ok in report.values() if ok)
    print(f"{passed}/{len(report)} passed")
    print("产物：", h.OUT)
    sys.exit(0 if report and passed == len(report) else 1)
