#!/usr/bin/env python3
"""空会话大厅的动画在斜杠命令前后、以及面板开着的时候都要一直走。

每 0.4s 抓一次星空那几行(0..7),连续 6 次里看有几次和上一次不同。基线应 6/6;
/session /effort /models 面板开着时至少 4/6(以前是 0/6:面板循环从不推动画);
关掉面板、/config 按 q 退出后回到大厅都应回到 ≥5/6。

和 session_picker.py 同一套骨架:一次性 MIYU_HOME、桩模型、带光标应答的 PTY。
Run: python3 testkit/tui/lobby_anim.py --binary /absolute/path/to/miyu
"""

import argparse
import os
import select
import socket
import time
from pathlib import Path
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402
from sync_view import SyncFeed  # noqa: E402


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    sandbox = sandbox_dir.make("miyu-lobby-anim-")
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=os.environ.get("OUT") or str(Path.home() / ".cache" / "miyu-lobby-anim"),
    )
    import round26 as q
    import pyte

    h = q.h
    h.BIN = args.binary.resolve()
    h.kill_stale_daemon = lambda: None
    h.EDIT_FILE = sandbox / "edit.txt"
    h.COLS, h.ROWS = 100, 32
    processes = []
    try:
        stub, daemon, tui, master, sink = q.start({"STUB_REPLY": "BODY"})
        processes = [tui, daemon, stub]
        screen = pyte.Screen(h.COLS, h.ROWS)
        # 按帧喂：PTY 一次读会切在一帧中间，逐块喂看到的是画了一半的屏（sync_view）。
        feed = SyncFeed(pyte.Stream(screen))
        feed.feed(bytes(sink))
        screen.write_process_input = lambda data: (
            os.write(master, data.encode()) if data.endswith("R") else None
        )

        def lines():
            return ["".join(screen.buffer[y][x].data for x in range(h.COLS)).rstrip()
                    for y in range(h.ROWS)]

        def pump(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    chunk = os.read(master, 65536)
                    if not chunk:
                        break
                    sink.extend(chunk)
                    feed.feed(chunk)

        def stars():
            return "\n".join(lines()[0:7])

        def state():
            actual = lines()
            lobby = any("██" in line for line in actual)
            return dict(
                lobby=lobby,
                menu=any("Enter" in line and ("取消" in line or "完成" in line or "确认" in line) for line in actual),
                # 设置界面 09-20 改用引导那套版面（main `a87ef15b`），标题从
                # 「MIYU 配置」变成「◉ 配置」，判据跟着换成只有它才有的那行。
                #
                # **不再要求大厅已经消失**：新版面自带 banner，艺术字和星空本来
                # 就该留在上面（用户 09-20 确认是新设计）。原来那条
                # `not lobby` 是按旧版面写的。
                config=any("保存并退出" in line for line in actual),
                footer=any("stub-model" in line for line in actual),
            )

        def save(name, rows):
            """存一屏产物。名字里的 `/` 要换掉——`/session-open` 这种名字拼进
            路径会变成**绝对路径**，写不进去反而抛 PermissionError，把真正的
            失败盖掉（09-20 撞到）。"""
            safe = name.replace("/", "").strip() or "screen"
            (h.OUT / f"{safe}.txt").write_text("\n".join(rows))

        def wait_for(name, predicate, timeout=10):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                pump(0.1)
                if predicate(state()):
                    return
            save(name, lines())
            raise AssertionError(f"{name}: expected screen not reached. See {h.OUT}")

        def measure(name, at_least):
            previous = stars()
            changed = 0
            for _ in range(6):
                pump(0.4)
                current = stars()
                if current != previous:
                    changed += 1
                previous = current
            print(f"  {name}: {changed}/6 star frames changed")
            if changed < at_least:
                save(name, lines())
                raise AssertionError(f"{name}: animation stalled ({changed}/6 < {at_least}). See {h.OUT}")

        wait_for("lobby", lambda s: s["footer"] and s["lobby"])
        measure("baseline", 5)

        def measure_while_typing(name, keys, at_least):
            """按住某个键不放的时候，星空还动不动。

            用户 09-20：「在 TUI 空会话按住退格键时，动画会暂停」。根因是推帧
            只写在「这一轮没有输入」那条分支里，按键比 40ms 一拍还密时那条分支
            一次都进不去。和 09-17 面板那次同源，当时只改了面板。
            """
            # **一次灌一串**，中间不留空档：真正的按键自动重复就是连续字节流。
            # 第一版每按一下 `pump(0.04)`，等于留出 40ms 空档，空闲分支照样进得
            # 去，于是撤掉修复也全过——等于没测（09-20）。
            # 要的是**持续**的按键流（真实自动重复约每秒几十下），不是一次灌
            # 一大串。一次灌完会被抽干一次、然后空闲，空闲分支照样进得去——头
            # 两版这么写，撤掉修复也全过，等于没测（09-20）。
            previous = stars()
            changed = 0
            for _ in range(6):
                deadline = time.monotonic() + 0.5
                while time.monotonic() < deadline:
                    os.write(master, keys)
                    # `pump(0.0)` **一个字节都不读**（它的循环条件当场为假）：
                    # 洪流期间 PTY 从没被抽干，渲染的是陈旧画面，星空当然
                    # 「不变」——我据此误判过一次「复现了」（09-20）。
                    pump(0.01)
                current = stars()
                if current != previous:
                    changed += 1
                previous = current
                whole = "\n".join(lines())
                if not hasattr(measure_while_typing, "_last"):
                    measure_while_typing._last = ""
                if whole != measure_while_typing._last:
                    changed_whole = changed_whole + 1 if "changed_whole" in dir() else 1
                measure_while_typing._last = whole
            print(f"  {name}: {changed}/6 star frames changed while holding a key")
            save(f"{name}-during", lines())
            if changed < at_least:
                save(name, lines())
                raise AssertionError(
                    f"{name}: animation stalled while typing ({changed}/6 < {at_least})."
                    f" See {h.OUT}"
                )

        # 先打一串字，再按住退格删——空输入时的退格和有内容时的退格走的分支不同。
        os.write(master, "删删删删删删删删删删".encode())
        pump(0.4)
        measure_while_typing("holding-backspace", b"\x7f", 4)
        # 空输入下继续按住退格（已经没东西可删了）。
        measure_while_typing("holding-backspace-empty", b"\x7f", 4)
        # 普通打字同理。
        measure_while_typing("holding-letter", b"a", 4)
        # 用**退格**清干净：`Ctrl+U` 在这个编辑器里不是清行，留着的字会把
        # 后面的 `/session` 变成普通文本，菜单就开不出来（09-20 实测）。
        # 按住字母那一步敲进去多少个字看测具跑多快（3 秒里每轮写一个字），一次
        # 80 个退格不一定删得完（按帧喂之后循环变快，敲进去九十几个）：删到输入框
        # 里没有字母为止。
        for _ in range(20):
            os.write(master, b"\x7f" * 80)
            pump(0.4)
            if not any("┃ a" in line for line in lines()):
                break
        def measure_while_navigating(name, at_least):
            # 按住 j/k 在面板里换行:按键比 40ms 一拍还密,节拍要按时刻算才推得出帧
            # (以前按「等满 40ms 没按键」算,扫光一顿一顿——用户实测)。
            previous = stars()
            changed = 0
            for step in range(6):
                for _ in range(8):
                    os.write(master, b"j" if step % 2 == 0 else b"k")
                    pump(0.05)
                current = stars()
                if current != previous:
                    changed += 1
                previous = current
            print(f"  {name}: {changed}/6 star frames changed while navigating")
            if changed < at_least:
                save(name, lines())
                raise AssertionError(f"{name}: animation stalled while navigating ({changed}/6 < {at_least}). See {h.OUT}")

        for command in ("/session", "/effort", "/models"):
            os.write(master, b"\x15" + command.encode() + b"\r")
            wait_for(f"{command}-open", lambda s: s["menu"])
            pump(0.3)
            measure(f"{command}-open", 4)
            if command == "/session":
                measure_while_navigating("/session-navigating", 4)
            os.write(master, b"\x1b")
            wait_for(f"{command}-closed", lambda s: not s["menu"] and s["lobby"])
            pump(0.3)
            measure(f"{command}-closed", 5)
        os.write(master, b"\x15/config\r")
        wait_for("config-open", lambda s: s["config"])
        pump(0.3)
        os.write(master, b"q")
        wait_for("config-closed", lambda s: s["lobby"] and s["footer"] and not s["config"])
        pump(0.5)
        measure("after-config", 5)
        print(f"PASS: lobby animation keeps running through panels and /config. Artifacts: {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "lobby-anim.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
