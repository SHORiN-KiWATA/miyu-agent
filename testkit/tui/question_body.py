#!/usr/bin/env python3
"""Question panels must keep the assistant's last row visible and scrollable.

Uses a disposable MIYU_HOME and a local stub, without touching the production daemon.
Run: python3 testkit/tui/question_body.py --binary /absolute/path/to/miyu
"""

import argparse
import fcntl
import json
import os
import socket
import struct
import termios
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
    parser.add_argument("--direct", action="store_true", help="Exercise the direct REPL event handler")
    args = parser.parse_args()
    os.environ.pop("MIYU_DIRECT", None)
    if args.direct:
        os.environ["MIYU_DIRECT"] = "1"
    sandbox = sandbox_dir.make("miyu-question-body-")
    os.environ.update(
        MIYU_HOME=str(sandbox / "home"),
        MIYU_TUI_RUNTIME=str(sandbox / "run"),
        MIYU_TUI_PORT=str(free_port()),
        STUB_PORT=str(free_port()),
        OUT=os.environ.get("OUT") or str(Path.home() / ".cache" / "miyu-question-body"),
        # The checks below read the English tab labels ("Review", not 「确认」). Pin the
        # interface language so a Chinese shell locale does not flip them. It must be
        # set before `import round26`: the harness copies the environment on import.
        MIYU_LANG="en",
    )
    import round26 as q

    h = q.h
    h.BIN = args.binary.resolve()
    h.kill_stale_daemon = lambda: None
    h.EDIT_FILE = sandbox / "edit.txt"
    h.COLS, h.ROWS = 100, 32
    questions = [
        {"header": "First", "question": "FIRST-QUESTION", "custom": False,
         "options": [{"label": "First answer", "description": "Short description"}]},
        {"header": "Second", "question": "SECOND-QUESTION", "custom": False,
         "options": [{"label": f"Second answer {i}", "description": "Longer panel"}
                     for i in range(4)]},
    ]
    processes = []
    try:
        stub, daemon, tui, master, sink = q.start({
            "STUB_ASK": "1",
            "STUB_ASK_PREFACE": "".join(f"BODY-ROW-{i:02d}\n" for i in range(1, 24)),
            "STUB_ASK_QUESTIONS": json.dumps(questions),
            "STUB_REPLY": "QUESTION-TEST-DONE",
        }, direct=args.direct)
        processes = [tui, daemon, stub]
        os.write(master, b"question regression\r")

        def check(name, required):
            screen = q.wait_screen(master, sink,
                                   lambda lines: all(any(text in line for line in lines)
                                                     for text in required), 8)
            actual = screen if screen is not None else h.render(bytes(sink))
            (h.OUT / f"{name}.txt").write_text("\n".join(actual))
            assert screen is not None, f"{name}: missing {required}. See {h.OUT}"
            if name != "answered":
                panel_top = next(i for i, line in enumerate(actual)
                                 if "First" in line and "Second" in line and "Review" in line)
                assert panel_top > 0 and not actual[panel_top - 1].strip(), (
                    f"{name}: no blank line between reply and question panel. See {h.OUT}"
                )

        check("initial", ["FIRST-QUESTION", "BODY-ROW-23"])
        os.write(master, b"\x1b[5~" * 3)
        check("page-up", ["FIRST-QUESTION", "BODY-ROW-01"])
        os.write(master, b"\x1b[6~" * 3)
        check("page-down", ["FIRST-QUESTION", "BODY-ROW-23"])
        os.write(master, b"\x1b[C")
        check("taller-question", ["SECOND-QUESTION", "BODY-ROW-23"])
        os.write(master, b"\x1b[D")
        check("shorter-question", ["FIRST-QUESTION", "BODY-ROW-23"])
        for rows in (24, 40):
            h.ROWS = rows
            h._VIEW["screen"].resize(lines=rows, columns=h.COLS)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, h.COLS, 0, 0))
            h.drain(master, 0.4, sink)
            check(f"resize-{rows}", ["FIRST-QUESTION", "BODY-ROW-23"])
        # Select each answer, move to review, and submit.
        os.write(master, b"\r\r\r")
        check("answered", ["QUESTION-TEST-DONE"])
        mode = "direct" if args.direct else "daemon"
        print(f"PASS ({mode}): initial layout, scrolling, question heights, resize, answer. Artifacts: {h.OUT}")
    finally:
        if "sink" in locals():
            (h.OUT / "question.raw").write_bytes(sink)
        q.stop(*processes)


if __name__ == "__main__":
    main()
