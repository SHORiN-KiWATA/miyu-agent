#!/usr/bin/env python3
"""A notification must expire while a reply is still streaming, in both REPL modes."""

import argparse
import os
import socket
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
    parser.add_argument("--direct", action="store_true")
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    if args.direct:
        os.environ["MIYU_DIRECT"] = "1"
    sandbox = sandbox_dir.make("miyu-toast-expiry-")
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=os.environ.get("OUT") or str(Path.home() / ".cache" / "miyu-toast-expiry"),
    )
    import round26 as q

    h = q.h
    h.BIN = args.binary.resolve()
    h.kill_stale_daemon = lambda: None
    h.EDIT_FILE = sandbox / "edit.txt"
    h.COLS, h.ROWS = 100, 32
    processes = []
    try:
        stub, daemon, tui, master, sink = q.start({
            "STUB_REPLY": "STREAM-START\n" + "Reply still streaming.\n" * 100 + "STREAM-END",
            "STUB_CHUNK_CHARS": "20",
            "STUB_CHUNK_SLEEP": "0.15",
        }, direct=args.direct)
        processes = [tui, daemon, stub]

        def has_toast(lines):
            return any("press Ctrl+D to exit" in line or "要退出请按 Ctrl+D" in line
                       for line in lines)

        ready = q.wait_screen(master, sink, lambda lines: any("NORMAL" in line for line in lines), 15)
        assert ready is not None, f"editor did not become ready: {h.OUT}"
        if args.direct:
            # Direct mode creates its live fullscreen tail on the first chat.
            os.write(master, b"initialize direct tail\r")
            completed = q.wait_screen(master, sink,
                                      lambda lines: any("STREAM-END" in line for line in lines), 35)
            assert completed is not None, f"direct warmup did not finish: {h.OUT}"
            h.drain(master, 0.5, sink)
        else:
            # Run one command before exercising the notification lifetime.
            #
            # 原来是 `/help\r` 一次写进去、等屏上出现 `Ctrl+D`——等到的其实是敲 `/`
            # 那一帧弹出的命令候选（「/session … 菜单内 Ctrl+D 删除」），不是帮助
            # （大厅里 /help 的输出被大厅盖着，看不见）。09-23 起编辑重画攒到一批
            # 按键处理完再画，一次到达的 `/help\r` 不再逐键出帧，候选框没机会闪
            # 出来。所以先等回显、再回车、再等输入框清空——这才是「命令被收下了」。
            os.write(master, b"/help")
            echoed = q.wait_screen(master, sink,
                                   lambda lines: any("┃ /help" in line for line in lines), 5)
            assert echoed is not None, f"/help was not echoed: {h.OUT}"
            os.write(master, b"\r")
            help_screen = q.wait_screen(master, sink,
                                        lambda lines: not any("┃ /help" in line for line in lines), 5)
            assert help_screen is not None, f"/help was not submitted: {h.OUT}"

        # Ctrl+C on an empty idle editor shows a harmless exit hint.
        os.write(master, b"\x03")
        shown = q.wait_screen(master, sink, has_toast, 3)
        assert shown is not None, f"notification was not shown: {h.OUT}"
        (h.OUT / "shown.txt").write_text("\n".join(shown))
        os.write(master, b"toast regression\r")
        streaming = q.wait_screen(master, sink,
                                  lambda lines: any("STREAM-START" in line for line in lines), 8)
        assert streaming is not None, f"reply did not start: {h.OUT}"
        assert has_toast(streaming), f"notification vanished before expiry was exercised: {h.OUT}"
        expired = q.wait_screen(master, sink, lambda lines: not has_toast(lines), 5)
        actual = expired if expired is not None else h.render(bytes(sink))
        (h.OUT / "after-expiry.txt").write_text("\n".join(actual))
        assert expired is not None, f"notification remained throughout streaming: {h.OUT}"

        def current_reply(lines):
            start = max(i for i, line in enumerate(lines) if "STREAM-START" in line)
            return lines[start:]

        # Direct mode's completed warmup can still be visible above this reply.
        assert not any("STREAM-END" in line for line in current_reply(expired)), (
            "reply ended before expiry check"
        )
        # New content must keep arriving after the notification is erased.
        before = sum("Reply still streaming." in line for line in current_reply(expired))
        h.drain(master, 0.5, sink)
        after = current_reply(h.render(bytes(sink)))
        assert sum("Reply still streaming." in line for line in after) > before, (
            "reply stopped growing after notification expiry"
        )
        print(f"PASS ({'direct' if args.direct else 'daemon'}): toast expired during streaming. {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "toast.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
