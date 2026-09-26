#!/usr/bin/env python3
"""回合跑着的时候 `/dev` 切走、再 `/normal` 切回来，之前说的那句话还在不在。

用户 09-20 实测：「我 miyu 命令开启一个空会话，然后说一句话，在 AI 输出过程中
运行 /dev 或者 /normal 切换到另一个模式的最近会话，然后再切回来，这个时候我之前
说的那句话就看不到了」。

根因：换会话会先 `wipe_transcript` 再按库回放，而 `session_replay` 的
`WHERE status IN ('completed','interrupted')` **把正在跑的那一轮排除在外**（它的
正文还没落库）。于是在飞的那一轮连同用户刚说的那句话整个从屏幕上消失。

修法：切进一条会话之后，它要是有正在跑的回合就**从头挂上去跟随**——用户消息由
`turn.started` 的 `display_content` 画出来，和同一个会话开第二个 TUI 是同一条路。

跑之前给它一套私有的端口和沙箱家，别跟别的走查抢（09-20 被另一个会话的走查清过
一次家，读数全废）：

    cargo build
    MIYU_HOME=/tmp/miyu-switchback/home MIYU_TUI_PORT=18455 STUB_PORT=18456 \\
      MIYU_TUI_RUNTIME=/tmp/mx-switchback OUT=~/.cache/miyu-switchback \\
      python3 testkit/tui/switch_back.py
"""

import os
import sqlite3
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

LONG = "这一段是为了把回合拖长，好让人来得及在中途切走再切回来。" * 200
STUB = {"STUB_CHUNK_SLEEP": "0.05", "STUB_CHUNK_CHARS": "6", "STUB_REPLY": LONG}


def streaming(master, sink):
    """屏幕还在不在变 —— 「回合还在跑」最直接的证据。"""
    before = "\n".join(h.render(bytes(sink)))
    h.drain(master, 0.7, sink)
    return "\n".join(h.render(bytes(sink))) != before


def db():
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    return candidates[0] if candidates else None


def query(sql, args=()):
    path = db()
    if not path:
        return []
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return connection.execute(sql, args).fetchall()
    finally:
        connection.close()


def pointer(persona):
    rows = query(
        "SELECT value FROM app_state WHERE key = ?",
        (f"repl_session_persona:{persona}",),
    )
    return rows[0][0] if rows else None


def say(master, sink, text, wait_stream=True):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    if not wait_stream:
        return h.render(bytes(sink))
    deadline = time.time() + 30
    while time.time() < deadline:
        h.drain(master, 0.2, sink)
        if any("这一段是为了把回合拖长" in line for line in h.render(bytes(sink))):
            break
    return h.render(bytes(sink))


def found_above(master, sink, text, pages=12):
    """切回来是按库完整回放、停在最底下（会话项目第 2 段）：这一轮的回复很长，用户那句在
    上面几屏。一页页往上翻，找得到就算在；翻完回到底，别影响后面的步骤。"""
    screen = h.render(bytes(sink))
    found = any(text in line for line in screen)
    turned = 0
    while not found and turned < pages:
        os.write(master, b"\x1b[5~")
        h.settle(master, sink, quiet=0.3, timeout=4)
        turned += 1
        found = any(text in line for line in h.render(bytes(sink)))
    for _ in range(turned):
        os.write(master, b"\x1b[6~")
    if turned:
        h.settle(master, sink, quiet=0.3, timeout=4)
    return found


def command(master, sink, text, wait=4.0):
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    h.drain(master, wait, sink)
    return h.render(bytes(sink))


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        # 先让开发车道有一条有内容的会话，`/dev` 才有地方可去。
        command(master, sink, "/dev")
        say(master, sink, "开发车道的老话")
        h.settle(master, sink, quiet=2.0, timeout=240)
        command(master, sink, "/normal")
        report["开发车道备好了一条会话"] = pointer("dev") is not None

        # 正戏：普通车道说一句，**输出中途**切走再切回来。
        say(master, sink, "这句话不能丢")
        normal_session = pointer("default")
        report["回合真的在流"] = streaming(master, sink)

        screen = command(master, sink, "/dev")
        r.save("switchback-dev", screen)
        report["切走之后看得见开发车道那条会话"] = found_above(master, sink, "开发车道的老话")

        screen = command(master, sink, "/normal", wait=6.0)
        r.save("switchback-normal", screen)
        # 这一条就是用户报的那个 bug。
        report["切回来还看得见之前说的那句话"] = any(
            "这句话不能丢" in line for line in screen
        )
        report["切回来还是同一条会话"] = pointer("default") == normal_session
        report["切回来不是空会话大厅"] = not any(
            "A G E N T" in line for line in screen
        )

        # 挂回去之后这一轮要照常跑完，不能被切会话掐断。
        h.settle(master, sink, quiet=3.0, timeout=300)
        statuses = [
            row[0]
            for row in query(
                "SELECT status FROM turns WHERE session_id = ?", (normal_session,)
            )
        ]
        report["_那条会话的轮状态"] = statuses
        report["被切走的那一轮照样跑完"] = "completed" in statuses

        # 跑完之后再切一趟：这次走的是普通回放那条路，也得看得见。
        command(master, sink, "/dev")
        screen = command(master, sink, "/normal", wait=6.0)
        r.save("switchback-again", screen)
        report["跑完之后再切一趟也看得见"] = found_above(master, sink, "这句话不能丢")
        return report
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{sum(1 for v in checks.values() if v)}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    print("产物：", h.OUT)
