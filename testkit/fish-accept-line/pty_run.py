#!/usr/bin/env python3
"""真 fish + 真 hook 的 PTY 走查:带没匹配上的通配符的一句话,回车之后去哪了。

修之前:fish 在展开阶段就报「未找到通配符的匹配项」,hook 根本没被调用。
修之后:这一行交给 Miyu。

不连模型:MIYU_HOME 指向一个空沙箱,Miyu 起来之后自己报没配模型即可——
要看的是「fish 的通配符报错消失了、miyu 被调起来了」,不是回答内容。

    python3 testkit/fish-accept-line/pty_run.py

前置:`cargo build`(hook 文本编译进二进制,改了要重新构建)。需要本机装了 fish。
"""

import os
import pty
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", REPO / "target" / "debug" / "miyu"))
LINE = ("输出这段命令sudo rm -f /var/lib/systemd/coredump/"
        "core.gamescope-wl.1000.efc4a04b18a6469fb58058eaa835d7ff.*.zst")
GLOB_ERROR = "未找到通配符"
GLOB_ERROR_EN = "No matches for wildcard"


# 哑 PTY 不回答终端能力查询,fish 会为此干等十秒再降级。照着回一声就行。
QUERIES = [
    (b"\x1b[c", b"\x1b[?1;2c"),        # DA1
    (b"\x1b[>c", b"\x1b[>0;10;1c"),    # DA2
    (b"\x1b[6n", b"\x1b[1;1R"),        # 光标位置
    (b"\x1b[?u", b"\x1b[?0u"),         # kitty 键盘协议
]


def read_for(fd, seconds):
    out = b""
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.3)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
        for query, answer in QUERIES:
            for _ in range(chunk.count(query)):
                os.write(fd, answer)
    return out.decode("utf-8", "replace")


def main():
    if not shutil.which("fish"):
        sys.exit("本机没有 fish,跳过")
    if not BIN.exists():
        sys.exit(f"先 cargo build:{BIN} 不存在")

    home = sandbox_dir.make("miyu-fish-pty-")
    env = dict(
        os.environ,
        MIYU_HOME=str(home / "miyu"),
        HOME=str(home),
        PATH=f"{BIN.parent}:{os.environ['PATH']}",
        TERM="xterm-256color",
        MIYU_LANG="zh",
    )
    # hook 文本是从二进制里现取的,不会和源码跑偏。
    hook = subprocess.run([str(BIN), "fish-init"], env=env, capture_output=True, text=True)
    hook_file = home / ".config" / "fish" / "conf.d" / "miyu.fish"
    if not hook_file.exists():
        sys.exit(f"fish-init 没写出 hook:{hook.stdout}{hook.stderr}")

    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(env)
        os.execvp("fish", ["fish", "-i"])
    try:
        read_for(fd, 2.0)                      # 等提示符
        os.write(fd, LINE.encode() + b"\r")
        screen = read_for(fd, 12.0)
    finally:
        try:
            os.write(fd, b"\x03exit\r")
            os.close(fd)
        except OSError:
            pass
        os.waitpid(pid, 0)

    plain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)", "", screen)
    raw = Path(tempfile.gettempdir()) / "miyu-fish-pty.txt"
    raw.write_text(screen)

    glob_error = GLOB_ERROR in plain or GLOB_ERROR_EN in plain
    # 空沙箱里 Miyu 接管后要么转圈等 daemon,要么抱怨没配模型/没引导过。
    # 盲文转轮是它自己的等待动画,fish 不会画。
    reached_miyu = any(mark in plain for mark in ("模型", "供应商", "引导", "Miyu"))
    reached_miyu = reached_miyu or any("\u2800" <= ch <= "\u28ff" for ch in plain)

    print(f"fish 通配符报错: {'出现(没修好)' if glob_error else '没出现'}")
    print(f"miyu 被调起来 : {'是' if reached_miyu else '否'}")
    print(f"原始输出落在 : {raw}")
    if glob_error or not reached_miyu:
        print("\n--- 去掉转义序列的屏幕 ---")
        print(plain[-2000:])
        sys.exit("没达到预期")
    print("\n预期达成:这一行没被通配符拦下,交到了 Miyu 手里")


if __name__ == "__main__":
    main()
