#!/usr/bin/env python3
"""引导里的「沙盒模式」页(09-23):说明目的、问默认开不开、选了就落盘。

    python3 testkit/oobe/sandbox_page.py [binary]

真二进制 + PTY + pyte,每一程一个临时 MIYU_HOME,不碰真配置、不联网(到接模型那屏
就 Ctrl+S 跳过)。终端集成那屏一律选「不装」——选别的会真往 shell 的配置里写。

两程:回车 = 开启(新装的默认值),下移再回车 = 不开;各看一眼配置文件里的
`tools.sandbox.default_enabled`。一条判定一行 ✅/❌,最后 n/m passed。
"""
import fcntl
import json
import os
import pty
import re
import select
import shutil
import struct
import sys
import termios
import time

import pyte
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "debug", "miyu"
)
COLS, ROWS = 100, 40
ENTER, DOWN, SPACE, CTRL_S = b"\r", b"\x1b[B", b" ", b"\x13"

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


class Oobe:
    def __init__(self):
        self.home = str(sandbox_dir.make("miyu-oobe-sandbox-", delete_at_exit=False))
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        pid, fd = pty.fork()
        if pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.environ["MIYU_HOME"] = self.home
            os.environ["MIYU_OOBE_NO_IME"] = "1"
            os.environ["LANG"] = "zh_CN.UTF-8"
            os.execvp(BIN, [BIN, "oobe"])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.pid, self.fd = pid, fd

    def pump(self, seconds=0.3):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    self.stream.feed(os.read(self.fd, 65536))
                except OSError:
                    return

    def text(self):
        return "\n".join(self.screen.display)

    def send(self, keys, wait=0.4):
        os.write(self.fd, keys)
        self.pump(wait)

    def wait_for(self, word, timeout=8.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.pump(0.2)
            if word in self.text():
                return True
        return False

    def config(self):
        path = os.path.join(self.home, "config", "config.jsonc")
        raw = re.sub(r"^\s*//.*$", "", open(path, encoding="utf-8").read(), flags=re.M)
        return json.loads(raw)

    def to_sandbox_page(self):
        self.pump(2.5)
        self.send(SPACE, 0.6)  # 跳过开场动画
        self.send(ENTER, 0.8)  # 欢迎 → 人格
        self.send(ENTER, 0.8)  # 内置人格 → 功能
        self.send(ENTER, 0.8)  # 功能 → 认识你
        self.send(DOWN, 0.3)  # 焦点从输入框移到「继续」
        self.send(ENTER, 0.8)  # 认识你 → 终端
        for _ in range(8):  # 一路下到「不装」(光标到底就停)
            self.send(DOWN, 0.1)
        self.send(ENTER, 0.8)
        return self.wait_for("沙盒模式")

    def close(self):
        try:
            os.kill(self.pid, 15)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        shutil.rmtree(self.home, ignore_errors=True)


def main():
    # 第一程:看页面,直接回车 = 开启。
    oobe = Oobe()
    try:
        check(oobe.to_sandbox_page(), "终端那屏之后是「沙盒模式」页")
        page = oobe.text()
        check("防误改" in page and "workspace" in page, "页面说明了沙盒的目的与可写的地方")
        check("开启" in page and "不开" in page, "两个选项都在")
        check("沙盒" in next((l for l in page.splitlines() if "终端" in l and "模型" in l), ""),
              "进度轨上多了「沙盒」这一步")
        oobe.send(ENTER, 0.8)
        check(oobe.wait_for("接模型"), "回车之后到接模型")
        enabled = oobe.config().get("tools", {}).get("sandbox", {}).get("default_enabled")
        check(enabled is True, "选「开启」落盘为 true", str(enabled))
        oobe.send(CTRL_S, 0.8)
    finally:
        oobe.close()

    # 第二程:下移选「不开」再回车。
    oobe = Oobe()
    try:
        check(oobe.to_sandbox_page(), "第二程也到了沙盒页")
        oobe.send(DOWN, 0.3)
        oobe.send(ENTER, 0.8)
        check(oobe.wait_for("接模型"), "选「不开」之后到接模型")
        enabled = oobe.config().get("tools", {}).get("sandbox", {}).get("default_enabled")
        check(enabled is False, "选「不开」落盘为 false", str(enabled))
        oobe.send(CTRL_S, 0.8)
    finally:
        oobe.close()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed")
    raise SystemExit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
