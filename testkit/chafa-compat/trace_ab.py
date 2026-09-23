#!/usr/bin/env python3
"""MIYU_IMAGE_TRACE 开/关，Miyu 写给终端的字节一样吗？

取证模式下 `run_chafa` 把 chafa 的 stdout 收进管道、判完格式再由 Rust 的
`io::stdout()` 写回；正常模式是 chafa 直接 inherit 写 fd 1。两条路径不同，
就有可能取证手段本身改变了被测对象——这个脚本就是来验它的。

跑法：trace_ab.py            （两轮，各起一次真回合）
"""
import os, pty, re, select, subprocess, sys, time, fcntl, termios, struct, errno

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from pty_probe import respond  # 会应答 DA1/sixel-geometry/cell-px 的假终端

SB = "/home/shorin/.cache/miyu-chafa-sandbox/miyu-sb"
IMG = "/home/shorin/.cache/miyu-chafa-sandbox/images/tall.png"
PROMPT = f"显示 {IMG}"


def run(trace: bool, cols=138, rows=67, timeout=70):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 9, rows * 20))
    env = dict(os.environ)
    env["TERM"] = "xterm-256color"
    for key in ("KITTY_WINDOW_ID", "KITTY_PID", "KITTY_INSTALLATION_DIR", "TERM_PROGRAM"):
        env.pop(key, None)
    if trace:
        env["MIYU_IMAGE_TRACE"] = "1"
    else:
        env.pop("MIYU_IMAGE_TRACE", None)

    def setup():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    proc = subprocess.Popen([SB], stdin=slave, stdout=slave, stderr=slave,
                            env=env, preexec_fn=setup, close_fds=True)
    os.close(slave)
    out = b""
    sent = False
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError as err:
                if err.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            out += chunk
            reply = respond(chunk)
            if reply:
                try:
                    os.write(master, reply)
                except OSError:
                    pass
        if not sent and time.time() > deadline - timeout + 6:
            os.write(master, PROMPT.encode() + b"\r")
            sent = True
        if sent and b"\x1bP" in out and time.time() > deadline - timeout + 30:
            break
    proc.kill()
    proc.wait()
    os.close(master)
    return out


def sixel_spans(data: bytes):
    """每段 sixel 的位置、长度、声明尺寸，以及中间有没有被别的转义打断。"""
    spans = []
    for match in re.finditer(rb"\x1bP", data):
        start = match.start()
        end = data.find(b"\x1b\\", start)
        if end < 0:
            spans.append((start, None, "未闭合!"))
            continue
        body = data[start:end]
        size = re.search(rb'q"(\d+);(\d+);(\d+);(\d+)', body)
        # sixel 载荷里出现 CSI 就说明被别的输出插进来了
        intruders = len(re.findall(rb"\x1b\[", body))
        spans.append((start, end - start,
                      (f"{int(size.group(3))}x{int(size.group(4))}px" if size else "无尺寸声明")
                      + (f"  !!载荷里有 {intruders} 个 CSI" if intruders else "")))
    return spans


for trace in (False, True):
    label = "TRACE=1 (捕获再写回)" if trace else "TRACE 关 (chafa 直接 inherit)"
    data = run(trace)
    spans = sixel_spans(data)
    print(f"\n=== {label} ===")
    print(f"  收到总字节 {len(data)}")
    if not spans:
        print("  没有 sixel 段")
        continue
    for start, length, note in spans:
        print(f"  sixel @{start} 长度 {length} {note}")
