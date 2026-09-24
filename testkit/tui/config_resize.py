#!/usr/bin/env python3
"""Config menus must repaint on resize and retain editing/dialog state.

Uses an isolated MIYU_HOME, a real PTY, and no model requests or daemon.

2026-09-20: the config TUI paints through ratatui now, so a repaint is no
longer announced by a full-screen erase (`ESC[2J`), and the starfield keeps
painting a frame every 30ms -- "output went quiet" is no longer a signal
either. The only usable signal is what the screen says.

Run: python3 testkit/tui/config_resize.py --binary /absolute/path/to/miyu
"""

import argparse
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import termios
import time
from pathlib import Path

import pyte
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = sandbox_dir.make("miyu-config-resize-")
    out = Path(os.environ.get("OUT") or Path.home() / ".cache" / "miyu-config-resize")
    out.mkdir(parents=True, exist_ok=True)
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub",
                       "base_url": "http://127.0.0.1:1/v1", "protocol": "openai-chat",
                       "api_key": "stub", "models": ["stub-model"]}],
        "display": {"language": "en"},
        "memory": {"enabled": False},
    }
    (home / "config/config.jsonc").write_text(json.dumps(config))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 110, 0, 0))

    def setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(args.binary.resolve()), "config"], stdin=slave, stdout=slave, stderr=slave,
        cwd=sandbox,
        env=dict(os.environ, MIYU_HOME=str(home), XDG_RUNTIME_DIR=str(sandbox / "run"),
                 TERM="xterm-256color"),
        preexec_fn=setup,
    )
    os.close(slave)
    screen = pyte.Screen(110, 32)
    stream = pyte.ByteStream(screen)
    sink = bytearray()

    def pump(seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([master], [], [], 0.02)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                sink.extend(chunk)
                stream.feed(chunk)

    def check(name, required, since=0):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            pump(0.05)
            text = "\n".join(screen.display)
            if all(word in text for word in required):
                # 换屏的内容是逐行落下的，第一眼看到的屏还差下面几行。
                pump(0.8)
                (out / f"{name}.txt").write_text("\n".join(screen.display))
                return
        (out / f"{name}.txt").write_text("\n".join(screen.display))
        raise AssertionError(f"{name}: no frame containing {required}. See {out}")

    def send(keys, name, required):
        os.write(master, keys)
        check(name, required)

    def resize(cols, rows, name, required):
        screen.resize(lines=rows, columns=cols)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        check(name, required)

    try:
        check("main", ["CONFIG", "Global settings"])
        resize(78, 24, "main-smaller", ["CONFIG", "Global settings"])
        resize(130, 40, "main-larger", ["CONFIG", "Global settings"])
        # The plain message dialog must stay open on resize, then consume a
        # real key. This provider deliberately has no image model.
        send(b"jj\r", "message", ["No models support image input", "any key"])
        resize(92, 30, "message-resized", ["No models support image input", "any key"])
        send(b"x", "message-dismissed", ["CONFIG"])
        # 2026-09-20：插件配置与自定义提示词并成了「人格」，主菜单少一项。
        send(b"j" * 5 + b"\r", "settings", ["GLOBAL SETTINGS", "Maximum tool rounds"])
        # 09-23:界面语言提到全局设置第一行,数字字段从第 2 行变第 3 行
        send(b"jj\r\x1b[H" + b"\x1b[3~" * 20 + b"resizecheck\x1b[D\x1b[D",
             "editing", ["resizecheck"])
        resize(110, 36, "editing-resized", ["GLOBAL SETTINGS", "resizecheck"])
        send(b"Z", "editing-cursor-retained", ["resizecheZck"])
        # Invalid numeric text triggers the separate boxed error dialog.
        send(b"\rq", "error", ["Something went wrong", "any key"])
        resize(88, 28, "error-resized", ["Something went wrong", "any key"])
        send(b"x", "error-dismissed", ["CONFIG"])
        print(f"PASS: menu resize, message/error stay open, editing text/cursor retained. {out}")
    finally:
        (out / "config.raw").write_bytes(sink)
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        os.close(master)


if __name__ == "__main__":
    main()
