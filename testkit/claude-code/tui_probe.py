#!/usr/bin/env python3
"""config TUI 的 Claude Code 特殊供应商 PTY 探针。

验证:①供应商列表给未启用的内置 Claude Code 打「(未启用)」标;②回车打开的
是专用表单(启用开关/binary/工具桥/看门狗),而不是含 Base URL/协议/API Key
的通用表单。只读不保存(Esc + q 退出)。

    MIYU_HOME=/tmp/miyu-cc/home python3 testkit/claude-code/tui_probe.py
"""

import fcntl
import os
import pty
import re
import struct
import subprocess
import sys
import termios
import threading
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
MIYU_BIN = REPO / "target" / "debug" / "miyu"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-cc/home"))

ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][0-9A-B]|\x1b[=>]")


def plain(data: bytes) -> str:
    return ANSI.sub("", data.decode("utf-8", "replace"))


def main():
    env = dict(os.environ)
    env["MIYU_HOME"] = str(HOME)
    env["TERM"] = "xterm-256color"
    env.setdefault("LANG", "zh_CN.UTF-8")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    proc = subprocess.Popen(
        [str(MIYU_BIN), "config"],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        close_fds=True,
        preexec_fn=os.setsid,
    )
    os.close(slave)
    chunks = []
    stop = threading.Event()

    def pump():
        buf = b""
        while not stop.is_set():
            try:
                chunk = os.read(master, 4096)
            except OSError:
                break
            if not chunk:
                break
            chunks.append(chunk)
            buf = (buf + chunk)[-64:]
            if b"\x1b[6n" in buf:
                os.write(master, b"\x1b[1;1R")
                buf = buf.replace(b"\x1b[6n", b"")

    threading.Thread(target=pump, daemon=True).start()

    def snapshot_from(mark: int) -> str:
        return plain(b"".join(chunks[mark:]))

    def wait_for(text: str, mark: int, timeout: float = 10) -> str:
        deadline = time.time() + timeout
        while time.time() < deadline:
            shot = snapshot_from(mark)
            if text in shot:
                return shot
            if proc.poll() is not None:
                raise RuntimeError(f"config TUI 提前退出;缓冲:\n{shot[-800:]}")
            time.sleep(0.2)
        raise TimeoutError(f"等待 {text!r} 超时;缓冲尾:\n{snapshot_from(mark)[-800:]}")

    try:
        wait_for("供应商和模型", 0)
        mark = len(chunks)
        os.write(master, b"\r")  # 进供应商浏览器(首项)
        listing = wait_for("Claude Code", mark)
        assert "未启用" in listing, f"列表应有未启用标:\n{listing[-600:]}"
        print("PASS: 供应商列表含「Claude Code(未启用)」标")

        mark = len(chunks)
        os.write(master, b"\r")  # 回车编辑第一行(claude-code)
        form = wait_for("编辑 Claude Code", mark)
        for expected in [
            "启用(中转 Claude Code",
            "claude 可执行文件",
            "原生工具作用域",
            "Miyu 工具挂给 claude",
            "权限模式",
            "看门狗",
        ]:
            assert expected in form, f"专用表单缺字段 {expected!r}:\n{form[-800:]}"
        for absent in ["Base URL", "API Key", "协议", "额外请求体"]:
            assert absent not in form, f"专用表单不该出现 {absent!r}:\n{form[-800:]}"
        print("PASS: 回车打开专用表单,HTTP 字段全部缺席")

        # 完整启用流:打开启用字段的选择器 → 选 true → s 保存表单。
        mark = len(chunks)
        os.write(master, b"\r")  # 打开字段 0(启用)的选择器
        wait_for("true", mark)
        os.write(master, b"\x1b[A")  # Up:无论初始停在哪,向上都落在 true
        time.sleep(0.3)
        os.write(master, b"\r")  # 确认选择
        time.sleep(0.5)
        mark = len(chunks)
        os.write(master, b"s")  # 保存表单,回到供应商浏览器
        listing = wait_for("Claude Code", mark)
        assert "未启用" not in listing, f"启用后列表不该再有未启用标:\n{listing[-600:]}"
        print("PASS: 表单内启用成功,列表标记消失")

        os.write(master, b"q")  # 退供应商浏览器
        wait_for("保存并退出", len(chunks))
        for _ in range(9):
            os.write(master, b"\x1b[B")  # 下移到「保存并退出」
            time.sleep(0.1)
        os.write(master, b"\r")
        proc.wait(timeout=15)
        import json

        config = json.loads((HOME / "config" / "config.jsonc").read_text())
        entry = next(
            p for p in config["providers"] if p.get("protocol") == "claude-code"
        )
        # enabled=true 是默认值,序列化时被 skip;键缺席即启用。
        assert entry.get("enabled", True), f"保存后应为启用态: {entry}"
        assert entry.get("models") == ["fable", "opus", "sonnet", "haiku"], entry
        print("PASS: 全部通过(启用态已落盘,预置模型完好)")
    finally:
        stop.set()
        if proc.poll() is None:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
