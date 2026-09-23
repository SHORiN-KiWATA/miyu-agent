#!/usr/bin/env python3
"""拉宽/收窄窗口之后，正文的软换行要跟着重排。

全屏 TUI 的正文缓冲是自己的终端模拟器（`tail/screen/term.rs`）：长行是写进去那
一刻按当时的宽度折的。窗口一变宽，`set_cols` 只管**以后**写进来的行，已经落下的
行还按旧宽度断着——右边多出来的宽度整片空着。收窄同理。

这个脚本起沙箱 daemon + 桩模型，让她回一段很长的单行，然后在真 PTY 上改窗口大小
（`TIOCSWINSZ`，内核自己发 SIGWINCH），用 pyte 跟着 resize 还原画面，量那一段正文
每一行有多宽。

跑法：

    cargo build
    python3 testkit/tui/softwrap.py

产物在 ~/.cache/miyu-softwrap/（每个宽度一张屏）。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import fcntl
import json
import os
import select
import shutil
import struct
import subprocess
import sys
import termios
import time
import unicodedata
from pathlib import Path

import pyte

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-softwrap"))
PROMPT = "走查一句"
START_COLS, ROWS = 80, 40
# 一段没有空格可断的长正文：中文本来就随处可断，正好逼出「按宽度折」这条路。
REPLY = "这是一段特别长的单行正文" * 12


class Terminal:
    """跟着 PTY 一起 resize 的 pyte 屏幕。"""

    def __init__(self, master, cols, rows):
        self.master = master
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.raw = bytearray()

    def drain(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            ready, _, _ = select.select([self.master], [], [], 0.1)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if not chunk:
                return
            self.raw.extend(chunk)
            self.stream.feed(chunk.decode("utf-8", "replace"))

    def drain_until(self, marker, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.drain(0.2)
            if any(marker in line for line in self.lines()):
                return True
        return False

    def resize(self, cols):
        fcntl.ioctl(
            self.master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, cols, 0, 0)
        )
        self.screen.resize(ROWS, cols)

    def lines(self):
        return [line.rstrip() for line in self.screen.display]


def display_width(text):
    """一行占多少**列**——中文一个字两列，按字符数量会差一倍。"""
    return sum(
        2 if unicodedata.east_asian_width(ch) in "WF" else 1 for ch in text
    )


def body_widths(lines, needle="这是一段特别长的单行正文"):
    """正文那几行各占多少列。"""
    return [display_width(line.rstrip()) for line in lines if needle in line]


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    h.COLS, h.ROWS = START_COLS, ROWS
    h.write_config()
    h.kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), STUB_REPLY=REPLY),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = h.spawn_tui()
        term = Terminal(master, START_COLS, ROWS)
        term.drain(3.0)
        os.write(master, PROMPT.encode())
        term.drain_until(PROMPT, 3.0)
        os.write(master, b"\r")
        assert term.drain_until("这是一段特别长", 40.0), "桩模型的长正文没出来"
        term.drain(1.5)

        def snapshot(tag):
            (OUT / f"{tag}.txt").write_text("\n".join(term.lines()), encoding="utf-8")
            (OUT / "raw.bin").write_bytes(bytes(term.raw))
            return body_widths(term.lines())

        narrow = snapshot(f"{START_COLS}-cols")
        report[f"{START_COLS} 列时正文折在 {START_COLS} 以内"] = bool(narrow) and max(
            narrow
        ) <= START_COLS

        # 拉宽：正文该重排到新宽度，而不是留着旧折点、右边空着
        term.resize(120)
        term.drain(2.0)
        wide = snapshot("120-cols")
        report["拉宽后正文重排到新宽度"] = bool(wide) and max(wide) > START_COLS + 4

        # 收窄：一个字都不该被切掉（行宽不超过新宽度）
        term.resize(60)
        term.drain(2.0)
        tight = snapshot("60-cols")
        report["收窄后正文不超出新宽度"] = bool(tight) and max(tight) <= 60
        report["收窄后正文还在"] = len(tight) >= 2

        # 再拉回来：来回改不该把内容弄丢
        term.resize(START_COLS)
        term.drain(2.0)
        back = snapshot("back-to-80")
        report["改回原宽度正文还在"] = bool(back) and max(back) <= START_COLS
    finally:
        for process in (tui, daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
