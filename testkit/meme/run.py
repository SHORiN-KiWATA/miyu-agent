#!/usr/bin/env python3
"""发表情包时时间线上的那几行。

用户 09-19 两条：

1. **shellhook（单次 `miyu "…"`）里每发一次表情包就多两行报错** `✗ 表情包 ·
   已中断`——而那一次其实是成功的；
2. **全屏 TUI 里图和后面的正文之间少一个空行**。

第一条的由来：表情包要往终端直接写图，写之前会请渲染器「收个尾」
（`prepare_for_external_output`）。收尾时这次调用**还没返回**（它要等图打完才
返回），于是被当成「没跑完 = 已中断」收成一步；真结果回来时统计已经被清空，
又记一次。

第二条的量法：全屏下图是**进缓冲**的（占位格当普通文字），所以整屏重绘之后
它还在画面上——pyte 还原出来的那一屏里，图那几行和正文之间有没有空行，一眼
可数。为了让 chafa 吐出真字形（纯色图会被画成"背景色 + 空格"，在 pyte 眼里
和空行没区别），种进库里的是一张格子图。

三条路各跑一遍：shellhook（图直接打到终端）、非 kitty 全屏（chafa 字符画进
缓冲）、kitty 全屏（占位格进缓冲，用户那台就是这条）。

跑法（先 cargo build）：

    python3 testkit/meme/run.py

产物在 ~/.cache/miyu-meme-rows/。
"""

import json
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import termios
import time
import urllib.error
import urllib.request
import zlib
from pathlib import Path
# 收缩行认法统一走 testkit/fold_summary.py(09-24 摘要改成按工具类别写)。
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fold_summary import is_fold_summary  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

try:
    import pyte
except ImportError:
    print("! 需要 pyte：pip install --user pyte", file=sys.stderr)
    raise SystemExit(2)

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-meme-rows/home"))
RUNTIME = os.environ.get("MIYU_MEME_RUNTIME", "/tmp/mx-meme")
PORT = int(os.environ.get("MIYU_MEME_PORT", "18455"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18497"))
BASE = f"http://127.0.0.1:{PORT}"
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-meme-rows"))
COLS, ROWS = 100, 46
MEME_ID = "probe-meme-0001"
# 库名显式给：省略的话走的是**人格库**（沙箱配置没写人格 → "miyu"），
# 种在 default 里的这张图根本不会被找到，工具直接报「meme not found」。
LIBRARY = "probe"
REPLY = "发出去了，能看到吗"


def png_bytes(width=120, height=90, cell=10):
    """现造一张格子 PNG，不依赖 Pillow。

    **别用纯色**：chafa 会把整块纯色画成「背景色 + 空格」，pyte 还原出来就是
    一片空行，和「图下面那一行空」分不开——这条走查也就白测了。
    """
    dark, light = (30, 30, 30), (230, 190, 80)
    rows = []
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            row.extend(dark if ((x // cell) + (y // cell)) % 2 else light)
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 6))
        + chunk(b"IEND", b"")
    )


def seed_meme_library():
    """在沙箱家目录里放一个只有一张图的表情包库。"""
    library = HOME / "data" / "memes" / LIBRARY
    library.mkdir(parents=True, exist_ok=True)
    (library / "probe.png").write_bytes(png_bytes())
    index = {
        "library": LIBRARY,
        "version": 1,
        "memes": [
            {
                "id": MEME_ID,
                "name": {"zh": "探针表情", "en": "probe meme"},
                "file": "probe.png",
                "mime_type": "image/png",
                "animated": False,
                "description": "走查用的格子图",
                "usage": "走查",
                "tags": ["走查", "探针"],
            }
        ],
        "disabled_ids": [],
    }
    (library / "index.json").write_text(
        json.dumps(index, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def tui_module():
    """借 `testkit/tui/run.py` 的 PTY 骨架，换成这一轮的沙箱。"""
    sys.path.insert(0, str(ROOT / "testkit" / "tui"))
    import run as tui  # noqa: E402

    tui.HOME = HOME
    tui.PORT = PORT
    tui.STUB_PORT = STUB_PORT
    tui.BIN = BIN
    tui.COLS, tui.ROWS = COLS, ROWS
    return tui


def wait_http(url, timeout=25):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            # 401/404 也是"已经在听了"。新家目录的 /api/config 要口令，
            # 当成没起来的话这一轮会空等到超时（第一版就是这么假报的）。
            return True
        except Exception:
            time.sleep(0.3)
    return False


def render(raw):
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)
    stream.feed(raw.decode("utf-8", "replace"))
    return [line.rstrip() for line in screen.display]


def run_pty(argv, env, quiet=1.2, timeout=90):
    """在真 PTY 里跑一条命令，读到静默为止，返回原始字节。"""
    master, slave = pty.openpty()
    import fcntl

    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    process = subprocess.Popen(
        argv, stdin=slave, stdout=slave, stderr=slave, env=env,
        cwd=str(HOME), close_fds=True,
        preexec_fn=lambda: (os.setsid(), fcntl.ioctl(1, termios.TIOCSCTTY, 0)),
    )
    os.close(slave)
    sink = bytearray()
    deadline = time.time() + timeout
    last = time.time()
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.3)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            sink.extend(chunk)
            last = time.time()
            # 光标位置查询得有人答。打图前要先问「现在在第几行」(腾地方用)，
            # 没人答就干等超时——这一步的耗时会白白多出 5 秒，看着像打图很慢。
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")
        elif process.poll() is not None and time.time() - last > quiet:
            break
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
    os.close(master)
    return bytes(sink)


def shellhook_case(env, report):
    """一、shellhook 那条路：单次 `miyu "…"`。"""
    raw = run_pty([str(BIN), "发个表情包"], dict(env, MIYU_TUI="0"))
    (OUT / "raw-shellhook.bin").write_bytes(raw)
    screen = render(raw)
    (OUT / "screen-shellhook.txt").write_text("\n".join(screen), encoding="utf-8")
    text = "\n".join(screen)
    interrupted = len(re.findall(r"已中断", text))
    report["shellhook 发图没有「已中断」"] = interrupted == 0
    report["_shellhook 已中断行数"] = interrupted
    report["shellhook 有表情包那一步"] = "表情包" in text
    # 那一步得是**跑完**的样子：带耗时、不带失败叉。修法是把没跑完的工具
    # 扣在手里等结果，扣漏了就会退回原来那两行。
    meme_rows = [line for line in screen if "表情包" in line and "加载" not in line]
    report["_shellhook 表情包行"] = meme_rows
    report["shellhook 表情包那一步不是失败态"] = bool(meme_rows) and not any(
        "✗" in line for line in meme_rows
    )
    # 图本身也得在，而且上下各空一行（这条路是直接往终端打，不走缓冲）。
    blocks = "▀▄█▉▊▋▌▍▎▏▐░▒▓"
    art = [i for i, line in enumerate(screen) if any(ch in line for ch in blocks)]
    report["shellhook 图打出来了"] = bool(art)
    # 顺序：先有「用了表情包工具」那一行，图才出来。反过来读着就不像
    # 「用了工具 → 图出来了」（用户 09-19 截图）。
    show_row = max(
        (i for i, line in enumerate(screen) if "表情包" in line and "加载" not in line),
        default=None,
    )
    report["_shellhook 表情包那一行在第几行"] = show_row
    report["_shellhook 图在第几行"] = art[0] if art else None
    report["shellhook 表情包那一步在图上面"] = (
        show_row is not None and bool(art) and show_row < art[0]
    )
    if art:
        first, last = art[0], art[-1]
        report["_shellhook 图上下两行"] = [
            screen[first - 1] if first else "<屏顶>",
            screen[last + 1] if last + 1 < len(screen) else "<屏底>",
        ]
        report["shellhook 图上下各空一行"] = (
            first > 0
            and not screen[first - 1].strip()
            and last + 1 < len(screen)
            and not screen[last + 1].strip()
        )
    else:
        report["shellhook 图上下各空一行"] = False


# kitty 的图形传输段（APC `ESC _G … ESC \\`）。发给终端的**指令**，不是正文；
# 客户端自己也是把它挑出来直发终端、只把占位格留进缓冲的。pyte 不认 APC，
# 会把里面的 base64 当字打到屏上，所以喂它之前照着客户端的做法先摘掉。
APC = re.compile(rb"\x1b_G.*?\x1b\\", re.S)


def tui_case(env, report, term="xterm-256color", tag="TUI", fresh=False):
    """二、全屏 TUI：图和后面的正文之间该有一行空。

    `term` 决定图走哪条路：非 kitty 走 chafa 字符画，`xterm-kitty` 走占位格
    （用户那台就是这条）。两条路进缓冲的形状不同，空行的账却是同一本。
    """
    tui = tui_module()
    tui.ENV = dict(env, MIYU_TUI="1", TERM=term)
    process, master = tui.spawn_tui()
    sink = bytearray()
    try:
        tui.drain(master, 3.0, sink)
        if fresh:
            # 换一条空会话再问。桩模型的阶段表按**整个会话**已经回过几次工具
            # 结果走，接着上一轮问的话它直接跳到"只说话"那一格，图根本不会出
            # 现——这条走查就在量一屏没有图的画面。
            os.write(master, "/new\r".encode())
            tui.settle(master, sink, quiet=0.6, timeout=15)
        os.write(master, "发个表情包\r".encode())
        got_reply = tui.drain_until(master, sink, REPLY, 90)
        tui.settle(master, sink, quiet=0.6, timeout=20)
    finally:
        os.write(master, b"\x03")
        time.sleep(0.4)
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
    raw = APC.sub(b"", bytes(sink))
    (OUT / f"raw-{tag}.bin").write_bytes(bytes(sink))
    screen = render(raw)
    (OUT / f"screen-{tag}.txt").write_text("\n".join(screen), encoding="utf-8")
    report[f"{tag} 收到回复"] = got_reply
    # 从**最后**一段往回找：同一个家目录可能已经聊过一轮，回放会把旧的也画上来。
    head = next((i for i in range(len(screen) - 1, -1, -1) if is_fold_summary(screen[i])), None)
    reply = next((i for i in range(len(screen) - 1, -1, -1) if REPLY in screen[i]), None)
    report[f"{tag} 有收缩行和正文"] = (
        head is not None and reply is not None and head < reply
    )
    if head is None or reply is None or head >= reply:
        report[f"{tag} 收缩行与图之间空一行"] = False
        report[f"{tag} 图与正文之间空一行"] = False
        return
    middle = screen[head + 1:reply]
    report[f"_{tag} 收缩行到正文之间"] = middle
    above = 0
    while above < len(middle) and not middle[above].strip():
        above += 1
    below = 0
    while below < len(middle) - above and not middle[len(middle) - 1 - below].strip():
        below += 1
    report[f"_{tag} 上方空行数"] = above
    report[f"_{tag} 下方空行数"] = below
    # 图本体还在：中间不能只剩空行。
    report[f"{tag} 图那几行还在画面上"] = above + below < len(middle)
    report[f"{tag} 收缩行与图之间空一行"] = above == 1
    report[f"{tag} 图与正文之间空一行"] = below == 1


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    tui_module().write_config()
    seed_meme_library()

    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_REASONING="1",
            STUB_EXTRA_CALLS=json.dumps(
                [
                    {"name": "load_tools", "arguments": {"names": ["use_meme"]}},
                    {
                        "name": "use_meme",
                        "arguments": {
                            "action": "show",
                            "id": MEME_ID,
                            "library": LIBRARY,
                        },
                    },
                ]
            ),
            STUB_REPLY=REPLY,
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    env = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME)
    # chafa 默认会**探测终端能力**（往 /dev/tty 发查询等回答）。测具这个 PTY
    # 没人答，它要干等五秒才退回字符画——那一步的耗时于是显示成 `5.0s`，看着
    # 像打图很慢。真终端会立刻回答，所以这是测具的事，不是产品的事。
    env["MIYU_CHAFA_ARGS"] = "--probe off --polite on"
    # chafa 认 `KITTY_*` 认得比 TERM 还早：留着的话它吐的是 kitty 图形协议
    # （APC），pyte 不认，整屏会被 base64 糊满，图那几行也就数不出来了。
    for stale in ("KITTY_WINDOW_ID", "KITTY_PID", "KITTY_LISTEN_ON",
                  "KITTY_INSTALLATION_DIR", "KITTY_PUBLIC_KEY", "TERM_PROGRAM"):
        env.pop(stale, None)
    daemon = None
    report = {}
    try:
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=env, cwd=str(HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not wait_http(f"{BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        shellhook_case(env, report)
        tui_case(env, report)
        # 用户那台是 kitty：图走占位格这条路，再量一遍同一本账。
        tui_case(env, report, term="xterm-kitty", tag="kitty TUI", fresh=True)
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    passed = 0
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(
        f"\n{passed}/{len(checks)} passed   "
        f"已中断行数={report.get('_shellhook 已中断行数')}   "
        f"图上下空行 chafa={report.get('_TUI 上方空行数')}/{report.get('_TUI 下方空行数')}"
        f" kitty={report.get('_kitty TUI 上方空行数')}/{report.get('_kitty TUI 下方空行数')}"
    )
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    raise SystemExit(main())
