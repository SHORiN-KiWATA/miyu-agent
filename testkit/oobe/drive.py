#!/usr/bin/env python3
"""用 pyte 在 PTY 里驱动真二进制的 `miyu oobe`，逐屏抓下来打印。

用法: python3 drive.py [binary] [cols] [rows]

抓的是「终端真正显示成什么样」，不是程序以为自己画了什么——中文宽度、两列对齐、
滚动视口的边界都能在这里现形。跑在一个临时 MIYU_HOME 里，不碰真配置；
MIYU_OOBE_NO_IME=1 免得反复开关真输入法。

走一遍：欢迎 → 人格（自己捏，起名）→ 功能（关一项）→ 认识你 → 终端（不装）
→ 沙盒模式（开启）→ 接模型（opencode Zen，会真的联网拉目录；没网就停在报错那屏）→ Ctrl+S 跳过收尾。
最后打印临时家目录里落了哪些文件。
"""
import fcntl, os, pty, select, struct, sys, tempfile, termios, time

import pyte

BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "debug", "miyu"
)
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 90
ROWS = int(sys.argv[3]) if len(sys.argv) > 3 else 34

home = tempfile.mkdtemp(prefix="miyu-oobe-")
screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.environ["MIYU_HOME"] = home
    os.environ["MIYU_OOBE_NO_IME"] = "1"
    os.environ["MIYU_OOBE_VERBOSE"] = "1"
    os.environ["LANG"] = "zh_CN.UTF-8"
    os.execvp(BIN, [BIN, "oobe"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))


def pump(t=0.3):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            stream.feed(data)
            # inline REPL 会用 ESC[6n 问光标位置并等回答;真终端会答,这里也得答,
            # 不然它会把随后的按键当成回答吞掉(testkit/tui 的 PTY 同样这么做)。
            for _ in range(data.count(b"\x1b[6n")):
                os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())


def send(keys, t=0.35):
    os.write(fd, keys)
    pump(t)


def display():
    """自己渲染:pyte 的 `display` 在宽字符被半截覆盖后会崩(续格是空串)。"""
    rows = []
    for y in range(ROWS):
        line = screen.buffer[y]
        out = []
        x = 0
        while x < COLS:
            data = line[x].data if x in line else " "
            if data == "":
                x += 1
                continue
            out.append(data)
            x += 1
        rows.append("".join(out))
    return rows


def shot(title):
    print()
    print(f"┌── {title} " + "─" * max(0, COLS - len(title) - 6))
    for line in display():
        print("│" + line.rstrip())
    print("└" + "─" * (COLS - 1))


ENTER, ESC, TAB, SPACE = b"\r", b"\x1b", b"\t", b" "
DOWN, UP = b"\x1b[B", b"\x1b[A"
CTRL_S = b"\x13"

# 首次运行会先跑 run_init（打印几行、每行 180ms），再进引导。
pump(2.5)
shot("00 欢迎（开场动画跑到一半）")
send(SPACE, 0.6)  # 任意键跳过动画
shot("00 欢迎（动画跳过后）")
send(ENTER, 0.6)
shot("01 人格")
send(DOWN, 0.3)  # 自己捏一个
send(DOWN, 0.3)  # 名字
send(ENTER, 0.3)  # 进编辑
send("小满".encode(), 0.3)
send(ENTER, 0.3)  # 结束编辑 → 焦点到设定
shot("01 人格（起了名）")
send(DOWN, 0.3)  # 继续
send(ENTER, 0.8)
shot("02 功能")
send(TAB, 0.3)  # 关掉光标底下那项
shot("02 功能（关了一项）")
send(ENTER, 0.6)
shot("03 认识你")
send(ENTER, 0.3)
send("我叫测试员。".encode(), 0.3)
send(ENTER, 0.3)
send(DOWN, 0.3)
send(ENTER, 0.6)
shot("04 终端")
# 光标停在第一个 shell 上；按 End 到「不装」不一定有，直接往下走到底
for _ in range(4):
    send(DOWN, 0.15)
shot("04 终端（选到不装）")
send(ENTER, 0.6)
shot("05 沙盒模式")
send(ENTER, 0.6)  # 开启(默认)
shot("06 接模型")
for _ in range(3):
    send(DOWN, 0.15)
shot("06 接模型（光标在 opencode Zen 上）")
send(ENTER, 0.5)
shot("06 填 key（Zen，默认用公共密钥）")
send(TAB, 0.4)
shot("06 填 key（Zen，切到自己的 key）")
send(TAB, 0.4)  # 切回公共密钥
send(DOWN, 0.3)  # 焦点到 API key（公共密钥开着它也在，可留空）
send(DOWN, 0.3)  # 焦点到「获取模型列表」
send(ENTER, 0.5)
# 目录要联网拉，最长等 25s；没网就停在报错那屏，下面的搜索走查跳过。
for _ in range(50):
    pump(0.5)
    if any("选个模型" in line for line in display()):
        break
shot("06 选模型（目录拉回来了才有）")
rows = display()
if any("选个模型" in line for line in rows):
    send(b"/", 0.3)
    send(b"glm", 0.5)
    shot("06 选模型（/glm 筛选中）")
    rows = display()
    # 进度轨那行也有 ● ，按「── 」排掉
    listed = [line for line in rows if ("○" in line or "●" in line) and "──" not in line]
    print("筛选后列表全含 glm:", bool(listed) and all("glm" in line.lower() for line in listed))
    print("表头显示匹配数:", any("匹配" in line for line in rows))
    send(ENTER, 0.3)  # 收起搜索，筛选留着
    send(DOWN, 0.3)  # j 也行，这里用方向键
    shot("06 选模型（收起搜索、下移一格）")
    rows = display()
    print("筛选仍在:", any("/glm" in line for line in rows))  # 那一行左边可能有星
    send(ESC, 0.4)  # 清筛选
    shot("06 选模型（Esc 清掉筛选）")
    rows = display()
    print("筛选已清:", not any("/glm" in line for line in rows))
    send(ESC, 0.5)  # 回到供应商列表
send(CTRL_S, 0.8)
shot("跳过之后（应已回到 shell，或进入 REPL）")

# 收尾：把进程结束掉，看看家目录里写了什么
try:
    os.write(fd, b"\x04")  # Ctrl+D 退出 REPL（如果进了）
except OSError:
    pass
pump(1.0)
try:
    os.kill(pid, 15)
except ProcessLookupError:
    pass

print()
print("家目录:", home)
for root, _, files in os.walk(home):
    for name in files:
        path = os.path.join(root, name)
        rel = os.path.relpath(path, home)
        if rel.endswith((".md", ".toml", ".jsonc")) and "kb" not in rel:
            print("──", rel)
            try:
                print(open(path, encoding="utf-8").read()[:600])
            except Exception as error:  # noqa: BLE001
                print("  (读不了:", error, ")")
