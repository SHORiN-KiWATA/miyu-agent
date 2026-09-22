#!/usr/bin/env python3
"""鼠标移出终端窗口时，终端到底发不发点什么？

悬浮提亮靠的是鼠标移动事件（`MouseEventKind::Moved`）。指针一出窗口，终端就
不再报告位置，于是最后那一下提亮永远留在屏幕上（用户 09-22）。修法取决于一件
事实：**kitty 在指针离开时有没有任何可用的信号**——标准的 SGR 鼠标协议里没有
「离开」这一条，但终端可能借焦点事件、或者边缘坐标透露一点什么。

这份探针把终端发来的原始字节原样记下来，好照着实况选修法，而不是照猜的选。

跑法（在你平时用的那个 kitty 窗口里）：

    python3 testkit/tui/pointer_leave_probe.py

然后按提示做三件事，全程别按键：

1. 把鼠标移到这个窗口**里面**，随便晃两下；
2. 把鼠标**移出窗口**（移到别的窗口或桌面上），停在外面别动；
3. 等 3 秒，再把鼠标移回窗口里。

结束按 q。屏幕上会打出收到的事件，最后一行是小结。
"""

import os
import sys
import termios
import time
import tty

LEGEND = {
    "\x1b[I": "焦点：进来了 (FocusIn)",
    "\x1b[O": "焦点：出去了 (FocusOut)",
}


def describe(chunk):
    if chunk in LEGEND:
        return LEGEND[chunk]
    if chunk.startswith("\x1b[<"):
        body = chunk[3:]
        end = body[-1] if body else "?"
        parts = body[:-1].split(";")
        if len(parts) == 3:
            btn, col, row = parts
            kind = "移动" if btn in ("35", "3") else f"按钮 {btn}"
            press = "按下" if end == "M" else "松开"
            return f"鼠标：{kind} @ 列 {col} 行 {row}（{press}）"
    return "其它"


def main():
    if not sys.stdin.isatty():
        print("这是要人拿鼠标配合的探针,不是能批量跑的走查:它得在一个真终端窗口里\n"
              "开着,等你把指针移进移出。没有控制终端就没有可测的东西。\n"
              "跑法:在你平时用的那个 kitty 窗口里 `python3 testkit/tui/"
              "pointer_leave_probe.py`。", file=sys.stderr)
        return 2
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    out = sys.stdout
    # 1003 = 报告所有移动（不按键也报）；1006 = SGR 坐标；1004 = 焦点进出。
    out.write("\x1b[?1003h\x1b[?1006h\x1b[?1004h")
    out.flush()
    print(__doc__.split("跑法")[0])
    print("开始记录。按 q 结束。\r")
    events = []
    try:
        tty.setraw(fd)
        buf = ""
        last_seen = time.time()
        while True:
            chunk = os.read(fd, 1024).decode("utf-8", "replace")
            if "q" in chunk:
                break
            buf += chunk
            # 按 ESC 切开，一条一条看。
            while "\x1b" in buf[1:]:
                index = buf.index("\x1b", 1)
                piece, buf = buf[:index], buf[index:]
                if piece:
                    gap = time.time() - last_seen
                    last_seen = time.time()
                    events.append((gap, piece))
                    sys.stdout.write(f"  +{gap:5.2f}s  {describe(piece)}  {piece!r}\r\n")
                    sys.stdout.flush()
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        out.write("\x1b[?1003l\x1b[?1006l\x1b[?1004l")
        out.flush()

    print(f"\n一共 {len(events)} 条事件。")
    focus = [piece for _, piece in events if piece in LEGEND]
    print(f"其中焦点事件 {len(focus)} 条：{focus or '一条都没有'}")
    if events:
        biggest = max(events, key=lambda item: item[0])
        print(f"最长的一段静默：{biggest[0]:.2f}s（它之后收到的是 {biggest[1]!r}）")
    print("\n请把上面这些贴给我。")


if __name__ == "__main__":
    sys.exit(main() or 0)
