#!/usr/bin/env python3
"""已经了结的提问，别的终端不该再弹面板；别处答了，开着的面板要自己收场。

用户 09-24：「当 AI 问了我问题，我已经回答完了，然后我退出 TUI，重新打开 TUI
进入那个 session，我又会进入同样的问问题界面，这时候我如果 esc 关掉面板的话，
会发现 AI 是在正常运行的，或者也有可能 AI 会停下。」

根因：重开的 TUI 挂到还在跑的那一轮上，让 daemon 把整轮从头补一遍，
`question.requested` 原样再来一次，TUI 当新题弹面板、卡着不往下画；Esc 两下
回发「取消这一轮」，回合还在跑就被掐了。同一件事的另一种样子：两个 TUI 同时
开着，一边答了，另一边的面板还挂着。

三个场景，一个 daemon：

- A 答完 → 关掉 TUI → 重开进同一个会话：不弹面板，问答块在，回合照常跑完；
- B 没答 → 关掉 TUI → 重开：面板照弹（题还在等），答完回合照常跑完；
- C 两个 TUI 同时开着同一个会话，在 B 屏答：A 屏的面板自己收场，两边都跑完。

跑法（先 cargo build）：

    python3 testkit/tui/question_replay.py

产物在 ~/.cache/miyu-question-replay/。用自己的端口和沙箱 home，不和别的走查抢。
"""

import codecs
import os
import select
import sys
import threading
import time
from pathlib import Path

# 在 import run 之前定好：run.py 在导入时就读这些。
_CACHE = Path.home() / ".cache" / "miyu-question-replay"
os.environ.setdefault("MIYU_HOME", str(_CACHE / "home"))
os.environ.setdefault("MIYU_TUI_RUNTIME", "/tmp/mx-question-replay")
os.environ.setdefault("MIYU_TUI_PORT", "18471")
os.environ.setdefault("STUB_PORT", "18479")
os.environ.setdefault("OUT", str(_CACHE))

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pyte  # noqa: E402
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 面板上才有的字：选项说明只在面板里列，答完的问答块只写选中的那一项。
PANEL = "第二个"
ANSWERED = "已回答"
CHOSEN = "甲选项"
# 桩在答完之后才吐的正文。
REPLY = "用于走查的回复"
CANCELLED = ("已中断", "已取消这一轮", "Interrupted")


class Term:
    """一个全屏 TUI：自己一根读取线程、自己一块虚拟屏。

    两个终端同时开着时不能轮流读：等 B 的时候不读 A，A 的 PTY 缓冲写满，进程
    就阻塞在写上，后半段全是假数据（two_views 09-19 踩过）。
    """

    def __init__(self, name, process=None, master=None, initial=b""):
        if process is None:
            process, master = h.spawn_tui()
        self.name = name
        self.process = process
        self.master = master
        self.raw = bytearray(initial)
        self.lock = threading.Lock()
        self._rebuild()
        self.alive = True
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()

    def _rebuild(self):
        self.screen = pyte.Screen(h.COLS, h.ROWS)
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.stream.feed(self.decoder.decode(bytes(self.raw)))

    def _pump(self):
        while self.alive:
            ready, _, _ = select.select([self.master], [], [], 0.1)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                break
            if not chunk:
                break
            with self.lock:
                self.raw.extend(chunk)
                try:
                    self.stream.feed(self.decoder.decode(chunk))
                except IndexError:
                    # pyte 偶尔在被覆写成空串的宽字符格上炸，整屏重放一次就过去了。
                    self._rebuild()

    def lines(self):
        with self.lock:
            try:
                return [line.rstrip() for line in self.screen.display]
            except IndexError:
                self._rebuild()
                return [line.rstrip() for line in self.screen.display]

    def has(self, text):
        return any(text in line for line in self.lines())

    def wait(self, predicate, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if predicate(self):
                return True
            time.sleep(0.1)
        return predicate(self)

    def wait_text(self, text, timeout):
        return self.wait(lambda term: term.has(text), timeout)

    def send(self, data):
        os.write(self.master, data if isinstance(data, bytes) else data.encode())

    def shot(self, tag):
        (h.OUT / f"qr-{tag}.txt").write_text("\n".join(self.lines()) + "\n", encoding="utf-8")

    def close(self):
        """拔掉终端：回合留给 daemon（用户退出 TUI 就是这样）。"""
        self.alive = False
        try:
            os.close(self.master)
        except OSError:
            pass
        r.stop(self.process)


def ask_and_wait_panel(term, message):
    """大厅画完 → 说一句 → 等提问面板弹出来。"""
    term.wait_text("A G E N T", 15.0)
    time.sleep(1.0)
    term.send(message)
    term.wait_text(message, 5.0)
    term.send(b"\r")
    return term.wait_text(PANEL, 40.0)


def reopen_into(name, message):
    """开一个新 TUI，`/new` 之后用 `/session` 切回 `message` 所在的那条会话。

    和 reopen_midturn 一样的走法（用户就是这么回去的）：新 TUI 自己是一条新会话，
    「切到当前会话」是 no-op 不走加载那条路。
    """
    term = Term(name)
    time.sleep(4.0)
    term.send(b"\x15/new\r")
    time.sleep(3.0)
    term.send(b"\x15/session\r")
    if not term.wait(
        lambda t: t.has("选择会话") or t.has("Select session"), 15.0
    ):
        return term, False
    # 当前那条排在最前；最近动过的那条就在它下面。
    term.send(b"\x1b[B")
    time.sleep(0.6)
    term.send(b"\r")
    return term, term.wait_text(message, 30.0)


def still_running(term):
    return any(line.lstrip()[:1] in r.BRAILLE for line in term.lines())


def no_cancel(term):
    return not any(term.has(mark) for mark in CANCELLED)


def scenario_answered_then_reopen(report):
    message = "问答重进走查A"
    first = Term("a1", *START)
    report["A1 第一个 TUI 弹出面板"] = ask_and_wait_panel(first, message)
    first.send(b"\r")
    report["A2 答完问答块在"] = first.wait(
        lambda t: t.has(ANSWERED) and t.has(CHOSEN) and not t.has(PANEL), 15.0
    )
    first.shot("a1-answered")
    # 回合还在跑（桩在慢慢吐思考）：这时候退出 TUI。
    time.sleep(2.0)
    first.close()

    second, loaded = reopen_into("a2", message)
    report["A3 重开后切回了那条会话"] = loaded
    # 得是挂到**还在跑**的那一轮上：跑完了的轮是从库里读历史画的，不走补发，
    # 下面几条就成了空验（第一版桩的思考太短，重开时回合已经收尾了）。
    report["A3b 重开时那一轮还在跑"] = second.wait(still_running, 5.0) and not second.has(REPLY)
    # 补发整轮要一点时间：给足 8 秒，这期间面板一次都不该出现。
    popped = second.wait_text(PANEL, 8.0)
    second.shot("a2-reopened")
    report["A4 重开不再弹已答过的面板"] = not popped
    report["A5 重开后问答块照样画出来"] = second.has(ANSWERED) and second.has(CHOSEN)
    finished = second.wait_text(REPLY, 120.0)
    second.shot("a2-finished")
    report["A6 回合照常跑完"] = finished
    report["A7 没有被取消"] = no_cancel(second)
    second.close()


def scenario_pending_then_reopen(report):
    message = "问答重进走查B"
    first = Term("b1")
    report["B1 第一个 TUI 弹出面板"] = ask_and_wait_panel(first, message)
    # 不答就走：这道题还在等。
    first.close()

    second, loaded = reopen_into("b2", message)
    report["B2 重开后切回了那条会话"] = loaded
    report["B3 还在等的题照弹面板"] = second.wait_text(PANEL, 15.0)
    second.shot("b2-panel")
    second.send(b"\r")
    report["B4 在重开的 TUI 上答得了"] = second.wait(
        lambda t: t.has(ANSWERED) and t.has(CHOSEN) and not t.has(PANEL), 15.0
    )
    report["B5 回合照常跑完"] = second.wait_text(REPLY, 120.0)
    second.shot("b2-finished")
    report["B6 没有被取消"] = no_cancel(second)
    second.close()


def scenario_two_live_views(report):
    message = "问答双屏走查C"
    first = Term("c1")
    report["C1 A 屏弹出面板"] = ask_and_wait_panel(first, message)
    second, loaded = reopen_into("c2", message)
    report["C2 B 屏进了同一条会话"] = loaded
    report["C3 B 屏也弹出面板（题还在等）"] = second.wait_text(PANEL, 15.0)
    second.shot("c2-panel")
    # 在 B 屏答。
    second.send(b"\r")
    report["C4 B 屏答完问答块在"] = second.wait(
        lambda t: t.has(ANSWERED) and t.has(CHOSEN) and not t.has(PANEL), 15.0
    )
    # A 屏的面板要自己收场，问答块照样画出来。
    closed = first.wait(
        lambda t: not t.has(PANEL) and t.has(ANSWERED) and t.has(CHOSEN), 10.0
    )
    first.shot("c1-after-b-answered")
    report["C5 A 屏面板自己收场并画出问答块"] = closed
    report["C6 A 屏回合照常跑完"] = first.wait_text(REPLY, 120.0)
    report["C7 B 屏回合照常跑完"] = second.wait_text(REPLY, 30.0)
    first.shot("c1-finished")
    second.shot("c2-finished")
    report["C8 两边都没有被取消"] = no_cancel(first) and no_cancel(second)
    first.close()
    second.close()


START = ()


def main():
    global START
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    only = set(sys.argv[1:])
    stub, daemon, tui, master, sink = r.start({
        "STUB_ASK": "1",
        # 答完之后那一段要跑得够久：关掉再开的时候它得还在跑。
        "STUB_REASONING": "1",
        # 约 50 秒：答完 → 关掉 → 重开 → /new → /session 切回去要十几秒。
        "STUB_REASONING_TEXT": "慢慢想一想这件事该怎么办才稳妥。" * 200,
        "STUB_CHUNK_CHARS": "8",
        "STUB_CHUNK_SLEEP": "0.12",
    })
    START = (tui, master, bytes(sink))
    report = {}
    try:
        if not only or "A" in only:
            scenario_answered_then_reopen(report)
        else:
            Term("a1", *START).close()
        if not only or "B" in only:
            scenario_pending_then_reopen(report)
        if not only or "C" in only:
            scenario_two_live_views(report)
    finally:
        r.stop(daemon, stub)

    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())
