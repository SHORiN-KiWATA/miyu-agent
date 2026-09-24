#!/usr/bin/env python3
"""空会话 banner 与 Tab 换车道的无头探针。

用法: python3 banner_probe.py [binary] [cols] [rows]

临时 MIYU_HOME 里先写一份「引导已做过」的配置（免得进引导），然后:
  1. `MIYU_TUI=1 miyu` 进全屏 REPL —— 空会话,正文区应画出 MIYU banner 与模式行;
  2. 等两秒再抓一帧,星星应该变了(动画在走);
  3. 按 Tab,模式行应从「◉ 普通模式」变成「◉ 开发模式」,输入框竖条换色;
  4. 再按 Tab 切回来;
  5. Ctrl+D 退出。
最后把临时家里的 daemon 停掉。
"""
import fcntl, json, os, pty, select, signal, struct, subprocess, sys, tempfile, termios, time

import pyte

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "debug", "miyu"
)
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 100
ROWS = int(sys.argv[3]) if len(sys.argv) > 3 else 36
FULLSCREEN = os.environ.get("PROBE_INLINE") is None

home = str(sandbox_dir.make("miyu-banner-"))
env = dict(os.environ)
env.update({
    "TERM": "xterm-256color",
    "COLORTERM": "truecolor",
    "MIYU_HOME": home,
    "LANG": "zh_CN.UTF-8",
})
# 先 init 一份配置,再把引导标成做过。
subprocess.run([BIN, "init"], env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
cfg_path = os.path.join(home, "config", "config.jsonc")
raw = open(cfg_path, encoding="utf-8").read()
cfg = json.loads(raw)
cfg["oobe_done"] = True
open(cfg_path, "w", encoding="utf-8").write(json.dumps(cfg, ensure_ascii=False, indent=2))

# daemon 自己起在私有端口上：裸 `miyu` 会去默认的 8300，那上面常蹲着真机的 daemon，
# 临时家的 CLI 撞上它就串家了。`__daemon --port` 会把端口写进临时家的 daemon-launch.json，
# 后面的 CLI 照着找。
PORT = int(os.environ.get("PROBE_PORT", "18436"))
daemon = subprocess.Popen(
    [BIN, "__daemon", "--port", str(PORT)], env=env, cwd=home,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)
import urllib.request, urllib.error
import os
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402
_deadline = time.time() + 30
while time.time() < _deadline:
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{PORT}/api/config", timeout=2)
        break
    except urllib.error.HTTPError:
        break
    except Exception:
        time.sleep(0.2)
else:
    raise SystemExit("daemon 没起来")

screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)

pid, fd = pty.fork()
if pid == 0:
    for key, value in env.items():
        os.environ[key] = value
    if FULLSCREEN:
        os.environ["MIYU_TUI"] = "1"
    os.execvp(BIN, [BIN])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))


LAST = {"tail": b""}


def pump(t=0.3):
    """读 t 秒。到点之后如果停在一帧中间（最后一段不是同步块结束 ESC[?2026l），
    再多读一小会儿等这一帧收尾——半帧里输入框可能刚擦掉还没写回。"""
    end = time.time() + t
    grace = None
    while True:
        now = time.time()
        if now >= end:
            if grace is None:
                if LAST["tail"].endswith(b"\x1b[?2026l") or LAST["tail"] == b"":
                    return
                grace = now + 0.3
            elif now >= grace:
                return
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            stream.feed(data)
            LAST["tail"] = data[-16:]
            # inline REPL 会用 ESC[6n 问光标位置并等回答;真终端会答,这里也得答,
            # 不然它会把随后的按键当成回答吞掉(testkit/tui 的 PTY 同样这么做)。
            for _ in range(data.count(b"\x1b[6n")):
                os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())


def send(keys, t=0.5):
    os.write(fd, keys)
    pump(t)


def display():
    rows = []
    for y in range(ROWS):
        line = screen.buffer[y]
        out = []
        for x in range(COLS):
            data = line[x].data if x in line else " "
            if data == "":
                continue
            out.append(data)
        rows.append("".join(out))
    return rows


def shot(title):
    rows = display()
    print()
    print(f"┌── {title} " + "─" * max(0, COLS - len(title) - 6))
    for line in rows:
        print("│" + line.rstrip())
    print("└" + "─" * (COLS - 1))
    return rows


def mode_line(rows):
    return next((line.strip() for line in rows if "Tab" in line and ("普通" in line or "开发" in line)), "")


pump(6.0)  # daemon 冷启动要几秒
a = shot("空会话(全屏)" if FULLSCREEN else "空会话(inline)")
pump(2.0)
b = shot("两秒后(星星该变了)")
print("动画在走:", a != b)
print("模式行:", mode_line(b))
send(b"\t", 1.2)
c = shot("按 Tab 之后")
print("模式行:", mode_line(c))
send(b"\t", 1.2)
d = shot("再按 Tab")
print("模式行:", mode_line(d))
send(b"/", 1.0)
e = shot("打了一个 / (命令候选该在输入框下面)")
send(b"\x7f", 0.6)
send(b"\x04", 1.0)

try:
    os.kill(pid, signal.SIGTERM)
except ProcessLookupError:
    pass
daemon.terminate()
try:
    daemon.wait(timeout=5)
except subprocess.TimeoutExpired:
    daemon.kill()
print("家目录:", home, "（跑完即删；要留着看设 MIYU_KEEP_SANDBOX=1）")
