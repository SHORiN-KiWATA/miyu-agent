#!/usr/bin/env python3
"""Check streaming redraw transactions in a sandbox PTY.

Usage: python3 testkit/tui/cursor_sync.py [--binary /absolute/path/to/miyu]
       Add --direct to exercise the in-process event loop.

The terminal may render between any two writes. Hiding its cursor does not
hide the position from a multiplexer or a cursor trail. Every streamed body
write must therefore belong to a complete synchronized update. This checks
the ANSI protocol, independently of PTY read boundaries and machine speed.
"""

import argparse
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import time

import pyte
import round26 as harness
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]


class Screen(pyte.Screen):
    def __init__(self, columns, lines):
        super().__init__(columns, lines)
        self.synchronized = False
        self.streaming = False
        self.recent = ""
        self.unguarded = []
        self.protocol_errors = []

    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 2026 in modes:
            if self.synchronized:
                self.protocol_errors.append("nested begin")
            self.synchronized = True
        super().set_mode(*modes, **kwargs)

    def reset_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 2026 in modes:
            if not self.synchronized:
                self.protocol_errors.append("unmatched end")
            self.synchronized = False
        super().reset_mode(*modes, **kwargs)

    def draw(self, data):
        self.recent = (self.recent + data)[-80:]
        if "CURSOR-PROBE" in self.recent:
            self.streaming = True
        if self.streaming and not self.synchronized and self.cursor.y < self.lines - 6:
            self.unguarded.append((self.cursor.x, self.cursor.y, data))
        super().draw(data)


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--direct", action="store_true")
    args = parser.parse_args()
    h = harness.h
    base = sandbox_dir.make("miyu-cursor-sync-")
    h.HOME, h.RUNTIME = base / "home", str(base / "run")
    h.OUT = Path(os.environ.get("OUT") or Path.home() / ".cache" / "miyu-cursor-sync")
    h.EDIT_FILE = base / "unused-edit.txt"
    h.PORT, h.STUB_PORT = free_port(), free_port()
    h.BASE = f"http://127.0.0.1:{h.PORT}"
    if args.binary:
        h.BIN = args.binary.resolve()
    h.COLS, h.ROWS = 120, 40
    h.ENV.update(MIYU_HOME=str(h.HOME), XDG_RUNTIME_DIR=h.RUNTIME, MIYU_TUI="1")
    h.ENV.pop("MIYU_DIRECT", None)
    if args.direct:
        h.ENV["MIYU_DIRECT"] = "1"
    # These ports belong to this run. Never stop an unrelated daemon.
    h.kill_stale_daemon = lambda: None
    print(f"Artifacts: {h.OUT}", flush=True)
    processes = []
    screen = Screen(h.COLS, h.ROWS)
    stream = pyte.ByteStream(screen)
    raw = bytearray()
    try:
        stub_env = {
            "STUB_REPLY": "\n".join(
                f"CURSOR-PROBE-{row:02} streaming body" for row in range(1, 9)
            ) + "\nCURSOR-END",
            "STUB_CHUNK_SLEEP": "0.01",
        }
        if args.direct:
            # A direct core cannot share its home with a running daemon.
            Path(h.RUNTIME).mkdir()
            h.OUT.mkdir(parents=True, exist_ok=True)
            h.write_config()
            stub = subprocess.Popen(
                [sys.executable, str(h.SMOKE / "stub_llm.py")],
                env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), **stub_env),
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            processes = [stub]
            assert h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"), "stub not ready"
            tui, master = h.spawn_tui()
            processes.insert(0, tui)
            initial = bytearray()
        else:
            stub, daemon, tui, master, initial = harness.start(stub_env)
            processes = [tui, daemon, stub]
        raw.extend(initial)
        stream.feed(initial)
        ready_deadline = time.monotonic() + 30
        while not any("┃" in row for row in screen.display) and time.monotonic() < ready_deadline:
            if tui.poll() is not None:
                break
            if select.select([master], [], [], 0.1)[0]:
                data = os.read(master, 65536)
                raw.extend(data)
                stream.feed(data)
        assert any("┃" in row for row in screen.display), "input never became ready"
        os.write(master, b"probe\r")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if tui.poll() is not None:
                raise AssertionError("TUI exited before the reply")
            if not select.select([master], [], [], 0.1)[0]:
                continue
            data = os.read(master, 65536)
            raw.extend(data)
            stream.feed(data)
            if not screen.synchronized and "CURSOR-END" in "\n".join(screen.display):
                break
        complete = "CURSOR-END" in "\n".join(screen.display)
        report = {
            "reply_complete": complete,
            "unguarded_body_writes": len(screen.unguarded),
            "examples": screen.unguarded[:10],
            "protocol_errors": screen.protocol_errors,
            "synchronized_at_end": screen.synchronized,
        }
        (h.OUT / "pty.bin").write_bytes(raw)
        (h.OUT / "screen.txt").write_text("\n".join(screen.display))
        (h.OUT / "report.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report, indent=2))
        assert complete and screen.streaming, "streaming reply was not exercised"
        assert not screen.protocol_errors and not screen.synchronized, "unbalanced updates"
        assert not screen.unguarded, "streaming body escaped its synchronized update"
    finally:
        (h.OUT / "pty.bin").write_bytes(raw)
        (h.OUT / "screen.txt").write_text("\n".join(screen.display))
        harness.stop(*processes)


if __name__ == "__main__":
    main()
