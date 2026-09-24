#!/usr/bin/env python3
"""别的会话跑着后台任务时：新开的终端、`/new` 换过去的新会话，状态行上都不该出现它。

todolist 09-24：「在有会话运行着后台任务/命令的情况下，在另一个终端运行 miyu 命令，此时
会进入新会话，但是依旧会在刚开启的一小段时间看到另一个会话的后台任务/命令的状态行」。
轮询线程起来时还不知道自己是哪条会话，第一次拉到的任务表没过滤（「不知道会话 = 全显示」），
要等下一轮（约 1 秒）才换成过滤过的；`/new` 换会话之后同理，旧会话的任务要留到下一轮。

- 新终端：第二个 `miyu` 从起来到之后 5 秒，**每一个同步帧**里都不能出现第一个会话那个
  任务的标题（逐帧看，闪一下也算）。
- `/new`：第一个终端换到新会话后，新会话的大厅里一帧都不能带着旧会话的任务。

    cargo build
    python3 testkit/tui/other_session_jobs.py
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

import pyte  # noqa: E402

TITLE = "走查后台任务"
STRIP = re.compile(r"命令 [0-9a-f]{4,} · " + TITLE)
STUB = {
    "STUB_BACKGROUND": "1",
    "STUB_BACKGROUND_COMMAND": 'for i in $(seq 1 120); do echo "后台第 $i 行"; sleep 1; done',
}


class Frames:
    """边收边切帧：每个同步块收尾（或块外显示光标）时屏上是什么样。只喂新来的字节——
    每次都从头解析整段输出的话，解析本身就要几百毫秒，量出来的全是它。"""

    TOKEN = re.compile(rb"\x1b\[\?2026h|\x1b\[\?2026l|\x1b\[\?25h")

    def __init__(self, raw=b""):
        self.screen = pyte.Screen(h.COLS, h.ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.depth = 0
        self.tail = b""
        self.shots = []
        self.feed(raw)

    def feed(self, chunk):
        data = self.tail + chunk
        # 末尾几个字节可能是半截序列：连同跨过这条线的那个序列一起留到下一截，
        # 每个序列只数一次（数两次的话嵌套深度只涨不落，之后一帧都切不出来）。
        safe = max(0, len(data) - 7)
        fed = 0
        for match in self.TOKEN.finditer(data):
            if match.end() > safe:
                safe = match.start()
                break
            seq = match.group(0)
            boundary = False
            if seq == b"\x1b[?2026h":
                self.depth += 1
            elif seq == b"\x1b[?2026l":
                self.depth = max(0, self.depth - 1)
                boundary = self.depth == 0
            elif self.depth == 0:
                boundary = True
            if boundary:
                self.stream.feed(data[fed:match.end()])
                fed = match.end()
                try:
                    self.shots.append((time.time(), list(self.screen.display)))
                except IndexError:
                    pass
        self.stream.feed(data[fed:safe])
        self.tail = data[safe:]


def main():
    report = {}
    stub, daemon, tui, master, sink = r.start(STUB)
    second = None
    try:
        # 第一个会话：派一条长跑的后台命令，等它上状态行。
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lambda s: any(STRIP.search(l) for l in s), 30.0)
        report["first_session_strip_shows_job"] = screen is not None
        if screen is None:
            return report
        h.settle(master, sink, quiet=1.0, timeout=20)

        # 新终端：第二个 `miyu` 进的是新会话。
        second, master2 = h.spawn_tui()
        sink2 = bytearray()
        h.drain(master2, 5.0, sink2)
        shots = [shot for _, shot in Frames(bytes(sink2)).shots]
        leaked = [i for i, shot in enumerate(shots) if any(TITLE in line for line in shot)]
        report["_second_terminal_frames"] = len(shots)
        report["_second_terminal_leaked_frames"] = len(leaked)
        report["second_terminal_never_shows_the_other_sessions_job"] = not leaked
        if leaked:
            r.save("other-session-leak", shots[leaked[0]])
        second.terminate()

        # `/new`：旧会话的任务当场从状态行上消失。判据看帧：新会话的大厅画出来之后，
        # 不能有哪一帧还带着旧会话的任务（原来要等下一轮轮询，最多一秒）。
        os.write(master, b"/new")
        h.drain_until(master, sink, "/new", 3.0)
        # 先把之前的输出喂进去：界面只重画变了的格子，从空白屏幕起步拼不出整屏。
        collector = Frames(bytes(sink))
        before = len(collector.shots)
        mark = len(sink)
        sent = time.time()
        os.write(master, b"\r")
        while time.time() - sent < 3.0:
            h.drain(master, 0.02, sink)
            collector.feed(bytes(sink[mark:]))
            mark = len(sink)
        lobby = [(at, shot) for at, shot in collector.shots[before:]
                 if any("A G E N T" in line for line in shot)]
        stale = [at for at, shot in lobby if any(STRIP.search(line) for line in shot)]
        report["_new_session_lobby_frames"] = len(lobby)
        report["_new_session_stale_frames"] = len(stale)
        report["_new_session_stale_ms"] = round((max(stale) - sent) * 1000) if stale else 0
        report["new_session_lobby_frames_seen"] = bool(lobby)
        report["new_session_never_shows_the_old_job"] = bool(lobby) and not stale
        h.drain(master, 1.5, sink)
        report["old_job_never_comes_back_after_new"] = not any(
            STRIP.search(line) for line in h.render(bytes(sink))
        )
        r.save("other-session-after-new", h.render(bytes(sink)))
        return report
    finally:
        if second is not None and second.poll() is None:
            second.terminate()
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [k for k, v in report.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", h.OUT)
