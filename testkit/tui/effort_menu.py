#!/usr/bin/env python3
"""/effort、/persona、/models 三个菜单在全屏 TUI 里的位置：大厅里贴在提示下方、与输入框
左对齐、不擦星空；会话里贴在正文底部。缩放、取消、确认之后版面都要还原。

和 session_picker.py 同一套骨架：一次性 MIYU_HOME、桩模型、带光标应答的 PTY。
Run: python3 testkit/tui/effort_menu.py --binary /absolute/path/to/miyu [--command /persona] [--observe]
--observe 只截屏打印不断言（复现用）。
"""

import argparse
import codecs
import fcntl
import struct
import termios
import os
import select
import socket
import time
from pathlib import Path
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--observe", action="store_true", help="只截屏打印，不断言")
    parser.add_argument("--command", default="/effort", help="大厅里敲的命令（复现别的菜单用）")
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    sandbox = sandbox_dir.make("miyu-effort-menu-")
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=os.environ.get("OUT") or str(Path.home() / ".cache" / "miyu-effort-menu"),
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
        stub, daemon, tui, master, sink = q.start({"STUB_REPLY": "\n".join(f"BODY-{i:02d}" for i in range(1, 41))})
        processes = [tui, daemon, stub]
        screen = pyte.Screen(h.COLS, h.ROWS)
        stream = pyte.Stream(screen)
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        stream.feed(decoder.decode(bytes(sink)))
        screen.write_process_input = lambda data: (
            os.write(master, data.encode()) if data.endswith("R") else None
        )

        def lines():
            return ["".join(screen.buffer[y][x].data for x in range(h.COLS)).rstrip()
                    for y in range(h.ROWS)]

        def wait_for(name, predicate, timeout=8):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    chunk = os.read(master, 65536)
                    if not chunk:
                        break
                    sink.extend(chunk)
                    stream.feed(decoder.decode(chunk))
                actual = lines()
                if predicate(actual):
                    (h.OUT / f"{name}.txt").write_text("\n".join(actual))
                    return actual
            (h.OUT / f"{name}.txt").write_text("\n".join(lines()))
            raise AssertionError(f"{name}: expected screen not reached. See {h.OUT}")

        def settle(name):
            ready_at = time.monotonic() + float(os.environ.get("OBSERVE_SETTLE", "0.8"))
            return wait_for(name, lambda actual: time.monotonic() >= ready_at, timeout=60)

        def dump(name, actual):
            print(f"===== {name}")
            for i, line in enumerate(actual):
                print(f"{i:02d}|{line}")

        def footer(actual):
            return any("stub-model" in line for line in actual)

        # 面板抬头按命令来：/effort「思考档位」、/persona「选择人格」、/models「选择模型」。
        title = {"/effort": "思考档位", "/variant": "思考档位", "/persona": "选择人格", "/models": "选择模型"}.get(args.command.split()[0], "")

        def menu(actual):
            return any(title in line for line in actual) and any("Enter" in line for line in actual)

        wait_for("lobby-before", footer)
        os.write(master, b"\x15" + args.command.encode() + b"\r")
        if args.observe:
            actual = settle("lobby-effort")
            dump("lobby-effort", actual)
            os.write(master, b"\x1b")
            actual = settle("lobby-after-esc")
            dump("lobby-after-esc", actual)
            os.write(master, b"hello\r")
            actual = settle("body-before")
            dump("body-before", actual)
            os.write(master, b"\x15" + args.command.encode() + b"\r")
            actual = settle("body-effort")
            dump("body-effort", actual)
            os.write(master, b"\x1b")
            actual = settle("body-after-esc")
            dump("body-after-esc", actual)
            print(f"OBSERVED. Artifacts: {h.OUT}")
            return
        actual = wait_for("lobby-menu", menu)
        assert any("██" in line for line in actual), f"/effort erased the lobby logo. See {h.OUT}"
        menu_top = next(i for i, line in enumerate(actual) if title in line)
        hint_row = next(i for i, line in enumerate(actual) if "/config" in line)
        assert menu_top > hint_row, f"Menu must be below lobby hints. See {h.OUT}"
        input_left = next(line.index("┃") for line in actual if "┃" in line)
        assert actual[menu_top].index("┃") == input_left, f"Menu must align with input. See {h.OUT}"
        # 星空还在：菜单没从第 0 列起笔盖掉左边那片。
        assert any(ch in actual[menu_top][:input_left] for ch in "✶✦+.") or not actual[menu_top][:input_left].strip(), (
            f"Menu row left of the input box must be lobby background. See {h.OUT}"
        )
        assert all("┃" not in line[:input_left] for line in actual), f"Menu drew a bar left of the input box. See {h.OUT}"
        for rows in (24, 40, 32):
            h.ROWS = rows
            screen.resize(lines=rows, columns=h.COLS)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, h.COLS, 0, 0))
            actual = settle(f"lobby-resize-{rows}")
            assert menu(actual) and footer(actual) and any("██" in line for line in actual), f"Resize lost lobby or menu. See {h.OUT}"
            menu_top = next(i for i, line in enumerate(actual) if title in line)
            hint_row = next(i for i, line in enumerate(actual) if "/config" in line)
            assert menu_top > hint_row, f"Resize covered lobby hints. See {h.OUT}"
        os.write(master, b"\x1b")
        wait_for("lobby-cancel", lambda a: footer(a) and any("██" in l for l in a) and not menu(a))
        # 命令收尾那几百毫秒里终端是 cooked 的（面板的 raw 守卫已放、输入循环还没回来），
        # 这时敲的回车会被行规程改成 \n、再被当成 Ctrl+J（插入换行）。真人不会在
        # 面板关掉后 100ms 内回车；测具等一拍再打字。
        settle("lobby-cancel-settled")
        os.write(master, b"hello\r")
        wait_for("body-before", lambda a: footer(a) and any(l.strip() == "BODY-40" for l in a))
        os.write(master, b"\x15" + args.command.encode() + b"\r")
        actual = wait_for("body-menu", menu)
        body_end = next(i for i, line in enumerate(actual) if line.strip() == "BODY-40")
        menu_top = next(i for i, line in enumerate(actual) if title in line)
        assert menu_top > body_end + 1 and menu_top > h.ROWS // 2, f"Menu did not follow the body. See {h.OUT}"
        assert not actual[menu_top - 1].strip(), f"Missing body separator. See {h.OUT}"
        for rows in (24, 40):
            h.ROWS = rows
            screen.resize(lines=rows, columns=h.COLS)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, h.COLS, 0, 0))
            actual = wait_for(f"body-resize-{rows}", lambda a: menu(a) and any("BODY-40" in l for l in a))
        os.write(master, b"\x1b[5~" * 3)
        wait_for("body-page-up", lambda a: menu(a) and any("BODY-01" in l for l in a))
        os.write(master, b"\x1b[6~" * 3)
        wait_for("body-page-down", lambda a: menu(a) and any("BODY-40" in l for l in a))
        os.write(master, b"\r")
        wait_for("body-selected", lambda a: footer(a) and not menu(a) and any(l.strip() == "BODY-40" for l in a))
        print(f"PASS {args.command}: lobby/body placement, resize, page scroll, cancel, confirm. Artifacts: {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "effort-menu.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
