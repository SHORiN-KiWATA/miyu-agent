#!/usr/bin/env python3
"""Session pickers retain the lobby and reserve a scrollable transcript viewport.

Uses a disposable MIYU_HOME, local stub, and PTY with cursor reports.
Run: python3 testkit/tui/session_picker.py --binary /absolute/path/to/miyu
"""

import argparse
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
from sync_view import SyncFeed  # noqa: E402


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--active-only", action="store_true", help="Check choosing the active session is a no-op")
    parser.add_argument("--explicit-active", action="store_true", help="Check explicit active session index is a no-op")
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    sandbox = sandbox_dir.make("miyu-session-picker-")
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=os.environ.get("OUT") or str(Path.home() / ".cache" / "miyu-session-picker"),
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
        stub, daemon, tui, master, sink = q.start({"STUB_REPLY": "\n".join(f"SESSION-BODY-{i:02d}" for i in range(1, 61))})
        processes = [tui, daemon, stub]
        screen = pyte.Screen(h.COLS, h.ROWS)
        # 按帧喂：PTY 一次读会切在一帧中间，逐块喂看到的是画了一半的屏（sync_view）。
        feed = SyncFeed(pyte.Stream(screen))
        feed.feed(bytes(sink))
        # Do not reply to startup probes while replaying old output. Cursor
        # reports must describe the live terminal position at the query.
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
                    feed.feed(chunk)
                actual = lines()
                if predicate(actual):
                    (h.OUT / f"{name}.txt").write_text("\n".join(actual))
                    return actual
            (h.OUT / f"{name}.txt").write_text("\n".join(lines()))
            raise AssertionError(f"{name}: expected screen not reached. See {h.OUT}")

        def settle(name):
            ready_at = time.monotonic() + 0.6
            return wait_for(name, lambda actual: time.monotonic() >= ready_at)

        def footer(actual):
            return any("stub-model" in line for line in actual)

        def switched(raw):
            """「已切换到会话」那条提示出现过没有。两种语言都认。"""
            return b"switched to session" in raw or "已切换到会话".encode() in raw

        # Accept both locales. The binary renders Chinese when the shell locale
        # says so, and this script used to only look for the English strings --
        # it then failed at "lobby-picker" with the picker plainly on screen.
        def picker(actual):
            return any(
                "Select session" in line or "选择会话" in line for line in actual
            ) and any("type search" in line or "输入搜索" in line for line in actual)

        wait_for("lobby-before", footer)
        if args.explicit_active:
            start = len(sink)
            os.write(master, b"\x15/session 1\r")
            settle("explicit-active-settled")
            assert not switched(sink[start:]), f"Explicit current session was reloaded. See {h.OUT}"
            print(f"PASS: explicit active selection is a no-op. Artifacts: {h.OUT}")
            return
        os.write(master, b"\x15/session\r")
        actual = wait_for("lobby-picker", picker)
        if args.active_only:
            start = len(sink)
            os.write(master, b"\r")
            wait_for("active-selected", lambda actual: footer(actual) and not picker(actual))
            settle("active-settled")
            assert not switched(sink[start:]), f"Active session was reloaded. See {h.OUT}"
            print(f"PASS: active selection is a no-op. Artifacts: {h.OUT}")
            return
        assert any("██" in line for line in actual), (
            f"Session picker erased the lobby logo. See {h.OUT}"
        )
        def menu_row(actual):
            return next(
                i
                for i, line in enumerate(actual)
                if "Select session" in line or "选择会话" in line
            )

        menu_top = menu_row(actual)
        hint_row = next(i for i, line in enumerate(actual) if "/config" in line)
        assert menu_top > hint_row, f"Picker must be below lobby hints. See {h.OUT}"
        input_left = next(line.index("┃") for line in actual if "┃" in line)
        assert actual[menu_top].index("┃") == input_left, f"Picker must align with input. See {h.OUT}"
        # Searching changes only panel height and must leave the complete lobby.
        os.write(master, b"zzzzzz")
        wait_for("lobby-no-matches", lambda actual: any("no matches" in line or "没有匹配项" in line for line in actual))
        os.write(master, b"\x7f" * len("zzzzzz"))
        wait_for("lobby-search-reset", lambda actual: picker(actual) and not any("no matches" in line or "没有匹配项" in line for line in actual))
        # Ctrl+D deletes on the spot now (user 09-20); no y/N step exists, so
        # the picker must not sprout a confirmation row. Nothing is deleted
        # here on purpose -- this lobby run shares the real home; actual
        # deletion is exercised in the disposable home further down.
        assert not any("y/N" in line for line in actual), (
            f"Picker still shows a y/N confirmation. See {h.OUT}"
        )
        for rows in (24, 40, 32):
            h.ROWS = rows
            screen.resize(lines=rows, columns=h.COLS)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, h.COLS, 0, 0))
            actual = settle(f"lobby-resize-{rows}")
            assert picker(actual) and footer(actual) and any("██" in line for line in actual), f"Resize lost lobby or picker. See {h.OUT}"
            menu_top = menu_row(actual)
            hint_row = next(i for i, line in enumerate(actual) if "/config" in line)
            assert menu_top > hint_row, f"Resize covered lobby hints. See {h.OUT}"
        os.write(master, b"\x1b")
        wait_for("lobby-cancel", lambda actual: footer(actual) and
                 any("██" in line for line in actual) and not picker(actual))
        # 命令收尾那几毫秒终端还是 cooked 的（面板的 raw 守卫已放、输入循环还没回来），
        # 这时敲的回车会被行规程改成 \n、再被当成 Ctrl+J。等一拍再打字。
        settle("lobby-cancel-settled")
        os.write(master, b"hello\r")
        wait_for("body-before", lambda actual: footer(actual) and
                 any(line.strip() == "SESSION-BODY-60" for line in actual))
        os.write(master, b"\x15/session\r")
        actual = wait_for("body-picker", picker)
        body_end = next(i for i, line in enumerate(actual)
                        if line.strip() == "SESSION-BODY-60")
        menu_top = menu_row(actual)
        active_index = next(
            i
            for i, line in enumerate(actual)
            if "* normal" in line or "* 普通" in line
        ) - menu_top
        assert menu_top > body_end + 1 and menu_top > h.ROWS // 2, (
            f"Session picker did not follow the body and displaced the editor. See {h.OUT}"
        )
        assert not actual[menu_top - 1].strip(), f"Missing body separator. See {h.OUT}"
        os.write(master, b"\x1b[5~" * 3)
        wait_for("body-page-up", lambda actual: picker(actual) and any("SESSION-BODY-01" in line for line in actual))
        os.write(master, b"\x1b[6~" * 3)
        wait_for("body-page-down", lambda actual: picker(actual) and any("SESSION-BODY-60" in line for line in actual))
        for rows in (24, 40):
            h.ROWS = rows
            screen.resize(lines=rows, columns=h.COLS)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, h.COLS, 0, 0))
            actual = wait_for(f"body-resize-{rows}", lambda actual: picker(actual) and any("SESSION-BODY-60" in line for line in actual))
            menu_top = menu_row(actual)
            assert not actual[menu_top - 1].strip(), f"Resize lost separator. See {h.OUT}"
        os.write(master, b"\x1b")
        wait_for("body-cancel", lambda actual: footer(actual) and not picker(actual) and
                 any(line.strip() == "SESSION-BODY-60" for line in actual))
        os.write(master, b"\x15/session\r")
        wait_for("body-picker-reopen", picker)
        os.write(master, b"\x1b[5~" * 3)
        wait_for("active-scrolled", lambda actual: picker(actual) and any("SESSION-BODY-01" in line for line in actual))
        start = len(sink)
        os.write(master, b"\r")
        wait_for("body-selected", lambda actual: footer(actual) and not picker(actual) and
                 any(line.strip() == "SESSION-BODY-01" for line in actual))
        actual = settle("body-selected-settled")
        assert any("SESSION-BODY-01" in line for line in actual), f"Active session selection reset scroll. See {h.OUT}"
        assert not switched(sink[start:]), f"Active session was reloaded. See {h.OUT}"
        start = len(sink)
        os.write(master, f"\x15/session {active_index}\r".encode())
        actual = settle("explicit-active-scrolled")
        assert any("SESSION-BODY-01" in line for line in actual), f"Explicit active switch reset scroll. See {h.OUT}"
        assert not switched(sink[start:]), f"Explicit active session was reloaded. See {h.OUT}"
        # Exercise actual deletion only in the disposable home, then switch
        # back to the prior conversation through its searchable user snippet.
        os.write(master, b"\x15/new PickerDelete\r")
        wait_for("new-session", lambda actual: footer(actual) and any("██" in line for line in actual))
        # 同 lobby-cancel：命令收尾那几毫秒终端还是 cooked 的，紧跟着敲的回车会变成
        # 换行，`/session` 就成了草稿。按帧喂之后大厅一画完就看得见，等一拍再打字。
        settle("new-session-settled")
        os.write(master, b"\x15/session\r")
        wait_for("delete-active-picker", lambda actual: picker(actual) and any("PickerDelete" in line for line in actual))
        # One Ctrl+D, gone -- no y/N (user 09-20: "should just delete").
        #
        # 这一行是**当前会话**，而删掉自己待着的那条现在会当场收面板、落到本
        # 车道另一条会话上（09-20：留在一条已经不存在的会话上会看见它的正文，
        # 回车还会跳进终端集成会话）。所以等的是「面板没了」，不是「列表刷新」。
        os.write(master, b"\x04")
        wait_for(
            "delete-active-done",
            lambda actual: not picker(actual)
            and not any("PickerDelete" in line for line in actual),
        )
        assert not any(
            "y/N" in line for line in lines()
        ), f"Ctrl+D still asked for confirmation. See {h.OUT}"
        assert not any(
            "终端集成会话" in line for line in lines()
        ), f"Deleting the active session landed on the terminal session. See {h.OUT}"
        settle("delete-active-settled")
        # 面板已经收掉了（删的是当前会话，09-20 起会当场离开）：重新打开，
        # 再按摘要搜回原来那条会话。
        os.write(master, b"\x15/session\r")
        wait_for("picker-reopened-after-delete", lambda actual: picker(actual)
            and not any(
                "switched to session" in line or "已切换到会话" in line for line in actual
            ))
        os.write(master, b"hello")
        wait_for(
            "switch-search",
            lambda actual: picker(actual)
            and any(
                "hello" in line and ("normal" in line or "普通" in line)
                for line in actual
            ),
        )
        os.write(master, b"\r")
        wait_for("switched-back", lambda actual: footer(actual) and not picker(actual) and any("SESSION-BODY-60" in line for line in actual))
        print(f"PASS: lobby/body placement, scroll, resize, search, deletion, selection, active no-op. Artifacts: {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "session-picker.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
