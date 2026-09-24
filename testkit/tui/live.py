#!/usr/bin/env python3
"""全屏 TUI 的**真模型**走查：用真的模型跑一轮，把屏幕抓下来看。

桩模型能钉住「机制对不对」，钉不住「好不好看」——真模型的思考是长句、工具名
五花八门、输出有长有短，版式会不会崩只有真跑一遍才知道。

    python3 testkit/tui/live.py                 # 默认跑一组任务
    python3 testkit/tui/live.py --prompt "…"    # 自己指定

沙箱：只从真配置里借一个供应商（连 key），会话库、记忆、日志全在
/tmp 下另开一份，不碰 ~/.miyu。

产物在 ~/.cache/miyu-tui-live/：每一步的 screen-NN.txt 是当时的整屏，
raw.bin 是原始字节流。
"""

import argparse
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
from pathlib import Path

import pyte
# 收缩行认法统一走 testkit/fold_summary.py(09-24 摘要改成按工具类别写)。
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fold_summary import is_fold_summary  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "release" / "miyu"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-tui-live/home"))
WORKSPACE = Path("/tmp/miyu-tui-live/work")
RUNTIME = os.environ.get("MIYU_TUI_RUNTIME", "/tmp/mx-live")
PORT = int(os.environ.get("MIYU_TUI_PORT", "18455"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-tui-live"))
COLS, ROWS = 120, 40
PROVIDER = os.environ.get("LIVE_PROVIDER", "opencodego")
MODEL = os.environ.get("LIVE_MODEL", "deepseek-v4.1-flash")

# 一组任务，逐条发。挑的是**会把版式压出问题**的那些：
#   长思考（窥视刷新）、多个工具（时间线串起来）、长输出（展开内容）、
#   子代理（覆盖层）、后台命令（状态行）。
TASKS = [
    "用一句话说说你现在看到的工作目录里有什么。先用 run_command 跑 ls -la 看一眼。",
    "把 /tmp/miyu-tui-live/work/notes.md 写成一份三行的待办清单，然后读回来确认。",
    "用 edit 工具把 notes.md 的第一行改成「第一件事：验收 TUI」，别用命令行改。",
    "用 markdown 表格列出三个终端模拟器：名字、一句话特点、是否支持图片。要有表头。",
    "开个后台命令：每秒打印一行，打十行就停。别等它，直接告诉我 job id。",
]


def jsonc(path):
    text = path.read_text(encoding="utf-8")
    text = re.sub(r"^\s*//.*$", "", text, flags=re.M)
    return json.loads(text)


def borrow_provider():
    """从真配置里借一个供应商（含 key）。借不到就没法真跑。"""
    real = Path.home() / ".miyu" / "config" / "config.jsonc"
    if not real.exists():
        raise SystemExit(f"! 找不到真配置 {real}")
    data = jsonc(real)
    for provider in data.get("providers", []):
        if provider.get("id") == PROVIDER:
            return provider
    raise SystemExit(f"! 真配置里没有供应商 {PROVIDER}")


def write_config():
    provider = borrow_provider()
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    WORKSPACE.mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": PROVIDER,
        "active_provider_models": [{"provider_id": PROVIDER, "model": MODEL}],
        "providers": [provider],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.2)
    return False


def spawn():
    master, slave = pty.openpty()
    import fcntl

    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def child_setup():
        os.setsid()
        fcntl.ioctl(1, termios.TIOCSCTTY, 0)

    env = dict(
        os.environ,
        MIYU_HOME=str(HOME),
        XDG_RUNTIME_DIR=RUNTIME,
        MIYU_TUI="1",
        TERM="xterm-256color",
    )
    process = subprocess.Popen(
        [str(BIN)],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=str(WORKSPACE),
        preexec_fn=child_setup,
        close_fds=True,
    )
    os.close(slave)
    return process, master


def drain(master, seconds, sink):
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        sink.extend(chunk)


def settle(master, sink, quiet=1.0, timeout=180.0):
    """读到输出静默为止。真模型慢，超时给得宽。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], quiet)
        if not ready:
            return True
        try:
            if not os.read(master, 65536):
                return False
        except OSError:
            return False
        sink.extend(os.read(master, 0) or b"")
    return False


def read_into(master, sink, quiet=1.0, timeout=180.0):
    deadline = time.time() + timeout
    last = time.time()
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                return
            if not chunk:
                return
            sink.extend(chunk)
            last = time.time()
        elif time.time() - last >= quiet:
            return


def click(master, sink, column, row):
    """原地点一下（按下 + 松开）。行列都是 0 基。"""
    os.write(master, f"\x1b[<0;{column + 1};{row + 1}M".encode())
    read_into(master, sink, quiet=0.5, timeout=10.0)
    os.write(master, f"\x1b[<0;{column + 1};{row + 1}m".encode())
    read_into(master, sink, quiet=0.5, timeout=10.0)


def render(raw):
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)
    stream.feed(raw.decode("utf-8", "replace"))
    return [line.rstrip() for line in screen.display]


def snapshot(sink, name):
    (OUT / name).write_text("\n".join(render(bytes(sink))), encoding="utf-8")


# 全屏下第 0–1 列是页边距，正文一律从第 2 列起。边框字符出现在那两列里，
# 说明某个东西按整屏宽排版、被缓冲硬折了一次，续行落回了第 0 列。
BOX = set("─│┌┐└┘├┤┬┴┼╭╮╰╯━┃┏┓┗┛┣┫┳┻╋═║╔╗╚╝")


def check_margin(lines):
    """返回越界的行（行号, 内容）。用户消息的 `┃` 在第 0 列，是设计，放过。"""
    bad = []
    for index, line in enumerate(lines):
        head = line[:2]
        if not head.strip():
            continue
        if head[0] == "┃":
            continue
        if any(ch in BOX for ch in head):
            bad.append((index, line))
    return bad


def check_graphics(lines):
    """图形传输段漏进正文的痕迹：占位符协议的键值对被当文字打出来了。"""
    return [
        (index, line)
        for index, line in enumerate(lines)
        if "_Gq=" in line or ",a=T,U=1" in line
    ]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prompt", action="append", help="自定义任务，可给多次")
    parser.add_argument("--keep", action="store_true", help="跑完别删沙箱")
    parser.add_argument("--no-reopen", action="store_true", help="跳过「重开一次」那步")
    args = parser.parse_args()
    tasks = args.prompt or TASKS

    if not BIN.exists():
        print(f"! 先 cargo build --release：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    # 上一轮的截图先清掉：某一步没跑到时留着旧图，看起来像"跑过了还是老样子"。
    for stale in OUT.glob("screen-*.txt"):
        stale.unlink()
    write_config()

    daemon = subprocess.Popen(
        [str(BIN), "__daemon", "--port", str(PORT)],
        env=dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME),
        cwd=str(WORKSPACE),
        stdout=(OUT / "daemon.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    if not wait_http(f"http://127.0.0.1:{PORT}/api/config", timeout=60):
        daemon.terminate()
        print("! daemon 没起来", file=sys.stderr)
        return 2

    process, master = spawn()
    sink = bytearray()
    findings = []
    try:
        drain(master, 3.0, sink)
        snapshot(sink, "screen-00-start.txt")
        for index, task in enumerate(tasks, start=1):
            os.write(master, task.encode())
            drain(master, 0.4, sink)
            os.write(master, b"\r")
            # 真模型一轮可能几十秒：等它静下来
            read_into(master, sink, quiet=2.5, timeout=300.0)
            snapshot(sink, f"screen-{index:02d}-done.txt")
            lines = render(bytes(sink))
            for row, line in check_margin(lines):
                findings.append(f"第 {index} 轮：第 {row} 行的边框压进页边距 → {line[:60]!r}")
            for row, line in check_graphics(lines):
                findings.append(f"第 {index} 轮：第 {row} 行漏出图形传输段 → {line[:60]!r}")
            print(f"  ✓ 第 {index} 轮跑完：{task[:28]}…")
        # 点开最后那条 `Worked for`，再点开里面一步——版式好不好看就看这儿
        screen = render(bytes(sink))
        head = None
        for index, line in enumerate(screen):
            if is_fold_summary(line):
                head = index
        if head is not None:
            click(master, sink, 3, head)
            snapshot(sink, "screen-50-expanded.txt")
            opened = render(bytes(sink))
            # 展开之后整块内容会往上顶（正文是贴着活动区长的），点开前算出来的
            # 行号在新的一屏里已经不指着那一行了——得按**展开标记**重新找。
            opened_head = next(
                (i for i, line in enumerate(opened) if "⌄" in line),
                None,
            )
            # 图标是 Nerd Font 的私有区字形，按字符认没法写死一张表；按**形状**
            # 认：展开区里带 ` · ` 的、不是收缩行的那几行就是步。
            step = next(
                (
                    i
                    for i, line in enumerate(opened)
                    if opened_head is not None
                    and i > opened_head
                    and line.startswith("  ")
                    and " · " in line
                    and not is_fold_summary(line)
                ),
                None,
            )
            if step is None:
                findings.append("展开后没找到可点的步（时间线里一行都没有？）")
            if step is not None:
                click(master, sink, 5, step)
                snapshot(sink, "screen-51-step.txt")
                stepped = render(bytes(sink))
                for row, line in check_margin(stepped):
                    findings.append(f"点开一步后：第 {row} 行的边框压进页边距 → {line[:60]!r}")
                # 只认 `↳`：`│` 既是时间线的连线也是 md 表格的竖边，按它判会误伤。
                # 装饰的另一半（`│ 输出`）有单测 `expanded_detail_drops_the_inline_decorations` 钉着。
                if any("↳" in line for line in stepped):
                    findings.append("点开一步后：展开内容里还留着 ↳ 装饰")
                click(master, sink, 5, step)
            collapsed = render(bytes(sink))
            head = next(
                (i for i, line in enumerate(collapsed) if "⌄" in line),
                head,
            )
            click(master, sink, 3, head)

        # 回翻看看历史还在不在
        os.write(master, b"\x1b[5~")
        read_into(master, sink, quiet=1.0, timeout=20.0)
        snapshot(sink, "screen-90-scrolled.txt")
        os.write(master, b"\x1b[6~")
        read_into(master, sink, quiet=1.0, timeout=20.0)
        os.write(master, b"\x04")
        drain(master, 3.0, sink)
    finally:
        (OUT / "raw.bin").write_bytes(bytes(sink))
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
        os.close(master)

    # 重开一次：历史回放该把思考和耗时都带回来
    if not args.no_reopen:
        process, master = spawn()
        sink2 = bytearray()
        try:
            # 重开要先连 daemon、再查库回放，中间可能安静好几秒——只等"静下来"
            # 会在回放还没画出来时就收工，抓到一屏空白。等到正文真的出现为止。
            deadline = time.time() + 90.0
            while time.time() < deadline:
                read_into(master, sink2, quiet=1.5, timeout=20.0)
                if any(is_fold_summary(line) for line in render(bytes(sink2))):
                    break
            (OUT / "screen-95-reopened.txt").write_text(
                "\n".join(render(bytes(sink2))), encoding="utf-8"
            )
            os.write(master, b"\x04")
            drain(master, 2.0, sink2)
        finally:
            (OUT / "raw-reopen.bin").write_bytes(bytes(sink2))
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
            os.close(master)
        print("  ✓ 重开了一次，看 screen-95-reopened.txt")

    if daemon.poll() is None:
        daemon.terminate()
        try:
            daemon.wait(timeout=8)
        except subprocess.TimeoutExpired:
            daemon.kill()
    if not args.keep and HOME.exists():
        shutil.rmtree(HOME, ignore_errors=True)

    if findings:
        print("\n! 版式问题：")
        for note in findings:
            print(f"  · {note}")
    else:
        print("\n✓ 版式检查：页边距没被压、图形传输段没漏进正文")

    print(f"\n产物：{OUT}")
    for path in sorted(OUT.glob("screen-*.txt")):
        print(f"  {path.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
